//! Open-document lifecycle, working files, page/text caches and undo history.
//!
//! ## The working-file model
//!
//! Every open document owns a *working file* in `%LOCALAPPDATA%\BritPDF\work`.
//! Nothing is ever written back to the user's file until they explicitly Save or
//! Save As, so a failed operation can never damage the original. Structural edits
//! (page operations, redaction, watermarks, ...) are performed by lopdf against the
//! working file and produce a *new* working file, which is then reopened through
//! PDFium. That means PDFium never has to reload a document it has already handed
//! out pointers into.

use crate::error::{BritError, ErrorKind, Result, io_err, read_file};
use crate::engine::text::ExtractedPageText;
use parking_lot::Mutex;
use pdfium_render::prelude::{PdfDocument, PdfPageIndex, PdfPoints, PdfRect};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A point on the page, in PDF points, origin top-left (viewer coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewPoint {
    pub x: f32,
    pub y: f32,
}

/// One reversible snapshot of the working file.
struct Snapshot {
    label: String,
    path: PathBuf,
    bytes: u64,
}

/// A rendered page image on disk, tracked in a byte-budgeted LRU so we never hold
/// hundreds of full resolution page bitmaps at once.
pub struct RenderCacheEntry {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub unrotated_width: u32,
    pub unrotated_height: u32,
    pub bytes: u64,
}

/// A single open document.
pub struct OpenDocument {
    pub id: String,
    pub file_name: String,
    /// The file the user opened, if any. `None` for a brand new blank document.
    pub source_path: Option<PathBuf>,
    /// The live copy of the document that we actually edit.
    pub work_path: PathBuf,
    /// The PDFium handle. Borrows the process-wide `&'static Pdfium`.
    pub document: PdfDocument<'static>,
    pub dirty: bool,
    pub is_encrypted: bool,
    /// Version string such as "1.7", straight from the PDF header.
    pub pdf_version: String,
    file_size: u64,
    /// The password this document was opened with, if any, so undo/redo can
    /// reload the working file through PDFium without prompting again.
    open_password: Option<String>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Total bytes the undo/redo stacks may occupy.
    undo_budget: u64,
    undo_used: u64,
    text_cache: HashMap<usize, ExtractedPageText>,
    render_cache: HashMap<String, RenderCacheEntry>,
    /// Bumped on every change so the UI can discard stale render requests.
    pub revision: u64,
    /// Pending viewer-only rotation, in degrees, applied on top of the page's /Rotate.
    pub view_rotation: u32,
    /// True once the document has unsaved annotation or structural edits.
    pub has_structural_changes: bool,
}

impl OpenDocument {
    pub fn page_count(&self) -> usize {
        self.document.pages().len() as usize
    }

    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// The password this document was opened with, if any. Stored so structural
    /// edits and undo/redo can reopen the working file without prompting again.
    pub fn open_password(&self) -> Option<&str> {
        self.open_password.as_deref()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Geometry of a page, in points, with `/Rotate` already applied.
    pub fn page_geometry(&self, index: usize) -> Result<crate::engine::types::PageGeometry> {
        use crate::engine::types::{PageGeometry, PageOrientation};
        let page = self
            .document
            .pages()
            .get(index as PdfPageIndex)
            .map_err(|_| BritError::new(ErrorKind::InvalidPage, format!("Page {} does not exist", index + 1)))?;
        let size: PdfRect = page.page_size();
        let rotation = self.page_rotation(index);
        let (w, h) = (size.width().value, size.height().value);
        let (rw, rh) = if rotation % 180 == 90 { (h, w) } else { (w, h) };
        Ok(PageGeometry {
            width: rw,
            height: rh,
            raw_width: w,
            raw_height: h,
            rotation,
            orientation: if rw > rh { PageOrientation::Landscape } else { PageOrientation::Portrait },
            label: page.label().map(|s| s.to_string()),
        })
    }

    fn page_rotation(&self, index: usize) -> u32 {
        use pdfium_render::prelude::PdfPageRenderRotation;
        let Ok(page) = self.document.pages().get(index as PdfPageIndex) else {
            return 0;
        };
        match page.rotation() {
            Ok(PdfPageRenderRotation::None) => 0,
            Ok(PdfPageRenderRotation::Degrees90) => 90,
            Ok(PdfPageRenderRotation::Degrees180) => 180,
            Ok(PdfPageRenderRotation::Degrees270) => 270,
            Err(_) => 0,
        }
    }

    pub fn text(&mut self, page: usize) -> Result<ExtractedPageText> {
        if let Some(entry) = self.text_cache.get(&page) {
            return Ok(entry.clone());
        }
        let entry = crate::engine::text::extract_page_text(self, page)?;
        self.text_cache.insert(page, entry.clone());
        Ok(entry)
    }

    pub fn invalidate_text(&mut self) {
        self.text_cache.clear();
    }

    pub fn put_render_cache(&mut self, key: String, entry: RenderCacheEntry) {
        self.render_cache.insert(key, entry);
    }

    pub fn render_cache_bytes(&self) -> u64 {
        self.render_cache.values().map(|e| e.bytes).sum()
    }

    pub fn render_cache_len(&self) -> usize {
        self.render_cache.len()
    }

    /// (cache key, size in bytes, on-disk path) for every live cached render.
    pub fn render_cache_paths(&self) -> Vec<(String, u64, PathBuf)> {
        self.render_cache
            .iter()
            .map(|(k, v)| (k.clone(), v.bytes, v.path.clone()))
            .collect()
    }

    pub fn drop_render_cache(&mut self, key: &str) {
        self.render_cache.remove(key);
    }

    pub fn clear_render_cache(&mut self) {
        self.render_cache.clear();
    }
}

/// Owns the set of open documents and all on-disk scratch space.
pub struct Session {
    inner: Mutex<SessionInner>,
    /// Where working files, undo snapshots, rendered pages and settings live.
    pub dirs: Dirs,
    /// Serialises operations that read-modify-write a document's working file.
    /// PDFium is internally thread safe; this mutex protects *our* invariants.
    pub op_lock: Mutex<()>,
}

struct SessionInner {
    docs: HashMap<String, OpenDocument>,
    /// Tab order; the last entry is the active tab.
    order: Vec<String>,
    active: Option<String>,
    counter: u64,
}

impl Session {
    pub fn new(dirs: Dirs) -> Self {
        Self {
            inner: Mutex::new(SessionInner {
                docs: HashMap::new(),
                order: Vec::new(),
                active: None,
                counter: 0,
            }),
            dirs,
            op_lock: Mutex::new(()),
        }
    }

    pub fn active_id(&self) -> Option<String> {
        self.inner.lock().active.clone()
    }

    pub fn set_active(&self, id: &str) {
        let mut inner = self.inner.lock();
        if inner.docs.contains_key(id) {
            inner.active = Some(id.to_string());
        }
    }

    pub fn ids(&self) -> Vec<String> {
        self.inner.lock().order.clone()
    }

    /// Run `f` with a mutable borrow of one open document.
    pub fn with_doc<T>(&self, id: &str, f: impl FnOnce(&mut OpenDocument) -> Result<T>) -> Result<T> {
        let mut inner = self.inner.lock();
        let doc = inner.docs.get_mut(id).ok_or_else(|| {
            BritError::new(ErrorKind::InvalidRequest, "That document is no longer open.")
        })?;
        f(doc)
    }

    /// Run `f` with the active document, if there is one.
    pub fn with_active<T>(&self, f: impl FnOnce(&mut OpenDocument) -> Result<T>) -> Result<T> {
        let id = self.active_id().ok_or_else(|| {
            BritError::new(ErrorKind::InvalidRequest, "No document is open.")
        })?;
        self.with_doc(&id, f)
    }

    pub fn new_id(&self) -> String {
        let mut inner = self.inner.lock();
        inner.counter += 1;
        format!("doc-{}", inner.counter)
    }

    /// Register a freshly opened document as the active tab.
    pub fn register(&self, doc: OpenDocument) {
        let mut inner = self.inner.lock();
        let id = doc.id.clone();
        inner.order.push(id.clone());
        inner.docs.insert(id.clone(), doc);
        inner.active = Some(id);
    }

    /// Close a document, deleting its working file and undo history.
    pub fn close(&self, id: &str) -> Result<()> {
        let mut inner = self.inner.lock();
        let doc = match inner.docs.remove(id) {
            Some(d) => d,
            None => return Ok(()),
        };
        inner.order.retain(|x| x != id);
        if inner.active.as_deref() == Some(id) {
            inner.active = inner.order.last().cloned();
        }
        drop(doc);
        let _ = std::fs::remove_file(&self.dirs.work_path(id));
        let _ = std::fs::remove_dir_all(self.dirs.undo_dir(id));
        let _ = std::fs::remove_dir_all(self.dirs.render_dir(id));
        Ok(())
    }

    pub fn close_all(&self) {
        let ids = self.ids();
        for id in ids {
            let _ = self.close(&id);
        }
    }

    /// Build an `OpenDocument` from bytes on disk.
    pub fn open_from_path(
        &self,
        id: String,
        path: &Path,
        password: Option<&str>,
    ) -> Result<OpenDocument> {
        let bytes = read_file(path)?;
        Self::open_from_bytes(&self.dirs, id, bytes, Some(path.to_path_buf()), password)
    }

    /// Build an `OpenDocument` from an in-memory PDF.
    pub fn open_from_bytes(
        dirs: &Dirs,
        id: String,
        bytes: Vec<u8>,
        source_path: Option<PathBuf>,
        password: Option<&str>,
    ) -> Result<OpenDocument> {
        let pdfium = super::pdfium()?;
        let file_name = source_path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "Untitled.pdf".to_string());

        let document = match pdfium.load_pdf_from_byte_vec(bytes.clone(), password) {
            Ok(doc) => doc,
            Err(pdfium_render::prelude::PdfiumError::PdfiumLibraryInternalError(
                pdfium_render::prelude::PdfiumInternalError::PasswordError,
            )) => {
                return Err(BritError::new(
                    ErrorKind::PasswordRequired,
                    "This PDF is password protected.",
                )
                .with_hint("Enter the document's open password to continue."))
            }
            Err(e) => {
                return Err(classify_pdfium_load_error(&e, &file_name));
            }
        };

        // An empty password is still needed to open many user-password documents,
        // so probe: if the document reports itself encrypted but PDFium gave us a
        // usable handle, treat it as owner-restricted rather than locked.
        let is_encrypted = detect_encryption(&bytes);

        let work_path = dirs.work_path(&id);
        std::fs::create_dir_all(dirs.work_dir()).map_err(|e| io_err("Could not create", &dirs.work_dir(), &e))?;
        write_file_atomically(&work_path, &bytes)
            .map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("Could not write the working file: {e}")))?;

        let pdf_version = detect_pdf_version(&bytes);
        let file_size = bytes.len() as u64;

        Ok(OpenDocument {
            id,
            file_name,
            source_path,
            work_path,
            document,
            dirty: false,
            is_encrypted,
            pdf_version,
            file_size,
            open_password: password.map(str::to_string),
            undo: Vec::new(),
            redo: Vec::new(),
            undo_budget: 192 * 1024 * 1024,
            undo_used: 0,
            text_cache: HashMap::new(),
            render_cache: HashMap::new(),
            revision: 0,
            view_rotation: 0,
            has_structural_changes: false,
        })
    }

    /// Push an undo snapshot of the *current* state before a destructive change.
    pub fn snapshot(doc: &mut OpenDocument, dirs: &Dirs, label: &str) -> Result<()> {
        let bytes = read_file(&doc.work_path)?;
        let dir = dirs.undo_dir(&doc.id);
        std::fs::create_dir_all(&dir).map_err(|e| io_err("Could not create", &dir, &e))?;
        let index = doc.undo.len();
        let path = dir.join(format!("{index:04}.pdf"));
        write_file_atomically(&path, &bytes).map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("Could not write the working file: {e}")))?;

        doc.undo_used += bytes.len() as u64;
        // Make room: drop the oldest snapshots, and the whole redo branch, if the
        // budget is blown. Losing undo history is far better than running the user's
        // machine out of memory.
        while doc.undo_used > doc.undo_budget && doc.undo.len() > 1 {
            // `remove(0)` always succeeds here, because the loop condition guarantees
            // at least one entry remains.
            if let Some(old) = doc.undo.first() {
                let old_path = old.path.clone();
                let old_bytes = old.bytes;
                doc.undo.remove(0);
                doc.undo_used = doc.undo_used.saturating_sub(old_bytes);
                let _ = std::fs::remove_file(old_path);
            }
        }
        // Snapshots no longer reachable from either stack are dead weight: clear the
        // redo branch and delete its files so undo history cannot fill the disk.
        for stale in doc.redo.drain(..) {
            let _ = std::fs::remove_file(stale.path);
        }
        doc.undo.push(Snapshot { label: label.to_string(), path, bytes: bytes.len() as u64 });
        Ok(())
    }

    /// Undo the most recent change: restore the last snapshot into the working file.
    pub fn undo(doc: &mut OpenDocument) -> Result<String> {
        let Some(snapshot) = doc.undo.pop() else {
            return Err(BritError::new(ErrorKind::InvalidRequest, "There is nothing to undo."));
        };
        // Save the current state onto the redo branch first.
        let current = read_file(&doc.work_path)?;
        let dir = doc.undo_dir_placeholder();
        let _ = std::fs::create_dir_all(&dir);
        let redo_path = dir.join(format!("redo-{}.pdf", doc.undo.len()));
        write_file_atomically(&redo_path, &current)
            .map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("Could not write the working file: {e}")))?;
        doc.undo_used = doc.undo_used.saturating_sub(snapshot.bytes);
        doc.redo.push(Snapshot {
            label: snapshot.label.clone(),
            path: redo_path,
            bytes: current.len() as u64,
        });
        let label = snapshot.label.clone();
        let restored = read_file(&snapshot.path)?;
        write_file_atomically(&doc.work_path, &restored)
            .map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("Could not write the working file: {e}")))?;
        Ok(label)
    }

    /// Redo the most recently undone change.
    pub fn redo(doc: &mut OpenDocument) -> Result<String> {
        let Some(snapshot) = doc.redo.pop() else {
            return Err(BritError::new(ErrorKind::InvalidRequest, "There is nothing to redo."));
        };
        let current = read_file(&doc.work_path)?;
        let dir = doc.undo_dir_placeholder();
        let _ = std::fs::create_dir_all(&dir);
        let undo_path = dir.join(format!("undo-{}.pdf", doc.undo.len()));
        write_file_atomically(&undo_path, &current)
            .map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("Could not write the working file: {e}")))?;
        doc.undo.push(Snapshot {
            label: snapshot.label.clone(),
            path: undo_path,
            bytes: current.len() as u64,
        });
        let label = snapshot.label.clone();
        let restored = read_file(&snapshot.path)?;
        write_file_atomically(&doc.work_path, &restored)
            .map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("Could not write the working file: {e}")))?;
        Ok(label)
    }

    /// Re-open the working file through PDFium so the live document matches the
    /// bytes on disk after an undo/redo restored a different version. The old
    /// PDFium handle is dropped when `self.document` is reassigned.
    pub fn reload_working(&self, id: &str) -> Result<()> {
        self.with_doc(id, |doc| {
            let pdfium = super::pdfium()?;
            let bytes = read_file(&doc.work_path)?;
            let document = pdfium
                .load_pdf_from_byte_vec(bytes.clone(), doc.open_password.as_deref())
                .map_err(|_| {
                    BritError::new(
                        ErrorKind::CorruptDocument,
                        "The document could not be reloaded after this change.",
                    )
                })?;
            doc.document = document;
            doc.file_size = bytes.len() as u64;
            doc.pdf_version = detect_pdf_version(&bytes);
            doc.text_cache.clear();
            doc.render_cache.clear();
            doc.revision += 1;
            Ok(())
        })
    }
}

impl OpenDocument {
    /// We only need a path to write redo/undo branches; reuse the work dir's parent.
    fn undo_dir_placeholder(&self) -> PathBuf {
        self.work_path
            .parent()
            .map(|p| p.join("undo").join(&self.id))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Classify a PDFium load failure into a user-actionable category.
pub fn classify_pdfium_load_error(err: &pdfium_render::prelude::PdfiumError, name: &str) -> BritError {
    use pdfium_render::prelude::{PdfiumError, PdfiumInternalError};
    let internal = match err {
        PdfiumError::PdfiumLibraryInternalError(e) => Some(e),
        _ => None,
    };
    match internal {
        Some(PdfiumInternalError::FileError) => BritError::new(
            ErrorKind::FileNotFound,
            format!("{name} could not be read."),
        )
        .with_hint("The file may have been moved or is in use by another program."),
        Some(PdfiumInternalError::PasswordError) => BritError::new(
            ErrorKind::PasswordRequired,
            format!("{name} is password protected."),
        )
        .with_hint("Enter the document's open password to continue."),
        Some(PdfiumInternalError::SecurityError) => BritError::new(
            ErrorKind::PasswordRequired,
            format!("The security settings of {name} are not supported."),
        )
        .with_hint("Ask the sender for an unprotected copy."),
        Some(PdfiumInternalError::FormatError) => BritError::new(
            ErrorKind::CorruptDocument,
            format!("Unable to open {name}."),
        )
        .with_hint(
            "The file may be damaged, encrypted, or use an unsupported feature. \
             If it opens in another reader, please report it.",
        ),
        _ => BritError::new(
            ErrorKind::CorruptDocument,
            format!("Unable to open {name}."),
        )
        .with_hint(
            "The file may be damaged, encrypted, or use an unsupported feature. \
             If it opens in another reader, please report it.",
        ),
    }
}

/// Cheap, non-cryptographic sniff for the `/Encrypt` entry in a PDF's trailer.
/// Deliberately conservative: false negatives just mean we do not show the
/// "restricted" badge; false positives would annoy users.
fn detect_encryption(bytes: &[u8]) -> bool {
    if let Ok(text) = std::str::from_utf8(bytes) {
        if text.contains("/Encrypt") {
            return true;
        }
    }
    // The trailer dictionary may live inside a compressed object stream, in which
    // case the string is not visible. PDFium still opens the file, so this is only
    // a hint for the UI.
    false
}

/// Read the `%PDF-x.y` header. We report exactly what the file claims, and never
/// invent a version when the header is missing or malformed.
fn detect_pdf_version(bytes: &[u8]) -> String {
    let head = &bytes[..bytes.len().min(1024)];
    if let Some(pos) = head.windows(5).position(|w| w == b"%PDF-") {
        let rest = &head[pos + 5..];
        let mut digits = String::new();
        for b in rest {
            if b.is_ascii_digit() || *b == b'.' {
                digits.push(*b as char);
            } else {
                break;
            }
        }
        if let Some(major) = digits.split('.').next() {
            if major.parse::<u8>().is_ok() {
                return digits.trim_end_matches('.').to_string();
            }
        }
    }
    "unknown".to_string()
}

/// Filesystem layout for all of BritPDF's own state.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub data: PathBuf,
}

impl Dirs {
    pub fn new(data: PathBuf) -> Self {
        Self { data }
    }

    pub fn work_dir(&self) -> PathBuf {
        self.data.join("work")
    }
    pub fn undo_dir(&self, id: &str) -> PathBuf {
        self.data.join("undo").join(id)
    }
    pub fn render_dir(&self, id: &str) -> PathBuf {
        self.data.join("renders").join(id)
    }
    pub fn renders_root(&self) -> PathBuf {
        self.data.join("renders")
    }
    pub fn recovery_dir(&self) -> PathBuf {
        self.data.join("recovery")
    }
    pub fn settings_file(&self) -> PathBuf {
        self.data.join("settings.json")
    }
    pub fn recents_file(&self) -> PathBuf {
        self.data.join("recents.json")
    }
    pub fn crash_file(&self) -> PathBuf {
        self.data.join("recovery").join("unsaved.json")
    }
    pub fn work_path(&self, id: &str) -> PathBuf {
        self.work_dir().join(format!("{id}.pdf"))
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            self.work_dir(),
            self.undo_dir("x"),
            self.renders_root(),
            self.recovery_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        // Best-effort cleanup of stale working files left by a crash. Files younger
        // than an hour may belong to a second running instance, so we leave them.
        if let Ok(entries) = std::fs::read_dir(self.work_dir()) {
            for entry in entries.flatten() {
                let stale = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t.elapsed().map(|d| d.as_secs() > 3600).unwrap_or(false))
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }
}

/// Write a file atomically: write to a sibling temp file, flush it to disk, then
/// move it into place over the destination.
///
/// On Windows `std::fs::rename` is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`,
/// so the destination is replaced in one step and an interrupted save can never
/// leave a half-written PDF where the user's document should be. If the move still
/// fails - typically because the destination is open in another program, or a virus
/// scanner has it locked - the temp file is cleaned up and the original is left
/// untouched, so the failure is safe and repeatable.
pub fn write_file_atomically(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        dest.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        // Without this the rename can land before the bytes reach the disk, which
        // would defeat the point of writing atomically.
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Convert a viewer-space point (points, top-left origin) into lopdf/PDF user space
/// (points, bottom-left origin) for a page of the given size.
pub fn to_pdf_space(x: f32, y: f32, page_height: f32) -> (f32, f32) {
    (x, page_height - y)
}

/// Convert a PDF-space point back to viewer space.
pub fn to_view_space(x: f32, y: f32, page_height: f32) -> (f32, f32) {
    (x, page_height - y)
}

/// A viewer-space rectangle as used by annotations and redactions.
pub fn rect_to_pdf_space(rect: crate::engine::types::Rect, page_height: f32) -> crate::engine::types::Rect {
    let (left, bottom) = to_pdf_space(rect.left, rect.bottom, page_height);
    let (right, top) = to_pdf_space(rect.right, rect.top, page_height);
    crate::engine::types::Rect::new(left, top, right, bottom)
}

/// Convenience: PDFium's `PdfPoints` -> plain f32.
pub fn pts(v: PdfPoints) -> f32 {
    v.value
}
