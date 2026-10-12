//! Text layer extraction.
//!
//! A text PDF has real, selectable characters with positions. A scanned PDF is a
//! single image with no characters at all. BritPDF preserves that difference: text
//! pages get a real text layer (selectable, searchable, copyable) and image-only
//! pages are reported as such, which is what drives the "this looks like a scan"
//! notice and the OCR offer.
//!
//! All rectangles produced here are in **PDF points with the origin at the
//! top-left** of the page, which is the coordinate system the viewer canvas uses.

use crate::engine::session::OpenDocument;
use crate::engine::types::{PageContentKind, Rect, TextChar};
use crate::error::{BritError, ErrorKind, Result};
use pdfium_render::prelude::{PdfPageIndex, PdfRect};

#[derive(Clone, serde::Serialize)]
pub struct ExtractedPageText {
    pub text: String,
    pub chars: Vec<TextChar>,
    pub content_kind: PageContentKind,
    pub is_scanned_candidate: bool,
}

/// Characters that carry no ink and no separation: zero-width joiners, marks,
/// bidi controls, BOMs. They must not survive into copied text.
const INVISIBLE: [char; 7] = ['\u{200b}', '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{2060}', '\u{feff}'];

pub fn extract_page_text(doc: &mut OpenDocument, page: usize) -> Result<ExtractedPageText> {
    let geometry = doc.page_geometry(page)?;
    let pdf_page = doc
        .document
        .pages()
        .get(page as PdfPageIndex)
        .map_err(|_| BritError::new(ErrorKind::InvalidPage, format!("Page {} does not exist", page + 1)))?;

    let page_text = match pdf_page.text() {
        Ok(t) => t,
        // A page whose content stream cannot be parsed still renders; it just has
        // no usable text layer. Report it as image-only rather than failing.
        Err(_) => {
            return Ok(ExtractedPageText {
                text: String::new(),
                chars: Vec::new(),
                content_kind: PageContentKind::ImageOnly,
                is_scanned_candidate: true,
            })
        }
    };

    if page_text.is_empty() {
        return Ok(ExtractedPageText {
            text: String::new(),
            chars: Vec::new(),
            content_kind: PageContentKind::ImageOnly,
            is_scanned_candidate: true,
        });
    }

    // PDFium's text page has a single canonical character order - the order
    // `FPDFText_GetText`/"Select all + copy" uses. We preserve it exactly: the
    // sidebar text and anything the user copies are then guaranteed to agree.
    // We deliberately do *not* re-sort by geometry: per-character metrics on
    // subset or embedded fonts are not reliable enough to use as an ordering
    // key (a mis-sized font splits one visual line into many bands and scrambles
    // the output). Geometry is used below only to reconstruct spacing.
    //
    // Whitespace characters are kept, not dropped: if the PDF itself carries an
    // explicit space between words, that space is the ground truth and must win
    // over any geometric guess. Geometric pitch is never used to *invent* word
    // separation (see `reconstruct_text`); only line/paragraph breaks come from
    // geometry, and only from the baseline, which is unambiguous.
    let mut chars: Vec<TextChar> = Vec::new();
    let mut meaningful = 0usize;
    for ch in page_text.chars().iter() {
        let Some(s) = ch.unicode_string() else { continue };
        // Drop characters that carry no ink and no meaning.
        if s.chars().all(|c| c.is_control() || INVISIBLE.contains(&c)) {
            continue;
        }
        // A whitespace character becomes a single ordinary space, whatever its
        // codepoint (no-break space included) - fine for a copyable text layer.
        let c: String = if s.chars().all(|c| c.is_whitespace()) {
            " ".to_string()
        } else {
            s
        };
        let Ok(bounds) = ch.loose_bounds() else { continue };
        let font_size = ch.scaled_font_size().value.max(0.1);
        if !c.chars().all(|c| c.is_whitespace()) {
            meaningful += 1;
        }
        chars.push(TextChar {
            c,
            rect: rect_of(bounds),
            font_size,
            font_is_serif: ch.font_is_serif(),
            font_is_italic: ch.font_is_italic(),
            font_is_bold: ch.font_is_bold_reenforced(),
        });
    }

    let text = reconstruct_text(&chars);
    // A page with a handful of characters or fewer is, for practical purposes, a
    // scanned page: OCR menus/digits aside, real text pages have hundreds.
    let (content_kind, is_scanned_candidate) = if meaningful == 0 {
        (PageContentKind::ImageOnly, true)
    } else if meaningful < 24 && geometry.width > 0.0 {
        (PageContentKind::MostlyImage, true)
    } else {
        (PageContentKind::Text, false)
    };

    Ok(ExtractedPageText { text, chars, content_kind, is_scanned_candidate })
}

/// Convert a PDFium `PdfRect` (top-left origin) into our serialisable rect.
pub fn rect_of(r: PdfRect) -> Rect {
    Rect {
        left: r.left().value,
        top: r.top().value,
        right: r.right().value,
        bottom: r.bottom().value,
    }
}

/// Rebuild readable text from positioned characters.
///
/// `chars` follow PDFium's text-page order (see `extract_page_text`), and may
/// include real space characters preserved from the PDF.
///
/// Nothing is invented. The output is the PDF's own text page in PDFium's
/// canonical character order - the order "select all + copy" produces in any
/// viewer - with the PDF's own space characters kept, and a line or paragraph
/// break inserted wherever the characters' baselines say a new line begins.
/// Geometric pitch is *never* used to guess word boundaries: doing so is what
/// produced phantom splits like "sim ulation" (a wide `m` reading as a word
/// gap in a PDF that already spaces its words itself). If the PDF's text layer
/// carries no spaces, the words sit adjacent, exactly as they do when copied
/// from any other reader - and that is honest, not corrupted.
///
/// Line breaks come from the baseline heuristic: a jump in `top` of at least
/// 60% of the taller neighbour's font size starts a new line, and one of more
/// than 2.6x the line height ends a paragraph. This is the one geometric rule
/// kept, because reading order is a *layout* property the text stream alone
/// does not convey, and the baseline signal is unambiguous.
fn is_space_char(t: &TextChar) -> bool {
    t.c.chars().all(|c| c.is_whitespace())
}

/// Indices at which a new line begins, using the baseline heuristic: a line
/// break is any jump in baseline (`top`) at least 60% of the taller neighbour's
/// font size. The first position (0) always starts a line.
fn line_starts_of(chars: &[TextChar]) -> Vec<usize> {
    let mut line_starts: Vec<usize> = vec![0];
    for (i, w) in chars.windows(2).enumerate() {
        let (prev, ch) = (&w[0], &w[1]);
        let line_gap = ch.rect.top - prev.rect.top;
        let line_height = (ch.font_size.max(prev.font_size)) * 0.6;
        if line_gap.abs() >= line_height {
            line_starts.push(i + 1);
        }
    }
    line_starts
}

fn reconstruct_text(chars: &[TextChar]) -> String {
    if chars.is_empty() {
        return String::new();
    }

    // Line and paragraph breaks only - see the module comment: no word spacing
    // is inferred from geometry, ever.
    let n = chars.len();
    enum Dec {
        Continue,
        Break { blank: bool },
    }
    let mut decisions: Vec<Dec> = (0..n).map(|_| Dec::Continue).collect();

    for &start in line_starts_of(chars).iter().skip(1) {
        let prev = &chars[start - 1];
        let ch = &chars[start];
        let line_height = (ch.font_size.max(prev.font_size)) * 0.6;
        let blank = (ch.rect.top - prev.rect.top).abs() > line_height * 2.6;
        decisions[start] = Dec::Break { blank };
    }

    let mut out = String::with_capacity(chars.len() + chars.len() / 8);
    for (i, ch) in chars.iter().enumerate() {
        if let Dec::Break { blank } = decisions[i] {
            out.push_str(if blank { "\n\n" } else { "\n" });
        }
        if is_space_char(ch) {
            out.push(' ');
        } else {
            out.push_str(&ch.c);
        }
    }
    out
}

/// Extract plain text for the whole document, using the page text cache.
pub fn document_text(doc: &mut OpenDocument) -> Result<(String, usize)> {
    let pages = doc.page_count();
    let mut out = String::new();
    let mut total = 0usize;
    for page in 0..pages {
        let extracted = doc.text(page)?;
        out.push_str(&format!("\n----- Page {} -----\n", page + 1));
        out.push_str(&extracted.text);
        out.push('\n');
        total += extracted.chars.len();
    }
    Ok((out, total))
}

/// Headless diagnostics for text-layer extraction, used by the `probe` helper
/// binary. Opens a PDF straight through PDFium (no Tauri, no UI) and prints, for
/// each requested page (1-based; every page by default), the raw per-character
/// metrics, the per-line gap thresholds, and the reconstructed text - exactly the
/// inputs and decision path `extract_page_text` uses. `pages` are 1-based.
pub fn probe_text_extraction(path: &str, pages: Option<Vec<usize>>) {
    let pdfium = match crate::engine::pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[probe] PDFium unavailable: {} {}", e.message, e.hint.unwrap_or_default());
            return;
        }
    };
    let doc = match pdfium.load_pdf_from_file(std::path::Path::new(path), None) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[probe] open failed for {path}: {e:?}");
            return;
        }
    };
    let total = doc.pages().len() as usize;
    let want: Vec<usize> = match pages {
        Some(ps) => ps,
        None => (1..=total).collect(),
    };

    for &page_no in &want {
        if page_no > total || page_no == 0 {
            eprintln!("[probe] page {page_no} out of range (1..={total})");
            continue;
        }
        let page = page_no - 1;
        let Ok(pdf_page) = doc.pages().get(page as PdfPageIndex) else { continue };
        let page_text = match pdf_page.text() {
            Ok(t) => t,
            Err(_) => {
                eprintln!("===== PAGE {page_no} ===== NO TEXT PAGE");
                continue;
            }
        };

        // Same pipeline as `extract_page_text`, but keeping per-char provenance.
        let mut chars: Vec<TextChar> = Vec::new();
        let mut from_pdf_space: Vec<bool> = Vec::new();
        for ch in page_text.chars().iter() {
            let Some(s) = ch.unicode_string() else { continue };
            if s.chars().all(|c| c.is_control() || INVISIBLE.contains(&c)) {
                continue;
            }
            let c: String = if s.chars().all(|c| c.is_whitespace()) {
                " ".to_string()
            } else {
                s
            };
            let Ok(bounds) = ch.loose_bounds() else { continue };
            let font_size = ch.scaled_font_size().value.max(0.1);
            from_pdf_space.push(c.chars().all(|c| c.is_whitespace()));
            chars.push(TextChar {
                c,
                rect: rect_of(bounds),
                font_size,
                font_is_serif: false,
                font_is_italic: false,
                font_is_bold: false,
            });
        }

        eprintln!(
            "===== PAGE {page_no} ===== {} chars ({} explicit PDF spaces)",
            chars.len(),
            from_pdf_space.iter().filter(|b| **b).count()
        );
        if chars.is_empty() {
            eprintln!("      (no text layer)");
            continue;
        }

        // Recompute line breaks exactly as `reconstruct_text` will.
        let n = chars.len();
        let line_starts = line_starts_of(&chars);
        let mut line_break: Vec<Option<bool>> = (0..n).map(|_| None).collect();
        for &start in line_starts.iter().skip(1) {
            let prev = &chars[start - 1];
            let ch = &chars[start];
            let line_height = (ch.font_size.max(prev.font_size)) * 0.6;
            let blank = (ch.rect.top - prev.rect.top).abs() > line_height * 2.6;
            line_break[start] = Some(blank);
        }

        // Print a per-character table.
        for i in 0..n {
            let gap = if i + 1 < n { chars[i + 1].rect.left - chars[i].rect.left } else { 0.0 };
            let ch = &chars[i];
            let sp = if is_space_char(ch) { " SP" } else { "" };
            let brk = match line_break[i] {
                Some(blank) => if blank { "[BRK \\n\\n]" } else { "[BRK \\n]" },
                None => "",
            };
            let disp: String = ch
                .c
                .chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .collect();
            eprintln!(
                "  [{i:4}] '{disp}'{sp} left={:8.2} top={:8.2} fs={:6.2} gap->next={gap:8.2} {brk}",
                ch.rect.left,
                ch.rect.top,
                ch.font_size
            );
        }

        let out = reconstruct_text(&chars);
        eprintln!("----- reconstructed ({} chars -> {} bytes) -----", chars.len(), out.len());
        eprintln!("{out}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: &str, left: f32, top: f32, size: f32) -> TextChar {
        TextChar {
            c: c.to_string(),
            rect: Rect::new(left, top, left + size * 0.5, top + size),
            font_size: size,
            font_is_serif: false,
            font_is_italic: false,
            font_is_bold: false,
        }
    }

    #[test]
    fn never_invents_word_spacing_without_explicit_spaces() {
        // A(10)->B(25) is a letter pitch (advance 15); B->C(50) and C->D are
        // wider, but the PDF carries no spaces here. Nothing is invented: the
        // words sit adjacent, exactly as "select all + copy" echoes them.
        let chars = vec![
            ch("A", 10.0, 100.0, 10.0),
            ch("B", 25.0, 100.0, 10.0),
            ch("C", 50.0, 100.0, 10.0),
            ch("D", 65.0, 100.0, 10.0),
        ];
        assert_eq!(reconstruct_text(&chars), "ABCD");
    }

    #[test]
    fn font_sizes_do_not_affect_word_spacing() {
        let chars = vec![
            ch("A", 10.0, 100.0, 40.0),
            ch("B", 25.0, 100.0, 40.0),
            ch("C", 50.0, 100.0, 40.0),
            ch("D", 65.0, 100.0, 40.0),
        ];
        assert_eq!(reconstruct_text(&chars), "ABCD");
    }

    #[test]
    fn preserves_explicit_space_characters() {
        // The PDF itself carries real spaces; they win over geometry.
        let chars = vec![
            ch("A", 10.0, 100.0, 10.0),
            ch(" ", 20.0, 100.0, 10.0),
            ch("B", 27.0, 100.0, 10.0),
            ch("C", 33.0, 100.0, 10.0),
        ];
        assert_eq!(reconstruct_text(&chars), "A BC");
    }

    #[test]
    fn does_not_add_geometry_spaces_next_to_explicit_spaces() {
        // Even a wide pitch after an explicit space must not double-space.
        let chars = vec![
            ch("A", 10.0, 100.0, 10.0),
            ch(" ", 20.0, 100.0, 10.0),
            ch("B", 80.0, 100.0, 10.0),
            ch("C", 90.0, 100.0, 10.0),
        ];
        assert_eq!(reconstruct_text(&chars), "A BC");
    }

    #[test]
    fn starts_a_new_line_when_the_baseline_moves() {
        let chars = vec![
            ch("A", 10.0, 100.0, 10.0),
            ch("B", 10.0, 115.0, 10.0),
        ];
        assert_eq!(reconstruct_text(&chars), "A\nB");
    }

    #[test]
    fn keeps_a_paragraph_gap() {
        let chars = vec![
            ch("A", 10.0, 100.0, 10.0),
            ch("B", 10.0, 200.0, 10.0),
        ];
        assert_eq!(reconstruct_text(&chars), "A\n\nB");
    }

    #[test]
    fn letterspaced_lines_keep_the_pdfs_own_glue() {
        // A PDF that places every glyph individually with no spaces: the glyph
        // run is emitted verbatim ("Intro") followed by the explicit space and
        // the next glyph run. Honest glue - same as copying from the reader.
        let size = 10.0;
        let letterspaced = vec![
            ch("I", 0.0, 100.0, size),
            ch("n", 6.0, 100.0, size),
            ch("t", 13.0, 100.0, size),
            ch("r", 20.0, 100.0, size),
            ch("o", 26.0, 100.0, size),
            ch("w", 38.0, 100.0, size),
            ch("o", 44.0, 100.0, size),
            ch("r", 51.0, 100.0, size),
            ch("d", 57.0, 100.0, size),
        ];
        assert_eq!(reconstruct_text(&letterspaced), "Introword");
    }

    #[test]
    fn lines_with_explicit_spaces_are_spaced_by_the_pdf_only() {
        // Mirrors a real body line from a generated PDF: every word boundary in
        // the text layer is an explicit space, and the `m` advance (8.3 - a wide
        // glyph) is much larger than a normal letter pitch. The PDF's own spaces
        // are what separate the words - "humans" must not becomes "hum ans".
        let chars = vec![
            ch("h", 78.0, 100.0, 10.0),
            ch("u", 83.6, 100.0, 10.0),
            ch("m", 89.1, 100.0, 10.0),
            ch("a", 97.4, 100.0, 10.0),
            ch("n", 102.9, 100.0, 10.0),
            ch("s", 108.0, 100.0, 10.0),
            ch(" ", 113.0, 100.0, 10.0),
            ch("e", 117.0, 100.0, 10.0),
            ch("r", 120.4, 100.0, 10.0),
            ch("r", 123.7, 100.0, 10.0),
            ch("o", 126.5, 100.0, 10.0),
            ch("r", 132.0, 100.0, 10.0),
            ch("s", 139.3, 100.0, 10.0),
        ];
        assert_eq!(reconstruct_text(&chars), "humans errors");
    }
}
