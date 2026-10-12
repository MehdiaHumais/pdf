//! The BritPDF PDF engine.
//!
//! Division of labour between the two mature libraries we use:
//!
//! * **PDFium** (BSD-3-Clause, the renderer that ships inside Chromium/Edge) via
//!   `pdfium-render`: opening documents, page geometry, rasterisation, the text
//!   layer, text search, and *reading back* annotations. PDFium is the reference
//!   implementation for PDF rendering fidelity, and it is what makes "what you see
//!   is what you print" true on Windows.
//! * **lopdf** (MIT): everything that changes the *structure* of a document - page
//!   trees, merge/split/extract/reorder/rotate, metadata, encryption, redaction,
//!   watermarks, headers/footers, compression and PDF authoring. PDFium cannot
//!   delete or reorder pages, and cannot encrypt, so lopdf owns those jobs.
//!
//! The two never fight over the same file: every structural edit is written to a
//! fresh working file, which is then reopened through PDFium.

pub mod annotations;
pub mod authoring;
pub mod ocr;
pub mod render;
pub mod search;
pub mod session;
pub mod structure;
pub mod text;
pub mod types;
pub mod writer;

use crate::error::{BritError, ErrorKind, Result};
use pdfium_render::prelude::Pdfium;
use std::path::Path;
use std::sync::OnceLock;

/// The process-wide PDFium handle.
///
/// `pdfium-render`'s `thread_safe` feature already funnels every `FPDF_*` call
/// through a process-wide mutex (PDFium itself is not thread safe), so holding a
/// single leaked handle is both necessary and sufficient. The handle is leaked on
/// purpose: open documents borrow it for `'static`, which lets a document live in a
/// map without self-referential lifetimes.
static PDFIUM: OnceLock<&'static Pdfium> = OnceLock::new();

/// Names of the vendored PDFium dynamic library, per platform.
fn candidate_library_paths() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    // Next to the executable: the packaged location used by the installer.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.to_path_buf());
        }
    }
    // The dev build: target/{profile}/ from the workspace root.
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    if let Some(dir) = &exe_dir {
        let mut cur = dir.to_path_buf();
        for _ in 0..4 {
            if cur.join("vendor").join("pdfium").join("bin").is_dir() {
                out.push(cur.join("vendor").join("pdfium").join("bin"));
            }
            match cur.parent() {
                Some(p) => cur = p.to_path_buf(),
                None => break,
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("vendor").join("pdfium").join("bin"));
        out.push(cwd.join("src-tauri").join("vendor").join("pdfium").join("bin"));
    }
    out
}

/// Initialise PDFium once, binding to the vendored copy of the library.
///
/// Binding order: the vendored `pdfium.dll` next to the executable, then any
/// `PDFIUM_LIBRARY_PATH` override, then a system-wide copy. A system-wide copy is
/// never preferred over the vendored one, because a mismatched version would give
/// unpredictable rendering.
pub fn pdfium() -> Result<&'static Pdfium> {
    if let Some(handle) = PDFIUM.get() {
        return Ok(handle);
    }
    let mut attempts: Vec<(String, pdfium_render::prelude::PdfiumError)> = Vec::new();
    for dir in candidate_library_paths() {
        let path = Pdfium::pdfium_platform_library_name_at_path(&dir);
        if !Path::new(&path).exists() {
            continue;
        }
        match Pdfium::bind_to_library(&path) {
            Ok(bindings) => {
                let leaked: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
                let _ = PDFIUM.set(leaked);
                return Ok(PDFIUM.get().expect("just set"));
            }
            Err(e) => attempts.push((path.display().to_string(), e)),
        }
    }
    if let Ok(custom) = std::env::var("BRITPDF_PDFIUM_PATH") {
        let path = Pdfium::pdfium_platform_library_name_at_path(&custom);
        match Pdfium::bind_to_library(&path) {
            Ok(bindings) => {
                let leaked: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
                let _ = PDFIUM.set(leaked);
                return Ok(PDFIUM.get().expect("just set"));
            }
            Err(e) => attempts.push((custom.clone(), e)),
        }
    }
    if let Ok(bindings) = Pdfium::bind_to_system_library() {
        let leaked: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
        let _ = PDFIUM.set(leaked);
        return Ok(PDFIUM.get().expect("just set"));
    }

let detail = attempts
        .iter()
        .map(|(p, e)| format!("{}: {}", p, e))
        .collect::<Vec<_>>()
        .join("; ");
    Err(BritError::new(
        ErrorKind::EngineUnavailable,
        "The PDF rendering engine (PDFium) could not be loaded.",
    )
    .with_hint(if detail.is_empty() {
        "Reinstall BritPDF, or set BRITPDF_PDFIUM_PATH to the folder containing pdfium.dll.".into()
    } else {
        format!("Tried: {detail}")
    }))
}

/// Human-readable description of the loaded engine, for the About screen.
/// Returns `(engine name, engine version)`.
pub fn engine_info() -> (String, String) {
    match pdfium() {
        Ok(_) => (
            "PDFium".to_string(),
            format!("Chromium PDFium ABI {PDFIUM_ABI} (via pdfium-render {PDFIUM_RENDER_VERSION})"),
        ),
        Err(e) => ("PDFium (unavailable)".to_string(), e.message),
    }
}

/// The PDFium ABI this build of BritPDF is tested against. It must match the
/// `pdfium.dll` vendored under `src-tauri/vendor/pdfium/bin`.
pub const PDFIUM_ABI: u32 = 7881;
/// The `pdfium-render` crate version compiled in, mirrored from `Cargo.toml`.
pub const PDFIUM_RENDER_VERSION: &str = "0.9.4";
