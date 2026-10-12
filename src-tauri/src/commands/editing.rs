//! Editing commands: annotations, page surgery and page-content decoration.
//!
//! Every command that changes the document follows the same flow - snapshot
//! the current working file, load it through lopdf, apply the change, write it
//! back atomically, and reopen through PDFium - so undo/redo and the live
//! canvas stay consistent with what is actually on disk.

use crate::engine::authoring::EncodedImage;
use crate::engine::session::write_file_atomically;
use crate::engine::types::{AnnotationSpec, HeaderFooterOptions, PageAnnotation, WatermarkOptions, WatermarkPosition};
use crate::engine::writer::{append_page_content, ensure_alpha_state, ensure_font, ContentBuilder};
use crate::engine::{structure, writer};
use crate::error::{BritError, ErrorKind, Result};
use crate::SharedState;
use lopdf::ObjectId;
use serde::Serialize;

use super::documents::{doc_summary, DocSummary};

/// Where page-number labels are placed on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageNumberPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// The label templates offered by the Insert ribbon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageNumberFormat {
    Plain,
    Dashed,
    Slashed,
    PageXofY,
}

/// Result of saving extracted pages to a new file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResult {
    pub path: String,
    pub pages: usize,
}

/// The snapshot â†’ lopdf-edit â†’ write â†’ reopen pipeline every document-changing
/// command uses.
fn apply_edit<F>(state: &SharedState<'_>, id: &str, label: &str, edit: F) -> Result<DocSummary>
where
    F: FnOnce(&mut lopdf::Document) -> Result<()>,
{
    let dirs = state.session.dirs.clone();
    state.session.with_doc(id, |doc| {
        crate::engine::session::Session::snapshot(doc, &dirs, label)
    })?;

    let work = dirs.work_path(id);
    let bytes = std::fs::read(&work).map_err(|e| {
        BritError::new(
            ErrorKind::AccessDenied,
            format!("Could not read the working file for this change: {e}"),
        )
    })?;
    let password = state.session.with_doc(id, |doc| Ok(doc.open_password().map(str::to_string)))?;

    let mut doc = structure::load(&bytes, password.as_deref())?;
    edit(&mut doc)?;
    let out = writer::serialise(&mut doc, true)?;
    write_file_atomically(&work, &out).map_err(|e| {
        BritError::new(
            ErrorKind::WriteFailed,
            format!("Could not write the working file: {e}"),
        )
    })?;

    state.session.with_doc(id, |doc| {
        doc.dirty = true;
        doc.has_structural_changes = true;
        Ok(())
    })?;
    state.session.reload_working(id)?;
    state.session.with_doc(id, |doc| doc_summary(doc))
}

/// Save the user's in-progress annotations (the drawing overlay) into the PDF
/// as real annotation objects, then reload so the canvas shows them embedded.
#[tauri::command]
pub fn apply_annotations(state: SharedState, id: String, annotations: Vec<AnnotationSpec>) -> Result<DocSummary> {
    if annotations.is_empty() {
        return state.session.with_doc(&id, |doc| doc_summary(doc));
    }
    let label = if annotations.len() == 1 {
        "Add annotation".to_string()
    } else {
        format!("Add {} annotations", annotations.len())
    };
    apply_edit(&state, &id, &label, |doc| {
        crate::engine::annotations::apply(doc, &annotations)
    })
}

/// Read every annotation embedded in the working file, so the drawing overlay
/// can show annotations that are already part of the PDF after a reopen.
#[tauri::command]
pub fn get_annotations(state: SharedState, id: String) -> Result<Vec<PageAnnotation>> {
    let work = state.session.dirs.work_path(&id);
    let bytes = std::fs::read(&work).map_err(|e| {
        BritError::new(
            ErrorKind::AccessDenied,
            format!("Could not read the working file: {e}"),
        )
    })?;
    let password = state.session.with_doc(&id, |doc| Ok(doc.open_password().map(str::to_string)))?;
    let doc = structure::load(&bytes, password.as_deref())?;
    crate::engine::annotations::read(&doc)
}

/// Insert a blank page before `before` (0-based), sized like the page that
/// follows it.
#[tauri::command]
pub fn insert_blank_page(state: SharedState, id: String, before: usize) -> Result<DocSummary> {
    apply_edit(&state, &id, "Insert blank page", |doc| {
        let sizes = structure::page_sizes(doc);
        let (w, h) = match sizes.as_slice() {
            [] => (612.0, 792.0),
            [only] => *only,
            list => *list.get(before.min(list.len() - 1)).unwrap_or(&(612.0, 792.0)),
        };
        structure::insert_blank_page(doc, before, w, h)
    })
}

/// Import every page of the PDF at `path` before `before` (0-based).
#[tauri::command]
pub fn insert_pdf_from(state: SharedState, id: String, path: String, before: usize) -> Result<DocSummary> {
    let raw = std::fs::read(&path).map_err(|e| {
        BritError::new(
            ErrorKind::AccessDenied,
            format!("Could not read the file to insert: {e}"),
        )
    })?;
    let source = structure::load(&raw, None)?;
    apply_edit(&state, &id, "Insert file", |doc| {
        structure::insert_pages_from(doc, before, &source)
    })
}

/// Duplicate one page (0-based) directly after itself.
#[tauri::command]
pub fn duplicate_page(state: SharedState, id: String, page: usize) -> Result<DocSummary> {
    apply_edit(&state, &id, "Duplicate page", |doc| {
        structure::duplicate_page(doc, page + 1)?;
        Ok(())
    })
}

/// Delete several pages (0-based).
#[tauri::command]
pub fn delete_pages(state: SharedState, id: String, pages: Vec<usize>) -> Result<DocSummary> {
    if pages.is_empty() {
        return state.session.with_doc(&id, |doc| doc_summary(doc));
    }
    let label = if pages.len() == 1 {
        "Delete page".to_string()
    } else {
        format!("Delete {} pages", pages.len())
    };
    apply_edit(&state, &id, &label, |doc| {
        let mut one_based: Vec<usize> = pages.iter().map(|p| p + 1).collect();
        one_based.sort_unstable();
        one_based.dedup();
        structure::delete_pages(doc, &one_based)
    })
}

/// Rotate several pages (0-based) clockwise by `degrees` (multiple of 90).
#[tauri::command]
pub fn rotate_pages(state: SharedState, id: String, pages: Vec<usize>, degrees: i32) -> Result<DocSummary> {
    if pages.is_empty() {
        return state.session.with_doc(&id, |doc| doc_summary(doc));
    }
    if degrees % 90 != 0 {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "Page rotation must be a multiple of 90 degrees.",
        ));
    }
    let label = format!("Rotate {}Â°", degrees);
    apply_edit(&state, &id, &label, |doc| {
        for page in pages {
            structure::rotate_page_by(doc, page + 1, degrees)?;
        }
        Ok(())
    })
}

/// Write the selected pages into their own PDF file. The current document is
/// untouched.
#[tauri::command]
pub fn extract_pages_to(state: SharedState, id: String, pages: Vec<usize>, path: String) -> Result<ExtractResult> {
    let dirs = state.session.dirs.clone();
    let work = dirs.work_path(&id);
    let bytes = std::fs::read(&work).map_err(|e| {
        BritError::new(
            ErrorKind::AccessDenied,
            format!("Could not read the working file: {e}"),
        )
    })?;
    let password = state.session.with_doc(&id, |doc| Ok(doc.open_password().map(str::to_string)))?;
    let doc = structure::load(&bytes, password.as_deref())?;
    let one_based: Vec<usize> = pages.iter().map(|p| p + 1).collect();
    let out = structure::extract_pages(&doc, &one_based)?;
    let dest = std::path::PathBuf::from(&path);
    write_file_atomically(&dest, &out).map_err(|e| {
        BritError::new(
            ErrorKind::WriteFailed,
            format!("Could not write the extracted file: {e}"),
        )
    })?;
    Ok(ExtractResult {
        path: dest.display().to_string(),
        pages: one_based.len(),
    })
}

/// Overlay an image onto a page. `rect` is in viewer coordinates (y grows down).
#[tauri::command]
pub fn place_image(
    state: SharedState,
    id: String,
    page: usize,
    data_url: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Result<DocSummary> {
    let raw = decode_data_url(&data_url)?;
    let image = EncodedImage::from_bytes(&raw)?;
    apply_edit(&state, &id, "Place image", |doc| {
        let ids = structure::page_ids(doc);
        let page_id = *ids.get(page).ok_or_else(|| {
            BritError::new(ErrorKind::InvalidPage, format!("Page {} does not exist.", page + 1))
        })?;
        let (_, _, pw, ph) = writer::page_size(doc, page_id);
        let aspect = image.height as f32 / image.width as f32;
        let mut width = if w > 0.0 { w } else { pw * 0.5 };
        let mut height = if h > 0.0 { h } else { width * aspect };
        if height > ph * 0.9 {
            height = ph * 0.9;
            width = height / aspect;
        }
        let draw_w = width.min(pw).max(1.0);
        let draw_h = height.min(ph).max(1.0);
        let (mut px, mut py) = crate::engine::session::to_pdf_space(x, y, ph);
        py -= draw_h;
        px = px.clamp(0.0, (pw - draw_w).max(0.0));
        py = py.clamp(0.0, (ph - draw_h).max(0.0));
        let name = crate::engine::authoring::embed_image(doc, page_id, &image)?;
        let ops = format!("{draw_w} 0 0 {draw_h} {px} {py} cm /{} Do\n", String::from_utf8_lossy(&name));
        append_page_content(doc, page_id, ops.as_bytes())?;
        Ok(())
    })
}

/// Draw header/footer text onto the selected pages.
#[tauri::command]
pub fn add_header_footer(state: SharedState, id: String, options: HeaderFooterOptions) -> Result<DocSummary> {
    apply_edit(&state, &id, "Add header & footer", |doc| {
        decorate_pages(doc, &options)
    })
}

/// Page numbers (1-based) on the selected pages, drawn as plain content so they
/// print exactly like the UI previews them.
#[tauri::command]
pub fn add_page_numbers(
    state: SharedState,
    id: String,
    position: PageNumberPosition,
    format: PageNumberFormat,
    pages: Option<Vec<usize>>,
) -> Result<DocSummary> {
    apply_edit(&state, &id, "Add page numbers", |doc| {
        draw_page_numbers(doc, position, format, pages.as_deref())
    })
}

/// Place a text (or image) watermark.
#[tauri::command]
pub fn add_watermark(state: SharedState, id: String, options: WatermarkOptions) -> Result<DocSummary> {
    if options.text.as_deref().filter(|t| !t.trim().is_empty()).is_none()
        && options.image_path.as_deref().filter(|p| !p.trim().is_empty()).is_none()
    {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "Enter some text or choose an image for the watermark.",
        ));
    }
    apply_edit(&state, &id, "Add watermark", |doc| {
        add_watermark_to_pages(doc, &options)
    })
}

/// The document's bookmark tree, flattened for the sidebar.
#[tauri::command]
pub fn get_outline(state: SharedState, id: String) -> Result<Vec<structure::OutlineEntry>> {
    let dirs = state.session.dirs.clone();
    let work = dirs.work_path(&id);
    let bytes = std::fs::read(&work).map_err(|e| {
        BritError::new(
            ErrorKind::AccessDenied,
            format!("Could not read the working file: {e}"),
        )
    })?;
    let password = state.session.with_doc(&id, |doc| Ok(doc.open_password().map(str::to_string)))?;
    let doc = structure::load(&bytes, password.as_deref())?;
    Ok(structure::outline(&doc))
}

/* ------------------------------ page decoration ------------------------------ */

fn decorate_pages(doc: &mut lopdf::Document, options: &HeaderFooterOptions) -> Result<()> {
    let ids = structure::page_ids(doc);
    let selected = selected_pages(ids.len(), options.pages.as_deref());
    let size = options.font_size.max(4.0);
    let colour = options.color;
    let margin = options.margin_points.clamp(8.0, 200.0);

    let slots: [(Option<String>, &str); 6] = [
        (options.header_left.clone(), "h-l"),
        (options.header_center.clone(), "h-c"),
        (options.header_right.clone(), "h-r"),
        (options.footer_left.clone(), "f-l"),
        (options.footer_center.clone(), "f-c"),
        (options.footer_right.clone(), "f-r"),
    ];

    let mutate = |doc: &mut lopdf::Document, page_id: ObjectId| -> Result<()> {
        let (_, _, w, h) = writer::page_size(doc, page_id);
        let font = ensure_font(doc, page_id, "Helvetica")?;
        let mut content = ContentBuilder::new().save_state();
        content = content.fill_color_rgb(colour[0], colour[1], colour[2]);
        for (text, where_) in &slots {
            let Some(text) = text else { continue };
            let text = expand_date(text, options.date_format.as_str());
            if text.trim().is_empty() {
                continue;
            }
            let is_header = where_.starts_with('h');
            let row_y = if is_header { h - margin } else { margin };
            let baseline = if is_header { row_y - size * 0.85 } else { row_y + size * 0.3 };
            let text_w = estimate_text_width(&text, size);
            let x = if where_.ends_with('c') {
                w / 2.0 - text_w / 2.0
            } else if where_.ends_with('r') {
                w - margin - text_w
            } else {
                margin
            };
            content = content
                .begin_text()
                .set_font(&font, size)
                .text_matrix(x, baseline)
                .show_text(&text)
                .end_text();
        }
        let raw = content.restore_state().encode()?;
        append_page_content(doc, page_id, &raw)
    };

    for page in selected {
        mutate(doc, ids[page])?;
    }
    Ok(())
}

fn draw_page_numbers(
    doc: &mut lopdf::Document,
    position: PageNumberPosition,
    format: PageNumberFormat,
    pages: Option<&[usize]>,
) -> Result<()> {
    let ids = structure::page_ids(doc);
    let total = ids.len();
    let selected = selected_pages(total, pages);
    let colour = [0.35, 0.35, 0.35];
    let size = 9.0;

    for page in selected {
        let page_id = ids[page];
        let number = page + 1;
        let label = match format {
            PageNumberFormat::Plain => number.to_string(),
            PageNumberFormat::Dashed => format!("-{number}-"),
            PageNumberFormat::Slashed => format!("{number} / {total}"),
            PageNumberFormat::PageXofY => format!("Page {number} of {total}"),
        };
        let (_, _, w, h) = writer::page_size(doc, page_id);
        let font = ensure_font(doc, page_id, "Helvetica")?;
        let text_w = estimate_text_width(&label, size);
        let x = match position {
            PageNumberPosition::TopLeft | PageNumberPosition::BottomLeft => 18.0,
            PageNumberPosition::TopCenter | PageNumberPosition::BottomCenter => w / 2.0 - text_w / 2.0,
            PageNumberPosition::TopRight | PageNumberPosition::BottomRight => w - 18.0 - text_w,
        };
        let is_top = matches!(position, PageNumberPosition::TopLeft | PageNumberPosition::TopCenter | PageNumberPosition::TopRight);
        let baseline = if is_top { h - 14.0 - size * 0.8 } else { 14.0 + size * 0.3 };
        let mut content = ContentBuilder::new().save_state();
        content = content.fill_color_rgb(colour[0], colour[1], colour[2]);
        let content = draw_text(content, &label, size, x, baseline, &font);
        let raw = content.restore_state().encode()?;
        append_page_content(doc, page_id, &raw)?;
    }
    Ok(())
}

fn add_watermark_to_pages(doc: &mut lopdf::Document, options: &WatermarkOptions) -> Result<()> {
    let ids = structure::page_ids(doc);
    let selected = selected_pages(ids.len(), options.pages.as_deref());
    let rot = if options.rotation_degrees % 90.0 != 0.0 || options.rotation_degrees.abs() < 1.0 {
        options.rotation_degrees
    } else {
        options.rotation_degrees
    };

    let embedded = if let Some(path) = options.image_path.as_deref().filter(|p| !p.is_empty()) {
        let raw = std::fs::read(path).map_err(|e| {
            BritError::new(
                ErrorKind::AccessDenied,
                format!("Could not read the watermark image: {e}"),
            )
        })?;
        Some(EncodedImage::from_bytes(&raw)?)
    } else {
        None
    };

    for page in selected {
        let page_id = ids[page];
        let (_, _, w, h) = writer::page_size(doc, page_id);
        let mut content = ContentBuilder::new().save_state();
        if options.opacity < 1.0 {
            let gs = ensure_alpha_state(doc, page_id, options.opacity, options.opacity)?;
            content = content.op_gs(&gs);
        }

        if let Some(image) = &embedded {
            let aspect = image.height as f32 / image.width as f32;
            let max_w = w * 0.4;
            let max_h = h * 0.4;
            let image_w = (max_w).min(max_h / aspect);
            let image_h = image_w * aspect;
            let (ax, ay) = watermark_anchor(w, h, options.position, image_w, image_h, rot);
            let name = crate::engine::authoring::embed_image(doc, page_id, image)?;
            let ops = format!("{} 0 0 {} {} {} cm /{} Do\n", image_w, image_h, ax, ay, String::from_utf8_lossy(&name));
            append_page_content(doc, page_id, ops.as_bytes())?;
            continue;
        }

        let text = options.text.as_deref().unwrap_or("CONFIDENTIAL");
        let size = options.font_size.max(12.0);
        if text.is_empty() {
            continue;
        }
        let font = ensure_font(doc, page_id, "Helvetica-Bold")?;
        let (text_w, text_h) = (estimate_text_width(text, size), size);
        let (ax, ay) = watermark_anchor(w, h, options.position, text_w, text_h, rot);
        content = content.fill_color_rgb(options.color[0], options.color[1], options.color[2]);
        if options.position == WatermarkPosition::Tiled {
            let spacing_x = estimate_text_width(text, size) + 60.0;
            let spacing_y = size * 2.4;
            let mut row = 0.0f32;
            while row < h {
                let mut col = -w;
                while col < w {
                    content = draw_text(content, text, size, col, row, &font);
                    col += spacing_x;
                }
                row += spacing_y;
            }
        } else if (rot % 90.0).abs() > 1.0 {
            let (ca, sa) = (rot.to_radians().cos(), rot.to_radians().sin());
            content = content.concat_matrix(ca, sa, -sa, ca, ax, ay);
            content = draw_text(content, text, size, -text_w / 2.0, -size / 3.0, &font);
        } else {
            content = draw_text(content, text, size, ax, ay, &font);
        }
        let raw = content.restore_state().encode()?;
        append_page_content(doc, page_id, &raw)?;
    }
    Ok(())
}

fn watermark_anchor(
    page_w: f32,
    page_h: f32,
    position: WatermarkPosition,
    obj_w: f32,
    obj_h: f32,
    rotation: f32,
) -> (f32, f32) {
    let x = match position {
        WatermarkPosition::TopLeft | WatermarkPosition::MiddleLeft | WatermarkPosition::BottomLeft => 30.0,
        WatermarkPosition::TopRight | WatermarkPosition::MiddleRight | WatermarkPosition::BottomRight => {
            page_w - 30.0 - obj_w
        }
        _ => page_w / 2.0 - obj_w / 2.0,
    };
    let y = match position {
        WatermarkPosition::TopLeft | WatermarkPosition::TopCenter | WatermarkPosition::TopRight => {
            page_h - 40.0 - obj_h
        }
        WatermarkPosition::BottomLeft | WatermarkPosition::BottomCenter | WatermarkPosition::BottomRight => 40.0,
        _ => page_h / 2.0 - obj_h / 2.0,
    };
    if rotation.abs() > 1.0 {
        (x + obj_w / 2.0, y + obj_h / 2.0)
    } else {
        (x, y)
    }
}

fn draw_text(
    content: ContentBuilder,
    text: &str,
    size: f32,
    x: f32,
    y: f32,
    font: &[u8],
) -> ContentBuilder {
    content
        .begin_text()
        .set_font(font, size)
        .text_matrix(x, y)
        .show_text(text)
        .end_text()
}

fn estimate_text_width(text: &str, size: f32) -> f32 {
    size * text.chars().count() as f32 * 0.55
}

fn selected_pages(total: usize, pages: Option<&[usize]>) -> Vec<usize> {
    match pages {
        Some(list) => {
            let mut out: Vec<usize> = list.iter().copied().filter(|p| *p < total).collect();
            out.sort_unstable();
            out.dedup();
            out
        }
        None => (0..total).collect(),
    }
}

fn expand_date(text: &str, format: &str) -> String {
    if !text.contains("<date>") && !text.contains("<Date>") {
        return text.to_string();
    }
    let now: chrono::DateTime<chrono::Local> = std::time::SystemTime::now().into();
    let rendered = now.format(format).to_string();
    text.replace("<date>", &rendered).replace("<Date>", &rendered)
}

fn decode_data_url(data_url: &str) -> Result<Vec<u8>> {
    let comma = data_url.find(',').ok_or_else(|| {
        BritError::new(ErrorKind::InvalidRequest, "The image data is not a data URL.")
    })?;
    let payload = &data_url[comma + 1..];
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| BritError::new(ErrorKind::InvalidRequest, format!("The image data could not be decoded: {e}")))
}