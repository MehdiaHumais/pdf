//! Document commands: open, list, close, render and text extraction.
//!
//! Every command here is a thin boundary over `engine::session`. Nothing is
//! simulated: rendering actually rasterises the page via PDFium, and text is the
//! real PDF text layer. A page without a text layer comes back as image-only and
//! the UI says so instead of pretending it has text.

use crate::engine::session::{write_file_atomically, OpenDocument};
use crate::engine::types::{
    DocumentMetadata, PageGeometry, PageOrientationRequest, PaperSize, PrintPageSpec, PrintResult,
    PrintScaling, RenderedPage,
};
use crate::engine::{render, text};
use crate::error::Result;
use crate::SharedState;
use pdfium_render::prelude::PdfDocumentMetadataTagType;
use serde::Serialize;
use std::path::PathBuf;

/// Everything a document list row or an About/Info panel needs, drawn from real
/// engine state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocSummary {
    pub id: String,
    pub file_name: String,
    /// The file the user opened, or `None` for a brand-new blank document.
    pub path: Option<String>,
    pub file_size: u64,
    pub page_count: usize,
    pub pdf_version: String,
    pub is_encrypted: bool,
    /// True once the document has unsaved edits (incl. unsaved structural changes).
    pub is_modified: bool,
    pub revision: u64,
    pub metadata: DocumentMetadata,
    /// The page label of the first page, when the document declares one.
    pub first_page_label: Option<String>,
    /// Whether there is anything left to undo/redo (drives the enabled state of
    /// the Undo/Redo buttons, exactly like a native editor).
    pub can_undo: bool,
    pub can_redo: bool,
    pub undo_label: Option<String>,
    pub redo_label: Option<String>,
}

pub(super) fn doc_summary(doc: &OpenDocument) -> Result<DocSummary> {
    let meta = doc.document.metadata();
    let mut metadata = DocumentMetadata::empty();
    let pick = |tag: PdfDocumentMetadataTagType| Some(meta.get(tag)?.value().to_string());
    metadata.title = pick(PdfDocumentMetadataTagType::Title);
    metadata.author = pick(PdfDocumentMetadataTagType::Author);
    metadata.subject = pick(PdfDocumentMetadataTagType::Subject);
    metadata.keywords = pick(PdfDocumentMetadataTagType::Keywords);
    metadata.creator = pick(PdfDocumentMetadataTagType::Creator);
    metadata.producer = pick(PdfDocumentMetadataTagType::Producer);
    metadata.creation_date = pick(PdfDocumentMetadataTagType::CreationDate);
    metadata.modification_date = pick(PdfDocumentMetadataTagType::ModificationDate);

    let first_page_label = if doc.page_count() > 0 {
        doc.page_geometry(0).ok().and_then(|g| g.label)
    } else {
        None
    };

    Ok(DocSummary {
        id: doc.id.clone(),
        file_name: doc.file_name.clone(),
        path: doc.source_path.as_ref().map(|p| p.display().to_string()),
        file_size: doc.file_size(),
        page_count: doc.page_count(),
        pdf_version: doc.pdf_version.clone(),
        is_encrypted: doc.is_encrypted,
        is_modified: doc.dirty || doc.has_structural_changes,
        revision: doc.revision,
        metadata,
        first_page_label,
        can_undo: doc.can_undo(),
        can_redo: doc.can_redo(),
        undo_label: doc.undo_label().map(str::to_string),
        redo_label: doc.redo_label().map(str::to_string),
    })
}

fn do_open(state: &SharedState<'_>, path: &str, password: Option<&str>) -> Result<DocSummary> {
    let p = PathBuf::from(path);
    let id = state.session.new_id();
    let opened = state.session.open_from_path(id, &p, password)?;
    let summary = doc_summary(&opened)?;
    let page_count = opened.page_count();
    let file_size = opened.file_size();
    let source = opened.source_path.clone();
    state.session.register(opened);
    if let Some(src) = source {
        let _ = state.system.touch_recent(&src, Some(page_count as u32), Some(file_size), None);
    }
    Ok(summary)
}

/// Open a PDF from disk. A document that requests a password fails with
/// `PasswordRequired`; the UI then prompts and calls `open_document_with_password`.
#[tauri::command]
pub fn open_document(state: SharedState, path: String) -> Result<DocSummary> {
    do_open(&state, &path, None)
}

#[tauri::command]
pub fn open_document_with_password(state: SharedState, path: String, password: String) -> Result<DocSummary> {
    do_open(&state, &path, Some(&password))
}

/// Every currently open document, most-recently-activated first.
#[tauri::command]
pub fn list_documents(state: SharedState) -> Result<Vec<DocSummary>> {
    let mut out = Vec::new();
    for id in state.session.ids() {
        if let Ok(summary) = state.session.with_doc(&id, |doc| doc_summary(doc)) {
            out.push(summary);
        }
    }
    Ok(out)
}

#[tauri::command]
pub fn doc_info(state: SharedState, id: String) -> Result<DocSummary> {
    state.session.with_doc(&id, |doc| doc_summary(doc))
}

#[tauri::command]
pub fn close_document(state: SharedState, id: String) -> Result<()> {
    state.session.close(&id)?;
    Ok(())
}

/// Page geometries for the whole document, one entry per page.
#[tauri::command]
pub fn list_pages(state: SharedState, id: String) -> Result<Vec<PageGeometry>> {
    state.session.with_doc(&id, |doc| {
        let n = doc.page_count();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(doc.page_geometry(i)?);
        }
        Ok(out)
    })
}

/// Rasterise one page at the requested display width (device pixels).
#[tauri::command]
pub fn render_page(
    state: SharedState,
    id: String,
    page: usize,
    width: u32,
    rotation: Option<u32>,
    grayscale: Option<bool>,
) -> Result<RenderedPage> {
    let dirs = state.session.dirs.clone();
    state.session.with_doc(&id, |doc| {
        render::render_page(
            doc,
            &dirs,
            render::RenderRequest {
                page,
                width,
                rotation: rotation.unwrap_or(0),
                render_annotations: true,
                render_forms: true,
                grayscale: grayscale.unwrap_or(false),
                max_area: Some(20_000_000),
            },
        )
    })
}

/// Undo the most recent change and reload the live document so the canvas
/// shows the restored version.
#[tauri::command]
pub fn undo_document(state: SharedState, id: String) -> Result<DocSummary> {
    state.session.with_doc(&id, |doc| {
        let label = crate::engine::session::Session::undo(doc)?;
        doc.dirty = true;
        doc.has_structural_changes = true;
        let _ = &label;
        Ok(())
    })?;
    state.session.reload_working(&id)?;
    state.session.with_doc(&id, |d| doc_summary(d))
}

/// Redo the most recently undone change and reload the live document.
#[tauri::command]
pub fn redo_document(state: SharedState, id: String) -> Result<DocSummary> {
    state.session.with_doc(&id, |doc| {
        let label = crate::engine::session::Session::redo(doc)?;
        doc.dirty = true;
        doc.has_structural_changes = true;
        let _ = &label;
        Ok(())
    })?;
    state.session.reload_working(&id)?;
    state.session.with_doc(&id, |d| doc_summary(d))
}

/// Print pages of the document to a printer. `pages` is 1-based; empty means
/// every page. Printing rasterises each page at the printer's own resolution
/// and goes straight to the spooler — no re-rendering through the viewer.
#[tauri::command]
pub fn print_document(
    state: SharedState,
    id: String,
    printer: Option<String>,
    pages: Option<Vec<u32>>,
    copies: Option<u32>,
) -> Result<PrintResult> {
    let printer_name = match printer {
        Some(name) if !name.trim().is_empty() => name,
        _ => crate::native::printing::default_printer_name().ok_or_else(|| {
            crate::error::BritError::new(
                crate::error::ErrorKind::PrinterUnavailable,
                "No printer is installed.",
            )
            .with_hint("Install a printer in Windows Settings, then try again.")
        })?,
    };

    let file_name = state
        .session
        .with_doc(&id, |doc| Ok(doc.file_name.clone()))?;

    state.session.with_doc(&id, |doc| {
        let page_count = doc.page_count();
        let page_list: Vec<usize> = match pages {
            Some(list) if !list.is_empty() => list
                .iter()
                .map(|p| (*p).saturating_sub(1) as usize)
                .filter(|p| *p < page_count)
                .collect(),
            _ => (0..page_count).collect(),
        };
        if page_list.is_empty() {
            return Err(crate::error::BritError::new(
                crate::error::ErrorKind::InvalidPage,
                "There are no pages to print.",
            ));
        }
        let copies = copies.unwrap_or(1).clamp(1, 99);

        let spec = PrintPageSpec {
            pages: page_list.iter().map(|p| p + 1).collect(),
            copies,
            orientation: PageOrientationRequest::Auto,
            paper: PaperSize::A4,
            scaling: PrintScaling::FitToPage,
            color: false,
            duplex: false,
            title: Some(file_name.clone()),
        };

        let job = crate::native::printing::open_job(&printer_name, &spec)?;
        job.start(&file_name)?;

        let geo = job.geometry;
        // Rasterise each page into the printable area at the driver's own DPI.
        let dpi_x = geo.driver_dpi_x.max(96) as f32 / 72.0;
        let dpi_y = geo.driver_dpi_y.max(96) as f32 / 72.0;
        let mut sheets = 0usize;
        for &page in &page_list {
            let page_geo = doc.page_geometry(page)?;
            let scale = (geo.printable_width_pt / page_geo.width.max(1.0) as f32)
                .min(geo.printable_height_pt / page_geo.height.max(1.0) as f32);
            let target_w = ((page_geo.width as f32 * scale) * dpi_x).round().clamp(64.0, 8192.0) as u32;
            let target_h = ((page_geo.height as f32 * scale) * dpi_y).round().clamp(64.0, 8192.0) as u32;
            let rgba = render::rasterise_to_rgba(doc, page, target_w, target_h, 0, true)?;
            for _ in 0..copies {
                job.print_page(rgba.as_raw(), target_w, target_h)?;
                sheets += 1;
            }
        }

        Ok(PrintResult {
            job_name: file_name.clone(),
            pages_sent: page_list.len(),
            copies: copies as usize,
            sheets,
        })
    })
}

/// A small cached raster of one page for the thumbnail panel.
#[tauri::command]
pub fn render_thumbnail(
    state: SharedState,
    id: String,
    page: usize,
    max_width: u32,
) -> Result<RenderedPage> {
    let dirs = state.session.dirs.clone();
    state.session.with_doc(&id, |doc| {
        render::render_thumbnail(doc, &dirs, page, max_width)
    })
}

fn do_save(state: &SharedState<'_>, id: &str, target: Option<&str>) -> Result<DocSummary> {
    let dirs = state.session.dirs.clone();
    let work = dirs.work_path(id);
    let bytes = std::fs::read(&work).map_err(|e| {
        crate::error::BritError::new(
            crate::error::ErrorKind::AccessDenied,
            format!("Could not read the working file for saving: {e}"),
        )
    })?;
    if let Some(path) = target {
        let dest = PathBuf::from(path);
        write_file_atomically(&dest, &bytes).map_err(|e| {
            crate::error::BritError::new(
                crate::error::ErrorKind::WriteFailed,
                format!("Could not write the target file: {e}"),
            )
        })?;
        state.session.with_doc(id, |doc| {
            doc.source_path = Some(dest.clone());
            doc.file_name = dest
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| doc.file_name.clone());
            doc.dirty = false;
            doc.has_structural_changes = false;
            doc_summary(doc)
        })
    } else {
        state.session.with_doc(id, |doc| {
            let dest = doc.source_path.clone().ok_or_else(|| {
                crate::error::BritError::new(
crate::error::ErrorKind::InvalidRequest,
            "This document has no path to save to; use Save As.",
                )
            })?;
            write_file_atomically(&dest, &bytes).map_err(|e| {
                crate::error::BritError::new(
                    crate::error::ErrorKind::WriteFailed,
                    format!("Could not write the target file: {e}"),
                )
            })?;
            doc.dirty = false;
            doc.has_structural_changes = false;
            doc_summary(doc)
        })
    }
}

/// Save the current working file back over the file it was opened from.
#[tauri::command]
pub fn save_document(state: SharedState, id: String) -> Result<DocSummary> {
    do_save(&state, &id, None)
}

/// Save the current working file to a new location and keep editing that one.
#[tauri::command]
pub fn save_document_as(state: SharedState, id: String, path: String) -> Result<DocSummary> {
    do_save(&state, &id, Some(&path))
}

/// The real text layer of one page (`text` empty + `contentKind` imageOnly when
/// the page is a scan with no characters).
#[tauri::command]
pub fn page_text(state: SharedState, id: String, page: usize) -> Result<text::ExtractedPageText> {
    state.session.with_doc(&id, |doc| doc.text(page))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentText {
    pub text: String,
    pub characters: usize,
}

/// Concatenated text of the whole document (used by the search sidebar).
#[tauri::command]
pub fn document_text(state: SharedState, id: String) -> Result<DocumentText> {
    let (text, characters) = state.session.with_doc(&id, text::document_text)?;
    Ok(DocumentText { text, characters })
}