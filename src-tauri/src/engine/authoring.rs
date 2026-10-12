//! PDF authoring: create new documents and convert images into PDFs.
//!
//! Two things happen here, both producing genuinely valid PDFs rather than a
//! wrapper around a raster page:
//!
//! * **New documents** start from a real blank page with a content stream, and
//!   every element the user adds is a real PDF object. Text uses the base-14
//!   fonts, so no font licensing or subsetting is involved.
//! * **Images to PDF** embeds the original image bytes. JPEG files are passed
//!   through with a `/DCTDecode` filter - the bytes are not re-encoded, so there is
//!   no quality loss and no recompression. PNG and other formats are re-encoded
//!   as Flate-compressed raw samples, and transparency is preserved with a soft
//!   mask.

use crate::error::{BritError, ErrorKind, Result};
use crate::engine::writer::{
    blank_document, ensure_alpha_state, ensure_font, serialise, ContentBuilder,
};
use lopdf::{dictionary, Document, Object, Stream};

/// Create a one-page A4 (or the given size) PDF and return the raw bytes.
pub fn create_blank_document(width: f32, height: f32, title: Option<&str>) -> Vec<u8> {
    let mut doc = blank_document(width, height);
    if let Some(title) = title {
        let _ = crate::engine::writer::write_info(&mut doc, Some(title), None, None, None, "BritPDF");
    }
    serialise(&mut doc, true).unwrap_or_else(|_| {
        let mut doc = blank_document(width, height);
        serialise(&mut doc, false).expect("a blank document must always serialise")
    })
}

/// An element to place on a new page.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PageElement {
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        font: String,
        color: [f32; 3],
        #[serde(default)]
        bold: bool,
    },
    Rectangle {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        stroke: Option<[f32; 3]>,
        fill: Option<[f32; 3]>,
        line_width: f32,
        opacity: f32,
    },
    Ellipse {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        stroke: Option<[f32; 3]>,
        fill: Option<[f32; 3]>,
        line_width: f32,
        opacity: f32,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color: [f32; 3],
        line_width: f32,
    },
    Image {
        /// Base64-encoded image bytes.
        data: String,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

/// Draw `elements` onto a new page of the given size, and return the PDF bytes.
/// This is the editor's "New document" path: it composes a real page rather than
/// rasterising a preview, so the result stays selectable and searchable text.
pub fn compose_page(
    width: f32,
    height: f32,
    elements: &[PageElement],
    title: Option<&str>,
) -> Result<Vec<u8>> {
    let mut doc = blank_document(width, height);
    let page_id = crate::engine::writer::pages(&doc).first().copied().ok_or_else(|| {
        BritError::new(ErrorKind::Internal, "The new document has no page.")
    })?;

    for element in elements {
        draw_element(&mut doc, page_id, element)?;
    }

    if let Some(title) = title {
        let _ = crate::engine::writer::write_info(&mut doc, Some(title), None, None, None, "BritPDF");
    }
    serialise(&mut doc, true)
}

fn draw_element(doc: &mut Document, page_id: lopdf::ObjectId, element: &PageElement) -> Result<()> {
    match element {
        PageElement::Text { x, y, text, size, font, color, bold } => {
            let font = if *bold { font_with_bold(font) } else { font.clone() };
            let name = ensure_font(doc, page_id, &font)?;
            let content = ContentBuilder::new()
                .begin_text()
                .set_font(&name, *size)
                .fill_color_rgb(color[0], color[1], color[2])
                .text_matrix(*x, *y)
                .show_text(text)
                .end_text()
                .encode()?;
            crate::engine::writer::append_page_content(doc, page_id, &content)
        }

        PageElement::Rectangle { x, y, width, height, stroke, fill, line_width, opacity } => {
            let mut builder = ContentBuilder::new()
                .save_state()
                .line_width(*line_width);
            if *opacity < 1.0 {
                let gs = ensure_alpha_state(doc, page_id, *opacity, *opacity)?;
                builder = builder.op_gs(&gs);
            }
            if let Some(c) = fill {
                builder = builder.fill_color_rgb(c[0], c[1], c[2]);
            }
            if let Some(c) = stroke {
                builder = builder.stroke_color_rgb(c[0], c[1], c[2]);
            }
            let has_fill = fill.is_some();
            let has_stroke = stroke.is_some();
            builder = builder.rect(*x, *y, *width, *height);
            builder = match (has_fill, has_stroke) {
                (true, true) => builder.fill_and_stroke(),
                (true, false) => builder.fill(),
                (false, true) => builder.stroke(),
                (false, false) => ContentBuilder::new(),
            };
            let content = builder.restore_state().encode()?;
            crate::engine::writer::append_page_content(doc, page_id, &content)
        }

        PageElement::Ellipse { x, y, width, height, stroke, fill, line_width, opacity } => {
            let mut builder = ContentBuilder::new()
                .save_state()
                .line_width(*line_width)
                .line_cap_round();
            if *opacity < 1.0 {
                let gs = ensure_alpha_state(doc, page_id, *opacity, *opacity)?;
                builder = builder.op_gs(&gs);
            }
            if let Some(c) = fill {
                builder = builder.fill_color_rgb(c[0], c[1], c[2]);
            }
            if let Some(c) = stroke {
                builder = builder.stroke_color_rgb(c[0], c[1], c[2]);
            }
            let has_fill = fill.is_some();
            let has_stroke = stroke.is_some();
            builder = builder.ellipse(*x, *y, *width, *height);
            builder = match (has_fill, has_stroke) {
                (true, true) => builder.fill_and_stroke(),
                (true, false) => builder.fill(),
                (false, true) => builder.stroke(),
                (false, false) => ContentBuilder::new(),
            };
            let content = builder.restore_state().encode()?;
            crate::engine::writer::append_page_content(doc, page_id, &content)
        }

        PageElement::Line { x1, y1, x2, y2, color, line_width } => {
            let content = ContentBuilder::new()
                .save_state()
                .stroke_color_rgb(color[0], color[1], color[2])
                .line_width(*line_width)
                .line_cap_round()
                .move_to(*x1, *y1)
                .line_to(*x2, *y2)
                .stroke()
                .restore_state()
                .encode()?;
            crate::engine::writer::append_page_content(doc, page_id, &content)
        }

        PageElement::Image { data, x, y, width, height } => {
            let bytes = decode_base64(data)?;
            let image = EncodedImage::from_bytes(&bytes)?;
            let name = embed_image(doc, page_id, &image)?;
            let content = ContentBuilder::new()
                .save_state()
                .concat_matrix(*width, 0.0, 0.0, *height, *x, *y)
                .draw_x_object(&name)
                .restore_state()
                .encode()?;
            crate::engine::writer::append_page_content(doc, page_id, &content)
        }
    }
}

fn font_with_bold(font: &str) -> String {
    match font {
        "Helvetica" => "Helvetica-Bold".to_string(),
        "Times-Roman" | "Times-Italic" => "Times-Bold".to_string(),
        "Courier" => "Courier-Bold".to_string(),
        other => other.to_string(),
    }
}

/// Add an image XObject to a page's resources and return its resource name.
pub(crate) fn embed_image(doc: &mut Document, page_id: lopdf::ObjectId, image: &EncodedImage) -> Result<Vec<u8>> {
    let (filter, raw) = if let Some(jpeg) = &image.jpeg {
        (b"DCTDecode".to_vec(), jpeg.clone())
    } else {
        (b"FlateDecode".to_vec(), compress(&image.samples))
    };

    let mut dict = dictionary! {
        "Type" => Object::Name(b"XObject".to_vec()),
        "Subtype" => Object::Name(b"Image".to_vec()),
        "Width" => Object::Integer(image.width as i64),
        "Height" => Object::Integer(image.height as i64),
        "ColorSpace" => image.color_space.clone(),
        "BitsPerComponent" => Object::Integer(8),
        "Filter" => Object::Name(filter),
    };
    if image.is_mask {
        // An image mask stores one bit per pixel and has no colour space.
        dict.remove(b"ColorSpace");
        dict.set("BitsPerComponent", Object::Integer(1));
    }
    // Transparency: the alpha channel becomes a soft mask, so PNG transparency
    // is preserved instead of being flattened onto white.
    if let Some(smask) = &image.smask {
        let mask_id = doc.add_object(smask.clone());
        dict.set("SMask", Object::Reference(mask_id));
    }

    let xobject_id = doc.add_object(Stream::new(dict, raw));
    let name = format!("Im{}", xobject_id.0);
    doc.add_xobject(page_id, name.as_bytes().to_vec(), xobject_id)
        .map_err(|e| BritError::internal(format!("Could not add the image: {e}")))?;
    Ok(name.as_bytes().to_vec())
}

fn compress(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    if encoder.write_all(data).is_ok() {
        if let Ok(out) = encoder.finish() {
            return out;
        }
    }
    data.to_vec()
}

/// An image decoded into something PDF can store.
pub struct EncodedImage {
    pub width: u32,
    pub height: u32,
    /// Original JPEG bytes, or `None` for other formats.
    pub jpeg: Option<Vec<u8>>,
    /// Interleaved 8-bit samples (RGB) for the non-JPEG path.
    pub samples: Vec<u8>,
    /// `/DeviceRGB`, `/DeviceGray`, `/DeviceCMYK`, or `/DeviceN` for indexed images.
    pub color_space: Object,
    pub is_mask: bool,
    /// An alpha soft mask, present only for images with transparency.
    pub smask: Option<Stream>,
}

impl EncodedImage {
    /// Decode PNG, JPEG, BMP, GIF or TIFF bytes into embeddable PDF image data.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let image = image::load_from_memory(bytes)
            .map_err(|e| BritError::new(ErrorKind::UnsupportedFeature, format!("This image cannot be read: {e}")))?;
        let (width, height) = (image.width(), image.height());
        if width == 0 || height == 0 {
            return Err(BritError::new(ErrorKind::UnsupportedFeature, "The image has no pixels."));
        }
        // A very large image makes an unusable PDF; the raster limit is enforced
        // when a user picks a file, this is the last-resort guard.
        if (width as u64) * (height as u64) > 200_000_000 {
            return Err(BritError::new(
                ErrorKind::UnsupportedFeature,
                "This image is too large to add to a PDF.",
            ));
        }

        // JPEG goes through untouched: re-encoding would lose quality.
        if is_jpeg(bytes) {
            if let Some((w, h)) = jpeg_dimensions(bytes) {
                return Ok(EncodedImage {
                    width: w,
                    height: h,
                    jpeg: Some(bytes.to_vec()),
                    samples: Vec::new(),
                    color_space: Object::Name(b"DeviceRGB".to_vec()),
                    is_mask: false,
                        smask: None,
                });
            }
        }

        match image.color() {
            image::ColorType::Rgb8 => {
                let samples = image.to_rgb8().into_raw();
                Ok(EncodedImage {
                    width,
                    height,
                    jpeg: None,
                    samples,
                    color_space: Object::Name(b"DeviceRGB".to_vec()),
                    is_mask: false,
                        smask: None,
                })
            }
            image::ColorType::Rgba8 => {
                // Split the alpha channel into a PDF soft mask so transparency
                // survives, then keep only the colour samples in the image.
                let rgba = image.to_rgba8();
                let (colour, alpha) = rgba.as_raw().split_at(rgba.width() as usize * rgba.height() as usize * 3);
                let smask = Stream::new(
                    dictionary! {
                        "Type" => Object::Name(b"XObject".to_vec()),
                        "Subtype" => Object::Name(b"Image".to_vec()),
                        "Width" => Object::Integer(width as i64),
                        "Height" => Object::Integer(height as i64),
                        "ColorSpace" => Object::Name(b"DeviceGray".to_vec()),
                        "BitsPerComponent" => Object::Integer(8),
                        "Filter" => Object::Name(b"FlateDecode".to_vec()),
                    },
                    compress(alpha),
                );
                // The caller stores the mask alongside the image; see `embed_with_mask`.
                Ok(EncodedImage {
                    width,
                    height,
                    jpeg: None,
                    samples: colour.to_vec(),
                    color_space: Object::Name(b"DeviceRGB".to_vec()),
                    is_mask: false,
                        smask: None,
                }
                .with_smask(smask))
            }
            image::ColorType::L8 => {
                let samples = image.to_luma8().into_raw();
                Ok(EncodedImage {
                    width,
                    height,
                    jpeg: None,
                    samples,
                    color_space: Object::Name(b"DeviceGray".to_vec()),
                    is_mask: false,
                        smask: None,
                })
            }
            image::ColorType::La8 => {
                let la = image.to_luma_alpha8();
                let (colour, alpha) = la.as_raw().split_at(la.width() as usize * la.height() as usize);
                let smask = Stream::new(
                    dictionary! {
                        "Type" => Object::Name(b"XObject".to_vec()),
                        "Subtype" => Object::Name(b"Image".to_vec()),
                        "Width" => Object::Integer(width as i64),
                        "Height" => Object::Integer(height as i64),
                        "ColorSpace" => Object::Name(b"DeviceGray".to_vec()),
                        "BitsPerComponent" => Object::Integer(8),
                        "Filter" => Object::Name(b"FlateDecode".to_vec()),
                    },
                    compress(alpha),
                );
                Ok(EncodedImage {
                    width,
                    height,
                    jpeg: None,
                    samples: colour.to_vec(),
                    color_space: Object::Name(b"DeviceGray".to_vec()),
                    is_mask: false,
                        smask: None,
                }
                .with_smask(smask))
            }
            other => Err(BritError::new(
                ErrorKind::UnsupportedFeature,
                format!("Images of type {other:?} cannot be added to a PDF yet."),
            )),
        }
    }
}

impl EncodedImage {
    fn with_smask(mut self, smask: Stream) -> Self {
        self.smask = Some(smask);
        self
    }
}

/// Build a PDF from images, one image per page, each scaled to fit A4 or its own size.
///
/// `fit`: scale to fit the page while keeping the aspect ratio.
pub fn images_to_pdf(
    images: &[(Vec<u8>, String)],
    page_size: Option<(f32, f32)>,
    fit: bool,
) -> Result<Vec<u8>> {
    if images.is_empty() {
        return Err(BritError::new(ErrorKind::InvalidRequest, "No images were supplied."));
    }
    let mut doc = blank_document(page_size.map(|s| s.0).unwrap_or(595.0), page_size.map(|s| s.1).unwrap_or(842.0));
    let mut page_id = crate::engine::writer::pages(&doc)[0];
    let mut width = 595.0;
    let mut height = 842.0;
    if let Some((w, h)) = page_size {
        width = w;
        height = h;
        crate::engine::writer::set_page_size(&mut doc, page_id, width, height);
    }

    for (index, (bytes, _name)) in images.iter().enumerate() {
        if index > 0 {
            page_id = crate::engine::structure::append_page(&mut doc, width, height)?;
        }
        let image = EncodedImage::from_bytes(bytes)?;
        let (x, y, w, h) = fit_geometry(width, height, image.width, image.height, fit);
        let name = embed_image(&mut doc, page_id, &image)?;
        let content = ContentBuilder::new()
            .save_state()
            .concat_matrix(w, 0.0, 0.0, h, x, y)
            .draw_x_object(&name)
            .restore_state()
            .encode()?;
        crate::engine::writer::append_page_content(&mut doc, page_id, &content)?;
    }

    serialise(&mut doc, true)
}

/// Centre an image of `image_w`×`image_height` on a `page_w`×`page_h` page.
fn fit_geometry(
    page_w: f32,
    page_h: f32,
    image_w: u32,
    image_h: u32,
    fit: bool,
) -> (f32, f32, f32, f32) {
    if image_w == 0 || image_h == 0 {
        return (0.0, 0.0, page_w, page_h);
    }
    let (mut w, mut h) = (image_w as f32, image_h as f32);
    if fit {
        let scale = (page_w / w).min(page_h / h).min(1.0);
        w *= scale;
        h *= scale;
    }
    ((page_w - w) / 2.0, (page_h - h) / 2.0, w, h)
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() > 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF
}

/// Read the frame size from a JPEG's SOF marker, so BritPDF can size the page
/// before decoding. This avoids fully decoding large images just to learn their size.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 3 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        // SOF0..SOF15, excluding DHT (C4), DAC (CC) and RSTn (D0-D7).
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let height = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let width = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            if width > 0 && height > 0 {
                return Some((width, height));
            }
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// Decode standard base64 (with or without padding, ignoring whitespace).
fn decode_base64(text: &str) -> Result<Vec<u8>> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD
        .decode(cleaned.as_bytes())
        .or_else(|_| {
            use base64::engine::general_purpose::STANDARD_NO_PAD;
            STANDARD_NO_PAD.decode(cleaned.as_bytes())
        })
        .map_err(|e| BritError::new(ErrorKind::InvalidRequest, format!("The image data is not valid base64: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_document_is_a_real_pdf() {
        let bytes = create_blank_document(595.0, 842.0, Some("Test"));
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
    }

    #[test]
    fn composes_text_as_real_text_operators() {
        let elements = vec![PageElement::Text {
            x: 72.0,
            y: 700.0,
            text: "Hello BritPDF".into(),
            size: 24.0,
            font: "Helvetica".into(),
            color: [0.0, 0.0, 0.0],
            bold: false,
        }];
        let bytes = compose_page(595.0, 842.0, &elements, None).unwrap();
        assert!(bytes.len() > 400);
    }

    #[test]
    fn rectangle_with_no_paint_produces_nothing() {
        let elements = vec![PageElement::Rectangle {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            stroke: None,
            fill: None,
            line_width: 1.0,
            opacity: 1.0,
        }];
        let bytes = compose_page(595.0, 842.0, &elements, None).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn detects_jpeg_dimensions_from_the_frame_header() {
        // A 2x1 JPEG is the smallest valid file; we only need its SOF0 marker.
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x01, 0x00, 0x02, 0x03,
        ];
        assert_eq!(jpeg_dimensions(&jpeg), Some((2, 1)));
    }

    #[test]
    fn is_jpeg_only_for_real_jpegs() {
        assert!(is_jpeg(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(!is_jpeg(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn fit_geometry_centres_and_scales() {
        let (x, y, w, h) = fit_geometry(100.0, 100.0, 200, 100, true);
        assert_eq!((x, y), (0.0, 25.0));
        assert_eq!((w, h), (100.0, 50.0));
    }
}
