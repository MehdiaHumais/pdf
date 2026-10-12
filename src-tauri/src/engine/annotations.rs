//! Writing annotations into a PDF document with lopdf.
//!
//! The UI keeps a live overlay of the user's annotations while drawing, then
//! hands them to the engine at save time. Every annotation here is a genuine PDF
//! annotation object: markup (highlight/underline/strikeout/squiggly), ink, the
//! four shape types, polygon/callout, free text, sticky notes and stamps. Vector
//! and text annotations get real `/AP` appearance streams so every viewer renders
//! them the same way Pdfium does.
//!
//! All coordinates arrive in *viewer* space - PDF points with the origin at the
//! top-left, y growing down - and are converted to PDF user space (origin at the
//! bottom-left, y growing up) here, so the UI never has to reason about PDF space.

use crate::engine::types::{AnnotationKind, AnnotationSpec, PageAnnotation, Point, Rect};
use crate::engine::writer::{appearance_stream, ensure_font, ContentBuilder};
use crate::engine::{structure, writer};
use crate::error::{BritError, ErrorKind, Result};
use lopdf::{dictionary, Dictionary, Object, ObjectId};
use std::sync::atomic::{AtomicU64, Ordering};

static ANNO_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Replace the annotation set in `doc` with `specs`. The UI keeps its overlay
/// in sync with the file (it reloads every annotation when drawing starts), so
/// `specs` is the *complete* desired set: pages that have no specs end up with
/// no `/Annots`, and every page that does have specs gets exactly those, which
/// makes saving idempotent - re-saving never duplicates annotations. Specs
/// reference pages by their 0-based index; page order is taken from the live
/// page tree.
pub fn apply(doc: &mut lopdf::Document, specs: &[AnnotationSpec]) -> Result<()> {
    let page_ids = structure::page_ids(doc);
    if page_ids.is_empty() {
        return Err(BritError::new(ErrorKind::InvalidRequest, "The document has no pages."));
    }
    for (idx, page_id) in page_ids.iter().enumerate() {
        let on_page: Vec<&AnnotationSpec> = specs.iter().filter(|s| s.page == idx).collect();
        let mut refs: Vec<Object> = Vec::new();
        if !on_page.is_empty() {
            let (_, _, _, h) = writer::page_size(doc, *page_id);
            for spec in on_page {
                for dict in build(doc, *page_id, spec, h)? {
                    let annot_id = doc.add_object(Object::Dictionary(dict));
                    refs.push(Object::Reference(annot_id));
                }
            }
        }
        set_page_annots(doc, *page_id, refs)?;
    }
    Ok(())
}

/// Set (or clear, when `refs` is empty) a page's `/Annots` array, garbage
/// collecting the annotation objects being replaced so repeated saves do not
/// bloat the file with dead objects (lopdf serialises every object in its
/// store, reachable or not). Only the annotation dict and the appearance
/// stream it points at through `/AP /N` are removed; anything deeper - fonts,
/// graphic states - is shared with the page and stays.
fn set_page_annots(doc: &mut lopdf::Document, page_id: lopdf::ObjectId, refs: Vec<Object>) -> Result<()> {
    let page = doc.get_dictionary_mut(page_id).map_err(|e| {
        BritError::new(ErrorKind::CorruptDocument, format!("The page could not be opened: {e}"))
    })?;
    let old: Vec<Object> = match page.get(b"Annots").ok().cloned() {
        Some(Object::Array(arr)) => arr,
        _ => Vec::new(),
    };
    if refs.is_empty() {
        page.remove(b"Annots");
    } else {
        page.set("Annots", Object::Array(refs));
    }

    let mut dead: Vec<ObjectId> = Vec::new();
    for obj in &old {
        let Object::Reference(id) = obj else { continue };
        if let Ok(Object::Dictionary(dict)) = doc.get_object(*id).cloned() {
            if let Ok(Object::Dictionary(ap)) = dict.get(b"AP") {
                if let Some(Object::Reference(stream_id)) = ap.get(b"N").ok().cloned() {
                    dead.push(stream_id);
                }
            }
        }
        dead.push(*id);
    }
    for id in dead {
        let _ = doc.remove_object(&id);
    }
    Ok(())
}

/// Read every annotation embedded in `doc`, in page order, translating each back
/// into viewer coordinates (top-left origin, y down). Annotations written by
/// other tools are included where their subtype maps to a known kind; anything
/// unmappable is skipped.
pub fn read(doc: &lopdf::Document) -> Result<Vec<PageAnnotation>> {
    let page_ids = structure::page_ids(doc);
    let mut out = Vec::new();
    for (page, page_id) in page_ids.iter().enumerate() {
        let (_, _, _, ph) = writer::page_size(doc, *page_id);
        let page_dict = doc.get_dictionary(*page_id).map_err(|e| {
            BritError::new(ErrorKind::CorruptDocument, format!("The page could not be read: {e}"))
        })?;
        let Some(Object::Array(annots)) = page_dict.get(b"Annots").ok().cloned() else {
            continue;
        };
        for obj in annots {
            let Object::Reference(id) = obj else { continue };
            let Ok(dict) = doc.get_dictionary(id) else { continue };
            let Some(spec) = parse_annotation(dict, ph)? else { continue };
            out.push(PageAnnotation { page, spec });
        }
    }
    Ok(out)
}

fn parse_annotation(dict: &Dictionary, ph: f32) -> Result<Option<AnnotationSpec>> {
    let subtype = match dict.get(b"Subtype").ok() {
        Some(Object::Name(name)) => String::from_utf8_lossy(name).into_owned(),
        _ => return Ok(None),
    };
    let kind = match subtype.as_str() {
        "Highlight" => AnnotationKind::Highlight,
        "Underline" => AnnotationKind::Underline,
        "StrikeOut" => AnnotationKind::Strikeout,
        "Squiggly" => AnnotationKind::Squiggly,
        "Ink" => AnnotationKind::Freehand,
        "Square" => AnnotationKind::Rectangle,
        "Circle" => AnnotationKind::Circle,
        "Line" => {
            if dict.get(b"LE").is_ok() {
                AnnotationKind::Arrow
            } else {
                AnnotationKind::Line
            }
        }
        "Polygon" => AnnotationKind::Polygon,
        "PolyLine" => AnnotationKind::Polygon,
        "Text" => AnnotationKind::StickyNote,
        "FreeText" => {
            let is_callout = match dict.get(b"IT").ok() {
                Some(Object::Name(name)) => name == b"FreeTextCallout",
                _ => false,
            };
            if is_callout {
                AnnotationKind::Callout
            } else {
                AnnotationKind::TextBox
            }
        }
        "Stamp" => AnnotationKind::Stamp,
        "Redact" => AnnotationKind::Redaction,
        _ => return Ok(None),
    };
    let quads = annotation_quads(dict, ph);
    let rect = annotation_rect(dict, ph, &quads);
    if rect.is_none() && quads.is_empty() && points_of(dict, ph).is_empty() {
        return Ok(None);
    }
    let contents = annotation_text(dict, b"Contents");
    let author = annotation_text(dict, b"T").filter(|s| !s.is_empty());
    Ok(Some(AnnotationSpec {
        kind,
        page: 0,
        rect: rect.unwrap_or_else(|| Rect::new(0.0, 0.0, 1.0, 1.0)),
        points: points_of(dict, ph),
        quads,
        contents,
        author,
        color: annotation_color(dict),
        thickness: annotation_thickness(dict),
        opacity: annotation_opacity(dict),
        font_size: annotation_font_size(dict),
        label: None,
        filled: dict.get(b"IC").is_ok(),
        rotation: match dict.get(b"Rotate").ok().and_then(num) {
            Some(r) => r,
            None => 0.0,
        },
    }))
}

fn num(o: &Object) -> Option<f32> {
    match o {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

fn annotation_text(dict: &Dictionary, key: &[u8]) -> Option<String> {
    match dict.get(key).ok()? {
        Object::String(bytes, _) => String::from_utf8(bytes.clone()).ok(),
        _ => None,
    }
}

/// PDF-space `/Rect` is `[x1 y1 x2 y2]` in bottom-left origin; translate into
/// viewer space (top-left origin, y down).
fn annotation_rect(dict: &Dictionary, ph: f32, quads: &[Rect]) -> Option<Rect> {
    if let Ok(Object::Array(a)) = dict.get(b"Rect") {
        let nums: Vec<f32> = a.iter().filter_map(num).collect();
        if nums.len() == 4 {
            let (x1, y1, x2, y2) = (nums[0], nums[1], nums[2], nums[3]);
            return Some(Rect {
                left: x1.min(x2),
                top: ph - y1.max(y2),
                right: x1.max(x2),
                bottom: ph - y1.min(y2),
            });
        }
    }
    if !quads.is_empty() {
        let mut left = f32::MAX;
        let mut top = f32::MAX;
        let mut right = f32::MIN;
        let mut bottom = f32::MIN;
        for q in quads {
            left = left.min(q.left);
            top = top.min(q.top);
            right = right.max(q.right);
            bottom = bottom.max(q.bottom);
        }
        return Some(Rect { left, top, right, bottom });
    }
    None
}

/// Each quad is 8 numbers `[x1 y1 x2 y2 x3 y3 x4 y4]` in PDF space; collapse to
/// its viewer-space bounding box.
fn annotation_quads(dict: &Dictionary, ph: f32) -> Vec<Rect> {
    let Ok(Object::Array(a)) = dict.get(b"QuadPoints") else {
        return Vec::new();
    };
    let nums: Vec<f32> = a.iter().filter_map(num).collect();
    let mut out = Vec::new();
    for group in nums.chunks_exact(8) {
        let mut left = f32::MAX;
        let mut right = f32::MIN;
        let mut max_y = f32::MIN;
        let mut min_y = f32::MAX;
        for (i, v) in group.iter().enumerate() {
            if i % 2 == 0 {
                left = left.min(*v);
                right = right.max(*v);
            } else {
                max_y = max_y.max(*v);
                min_y = min_y.min(*v);
            }
        }
        if left <= right {
            out.push(Rect { left, top: ph - max_y, right, bottom: ph - min_y });
        }
    }
    out
}

fn points_of(dict: &Dictionary, ph: f32) -> Vec<Point> {
    let Ok(Object::Array(strokes)) = dict.get(b"InkList") else {
        return Vec::new();
    };
    for o in strokes {
        let Object::Array(stroke) = o else { continue };
        let mut pts = Vec::new();
        for pair in stroke.chunks(2) {
            if pair.len() == 2 {
                if let (Some(x), Some(y)) = (num(&pair[0]), num(&pair[1])) {
                    pts.push(Point { x, y: ph - y });
                }
            }
        }
        if !pts.is_empty() {
            return pts;
        }
    }
    Vec::new()
}

fn annotation_color(dict: &Dictionary) -> [f32; 3] {
    if let Ok(Object::Array(a)) = dict.get(b"C") {
        let nums: Vec<f32> = a.iter().filter_map(num).collect();
        if nums.len() >= 3 {
            return [nums[0].clamp(0.0, 1.0), nums[1].clamp(0.0, 1.0), nums[2].clamp(0.0, 1.0)];
        }
    }
    [1.0, 0.85, 0.2]
}

fn annotation_opacity(dict: &Dictionary) -> f32 {
    match dict.get(b"CA").ok().and_then(num) {
        Some(v) => v.clamp(0.0, 1.0),
        None => 1.0,
    }
}

fn annotation_thickness(dict: &Dictionary) -> f32 {
    if let Ok(Object::Dictionary(bs)) = dict.get(b"BS") {
        if let Some(w) = bs.get(b"W").ok().and_then(num) {
            return w.max(0.1);
        }
    }
    2.0
}

fn annotation_font_size(dict: &Dictionary) -> f32 {
    if let Ok(Object::Dictionary(fs)) = dict.get(b"FS") {
        if let Some(v) = fs.get(b"Size").ok().and_then(num) {
            return v;
        }
    }
    if let Ok(Object::String(da, _)) = dict.get(b"DA") {
        let s = String::from_utf8_lossy(da);
        for tok in s.split_whitespace() {
            if let Ok(v) = tok.parse::<f32>() {
                return v;
            }
        }
    }
    12.0
}

/// Build the annotation dictionary (appearance + all fields) for one spec.
/// Callout produces two objects: the text and the pointer line.
fn build(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    spec: &AnnotationSpec,
    ph: f32,
) -> Result<Vec<Dictionary>> {
    let rect = to_pdf_rect(spec.rect, ph);
    let color = rgb(spec.color);
    let nm = format!(
        "britpdf-{:03}-{}",
        spec.page + 1,
        ANNO_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let author = spec.author.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "BritPDF".to_string());

    let base = |subtype: &[u8], rect: Rect| -> Dictionary {
        dictionary! {
            "Type" => Object::Name(b"Annot".to_vec()),
            "Subtype" => Object::Name(subtype.to_vec()),
            "Rect" => rect_array(rect),
            "F" => Object::Integer(12),
            "NM" => Object::String(nm.clone().into_bytes(), lopdf::StringFormat::Literal),
            "O" => Object::String(author.clone().into_bytes(), lopdf::StringFormat::Literal),
        }
    };

    match spec.kind {
        AnnotationKind::Highlight
        | AnnotationKind::Underline
        | AnnotationKind::Strikeout
        | AnnotationKind::Squiggly => {
            let subtype: &[u8] = match spec.kind {
                AnnotationKind::Highlight => b"Highlight",
                AnnotationKind::Underline => b"Underline",
                AnnotationKind::Strikeout => b"StrikeOut",
                _ => b"Squiggly",
            };
            let mut d = base(subtype, rect);
            let quads = if spec.quads.is_empty() {
                vec![spec.rect]
            } else {
                spec.quads.clone()
            };
            let mut arr = Vec::with_capacity(quads.len() * 8);
            for q in quads {
                let (l, b, r, t) = to_pdf_rect(q, ph).sides();
                arr.extend_from_slice(&[
                    Object::Real(l), Object::Real(t),
                    Object::Real(r), Object::Real(t),
                    Object::Real(l), Object::Real(b),
                    Object::Real(r), Object::Real(b),
                ]);
            }
            d.set("QuadPoints", Object::Array(arr));
            d.set("C", Object::Array(color));
            d.set("CA", Object::Real(spec.opacity.clamp(0.0, 1.0)));
            if let Some(contents) = spec.contents.as_deref().filter(|s| !s.is_empty()) {
                d.set("Contents", Object::String(contents.as_bytes().to_vec(), lopdf::StringFormat::Literal));
                d.set("T", Object::String(contents.as_bytes().to_vec(), lopdf::StringFormat::Literal));
            }
            Ok(vec![d])
        }

        AnnotationKind::Freehand | AnnotationKind::Pen => {
            let mut d = base(b"Ink", rect);
            d.set(
                "InkList",
                Object::Array(vec![Object::Array(
                    spec.points.iter().map(|p| pdf_pt(p, ph)).flat_map(|(x, y)| [Object::Real(x), Object::Real(y)]).collect(),
                )]),
            );
            d.set("C", Object::Array(color));
            d.set("CA", Object::Real(spec.opacity.clamp(0.0, 1.0)));
            d.set(
                "BS",
                Object::Dictionary(dictionary! {
                    "W" => Object::Real(spec.thickness.max(0.5)),
                    "S" => Object::Name(b"S".to_vec()),
                }),
            );
            let ap = ink_ap(doc, page_id, rect, spec)?;
            set_ap(doc, page_id, &mut d, rect, ap);
            Ok(vec![d])
        }

        AnnotationKind::Rectangle | AnnotationKind::Circle => {
            let subtype: &[u8] = if spec.kind == AnnotationKind::Rectangle { b"Square" } else { b"Circle" };
            let mut d = base(subtype, rect);
            d.set("C", Object::Array(color.clone()));
            d.set("CA", Object::Real(spec.opacity.clamp(0.0, 1.0)));
            if spec.filled {
                d.set("IC", Object::Array(color.clone()));
            }
            d.set(
                "BS",
                Object::Dictionary(dictionary! {
                    "W" => Object::Real(spec.thickness.max(0.5)),
                    "S" => Object::Name(b"S".to_vec()),
                }),
            );
            let ap = shape_ap(doc, page_id, rect, spec)?;
            set_ap(doc, page_id, &mut d, rect, ap);
            Ok(vec![d])
        }

        AnnotationKind::Line | AnnotationKind::Arrow | AnnotationKind::Callout => {
            let a = spec.points.first().copied().unwrap_or(Point::new(rect.left(), rect.bottom()));
            let b = spec.points.get(1).copied().unwrap_or(Point::new(rect.right(), rect.top()));
            let (ax, ay) = pdf_pt(&a, ph);
            let (bx, by) = pdf_pt(&b, ph);
            let mut d = base(b"Line", rect);
            d.set("L", Object::Array(vec![Object::Real(ax), Object::Real(ay), Object::Real(bx), Object::Real(by)]));
            d.set("C", Object::Array(color));
            d.set("CA", Object::Real(spec.opacity.clamp(0.0, 1.0)));
            d.set(
                "BS",
                Object::Dictionary(dictionary! {
                    "W" => Object::Real(spec.thickness.max(0.5)),
                    "S" => Object::Name(b"S".to_vec()),
                }),
            );
            if spec.kind == AnnotationKind::Arrow {
                d.set(
                    "LE",
                    Object::Array(vec![
                        Object::Name(b"None".to_vec()),
                        Object::Name(b"OpenArrow".to_vec()),
                    ]),
                );
            }
            let ap = line_ap(doc, page_id, rect, spec, a, b)?;
            set_ap(doc, page_id, &mut d, rect, ap);
            if spec.kind == AnnotationKind::Callout {
                // A callout couples the pointer with a free-host text note.
                let label = spec
                    .label
                    .clone()
                    .or_else(|| spec.contents.clone())
                    .unwrap_or_else(|| "Note".to_string());
                let text_w = (spec.font_size * label.chars().count() as f32 * 0.55).clamp(40.0, 260.0);
                let text_h = spec.font_size.max(4.0) * 1.25 + 4.0;
                let (mut tx, mut ty) = (a.x - text_w / 2.0, a.y - text_h);
                tx = tx.clamp(4.0, ph - text_w - 4.0);
                ty = ty.clamp(4.0, ph - text_h - 4.0);
                let (pdf_l, pdf_b) = pdf_pt(&Point::new(tx, ty + text_h), ph);
                let (pdf_r, pdf_t) = pdf_pt(&Point::new(tx + text_w, ty), ph);
                let pdf_rect = Rect::new(pdf_l, pdf_t, pdf_r, pdf_b);
                let mut ns = spec.clone();
                ns.rect = pdf_rect;
                ns.kind = AnnotationKind::TextBox;
                ns.contents = Some(label.clone());
                let mut n = base(b"FreeText", pdf_rect);
                n.set(
                    "Contents",
                    Object::String(label.as_bytes().to_vec(), lopdf::StringFormat::Literal),
                );
                n.set("DA", Object::String(format!("/Helv {} Tf 0 g", spec.font_size.max(4.0)).into_bytes(), lopdf::StringFormat::Literal));
                n.set("Q", Object::Integer(0));
                n.set("C", Object::Array(rgb([0.0, 0.0, 0.0])));
                n.set("IC", Object::Array(rgb([1.0, 0.97, 0.85])));
                let ap = text_ap(doc, page_id, pdf_rect, &ns, &label)?;
                set_ap(doc, page_id, &mut n, pdf_rect, ap);
                return Ok(vec![d, n]);
            }
            Ok(vec![d])
        }

        AnnotationKind::Triangle | AnnotationKind::Polygon | AnnotationKind::Cloud => {
            let subtype: &[u8] = match spec.kind {
                AnnotationKind::Cloud => b"Polygon",
                _ => b"Polygon",
            };
            let mut d = base(subtype, rect);
            let vertices: Vec<Object> = spec
                .points
                .iter()
                .map(|p| pdf_pt(p, ph))
                .flat_map(|(x, y)| [Object::Real(x), Object::Real(y)])
                .collect();
            d.set("Vertices", Object::Array(vertices));
            d.set("C", Object::Array(color.clone()));
            d.set("CA", Object::Real(spec.opacity.clamp(0.0, 1.0)));
            if spec.filled {
                d.set("IC", Object::Array(color.clone()));
            }
            d.set(
                "BS",
                Object::Dictionary(dictionary! {
                    "W" => Object::Real(spec.thickness.max(0.5)),
                    "S" => Object::Name(b"S".to_vec()),
                }),
            );
            if spec.kind == AnnotationKind::Cloud {
                d.set(
                    "BS",
                    Object::Dictionary(dictionary! {
                        "W" => Object::Real(spec.thickness.max(1.0)),
                        "S" => Object::Name(b"C".to_vec()),
                    }),
                );
            }
            let ap = polygon_ap(doc, page_id, rect, spec)?;
            set_ap(doc, page_id, &mut d, rect, ap);
            Ok(vec![d])
        }

        AnnotationKind::TextBox | AnnotationKind::Stamp => {
            let is_box = spec.kind == AnnotationKind::TextBox;
            let mut d = base(if is_box { b"FreeText" } else { b"Stamp" }, rect);
            let label = spec
                .label
                .clone()
                .or_else(|| spec.contents.clone())
                .unwrap_or_default();
            if !label.is_empty() {
                d.set(
                    "Contents",
                    Object::String(label.as_bytes().to_vec(), lopdf::StringFormat::Literal),
                );
                d.set(
                    "T",
                    Object::String(label.as_bytes().to_vec(), lopdf::StringFormat::Literal),
                );
            }
            d.set("C", Object::Array(color.clone()));
            let da = if is_box {
                format!("/Helv {} Tf 0 g", spec.font_size.max(4.0))
            } else {
                "/Helv 14 Tf 0.25 g".to_string()
            };
            d.set("DA", Object::String(da.into_bytes(), lopdf::StringFormat::Literal));
            if is_box {
                d.set("Q", Object::Integer(0));
                d.set("CA", Object::Real(1.0));
            }
            let ap = text_ap(doc, page_id, rect, spec, &label)?;
            set_ap(doc, page_id, &mut d, rect, ap);
            Ok(vec![d])
        }

        AnnotationKind::StickyNote => {
            let mut d = base(b"Text", rect);
            d.set("C", Object::Array(color));
            if let Some(contents) = spec.contents.as_deref().filter(|s| !s.is_empty()) {
                d.set(
                    "Contents",
                    Object::String(contents.as_bytes().to_vec(), lopdf::StringFormat::Literal),
                );
                d.set(
                    "Subj",
                    Object::String(b"Note".to_vec(), lopdf::StringFormat::Literal),
                );
            }
            // A closed note: the popup is hidden until the user opens it.
            let mut popup = dictionary! {
                "Type" => Object::Name(b"Annot".to_vec()),
                "Subtype" => Object::Name(b"Popup".to_vec()),
                "Parent" => Object::Null,
                "Open" => Object::Boolean(false),
            };
            popup.set("Rect", rect_array(rect));
            let popup_id = doc.add_object(Object::Dictionary(popup));
            if let Ok(page) = doc.get_dictionary_mut(page_id) {
                let arr = match page.get(b"Annots").ok().cloned() {
                    Some(Object::Array(mut arr)) => {
                        arr.push(Object::Reference(popup_id));
                        arr
                    }
                    _ => vec![Object::Reference(popup_id)],
                };
                page.set("Annots", Object::Array(arr));
            }
            d.set("Popup", Object::Reference(popup_id));
            Ok(vec![d])
        }

        AnnotationKind::Redaction => Err(BritError::new(
            ErrorKind::UnsupportedFeature,
            "Redaction is applied through the redaction workflow, not the markup tools.",
        )),
    }
}

fn set_ap(
    doc: &mut lopdf::Document,
    _page_id: lopdf::ObjectId,
    dict: &mut Dictionary,
    _rect: Rect,
    ap: lopdf::Stream,
) {
    let id = doc.add_object(ap);
    let mut normal = dictionary! {};
    normal.set("N", Object::Reference(id));
    dict.set("AP", Object::Dictionary(normal));
}

/* ------------------------------ appearance streams ------------------------------ */

/// Appearance for a freehand / pen stroke inside its own annotation box.
fn ink_ap(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    rect: Rect,
    spec: &AnnotationSpec,
) -> Result<lopdf::Stream> {
    let (l, b, w, h, _) = ap_dimensions(rect);
    let mut content = ContentBuilder::new()
        .save_state()
        .line_width(spec.thickness.max(0.5))
        .line_cap_round()
        .line_join_round();
    if spec.opacity < 1.0 {
        let gs = writer::ensure_alpha_state(doc, page_id, spec.opacity, spec.opacity)?;
        content = content.op_gs(&gs);
    }
    content = content.stroke_color_rgb(spec.color[0], spec.color[1], spec.color[2]);
    let mut first = true;
    for p in &spec.points {
        let (x, y) = (p.x - l, p.y - b);
        content = if first {
            first = false;
            content.move_to(x, y)
        } else {
            content.line_to(x, y)
        };
    }
    let raw = content.stroke().restore_state().encode()?;
    Ok(appearance_stream([0.0, 0.0, w, h], raw, Dictionary::new()))
}

/// Square / circle appearance.
fn shape_ap(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    rect: Rect,
    spec: &AnnotationSpec,
) -> Result<lopdf::Stream> {
    let (_, _, w, h, _) = ap_dimensions(rect);
    let lw = spec.thickness.max(0.5);
    let mut content = ContentBuilder::new().save_state().line_width(lw);
    if spec.opacity < 1.0 {
        let gs = writer::ensure_alpha_state(doc, page_id, spec.opacity, spec.opacity)?;
        content = content.op_gs(&gs);
    }
    if spec.filled {
        content = content.fill_color_rgb(spec.color[0], spec.color[1], spec.color[2]);
    }
    content = content.stroke_color_rgb(spec.color[0], spec.color[1], spec.color[2]);
    let inset = lw / 2.0;
    content = if spec.kind == AnnotationKind::Rectangle {
        content.rect(inset, inset, (w - lw).max(0.1), (h - lw).max(0.1))
    } else {
        content.ellipse(inset, inset, (w - lw).max(0.1), (h - lw).max(0.1))
    };
    content = if spec.filled { content.fill_and_stroke() } else { content.stroke() };
    let raw = content.restore_state().encode()?;
    Ok(appearance_stream([0.0, 0.0, w, h], raw, Dictionary::new()))
}

/// Line / arrow appearance (and the callout pointer).
fn line_ap(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    rect: Rect,
    spec: &AnnotationSpec,
    a: Point,
    b: Point,
) -> Result<lopdf::Stream> {
    let (l, b0, w, h, _) = ap_dimensions(rect);
    let (ax, ay) = (a.x - l, a.y - b0);
    let (bx, by) = (b.x - l, b.y - b0);
    let lw = spec.thickness.max(0.5);
    let mut content = ContentBuilder::new()
        .save_state()
        .line_width(lw)
        .line_cap_round();
    if spec.opacity < 1.0 {
        let gs = writer::ensure_alpha_state(doc, page_id, spec.opacity, spec.opacity)?;
        content = content.op_gs(&gs);
    }
    content = content
        .stroke_color_rgb(spec.color[0], spec.color[1], spec.color[2])
        .move_to(ax, ay)
        .line_to(bx, by);
    if spec.kind == AnnotationKind::Arrow {
        let ang = f32::atan2(by - ay, bx - ax);
        let len = (8.0 + lw).max(10.0);
        for side in [1.0f32, -1.0] {
            let (dx, dy) = (len * f32::cos(ang + side * 0.70), len * f32::sin(ang + side * 0.70));
            content = content.line_to(bx + dx, by + dy).move_to(bx, by);
        }
    }
    let raw = content.stroke().restore_state().encode()?;
    Ok(appearance_stream([0.0, 0.0, w, h], raw, Dictionary::new()))
}

/// Polygon / triangle / cloud appearance.
fn polygon_ap(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    rect: Rect,
    spec: &AnnotationSpec,
) -> Result<lopdf::Stream> {
    let (l, b0, w, h, _) = ap_dimensions(rect);
    let lw = spec.thickness.max(0.5);
    let mut content = ContentBuilder::new().save_state().line_width(lw);
    if spec.opacity < 1.0 {
        let gs = writer::ensure_alpha_state(doc, page_id, spec.opacity, spec.opacity)?;
        content = content.op_gs(&gs);
    }
    if spec.filled {
        content = content.fill_color_rgb(spec.color[0], spec.color[1], spec.color[2]);
    }
    content = content.stroke_color_rgb(spec.color[0], spec.color[1], spec.color[2]);
    let mut first = true;
    for p in &spec.points {
        let (x, y) = (p.x - l, p.y - b0);
        content = if first {
            first = false;
            content.move_to(x, y)
        } else {
            content.line_to(x, y)
        };
    }
    content = content.close_path();
    content = if spec.filled { content.fill_and_stroke() } else { content.stroke() };
    let raw = content.restore_state().encode()?;
    if spec.kind == AnnotationKind::Cloud {
        // A cloud border needs a bit of headroom; leave the stroke default.
        return Ok(appearance_stream([0.0, 0.0, w, h], raw, Dictionary::new()));
    }
    Ok(appearance_stream([0.0, 0.0, w, h], raw, Dictionary::new()))
}

/// Free text / stamp appearance: a translucent box with the label centred.
fn text_ap(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    rect: Rect,
    spec: &AnnotationSpec,
    label: &str,
) -> Result<lopdf::Stream> {
    let (_, _, w, h, _) = ap_dimensions(rect);
    let font = font_name(spec.label.as_deref().unwrap_or(""), spec.kind == AnnotationKind::TextBox);
    let font_resource = ensure_font(doc, page_id, font)?;
    let font_ref = page_font_ref(doc, page_id, &font_resource)?;
    let mut font_dict = Dictionary::new();
    font_dict.set(font_resource.clone(), Object::Reference(font_ref));

    let font_size = if spec.kind == AnnotationKind::TextBox {
        spec.font_size.max(4.0).min(h * 0.9)
    } else {
        50.0f32.min(h * 0.62).min(w * 0.2)
    };
    let box_h = if spec.kind == AnnotationKind::TextBox { spec.font_size * 1.25 + 4.0 } else { h };

    let mut content = ContentBuilder::new().save_state();
    if spec.kind == AnnotationKind::Stamp {
        content = content
            .fill_color_rgb(spec.color[0], spec.color[1], spec.color[2]);
        content = content
            .rect(0.0, 0.0, w, h)
            .fill_and_stroke();
        content = content.fill_color_rgb(1.0, 1.0, 1.0);
    } else {
        content = content
            .fill_color_rgb(1.0, 0.97, 0.85)
            .rect(0.0, 0.0, w, box_h)
            .fill_and_stroke()
            .fill_color_rgb(0.1, 0.1, 0.1);
    }

    // Stamp labels are commonly rotated -45 degrees; boxes are plain text.
    if spec.kind == AnnotationKind::Stamp && (spec.rotation % 360.0).abs() > 1.0 {
        let rot = spec.rotation.to_radians();
        content = content.concat_matrix(f32::cos(rot), f32::sin(rot), -f32::sin(rot), f32::cos(rot), w / 2.0, h / 2.0);
        content = content
            .begin_text()
            .set_font(&font_resource, font_size)
            .show_text(label)
            .end_text();
    } else {
        let baseline = (box_h - font_size) / 2.0 + font_size * 0.85;
        let x = w / 2.0 - (font_size * label.chars().count() as f32 * 0.52) / 2.0;
        content = content
            .begin_text()
            .set_font(&font_resource, font_size)
            .text_matrix(x.max(1.0), baseline)
            .show_text(label)
            .end_text();
    }
    let raw = content.restore_state().encode()?;

    let mut resources = Dictionary::new();
    resources.set("Font", Object::Dictionary(font_dict));
    Ok(appearance_stream([0.0, 0.0, w, h], raw, resources))
}

/// Read the object id of the font resource `name` on `page_id`.
fn page_font_ref(doc: &lopdf::Document, page_id: lopdf::ObjectId, name: &[u8]) -> Result<lopdf::ObjectId> {
    let page = doc.get_dictionary(page_id).map_err(|e| {
        BritError::new(ErrorKind::CorruptDocument, format!("The page could not be opened: {e}"))
    })?;
    let resources = page.get(b"Resources").ok().and_then(|r| r.as_dict().ok());
    let fonts = resources.and_then(|r| r.get(b"Font").ok().and_then(|f| f.as_dict().ok()));
    if let Some(fonts) = fonts {
        for (key, value) in fonts.iter() {
            if key.as_slice() == name {
                if let Ok(id) = value.as_reference() {
                    return Ok(id);
                }
            }
        }
    }
    Err(BritError::new(ErrorKind::Internal, "The appearance font could not be registered."))
}

/// Map the UI's font-family label onto a base-14 font ensure_font understands.
fn font_name(label: &str, is_box: bool) -> &'static str {
    match label {
        "Times" | "Times-Roman" => "Times-Roman",
        "Courier" => "Courier",
        _ if is_box => "Helvetica",
        _ => "Helvetica-Bold",
    }
}

/* ------------------------------ coordinate helpers ------------------------------ */

/// Convert a viewer-space rect (top-left origin) into PDF space.
fn to_pdf_rect(r: Rect, page_height: f32) -> Rect {
    let (left, bottom) = crate::engine::session::to_pdf_space(r.left, r.bottom, page_height);
    let (right, top) = crate::engine::session::to_pdf_space(r.right, r.top, page_height);
    if right < left {
        Rect::new(right, top, left, bottom)
    } else {
        Rect::new(left, top, right, bottom)
    }
}

fn pdf_pt(p: &Point, page_height: f32) -> (f32, f32) {
    crate::engine::session::to_pdf_space(p.x, p.y, page_height)
}

fn rgb(color: [f32; 3]) -> Vec<Object> {
    vec![
        Object::Real(color[0].clamp(0.0, 1.0)),
        Object::Real(color[1].clamp(0.0, 1.0)),
        Object::Real(color[2].clamp(0.0, 1.0)),
    ]
}

fn rect_array(r: Rect) -> Vec<Object> {
    let (l, t, r_, b) = r.sides();
    vec![
        Object::Real(l.min(r_)),
        Object::Real(b.min(t)),
        Object::Real(l.max(r_)),
        Object::Real(b.max(t)),
    ]
}

impl Rect {
    fn sides(&self) -> (f32, f32, f32, f32) {
        (self.left, self.bottom, self.right, self.top)
    }
    fn left(&self) -> f32 {
        self.left.min(self.right)
    }
    fn right(&self) -> f32 {
        self.left.max(self.right)
    }
    fn bottom(&self) -> f32 {
        self.bottom.min(self.top)
    }
    fn top(&self) -> f32 {
        self.bottom.max(self.top)
    }
}

/// (left, bottom, width, height, rotation) of a PDF-space rect.
fn ap_dimensions(r: Rect) -> (f32, f32, f32, f32, f32) {
    (r.left(), r.bottom(), r.right() - r.left(), r.top() - r.bottom(), 0.0)
}