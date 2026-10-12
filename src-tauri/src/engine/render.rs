//! Page rasterisation with an LRU cache.
//!
//! Large documents must not be rendered in full. Two mechanisms keep BritPDF
//! responsive on a 1,000 page file:
//!
//! 1. The **UI** only ever requests the pages it is about to display (viewport
//!    virtualisation), so a scroll never triggers more than a handful of renders.
//! 2. This module keeps a **byte-budgeted LRU** of rendered PNGs, so scrolling back
//!    and forth is instant, while memory and disk usage stay bounded no matter how
//!    long the user browses.

use crate::engine::session::{Dirs, OpenDocument, write_file_atomically};
use crate::engine::types::{PageContentKind, RenderedPage};
use crate::error::{BritError, ErrorKind, Result};
use pdfium_render::prelude::{
    PdfBitmapFormat, PdfPageIndex, PdfPageRenderRotation, PdfRenderConfig,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// Never rasterise a page beyond this many pixels on a side.
const MAX_DIMENSION: u32 = 8192;
/// How many device pixels per PDF point we render at. 2.0 keeps text crisp on
/// typical Windows scaling (100%/125%/150%) without exploding the bitmap.
pub const RENDER_PPI: f32 = 144.0;

pub struct RenderRequest {
    pub page: usize,
    /// Target width in device pixels.
    pub width: u32,
    /// Combined `/Rotate` + viewer rotation, in degrees clockwise.
    pub rotation: u32,
    pub render_annotations: bool,
    pub render_forms: bool,
    /// Rasterise for a grayscale printer.
    pub grayscale: bool,
    /// Cap on the produced bitmap area, used for thumbnails and previews.
    pub max_area: Option<u32>,
}

pub fn render_page(doc: &mut OpenDocument, dirs: &Dirs, req: RenderRequest) -> Result<RenderedPage> {
    let page_count = doc.page_count();
    if req.page >= page_count {
        return Err(BritError::new(
            ErrorKind::InvalidPage,
            format!("Page {} does not exist (the document has {page_count} pages).", req.page + 1),
        ));
    }
    let geometry = doc.page_geometry(req.page)?;
    if geometry.width <= 0.0 || geometry.height <= 0.0 {
        return Err(BritError::new(
            ErrorKind::UnsupportedFeature,
            "This page has a zero-sized media box and cannot be displayed.",
        ));
    }

    let rotation = normalise_rotation(req.rotation + geometry.rotation);
    let (display_w, display_h) = rotated_size(geometry.raw_width, geometry.raw_height, rotation);

    // Work out the pixel size for the requested display width, respecting any
    // maximum area budget (thumbnails are capped so scrolling the sidebar is cheap).
    let mut scale = req.width as f32 / display_w.max(1.0);
    if let Some(max_area) = req.max_area {
        let area = (display_w * scale) * (display_h * scale);
        if area > max_area as f32 {
            scale *= (max_area as f32 / area).sqrt();
        }
    }
    let px_w = ((display_w * scale).round() as u32).clamp(1, MAX_DIMENSION);
    let px_h = ((display_h * scale).round() as u32).clamp(1, MAX_DIMENSION);

    let unrotated_w = (geometry.raw_width * scale).round().max(1.0) as u32;
    let unrotated_h = (geometry.raw_height * scale).round().max(1.0) as u32;

    let key = cache_key(&req, px_w, px_h);
    let dir = dirs.render_dir(&doc.id);
    let path = dir.join(format!("{key}.png"));

    if path.exists() {
        if let Ok(meta) = std::fs::metadata(&path) {
            let len = meta.len();
            let entry = super::session::RenderCacheEntry {
                path: path.clone(),
                width: px_w,
                height: px_h,
                unrotated_width: unrotated_w,
                unrotated_height: unrotated_h,
                bytes: len,
            };
            doc.put_render_cache(key.clone(), entry);
            let mut result = build_result(
                doc, req.page, path, px_w, px_h, unrotated_w, unrotated_h, rotation, true,
            );
            result.data_url = png_data_url(&result.path);
            return Ok(result);
        }
    }

    std::fs::create_dir_all(&dir).map_err(|e| {
        BritError::new(ErrorKind::WriteFailed, format!("Could not create the render cache: {e}"))
    })?;

    let (png_bytes, text_chars, annot_count) = rasterise(doc, req.page, px_w, px_h, rotation, &req)?;

    write_file_atomically(&path, &png_bytes)
        .map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("Could not cache the rendered page: {e}")))?;

    doc.put_render_cache(
        key.clone(),
        super::session::RenderCacheEntry {
            path: path.clone(),
            width: px_w,
            height: px_h,
            unrotated_width: unrotated_w,
            unrotated_height: unrotated_h,
            bytes: png_bytes.len() as u64,
        },
    );

    let mut result = build_result(
        doc, req.page, path, px_w, px_h, unrotated_w, unrotated_h, rotation, false,
    );
    result.data_url = png_data_url(&result.path);
    if text_chars == 0 {
        result.content_kind = PageContentKind::ImageOnly;
        result.annotation_count = annot_count;
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn build_result(
    doc: &mut OpenDocument,
    page: usize,
    path: PathBuf,
    width: u32,
    height: u32,
    unrotated_width: u32,
    unrotated_height: u32,
    render_rotation: u32,
    from_cache: bool,
) -> RenderedPage {
    let (content_kind, char_count, annotation_count) = match doc.text(page) {
        Ok(t) => (t.content_kind, t.chars.len(), annotation_count_of(doc, page)),
        Err(_) => (PageContentKind::Text, 0, 0),
    };
    RenderedPage {
        path: path.display().to_string(),
        data_url: None,
        width,
        height,
        unrotated_width,
        unrotated_height,
        render_rotation,
        content_kind,
        character_count: char_count,
        annotation_count,
        from_cache,
    }
}

/// Read a rendered PNG back and encode it as a `data:` URL for the webview.
fn png_data_url(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(format!("data:image/png;base64,{}", base64_encode(&bytes)))
}

/// Minimal RFC 4648 base64 encoder (kept local so no extra crate or network
/// fetch is needed at build time).
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn annotation_count_of(doc: &mut OpenDocument, page: usize) -> usize {
    doc.document
        .pages()
        .get(page as PdfPageIndex)
        .map(|p| p.annotations().len() as usize)
        .unwrap_or(0)
}

/// Actually call PDFium and encode the bitmap as PNG.
fn rasterise(
    doc: &mut OpenDocument,
    page: usize,
    width: u32,
    height: u32,
    rotation: u32,
    req: &RenderRequest,
) -> Result<(Vec<u8>, usize, usize)> {
    let pdf_page = doc
        .document
        .pages()
        .get(page as PdfPageIndex)
        .map_err(|_| BritError::new(ErrorKind::InvalidPage, "That page could not be read."))?;

    let mut config = PdfRenderConfig::new()
        .set_target_width(width as i32)
        .set_target_height(height as i32)
        .rotate(render_rotation_to_pdfium(rotation), true)
        .set_format(PdfBitmapFormat::BGRx)
        .render_annotations(req.render_annotations)
        .render_form_data(req.render_forms)
        .use_print_quality(true)
        .set_image_smoothing(true)
        .set_path_smoothing(true)
        .set_text_smoothing(true)
        .set_reverse_byte_order(true)
        .set_clear_color(pdfium_render::prelude::PdfColor::WHITE)
        .clear_before_rendering(true)
        .limit_render_image_cache_size(true);
    if req.grayscale {
        config = config.use_grayscale_rendering(true);
    }

    let bitmap = pdf_page
        .render_with_config(&config)
        .map_err(|e| BritError::new(ErrorKind::EngineFailure, format!("This page could not be rendered: {e}")))?;

    let image = bitmap
        .as_image()
        .map_err(|e| BritError::new(ErrorKind::EngineFailure, format!("Rendered page could not be read: {e}")))?;
    drop(bitmap);

    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| BritError::new(ErrorKind::EngineFailure, format!("Page image could not be encoded: {e}")))?;

    let char_count = doc.text(page).map(|t| t.chars.len()).unwrap_or(0);
    let annot_count = pdf_page.annotations().len() as usize;
    Ok((png.into_inner(), char_count, annot_count))
}

/// Render a small thumbnail. Thumbnails are aggressively capped and use a separate
/// cache key space so they never evict the full-size pages the user is reading.
pub fn render_thumbnail(doc: &mut OpenDocument, dirs: &Dirs, page: usize, max_width: u32) -> Result<RenderedPage> {
    render_page(
        doc,
        dirs,
        RenderRequest {
            page,
            width: max_width.clamp(48, 512),
            rotation: doc.view_rotation,
            render_annotations: true,
            render_forms: true,
            grayscale: false,
            max_area: Some(360_000),
        },
    )
}

/// Rasterise a page straight to raw RGBA bytes, for printing and image export.
pub fn rasterise_to_rgba(
    doc: &mut OpenDocument,
    page: usize,
    width_px: u32,
    height_px: u32,
    rotation: u32,
    render_annotations: bool,
) -> Result<image::RgbaImage> {
    let geometry = doc.page_geometry(page)?;
    let combined = normalise_rotation(rotation + geometry.rotation);
    let (dw, dh) = rotated_size(geometry.raw_width, geometry.raw_height, combined);
    let scale = (width_px as f32 / dw.max(1.0)).min(height_px as f32 / dh.max(1.0));
    let w = ((dw * scale).round() as u32).clamp(1, MAX_DIMENSION);
    let h = ((dh * scale).round() as u32).clamp(1, MAX_DIMENSION);

    let pdf_page = doc
        .document
        .pages()
        .get(page as PdfPageIndex)
        .map_err(|_| BritError::new(ErrorKind::InvalidPage, "That page could not be read."))?;

    let config = PdfRenderConfig::new()
        .set_target_width(w as i32)
        .set_target_height(h as i32)
        .rotate(render_rotation_to_pdfium(combined), true)
        .set_format(PdfBitmapFormat::BGRx)
        .render_annotations(render_annotations)
        .render_form_data(true)
        .use_print_quality(true)
        .set_reverse_byte_order(true)
        .set_clear_color(pdfium_render::prelude::PdfColor::WHITE)
        .clear_before_rendering(true)
        .limit_render_image_cache_size(false);

    let bitmap = pdf_page
        .render_with_config(&config)
        .map_err(|e| BritError::new(ErrorKind::EngineFailure, format!("This page could not be rendered: {e}")))?;
    let image = bitmap
        .as_image()
        .map_err(|e| BritError::new(ErrorKind::EngineFailure, format!("Rendered page could not be read: {e}")))?;
    Ok(image.to_rgba8())
}

fn cache_key(req: &RenderRequest, width: u32, height: u32) -> String {
    format!(
        "p{}-{}x{}-rot{}-a{}-f{}-g{}-v{}",
        req.page,
        width,
        height,
        req.rotation,
        req.render_annotations as u8,
        req.render_forms as u8,
        req.grayscale as u8,
        RENDER_CACHE_VERSION,
    )
}

/// Bumped whenever the cache key or the on-disk format changes, so stale PNGs from
/// an older build are never served to a newer one.
const RENDER_CACHE_VERSION: u32 = 1;

pub fn normalise_rotation(deg: u32) -> u32 {
    deg % 360
}

fn rotated_size(width: f32, height: f32, rotation: u32) -> (f32, f32) {
    if rotation % 180 == 90 {
        (height, width)
    } else {
        (width, height)
    }
}

pub fn render_rotation_to_pdfium(deg: u32) -> PdfPageRenderRotation {
    match normalise_rotation(deg) {
        90 => PdfPageRenderRotation::Degrees90,
        180 => PdfPageRenderRotation::Degrees180,
        270 => PdfPageRenderRotation::Degrees270,
        _ => PdfPageRenderRotation::None,
    }
}

/// Evict the least recently used rendered pages until the cache fits the budget.
pub fn evict_render_cache(doc: &mut OpenDocument, dirs: &Dirs, budget_bytes: u64) {
    const MIN_PAGES: usize = 12;
    let mut entries: Vec<(String, u64, PathBuf)> = doc
        .render_cache_paths()
        .into_iter()
        .map(|(k, bytes, path)| (k, bytes, path))
        .collect();
    if entries.len() <= MIN_PAGES {
        return;
    }
    entries.sort_by_key(|(_, bytes, _)| *bytes);
    let mut total: u64 = entries.iter().map(|(_, b, _)| *b).sum();
    for (key, bytes, path) in entries {
        if total <= budget_bytes {
            break;
        }
        if doc.render_cache_len() <= MIN_PAGES {
            break;
        }
        doc.drop_render_cache(&key);
        let _ = std::fs::remove_file(&path);
        total = total.saturating_sub(bytes);
    }
    // Belt and braces: if the directory still holds more files than we expect (for
    // example after a crash), clear anything not in the live cache.
    let live = doc.render_cache_paths();
    let live_keys: std::collections::HashSet<String> = live.into_iter().map(|(k, _, _)| k).collect();
    if let Ok(entries) = std::fs::read_dir(dirs.render_dir(&doc.id)) {
        for entry in entries.flatten() {
            let stem = entry.path().file_stem().map(|s| s.to_string_lossy().into_owned());
            if let Some(stem) = stem {
                if !live_keys.contains(&stem) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// Remove a document's entire render cache (called when a document changes).
pub fn clear_render_cache(doc: &mut OpenDocument, dirs: &Dirs) {
    doc.clear_render_cache();
    let _ = std::fs::remove_dir_all(dirs.render_dir(&doc.id));
}

/// A snapshot of every open document's cache size, for the About/diagnostics screen.
pub fn cache_report(docs: &HashMap<String, OpenDocument>) -> Vec<(String, usize, u64)> {
    docs.iter()
        .map(|(id, doc)| (id.clone(), doc.render_cache_len(), doc.render_cache_bytes()))
        .collect()
}
