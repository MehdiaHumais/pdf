//! Low-level lopdf helpers shared by every editing feature.
//!
//! Everything BritPDF adds to a document goes through this module so that the
//! results are valid, self-consistent PDF: content streams are always wrapped in
//! `q`/`Q`, fonts and graphics states are registered in the page's resources
//! exactly once, and no feature ever hand-writes a raw object without going
//! through `Document::add_object`.

use crate::error::{BritError, ErrorKind, Result};
use lopdf::content::Content;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};

/// A fluent builder for PDF content streams.
///
/// Coordinates are in PDF user space: origin bottom-left of the page, y increasing
/// upwards. Callers convert from viewer space with
/// [`crate::engine::session::to_pdf_space`].
#[derive(Default, Clone)]
pub struct ContentBuilder {
    ops: Vec<lopdf::content::Operation>,
}

impl ContentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn op(mut self, operator: &str, operands: Vec<Object>) -> Self {
        self.ops.push(lopdf::content::Operation::new(operator, operands));
        self
    }

    pub fn save_state(self) -> Self {
        self.op("q", vec![])
    }

    pub fn restore_state(self) -> Self {
        self.op("Q", vec![])
    }

    pub fn concat_matrix(self, a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        self.op(
            "cm",
            vec![
                Object::Real(a),
                Object::Real(b),
                Object::Real(c),
                Object::Real(d),
                Object::Real(e),
                Object::Real(f),
            ],
        )
    }

    /// Fill colour, components 0..=1.
    pub fn fill_color_rgb(self, r: f32, g: f32, b: f32) -> Self {
        self.op("rg", vec![
            Object::Real(clamp01(r)),
            Object::Real(clamp01(g)),
            Object::Real(clamp01(b)),
        ])
    }

    /// Stroke colour, components 0..=1.
    pub fn stroke_color_rgb(self, r: f32, g: f32, b: f32) -> Self {
        self.op("RG", vec![
            Object::Real(clamp01(r)),
            Object::Real(clamp01(g)),
            Object::Real(clamp01(b)),
        ])
    }

    pub fn gray(self, v: f32) -> Self {
        self.op("g", vec![Object::Real(clamp01(v))])
    }

    pub fn line_width(self, w: f32) -> Self {
        self.op("w", vec![Object::Real(w.max(0.0))])
    }

    /// Dash pattern in points, e.g. `[3.0, 2.0]`.
    pub fn dash_pattern(self, pattern: &[f32], phase: f32) -> Self {
        let arr: Vec<Object> = pattern.iter().map(|v| Object::Real(*v)).collect();
        self.op("d", vec![Object::Array(arr), Object::Real(phase)])
    }

    pub fn line_cap_round(self) -> Self {
        self.op("J", vec![Object::Integer(1)])
    }

    pub fn line_join_round(self) -> Self {
        self.op("j", vec![Object::Integer(1)])
    }

    pub fn move_to(self, x: f32, y: f32) -> Self {
        self.op("m", vec![Object::Real(x), Object::Real(y)])
    }

    pub fn line_to(self, x: f32, y: f32) -> Self {
        self.op("l", vec![Object::Real(x), Object::Real(y)])
    }

    pub fn curve_to(self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) -> Self {
        self.op("c", vec![
            Object::Real(x1),
            Object::Real(y1),
            Object::Real(x2),
            Object::Real(y2),
            Object::Real(x3),
            Object::Real(y3),
        ])
    }

    pub fn close_path(self) -> Self {
        self.op("h", vec![])
    }

    pub fn rect(self, x: f32, y: f32, w: f32, h: f32) -> Self {
        self.op("re", vec![
            Object::Real(x),
            Object::Real(y),
            Object::Real(w),
            Object::Real(h),
        ])
    }

    /// An ellipse inscribed in the given box, as four cubic Béziers.
    pub fn ellipse(self, x: f32, y: f32, w: f32, h: f32) -> Self {
        // Magic constant for approximating a quarter circle with a cubic Bézier.
        const K: f32 = 0.552_284_8;
        let (rx, ry) = (w / 2.0, h / 2.0);
        let (cx, cy) = (x + rx, y + ry);
        self.move_to(cx + rx, cy)
            .curve_to(cx + rx, cy + ry * K, cx + rx * K, cy + ry, cx, cy + ry)
            .curve_to(cx - rx * K, cy + ry, cx - rx, cy + ry * K, cx - rx, cy)
            .curve_to(cx - rx, cy - ry * K, cx - rx * K, cy - ry, cx, cy - ry)
            .curve_to(cx + rx * K, cy - ry, cx + rx, cy - ry * K, cx + rx, cy)
    }

    pub fn stroke(self) -> Self {
        self.op("S", vec![])
    }

    pub fn fill(self) -> Self {
        self.op("f", vec![])
    }

    pub fn even_odd_fill(self) -> Self {
        self.op("f*", vec![])
    }

    pub fn fill_and_stroke(self) -> Self {
        self.op("B", vec![])
    }

    /// The non-zero winding rule for the next fill.
    pub fn nonzero_winding(self) -> Self {
        self.op("W", vec![])
    }

    pub fn clip(self) -> Self {
        self.op("W", vec![]).op("n", vec![])
    }

    /// Begin/end marked content, used to scope form XObjects.
    pub fn begin_marked_content(self, tag: &str) -> Self {
        self.op("BMC", vec![Object::Name(tag.as_bytes().to_vec())])
    }

    pub fn end_marked_content(self) -> Self {
        self.op("EMC", vec![])
    }

    /// Select a font already registered as `name` in the page's resources.
    pub fn set_font(self, name: &[u8], size: f32) -> Self {
        self.op(
            "Tf",
            vec![Object::Name(name.to_vec()), Object::Real(size.max(0.1))],
        )
    }

    /// Select an ExtGState already registered as `name` in the page's resources.
    pub fn op_gs(self, name: &[u8]) -> Self {
        self.op("gs", vec![Object::Name(name.to_vec())])
    }

    pub fn begin_text(self) -> Self {
        self.op("BT", vec![])
    }

    pub fn end_text(self) -> Self {
        self.op("ET", vec![])
    }

    pub fn text_matrix(self, x: f32, y: f32) -> Self {
        self.op("Td", vec![Object::Real(x), Object::Real(y)])
    }

    /// Show a string with the current font and position.
    pub fn show_text(self, text: &str) -> Self {
        self.op("Tj", vec![Object::string_literal(text)])
    }

    /// Text render mode: 0 fill, 1 stroke, 3 invisible, 7 clip.
    pub fn set_text_render_mode(self, mode: i32) -> Self {
        self.op("Tr", vec![Object::Integer(mode as i64)])
    }

    /// Draw an image XObject into the unit square, scaled by the CTM already set.
    pub fn draw_x_object(self, name: &[u8]) -> Self {
        self.op("Do", vec![Object::Name(name.to_vec())])
    }

    /// Approximate the text width of a string in a base-14 font, in 1/1000 em.
    /// Good enough for centring watermarks and headers without embedding metrics.
    pub fn text_width_em(text: &str, size: f32, bold: bool, mono: bool) -> f32 {
        let per_char: f32 = if mono {
            600.0
        } else {
            match text.chars().next() {
                Some('i') | Some('j') | Some('l') | Some('I') | Some('t') | Some('f') | Some('r') => 300.0,
                Some('W') | Some('M') | Some('m') | Some('w') => 850.0,
                Some(c) if c.is_uppercase() => 660.0,
                Some(' ') => 260.0,
                _ => 500.0,
            }
        };
        let base = if bold { per_char * 1.06 } else { per_char };
        base / 1000.0 * size * text.chars().count() as f32
    }

    pub fn build(self) -> Content {
        Content { operations: self.ops }
    }

    pub fn encode(self) -> Result<Vec<u8>> {
        self.build()
            .encode()
            .map_err(|e| BritError::new(ErrorKind::Internal, format!("Content stream could not be encoded: {e}")))
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

fn clamp01(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// The base-14 fonts BritPDF may reference without embedding.
///
/// All are guaranteed present in every conforming PDF viewer, so generated
/// documents stay small and render identically everywhere.
pub fn base_font(name: &str) -> Dictionary {
    let (subtype, base) = match name {
        "Helvetica-Bold" => ("Type1", "Helvetica-Bold"),
        "Helvetica-Oblique" => ("Type1", "Helvetica-Oblique"),
        "Times-Roman" => ("Type1", "Times-Roman"),
        "Times-Bold" => ("Type1", "Times-Bold"),
        "Times-Italic" => ("Type1", "Times-Italic"),
        "Courier" => ("Type1", "Courier"),
        "Courier-Bold" => ("Type1", "Courier-Bold"),
        "Symbol" => ("Type1", "Symbol"),
        _ => ("Type1", "Helvetica"),
    };
    dictionary! {
        "Type" => Object::Name(b"Font".to_vec()),
        "Subtype" => Object::Name(subtype.as_bytes().to_vec()),
        "BaseFont" => Object::Name(base.as_bytes().to_vec()),
        "Encoding" => Object::Name(b"WinAnsiEncoding".to_vec()),
    }
}

/// Register a base-14 font in a page's resources, returning the resource name.
/// Idempotent: asking twice for the same font reuses the existing entry.
pub fn ensure_font(doc: &mut Document, page_id: ObjectId, font: &str) -> Result<Vec<u8>> {
    let key = font.as_bytes().to_vec();
    let existing = font_resource_name(doc, page_id, &key);
    if let Some(name) = existing {
        return Ok(name);
    }
    let font_id = doc.add_object(base_font(font));
    let name = unique_resource_name(doc, page_id, b"Font", font_id)?;
    add_font_resource(doc, page_id, name.clone(), font_id)
        .map_err(|e| BritError::internal(format!("Could not register the {font} font: {e}")))?;
    Ok(name)
}

/// Insert a font object into a page's `Resources -> Font` dictionary under `name`.
///
/// lopdf has no dedicated helper for this, and `Document::add_xobject` writes to
/// the *XObject* sub-dictionary, which readers (and our own `page_font_ref`)
/// never look at for fonts.
fn add_font_resource(
    doc: &mut Document,
    page_id: ObjectId,
    name: Vec<u8>,
    font_id: ObjectId,
) -> std::result::Result<(), lopdf::Error> {
    let indirect = {
        let resources = doc.get_or_create_resources(page_id).and_then(Object::as_dict_mut)?;
        if !resources.has(b"Font") {
            resources.set("Font", Dictionary::new());
        }
        resources.get(b"Font").ok().and_then(|f| match f {
            Object::Reference(id) => Some(*id),
            _ => None,
        })
    };
    match indirect {
        Some(id) => {
            let fonts = doc.get_object_mut(id).and_then(Object::as_dict_mut)?;
            fonts.set(name, Object::Reference(font_id));
        }
        None => {
            let resources = doc.get_or_create_resources(page_id).and_then(Object::as_dict_mut)?;
            let fonts = resources.get_mut(b"Font").and_then(Object::as_dict_mut)?;
            fonts.set(name, Object::Reference(font_id));
        }
    }
    Ok(())
}

fn font_resource_name(doc: &Document, page_id: ObjectId, font: &[u8]) -> Option<Vec<u8>> {
    let page = doc.get_dictionary(page_id).ok()?;
    let resources = page.get(b"Resources").ok()?.as_dict().ok()?;
    let fonts = resources.get(b"Font").ok()?.as_dict().ok()?;
    for (name, value) in fonts.iter() {
        let Object::Reference(id) = value else { continue };
        let Ok(font_dict) = doc.get_dictionary(*id) else { continue };
        let Some(Object::Name(base)) = font_dict.get(b"BaseFont").ok() else { continue };
        if base.as_slice().eq_ignore_ascii_case(font) {
            return Some(name.clone());
        }
    }
    None
}

/// Find an unused resource name of the form `Prefix1`, `Prefix2`, ...
fn unique_resource_name(
    doc: &Document,
    page_id: ObjectId,
    category: &[u8],
    object_id: ObjectId,
) -> Result<Vec<u8>> {
    let used: Vec<Vec<u8>> = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Resources").ok().and_then(|r| r.as_dict().ok()).cloned())
        .and_then(|resources| resources.get(category).ok().and_then(|c| c.as_dict().ok()).cloned())
        .map(|d| d.iter().map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    let prefix = match category {
        b"Font" => b"F".to_vec(),
        b"XObject" => b"Im".to_vec(),
        b"ExtGState" => b"GS".to_vec(),
        b"Shading" => b"Sh".to_vec(),
        _ => b"R".to_vec(),
    };
    for n in 1..10_000u32 {
        let mut candidate = prefix.clone();
        candidate.extend_from_slice(n.to_string().as_bytes());
        if !used.contains(&candidate) {
            return Ok(candidate);
        }
    }
    let _ = object_id;
    Err(BritError::new(ErrorKind::Internal, "Could not allocate a resource name."))
}

/// Register an ExtGState with constant alpha, returning its resource name.
pub fn ensure_alpha_state(
    doc: &mut Document,
    page_id: ObjectId,
    fill_alpha: f32,
    stroke_alpha: f32,
) -> Result<Vec<u8>> {
    let key = format!("{fill_alpha:.3}/{stroke_alpha:.3}");
    if let Some(existing) = resource_name_for(doc, page_id, b"ExtGState", &key) {
        return Ok(existing);
    }
    let state = dictionary! {
        "Type" => Object::Name(b"ExtGState".to_vec()),
        "ca" => Object::Real(clamp01(fill_alpha)),
        "CA" => Object::Real(clamp01(stroke_alpha)),
    };
    let state_id = doc.add_object(state);
    let name = unique_resource_name(doc, page_id, b"ExtGState", state_id)?;
    doc.add_graphics_state(page_id, name.clone(), state_id)
        .map_err(|e| BritError::internal(format!("Could not register transparency: {e}")))?;
    Ok(name)
}

fn resource_name_for(
    doc: &Document,
    page_id: ObjectId,
    category: &[u8],
    marker: &str,
) -> Option<Vec<u8>> {
    let page = doc.get_dictionary(page_id).ok()?;
    let resources = page.get(b"Resources").ok()?.as_dict().ok()?;
    let entries = resources.get(category).ok()?.as_dict().ok()?;
    for (name, value) in entries.iter() {
        let Object::Reference(id) = value else { continue };
        if let Ok(dict) = doc.get_dictionary(*id) {
            let rendered = dict
                .iter()
                .map(|(k, v)| format!("{}/{}", String::from_utf8_lossy(k), render_scalar(v)))
                .collect::<Vec<_>>()
                .join(",");
            if rendered.contains(marker) {
                return Some(name.clone());
            }
        }
    }
    None
}

fn render_scalar(value: &Object) -> String {
    match value {
        Object::Real(v) => format!("{v:.3}"),
        Object::Integer(v) => format!("{v:.3}"),
        Object::Name(v) => String::from_utf8_lossy(v).into_owned(),
        Object::String(v, _) => String::from_utf8_lossy(v).into_owned(),
        _ => String::new(),
    }
}

/// Build a normal appearance stream for a page-level XObject.
pub fn appearance_stream(
    bbox: [f32; 4],
    content: Vec<u8>,
    resources: Dictionary,
) -> Stream {
    let dict = dictionary! {
        "Type" => Object::Name(b"XObject".to_vec()),
        "Subtype" => Object::Name(b"Form".to_vec()),
        "FormType" => Object::Integer(1),
        "BBox" => Object::Array(vec![
            Object::Real(bbox[0]),
            Object::Real(bbox[1]),
            Object::Real(bbox[2]),
            Object::Real(bbox[3]),
        ]),
        "Matrix" => Object::Array(vec![
            Object::Real(1.0),
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(1.0),
            Object::Real(0.0),
            Object::Real(0.0),
        ]),
        "Resources" => Object::Dictionary(resources),
    };
    let mut stream = Stream::new(dict, content);
    let _ = stream.compress();
    stream
}

/// A new, valid, empty PDF with a single blank page of the given size.
pub fn blank_document(width: f32, height: f32) -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();

    let page_id = doc.new_object_id();
    let content = ContentBuilder::new().encode().unwrap_or_default();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    let page = dictionary! {
        "Type" => Object::Name(b"Page".to_vec()),
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(width),
            Object::Real(height),
        ]),
        "Resources" => dictionary! {},
        "Contents" => Object::Reference(content_id),
    };
    doc.set_object(page_id, Object::Dictionary(page));

    let pages = dictionary! {
        "Type" => Object::Name(b"Pages".to_vec()),
        "Kids" => Object::Array(vec![Object::Reference(page_id)]),
        "Count" => Object::Integer(1),
    };
    doc.set_object(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.new_object_id();
    let catalog = dictionary! {
        "Type" => Object::Name(b"Catalog".to_vec()),
        "Pages" => Object::Reference(pages_id),
    };
    doc.set_object(catalog_id, Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// Append a content stream to a page, creating one if the page has none.
pub fn append_page_content(doc: &mut Document, page_id: ObjectId, content: &[u8]) -> Result<()> {
    if content.is_empty() {
        return Ok(());
    }
    // Wrap the new operators in q/Q so they cannot leak graphics state into, or out
    // of, the page's existing content.
    let mut wrapped = Vec::with_capacity(content.len() + 4);
    wrapped.extend_from_slice(b"\nq\n");
    wrapped.extend_from_slice(content);
    wrapped.extend_from_slice(b"\nQ\n");

    let existing = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Contents").ok().cloned());

    match existing {
        Some(Object::Reference(stream_id)) => {
            // Never discard the page's existing content: decode it and append our
            // operators to it. If it cannot be decoded, add our operators as a
            // separate stream in an array instead of clobbering the original.
            let decoded = doc
                .get_object(stream_id)
                .ok()
                .and_then(|o| o.as_stream().ok())
                .map(Stream::decompressed_content);
            match decoded {
                Some(Ok(existing)) => {
                    let mut combined = existing;
                    combined.extend_from_slice(&wrapped);
                    if let Some(Object::Stream(stream)) = doc.get_object_mut(stream_id).ok() {
                        stream.set_plain_content(combined);
                        let _ = stream.compress();
                    }
                }
                _ => {
                    let new_id = doc.add_object(Stream::new(dictionary! {}, wrapped));
                    if let Ok(page) = doc.get_dictionary_mut(page_id) {
                        page.set(
                            "Contents",
                            Object::Array(vec![Object::Reference(stream_id), Object::Reference(new_id)]),
                        );
                    }
                }
            }
        }
        Some(Object::Array(items)) => {
            let combined = decode_stream_content(doc, &items);
            let mut all = combined;
            all.extend_from_slice(&wrapped);
            doc.change_page_content(page_id, all)
                .map_err(|e| BritError::new(ErrorKind::Internal, format!("Could not update the page: {e}")))?;
        }
        Some(_) | None => {
            doc.change_page_content(page_id, wrapped)
                .map_err(|e| BritError::new(ErrorKind::Internal, format!("Could not update the page: {e}")))?;
        }
    }
    Ok(())
}

/// Decode and concatenate a page's (possibly arrayed) content streams.
pub fn decode_stream_content(doc: &Document, items: &[Object]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        let Object::Reference(id) = item else { continue };
        let Ok(Object::Stream(stream)) = doc.get_object(*id) else { continue };
        match stream.decompressed_content() {
            Ok(bytes) => {
                out.extend_from_slice(&bytes);
                out.push(b'\n');
            }
            Err(_) => {
                // A stream we cannot decode is left untouched rather than being
                // dropped: losing content would silently damage the document.
            }
        }
    }
    out
}

/// The page's `/MediaBox` as (x, y, width, height), falling back to US Letter.
pub fn page_size(doc: &Document, page_id: ObjectId) -> (f32, f32, f32, f32) {
    doc.get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"MediaBox").ok().cloned())
        .and_then(|box_value| {
            let array = box_value.as_array().ok()?.clone();
            if array.len() < 4 {
                return None;
            }
            let nums: Vec<f32> = array
                .iter()
                .map(|o| match o {
                    Object::Integer(v) => *v as f32,
                    Object::Real(v) => *v,
                    _ => 0.0,
                })
                .collect();
            Some((nums[0], nums[1], nums[2].abs(), nums[3].abs()))
        })
        .unwrap_or((0.0, 0.0, 612.0, 792.0))
}

/// The page object ids, in reading order.
///
/// lopdf returns a `BTreeMap` keyed by page number, which lopsided page trees can
/// leave with gaps. This walks the tree instead, so the order always matches what
/// a reader shows, and falls back to the numeric keys if the walk fails.
pub fn pages(doc: &Document) -> Vec<ObjectId> {
    match pages_root(doc) {
        Ok(root) => {
            let mut out = Vec::new();
            walk_pages(doc, root, &mut out, 0);
            if !out.is_empty() {
                return out;
            }
        }
        Err(_) => {}
    }
    doc.get_pages().values().copied().collect()
}

fn walk_pages(doc: &Document, node: ObjectId, out: &mut Vec<ObjectId>, depth: u32) {
    // A malformed file can contain a cycle in the page tree; the depth cap is the
    // backstop that stops BritPDF hanging on it.
    if depth > 64 || out.len() > 200_000 {
        return;
    }
    let Ok(dict) = doc.get_dictionary(node) else { return };
    if dict.has_type(b"Page") {
        out.push(node);
        return;
    }
    let kids = dict.get(b"Kids").ok().and_then(|k| k.as_array().ok()).cloned();
    let Some(kids) = kids else { return };
    for kid in kids {
        if let Object::Reference(id) = kid {
            walk_pages(doc, id, out, depth + 1);
        }
    }
}

/// The document's page-tree root.
pub fn pages_root(doc: &Document) -> Result<ObjectId> {
    let catalog = doc.catalog().map_err(|e| {
        BritError::new(
            ErrorKind::CorruptDocument,
            format!("This file has no document catalogue, so it is not a usable PDF: {e}"),
        )
    })?;
    let id = catalog
        .get(b"Pages")
        .and_then(|o| o.as_reference())
        .map_err(|e| {
            BritError::new(
                ErrorKind::CorruptDocument,
                format!("This file has no page tree, so its pages cannot be read: {e}"),
            )
        })?;
    Ok(id)
}

/// Rewrite a page's `/MediaBox`.
pub fn set_page_size(doc: &mut Document, page_id: ObjectId, width: f32, height: f32) {
    if let Ok(page) = doc.get_dictionary_mut(page_id) {
        page.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(width),
                Object::Real(height),
            ]),
        );
    }
}

/// Copy an object and everything it references from `src` into `dst`, returning
/// the new object id. `mapping` caches ids already copied, so a shared font or
/// image is duplicated once and then referenced, not cloned repeatedly.
///
/// This is the deep copy that page extraction and merging need: two pages in one
/// document may not share mutable state, and object ids from different documents
/// routinely collide.
pub fn deep_copy(
    src: &Document,
    dst: &mut Document,
    id: ObjectId,
    mapping: &mut std::collections::HashMap<ObjectId, ObjectId>,
    depth: u32,
) -> ObjectId {
    if depth > 96 {
        return dst.add_object(Object::Null);
    }
    if let Some(existing) = mapping.get(&id) {
        return *existing;
    }
    // Reserve the slot before recursing so a cycle resolves to the same object
    // rather than recursing forever.
    let placeholder: ObjectId = (dst.max_id + 1, 0);
    mapping.insert(id, placeholder);

    let object = src.get_object(id).cloned().unwrap_or(Object::Null);
    let copied = match object {
        Object::Reference(target) => Object::Reference(deep_copy(src, dst, target, mapping, depth + 1)),
        Object::Array(items) => Object::Array(
            items
                .into_iter()
                .map(|item| match item {
                    Object::Reference(target) => {
                        Object::Reference(deep_copy(src, dst, target, mapping, depth + 1))
                    }
                    other => other,
                })
                .collect(),
        ),
        Object::Dictionary(dict) => Object::Dictionary(copy_dictionary(src, dst, dict, mapping, depth + 1)),
        Object::Stream(stream) => {
            let dict = copy_dictionary(src, dst, stream.dict, mapping, depth + 1);
            let mut new_stream = Stream::new(dict, stream.content.clone());
            new_stream.allows_compression = stream.allows_compression;
            Object::Stream(new_stream)
        }
        other => other,
    };

    dst.set_object(placeholder, copied);
    if dst.max_id < placeholder.0 {
        dst.max_id = placeholder.0;
    }
    placeholder
}

fn copy_dictionary(
    src: &Document,
    dst: &mut Document,
    dict: Dictionary,
    mapping: &mut std::collections::HashMap<ObjectId, ObjectId>,
    depth: u32,
) -> Dictionary {
    let mut out = Dictionary::new();
    for (key, value) in dict.iter() {
        let value = match value {
            Object::Reference(target) => {
                Object::Reference(deep_copy(src, dst, *target, mapping, depth))
            }
            other => other.clone(),
        };
        out.set(key.clone(), value);
    }
    out
}

/// Serialise a document, using object streams when asked. Object streams shrink a
/// typical file by 10-60% and are supported by every reader since PDF 1.5, so this
/// is a safe, genuinely lossless optimisation.
pub fn serialise(doc: &mut Document, modern: bool) -> Result<Vec<u8>> {
    let mut buffer = Vec::with_capacity(256 * 1024);
    if modern {
        doc.save_modern(&mut buffer)
    } else {
        doc.save_to(&mut buffer)
    }
    .map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("This PDF could not be written: {e}")))?;
    Ok(buffer)
}

/// Read a PDF's Info dictionary into plain optional strings.
pub fn read_info(doc: &Document) -> crate::engine::types::DocumentMetadata {
    use crate::engine::types::DocumentMetadata;
    let mut meta = DocumentMetadata::empty();
    let Some(Object::Reference(info_id)) = doc.trailer.get(b"Info").ok().cloned() else {
        return meta;
    };
    let Ok(info) = doc.get_dictionary(info_id) else { return meta };
    let get = |key: &[u8]| -> Option<String> {
        match info.get(key).ok()? {
            Object::String(bytes, _) => decode_pdf_text(bytes),
            Object::Name(name) => Some(String::from_utf8_lossy(name).into_owned()),
            _ => None,
        }
    };
    meta.title = get(b"Title");
    meta.author = get(b"Author");
    meta.subject = get(b"Subject");
    meta.keywords = get(b"Keywords");
    meta.creator = get(b"Creator");
    meta.producer = get(b"Producer");
    meta.creation_date = get(b"CreationDate");
    meta.modification_date = get(b"ModDate");
    meta
}

/// Decode a PDF text string: UTF-16BE with a byte-order mark, UTF-8 with a
/// `FEFF` marker, or PDFDocEncoding/Latin-1.
pub fn decode_pdf_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let utf16: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&utf16).ok();
    }
    if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        return String::from_utf8(bytes[3..].to_vec()).ok();
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let utf16: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&utf16).ok();
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// Write the Info dictionary, creating it if the document has none.
/// Returns `true` when a field the user asked for actually changed.
pub fn write_info(
    doc: &mut Document,
    title: Option<&str>,
    author: Option<&str>,
    subject: Option<&str>,
    keywords: Option<&str>,
    producer: &str,
) -> Result<bool> {
    let info_id: ObjectId = match doc.trailer.get(b"Info").ok().and_then(|o| o.as_reference().ok()) {
        Some(id) => id,
        None => {
            let id: ObjectId = (doc.max_id + 1, 0);
            doc.set_object(id, Object::Dictionary(dictionary! {}));
            if doc.max_id < id.0 {
                doc.max_id = id.0;
            }
            doc.trailer.set("Info", Object::Reference(id));
            id
        }
    };

    let mut changed = false;
    let fields: [(&[u8], Option<&str>); 4] = [
        (b"Title", title),
        (b"Author", author),
        (b"Subject", subject),
        (b"Keywords", keywords),
    ];

    for (key, value) in fields {
        // `None` means "leave this field alone"; an empty string means "clear it".
        let Some(text) = value else { continue };
        let text = text.trim();
        let new = if text.is_empty() {
            Object::Null
        } else {
            Object::String(text.as_bytes().to_vec(), lopdf::StringFormat::Literal)
        };
        let current = doc.get_dictionary(info_id).ok().and_then(|d| d.get(key).ok().cloned());
        let differs = match (&current, &new) {
            (None, Object::Null) => false,
            (None, _) => true,
            (Some(existing), Object::Null) => *existing != Object::Null,
            (Some(existing), _) => *existing != new,
        };
        if !differs {
            continue;
        }
        if let Ok(dict) = doc.get_dictionary_mut(info_id) {
            if new == Object::Null {
                dict.remove(key);
            } else {
                dict.set(key, new);
            }
        }
        changed = true;
    }

    // Producer is always stamped so it is obvious which tool last wrote the file.
    let producer = format!("{producer} (BritPDF {})", env!("CARGO_PKG_VERSION"));
    let stamp_producer = Object::String(producer.into_bytes(), lopdf::StringFormat::Literal);
    let same = doc
        .get_dictionary(info_id)
        .ok()
        .and_then(|d| d.get(b"Producer").ok().cloned())
        .map(|existing| existing == stamp_producer)
        .unwrap_or(false);
    if !same {
        if let Ok(dict) = doc.get_dictionary_mut(info_id) {
            dict.set("Producer", stamp_producer);
        }
        changed = true;
    }

    // ModDate always reflects this save.
    let now = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    let stamp = Object::String(format!("D:{now}+00'00'").into_bytes(), lopdf::StringFormat::Literal);
    if let Ok(dict) = doc.get_dictionary_mut(info_id) {
        dict.set("ModDate", stamp);
    }

    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_builder_round_trips() {
        let content = ContentBuilder::new()
            .save_state()
            .fill_color_rgb(1.0, 0.0, 0.0)
            .rect(10.0, 10.0, 50.0, 20.0)
            .fill()
            .restore_state()
            .encode()
            .unwrap();
        let text = String::from_utf8_lossy(&content);
        assert!(text.contains("1 0 0 rg"));
        assert!(text.contains("10 10 50 20 re"));
        assert!(text.contains("f\n"));
        assert!(text.contains("Q"));
    }

    #[test]
    fn ellipse_is_drawn_with_curves() {
        let bytes = ContentBuilder::new().ellipse(0.0, 0.0, 100.0, 50.0).encode().unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(text.matches(" c\n").count(), 4);
    }

    #[test]
    fn blank_document_has_one_page_and_a_catalog() {
        let mut doc = blank_document(595.0, 842.0);
        let bytes = serialise(&mut doc, false).unwrap();
        assert_eq!(doc.get_pages().len(), 1);
        assert!(bytes.starts_with(b"%PDF-1.7"));
        assert!(doc.trailer.get(b"Root").is_ok());
    }

    #[test]
    fn text_width_grows_with_length() {
        let short = ContentBuilder::text_width_em("ab", 12.0, false, false);
        let long = ContentBuilder::text_width_em("abcd", 12.0, false, false);
        assert!(long > short);
    }

    #[test]
    fn clamps_colour_components() {
        let bytes = ContentBuilder::new().fill_color_rgb(2.0, -1.0, 0.5).encode().unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("1 0 0.5 rg"));
    }
}
