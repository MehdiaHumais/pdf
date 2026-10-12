//! Full-document text search built on PDFium's text-page search.
//!
//! Searching the real text layer (rather than a flattened image) is what makes
//! search work on text PDFs and correctly return nothing on scans unless OCR has
//! added a text layer.

use crate::engine::session::OpenDocument;
use crate::engine::types::{Rect, SearchMatch, SearchResults};
use crate::error::{BritError, ErrorKind, Result};
use pdfium_render::prelude::{
    PdfPageIndex, PdfSearchDirection, PdfSearchOptions,
};
use std::time::Instant;

/// Stop after this many matches so a pathological document cannot lock the UI.
const MAX_MATCHES: usize = 20_000;
/// Stop after this much time for the same reason.
const MAX_MILLIS: u64 = 8_000;

pub struct SearchRequest<'a> {
    pub query: &'a str,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// 0-based inclusive start page, or `None` to search the whole document.
    pub from_page: Option<usize>,
}

pub fn search_document(doc: &mut OpenDocument, req: SearchRequest<'_>) -> Result<SearchResults> {
    let started = Instant::now();
    let query = req.query.trim();
    if query.is_empty() {
        return Err(BritError::new(ErrorKind::InvalidRequest, "Enter something to search for."));
    }

    let page_count = doc.page_count();
    let first = req.from_page.unwrap_or(0).min(page_count.saturating_sub(1));
    let mut matches: Vec<SearchMatch> = Vec::new();
    let mut pages_searched = 0usize;
    let mut truncated = false;

    'outer: for page in first..page_count {
        if started.elapsed().as_millis() as u64 > MAX_MILLIS {
            truncated = true;
            break;
        }
        pages_searched += 1;
        let pdf_page = match doc.document.pages().get(page as PdfPageIndex) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let page_text = match pdf_page.text() {
            Ok(t) if !t.is_empty() => t,
            _ => continue,
        };
        let options = PdfSearchOptions::new()
            .match_case(req.case_sensitive)
            .match_whole_word(req.whole_word);
        let search = match page_text.search(query, &options) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for segments in search.iter(PdfSearchDirection::SearchForward) {
            if segments.is_empty() {
                continue;
            }
            let mut rects: Vec<Rect> = Vec::new();
            let mut matched = String::new();
            let mut first_index = usize::MAX;
            let mut last_index = 0usize;

            for segment in segments.iter() {
                let bounds = segment.bounds();
                rects.push(crate::engine::text::rect_of(bounds));
                matched.push_str(&segment.text());
                if let Ok(chars) = segment.chars() {
                if let Some(start) = chars.first_char_index() {
                    let s = start as usize;
                    first_index = first_index.min(s);
                }
                if let Some(end) = chars.last_char_index() {
                    last_index = last_index.max(end as usize);
                }
                }
            }

            // Merge adjacent rects that sit on the same text line so a match
            // spanning a word break is one continuous highlight.
            let rects = merge_same_line_rects(rects);
            matches.push(SearchMatch {
                page,
                text: matched,
                rects,
                start_index: if first_index == usize::MAX { 0 } else { first_index },
                end_index: last_index,
            });

            if matches.len() >= MAX_MATCHES {
                truncated = true;
                break 'outer;
            }
        }
    }

    Ok(SearchResults {
        query: query.to_string(),
        case_sensitive: req.case_sensitive,
        whole_word: req.whole_word,
        matches,
        pages_searched,
        truncated,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Combine rectangles whose vertical extents overlap into one run.
fn merge_same_line_rects(mut rects: Vec<Rect>) -> Vec<Rect> {
    if rects.len() < 2 {
        return rects;
    }
    rects.sort_by(|a, b| {
        a.top.partial_cmp(&b.top).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut merged: Vec<Rect> = Vec::with_capacity(rects.len());
    for rect in rects {
        match merged.last_mut() {
            Some(last) if rect.top < last.bottom + 1.5 && rect.bottom > last.top - 1.5 => {
                last.left = last.left.min(rect.left);
                last.right = last.right.max(rect.right);
                last.top = last.top.min(rect.top);
                last.bottom = last.bottom.max(rect.bottom);
            }
            _ => merged.push(rect),
        }
    }
    merged
}

/// Expand a human page-range expression ("1-3, 7, 10-12") into 0-based page indices.
/// Returns a precise error for malformed input so the UI can point at the problem.
pub fn parse_page_ranges(spec: &str, page_count: usize) -> Result<Vec<usize>> {
    let mut out: Vec<usize> = Vec::new();
    let cleaned = spec.replace(';', ",");
    for part in cleaned.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let start: usize = a.trim().parse().map_err(|_| {
                BritError::new(ErrorKind::InvalidRequest, format!("'{a}' is not a page number."))
            })?;
            let end: usize = b.trim().parse().map_err(|_| {
                BritError::new(ErrorKind::InvalidRequest, format!("'{b}' is not a page number."))
            })?;
            if start == 0 || end == 0 {
                return Err(BritError::new(
                    ErrorKind::InvalidRequest,
                    "Page numbers start at 1.",
                ));
            }
            if start > end {
                return Err(BritError::new(
                    ErrorKind::InvalidRequest,
                    format!("'{part}' is backwards: the range ends before it starts."),
                ));
            }
            if end > page_count {
                return Err(BritError::new(
                    ErrorKind::InvalidRequest,
                    format!("This document has {page_count} pages, so page {end} does not exist."),
                ));
            }
            for p in start..=end {
                out.push(p - 1);
            }
        } else {
            let p: usize = part.parse().map_err(|_| {
                BritError::new(ErrorKind::InvalidRequest, format!("'{part}' is not a page number."))
            })?;
            if p == 0 {
                return Err(BritError::new(
                    ErrorKind::InvalidRequest,
                    "Page numbers start at 1.",
                ));
            }
            if p > page_count {
                return Err(BritError::new(
                    ErrorKind::InvalidRequest,
                    format!("This document has {page_count} pages, so page {p} does not exist."),
                ));
            }
            out.push(p - 1);
        }
    }
    if out.is_empty() {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "Enter at least one page or range, for example 1-3.",
        ));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_pages_and_ranges() {
        assert_eq!(parse_page_ranges("1-3, 7, 10-12", 20).unwrap(), vec![0, 1, 2, 6, 9, 10, 11]);
    }

    #[test]
    fn rejects_zero() {
        assert!(parse_page_ranges("0-2", 10).is_err());
    }

    #[test]
    fn rejects_backwards_range() {
        assert!(parse_page_ranges("5-2", 10).is_err());
    }

    #[test]
    fn rejects_page_beyond_document() {
        assert!(parse_page_ranges("1-99", 10).is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_page_ranges("abc", 10).is_err());
        assert!(parse_page_ranges("   ", 10).is_err());
    }

    #[test]
    fn merges_rects_on_one_line() {
        let merged = merge_same_line_rects(vec![
            Rect::new(10.0, 100.0, 20.0, 110.0),
            Rect::new(21.0, 101.0, 30.0, 111.0),
            Rect::new(10.0, 140.0, 20.0, 150.0),
        ]);
        assert_eq!(merged.len(), 2);
        assert!((merged[0].left - 10.0).abs() < 0.001);
        assert!((merged[0].right - 30.0).abs() < 0.001);
    }
}
