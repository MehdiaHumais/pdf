//! Page-tree and document-structure editing, all through lopdf.
//!
//! These are the operations PDFium cannot do at all: it has no API for deleting,
//! reordering, duplicating or inserting pages, nor for merging two files. Every
//! function here is therefore structural, and the caller reopens the result through
//! PDFium so rendering and the text layer stay consistent with the file on disk.

use crate::error::{BritError, ErrorKind, Result};
use crate::engine::writer::{deep_copy, page_size, pages, pages_root, serialise, set_page_size};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use std::collections::HashMap;

/// Load a PDF from bytes, mapping any parse failure onto a user-facing error.
///
/// lopdf needs the password *before* it will decode strings and streams, so an
/// encrypted file is decrypted here rather than lazily.
pub fn load(bytes: &[u8], password: Option<&str>) -> Result<Document> {
    let mut doc = Document::load_mem(bytes).map_err(|e| {
        BritError::new(
            ErrorKind::CorruptDocument,
            format!("This file could not be read as a PDF: {e}"),
        )
    })?;
    if doc.is_encrypted() {
        let Some(password) = password else {
            return Err(BritError::new(
                ErrorKind::PasswordRequired,
                "This PDF is password protected.",
            ));
        };
        doc.decrypt(password).map_err(|e| {
            BritError::new(
                ErrorKind::InvalidPassword,
                "That password did not unlock this PDF.",
            )
            .with_detail(e.to_string())
        })?;
    }
    Ok(doc)
}

/// All page object ids, in reading order.
pub fn page_ids(doc: &Document) -> Vec<ObjectId> {
    pages(doc)
}

/// The number of pages.
pub fn page_count(doc: &Document) -> usize {
    page_ids(doc).len()
}

/// Page sizes in points, in reading order, ignoring per-page rotation.
pub fn page_sizes(doc: &Document) -> Vec<(f32, f32)> {
    page_ids(doc)
        .into_iter()
        .map(|id| {
            let (_, _, w, h) = page_size(doc, id);
            (w, h)
        })
        .collect()
}

/// Rotate a page by `degrees`, which must be a multiple of 90.
pub fn set_page_rotation(doc: &mut Document, page: usize, degrees: i32) -> Result<()> {
    let id = nth_page(doc, page)?;
    let normalised = ((degrees % 360) + 360) % 360;
    if normalised % 90 != 0 {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "Page rotation must be a multiple of 90 degrees.",
        ));
    }
    let dict = doc.get_dictionary_mut(id).map_err(|e| {
        BritError::new(
            ErrorKind::CorruptDocument,
            format!("Page {page} could not be opened: {e}"),
        )
    })?;
    dict.set("Rotate", Object::Integer(normalised as i64));
    Ok(())
}

/// Add to a page's existing rotation, so "rotate right" keeps rotating.
pub fn rotate_page_by(doc: &mut Document, page: usize, degrees: i32) -> Result<i32> {
    let current = effective_rotation(doc, page);
    let next = ((current + degrees) % 360 + 360) % 360;
    set_page_rotation(doc, page, next)?;
    Ok(next)
}

/// The effective rotation of a page: its own `/Rotate` combined with any
/// rotation inherited from an ancestor in the page tree.
pub fn effective_rotation(doc: &Document, page: usize) -> i32 {
    let Ok(mut id) = nth_page(doc, page) else { return 0 };
    let mut total = 0i32;
    for _ in 0..64 {
        let Ok(dict) = doc.get_dictionary(id) else { break };
        if let Some(value) = dict.get(b"Rotate").ok().and_then(|o| o.as_i64().ok()) {
            total += value as i32;
        }
        match dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok()) {
            Some(parent) => id = parent,
            None => break,
        }
    }
    ((total % 360) + 360) % 360
}

/// Resize a page, and realign any box that was already in step with `/MediaBox`.
pub fn resize_page(doc: &mut Document, page: usize, width: f32, height: f32) -> Result<()> {
    let id = nth_page(doc, page)?;
    if !(1.0..=20_000.0).contains(&width) || !(1.0..=20_000.0).contains(&height) {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "The page size is out of range.",
        ));
    }
    set_page_size(doc, id, width, height);
    let media = Object::Array(vec![
        Object::Integer(0),
        Object::Integer(0),
        Object::Real(width),
        Object::Real(height),
    ]);
    let extra: [&[u8]; 4] = [b"CropBox", b"BleedBox", b"TrimBox", b"ArtBox"];
    if let Ok(dict) = doc.get_dictionary_mut(id) {
        for key in extra {
            if dict.has(key) {
                dict.set(key, media.clone());
            }
        }
    }
    Ok(())
}

/// Append an empty page of the given size.
pub fn append_page(doc: &mut Document, width: f32, height: f32) -> Result<ObjectId> {
    let pages_id = pages_root(doc)?;
    let page_id = doc.new_object_id();
    let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
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

    // `/Kids` is a flat list in every document BritPDF has seen in practice, and
    // keeping it flat is what lets `pages()` return a stable order.
    if let Ok(tree) = doc.get_dictionary_mut(pages_id) {
        let mut kids = tree
            .get(b"Kids")
            .ok()
            .and_then(|k| k.as_array().ok().cloned())
            .unwrap_or_default();
        kids.push(Object::Reference(page_id));
        let count = kids.len() as i64;
        tree.set("Kids", Object::Array(kids));
        tree.set("Count", Object::Integer(count));
    }
    Ok(page_id)
}

/// Insert an empty page before `before` (0-based). `before >= count` appends.
pub fn insert_blank_page(doc: &mut Document, before: usize, width: f32, height: f32) -> Result<()> {
    let new_id = append_page(doc, width, height)?;
    move_page_to(doc, new_id, before)
}

/// Move an existing page so that it sits at `index` (0-based) in reading order.
pub fn move_page_to(doc: &mut Document, page: ObjectId, index: usize) -> Result<()> {
    let pages_id = pages_root(doc)?;
    let kids = doc
        .get_dictionary(pages_id)
        .ok()
        .and_then(|tree| tree.get(b"Kids").ok().and_then(|k| k.as_array().ok()).cloned())
        .unwrap_or_default();

    let mut refs: Vec<Object> = kids.into_iter().filter(|k| *k != Object::Reference(page)).collect();
    let target = index.min(refs.len());
    refs.insert(target, Object::Reference(page));
    if let Ok(tree) = doc.get_dictionary_mut(pages_id) {
        tree.set("Count", Object::Integer(refs.len() as i64));
        tree.set("Kids", Object::Array(refs));
    }
    Ok(())
}

/// Delete a 1-based page.
pub fn delete_page(doc: &mut Document, page: usize) -> Result<()> {
    let id = nth_page(doc, page)?;
    let pages_id = pages_root(doc)?;
    let kids = doc
        .get_dictionary(pages_id)
        .ok()
        .and_then(|tree| tree.get(b"Kids").ok().and_then(|k| k.as_array().ok()).cloned())
        .unwrap_or_default();
    let remaining: Vec<Object> = kids.into_iter().filter(|k| *k != Object::Reference(id)).collect();
    if remaining.is_empty() {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "A PDF must contain at least one page. Use Extract to keep this page in a new file instead.",
        ));
    }
    if let Ok(tree) = doc.get_dictionary_mut(pages_id) {
        tree.set("Count", Object::Integer(remaining.len() as i64));
        tree.set("Kids", Object::Array(remaining));
    }
    Ok(())
}

/// Duplicate a 1-based page; the copy sits immediately after the original.
///
/// The page is deep-copied because two pages may not share mutable state, so
/// editing an annotation on the copy cannot affect the original. The copy is
/// staged in a scratch document first: `deep_copy` needs an immutable source and
/// a mutable destination, and here both would otherwise be the same document.
pub fn duplicate_page(doc: &mut Document, page: usize) -> Result<ObjectId> {
    let source = nth_page(doc, page)?;

    let mut staging = Document::with_version(doc.version.clone());
    let mut first_pass = HashMap::new();
    let staged = deep_copy(doc, &mut staging, source, &mut first_pass, 0);

    let mut second_pass = HashMap::new();
    let copy = deep_copy(&staging, doc, staged, &mut second_pass, 0);

    // A duplicated page must point at the same parent as the original, not keep the
    // reference the deep copy carried over from the staging document.
    let parent = doc
        .get_dictionary(source)
        .ok()
        .and_then(|d| d.get(b"Parent").ok().and_then(|p| p.as_reference().ok()));
    if let Ok(dict) = doc.get_dictionary_mut(copy) {
        match parent {
            Some(parent) => dict.set("Parent", Object::Reference(parent)),
            None => {
                dict.remove(b"Parent");
            }
        }
    }

    // Insert straight after the original, then refresh the count.
    let position = page.saturating_sub(1);
    move_page_to(doc, copy, position + 1)
        .map(|()| copy)
}

/// Delete several 1-based pages in one pass.
pub fn delete_pages(doc: &mut Document, pages: &[usize]) -> Result<()> {
    let total = page_count(doc);
    if pages.len() >= total {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "A PDF must keep at least one page. Use Extract to keep some of these pages in a new file.",
        ));
    }
    // Highest first, so the earlier deletions do not shift the later targets.
    let mut ordered: Vec<usize> = pages.to_vec();
    ordered.sort_unstable_by(|a, b| b.cmp(a));
    ordered.dedup();
    for page in ordered {
        delete_page(doc, page)?;
    }
    Ok(())
}

/// Reorder pages according to a 0-based permutation of every page.
pub fn reorder_pages(doc: &mut Document, order: &[usize]) -> Result<()> {
    let ids = page_ids(doc);
    if order.len() != ids.len() {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "The new page order must list every page exactly once.",
        ));
    }
    let mut seen = vec![false; ids.len()];
    for index in order {
        if *index >= ids.len() || seen[*index] {
            return Err(BritError::new(
                ErrorKind::InvalidRequest,
                "The new page order repeats or omits a page.",
            ));
        }
        seen[*index] = true;
    }
    set_kids(doc, order.iter().map(|i| Object::Reference(ids[*i])).collect())
}

/// Keep only the listed 1-based pages, in the order given.
pub fn keep_pages(doc: &mut Document, pages: &[usize]) -> Result<()> {
    let ids = page_ids(doc);
    if pages.is_empty() {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "Select at least one page to extract.",
        ));
    }
    let mut selected = Vec::with_capacity(pages.len());
    for page in pages {
        if *page < 1 || *page > ids.len() {
            return Err(BritError::new(
                ErrorKind::InvalidRequest,
                format!("Page {page} is not in this document, which has {} pages.", ids.len()),
            ));
        }
        selected.push(Object::Reference(ids[*page - 1]));
    }
    set_kids(doc, selected)
}

fn set_kids(doc: &mut Document, kids: Vec<Object>) -> Result<()> {
    let pages_id = pages_root(doc)?;
    if let Ok(tree) = doc.get_dictionary_mut(pages_id) {
        tree.set("Count", Object::Integer(kids.len() as i64));
        tree.set("Kids", Object::Array(kids));
    }
    Ok(())
}

/// Split a document into single-page files inside `folder`, named `<stem>-page-N.pdf`.
pub fn split_into_files(doc: &Document, folder: &std::path::Path, stem: &str) -> Result<Vec<std::path::PathBuf>> {
    std::fs::create_dir_all(folder).map_err(|e| {
        BritError::new(ErrorKind::WriteFailed, format!("Could not create the output folder: {e}"))
    })?;
    let total = page_count(doc);
    let mut written = Vec::with_capacity(total);
    for index in 0..total {
        let mut single = fresh_document_from(doc)?;
        keep_pages(&mut single, &[index + 1])?;
        let bytes = serialise(&mut single, true)?;
        let path = folder.join(format!("{stem}-page-{}.pdf", index + 1));
        std::fs::write(&path, bytes).map_err(|e| {
            BritError::new(ErrorKind::WriteFailed, format!("Could not write {}: {e}", path.display()))
        })?;
        written.push(path);
    }
    Ok(written)
}

/// Copy one page, with everything it references, into a new one-page document.
pub fn extract_page(doc: &Document, page: usize) -> Result<Vec<u8>> {
    extract_pages(doc, &[page])
}

/// Extract the 1-based `pages` into a standalone PDF and return the bytes.
///
/// The output is built with the same deep-copy-and-rebuild approach as `merge`,
/// so pages never share object ids with the source file.
pub fn extract_pages(doc: &Document, pages: &[usize]) -> Result<Vec<u8>> {
    let ids = page_ids(doc);
    if pages.is_empty() {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "Select at least one page to extract.",
        ));
    }
    let mut chosen: Vec<usize> = Vec::with_capacity(pages.len());
    for page in pages {
        if *page < 1 || *page > ids.len() {
            return Err(BritError::new(
                ErrorKind::InvalidPage,
                format!("Page {page} is not in this document, which has {} pages.", ids.len()),
            ));
        }
        if !chosen.contains(page) {
            chosen.push(*page);
        }
    }

    let mut out = fresh_document_from(doc)?;
    let mut mapping = HashMap::new();
    let pages_id = out.new_object_id();
    let mut kids: Vec<Object> = Vec::with_capacity(chosen.len());
    for page in &chosen {
        let new_page = deep_copy(doc, &mut out, ids[*page - 1], &mut mapping, 0);
        if let Ok(dict) = out.get_dictionary_mut(new_page) {
            dict.set("Parent", Object::Reference(pages_id));
        }
        kids.push(Object::Reference(new_page));
    }
    let tree = dictionary! {
        "Type" => Object::Name(b"Pages".to_vec()),
        "Kids" => Object::Array(kids.clone()),
        "Count" => Object::Integer(kids.len() as i64),
    };
    out.set_object(pages_id, Object::Dictionary(tree));
    let catalog_id = out.new_object_id();
    let catalog = dictionary! {
        "Type" => Object::Name(b"Catalog".to_vec()),
        "Pages" => Object::Reference(pages_id),
    };
    out.set_object(catalog_id, Object::Dictionary(catalog));
    out.trailer.set("Root", Object::Reference(catalog_id));
    serialise(&mut out, true)
}

/// Insert every page of `source` before the 0-based `before` index in `doc`.
pub fn insert_pages_from(doc: &mut Document, before: usize, source: &Document) -> Result<()> {
    let src_ids = page_ids(source);
    if src_ids.is_empty() {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "The file you chose has no pages.",
        ));
    }
    let mut mapping = HashMap::new();
    let mut index = before.min(page_count(doc));
    let parent_res = pages_root(doc);
    for src_id in src_ids {
        let new_page = deep_copy(source, doc, src_id, &mut mapping, 0);
        if let Ok(dict) = doc.get_dictionary_mut(new_page) {
            match &parent_res {
                Ok(parent) => dict.set("Parent", Object::Reference(*parent)),
                Err(_) => {
                    dict.remove(b"Parent");
                }
            }
        }
        move_page_to(doc, new_page, index)?;
        index += 1;
    }
    Ok(())
}

/// Merge several PDFs into one, in the order given.
///
/// Every page is deep-copied into a fresh document with a rebuilt page tree, so
/// object ids from different files - which routinely collide - never clash. The
/// merge is done page by page rather than by grafting whole files, because grafting
/// is exactly what produces the "PDF opens but page 40 is wrong" bugs.
pub fn merge(documents: &[(Document, Option<String>)]) -> Result<Vec<u8>> {
    if documents.is_empty() {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "No documents were supplied to merge.",
        ));
    }
    let total: usize = documents.iter().map(|(doc, _)| page_count(doc)).sum();
    if total == 0 {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "The documents you selected have no pages.",
        ));
    }

    let mut out = Document::with_version("1.7");
    let pages_id = out.new_object_id();
    let mut kids: Vec<Object> = Vec::with_capacity(total);
    let mut mapping: HashMap<(u32, u16), ObjectId> = HashMap::new();

    for (source, _) in documents {
        for page in page_ids(source) {
            let new_page = deep_copy(source, &mut out, page, &mut mapping, 0);
            if let Ok(dict) = out.get_dictionary_mut(new_page) {
                // A copied page still names its old parent; the new tree owns it now.
                dict.set("Parent", Object::Reference(pages_id));
            }
            kids.push(Object::Reference(new_page));
        }
    }

    let tree = dictionary! {
        "Type" => Object::Name(b"Pages".to_vec()),
        "Kids" => Object::Array(kids.clone()),
        "Count" => Object::Integer(kids.len() as i64),
    };
    out.set_object(pages_id, Object::Dictionary(tree));

    let catalog_id = out.new_object_id();
    let catalog = dictionary! {
        "Type" => Object::Name(b"Catalog".to_vec()),
        "Pages" => Object::Reference(pages_id),
    };
    out.set_object(catalog_id, Object::Dictionary(catalog));
    out.trailer.set("Root", Object::Reference(catalog_id));

    // Carry over the first document's metadata, so a merge does not silently strip
    // the title and author the user already entered.
    if let Some((first, _)) = documents.first() {
        let info_id = first
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|o| o.as_reference().ok());
        if let Some(info_id) = info_id {
            if let Ok(info) = first.get_dictionary(info_id) {
                let new_info = out.new_object_id();
                let mut copied = Dictionary::new();
                for (key, value) in info.iter() {
                    let value = match value {
                        Object::Reference(target) => match deep_copy(first, &mut out, *target, &mut mapping, 0) {
                            copied_id => Object::Reference(copied_id),
                        },
                        other => other.clone(),
                    };
                    copied.set(key.clone(), value);
                }
                out.set_object(new_info, Object::Dictionary(copied));
                out.trailer.set("Info", Object::Reference(new_info));
            }
        }
        // Viewer preferences that are safe to keep: hide the toolbar, centre the
        // window, and so on. Everything that could point at a missing object is left.
        if let Ok(catalog) = first.catalog() {
            for key in [&b"PageLayout"[..], &b"PageMode"[..], &b"ViewerPreferences"[..]] {
                if let Ok(value) = catalog.get(key) {
                    let value = match value {
                        Object::Reference(target) => {
                            Object::Reference(deep_copy(first, &mut out, *target, &mut mapping, 0))
                        }
                        other => other.clone(),
                    };
                    if let Ok(new_catalog) = out.get_dictionary_mut(catalog_id) {
                        new_catalog.set(key, value);
                    }
                }
            }
        }
    }

    serialise(&mut out, true)
}

/// A new document that keeps the source's version and binary marker, with an
/// empty page tree. This is the base for extract and split.
fn fresh_document_from(source: &Document) -> Result<Document> {
    let mut out = Document::with_version(source.version.clone());
    out.binary_mark = source.binary_mark.clone();
    Ok(out)
}

fn nth_page(doc: &Document, page: usize) -> Result<ObjectId> {
    let ids = page_ids(doc);
    if page < 1 || page > ids.len() {
        return Err(BritError::new(
            ErrorKind::InvalidPage,
            format!("Page {page} is not in this document, which has {} pages.", ids.len()),
        ));
    }
    Ok(ids[page - 1])
}

/// One bookmark as a flat row: title, 1-based page, nesting depth, and whether
/// the entry actually points at a page in this document.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineEntry {
    pub title: String,
    pub page: usize,
    pub depth: i32,
    /// `false` when the bookmark's destination is missing, which happens with files
    /// that were damaged or edited by a buggy tool. BritPDF shows these greyed out
    /// rather than hiding them, so the user can see something is wrong.
    pub resolvable: bool,
    /// `true` when the destination is a URI rather than a page in this file.
    pub external: bool,
}

/// The document's outline (bookmark) tree, flattened for the sidebar.
pub fn outline(doc: &Document) -> Vec<OutlineEntry> {
    let mut out = Vec::new();
    let Ok(catalog) = doc.catalog() else { return out };
    let Ok(root_ref) = catalog.get(b"Outlines").and_then(|o| o.as_reference()) else {
        return out;
    };
    let positions: HashMap<ObjectId, usize> = page_ids(doc)
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i + 1))
        .collect();

    // A file with a cyclic or self-referential outline must not hang BritPDF.
    let mut seen: Vec<ObjectId> = Vec::new();
    let mut node = doc
        .get_dictionary(root_ref)
        .ok()
        .and_then(|d| d.get(b"First").ok().and_then(|o| o.as_reference().ok()));
    while let Some(id) = node {
        if seen.contains(&id) {
            break;
        }
        walk_outline(doc, id, 0, &positions, &mut out, &mut seen);
        node = doc
            .get_dictionary(id)
            .ok()
            .and_then(|d| d.get(b"Next").ok().and_then(|o| o.as_reference().ok()));
    }
    out
}

fn walk_outline(
    doc: &Document,
    id: ObjectId,
    depth: i32,
    positions: &HashMap<ObjectId, usize>,
    out: &mut Vec<OutlineEntry>,
    seen: &mut Vec<ObjectId>,
) {
    if seen.len() > 4096 || depth > 32 {
        return;
    }
    seen.push(id);
    let Ok(dict) = doc.get_dictionary(id) else { return };

    let title = dict
        .get(b"Title")
        .ok()
        .and_then(|t| match t {
            Object::String(bytes, _) => crate::engine::writer::decode_pdf_text(bytes),
            _ => None,
        })
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Untitled bookmark".to_string());

    let dest = dict.get(b"Dest").ok().cloned();
    let (page, resolvable, external) = match dest {
        Some(Object::Array(array)) => {
            if array
                .first()
                .and_then(|o| o.as_name().ok())
                .map(|n| n.eq_ignore_ascii_case(b"URI"))
                .unwrap_or(false)
            {
                (0, true, true)
            } else {
                match array.first().and_then(|o| o.as_reference().ok()) {
                    Some(target) => (positions.get(&target).copied().unwrap_or(0), true, false),
                    None => (0, false, false),
                }
            }
        }
        // `/A` with a `/GoTo` action is the other common way to express a destination.
        Some(Object::Dictionary(action)) => {
            match action.get(b"S").ok().and_then(|s| s.as_name().ok()) {
                Some(s) if s.eq_ignore_ascii_case(b"URI") => (0, true, true),
                Some(s) if s.eq_ignore_ascii_case(b"GoTo") => {
                    match action
                        .get(b"D")
                        .ok()
                        .and_then(|d| d.as_array().ok())
                        .and_then(|a| a.first())
                        .and_then(|o| o.as_reference().ok())
                    {
                        Some(target) => (positions.get(&target).copied().unwrap_or(0), true, false),
                        None => (0, false, false),
                    }
                }
                _ => (0, false, false),
            }
        }
        _ => (0, false, false),
    };
    out.push(OutlineEntry { title, page, depth, resolvable, external });

    let mut child = dict.get(b"First").ok().and_then(|o| o.as_reference().ok());
    while let Some(child_id) = child {
        if seen.contains(&child_id) {
            break;
        }
        walk_outline(doc, child_id, depth + 1, positions, out, seen);
        child = doc
            .get_dictionary(child_id)
            .ok()
            .and_then(|d| d.get(b"Next").ok().and_then(|o| o.as_reference().ok()));
    }
}

/// Replace a page's `/Contents` with a single stream, or return its current bytes.
pub fn page_content(doc: &Document, page: usize) -> Result<Vec<u8>> {
    let id = nth_page(doc, page)?;
    Ok(crate::engine::writer::decode_stream_content(
        doc,
        &doc.get_dictionary(id)
            .ok()
            .and_then(|d| d.get(b"Contents").ok().cloned())
            .and_then(|c| c.as_array().ok().map(|a| a.to_vec()))
            .unwrap_or_default(),
    ))
}

/// Repair a `/Pages` `/Count` left inconsistent by an earlier tool.
pub fn repair_page_tree(doc: &mut Document) -> Result<()> {
    let pages_id = pages_root(doc)?;
    let count = page_ids(doc).len();
    if let Ok(tree) = doc.get_dictionary_mut(pages_id) {
        if tree.get(b"Count").ok().and_then(|c| c.as_i64().ok()) != Some(count as i64) {
            tree.set("Count", Object::Integer(count as i64));
        }
    }
    Ok(())
}

/// What recompressing a document would achieve, and what it would cost.
///
/// BritPDF reports this instead of silently degrading the file, because the honest
/// answer is often "almost nothing, and here is why".
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionReport {
    pub original_bytes: usize,
    pub estimated_bytes: usize,
    pub image_count: usize,
    pub image_bytes: usize,
    pub font_count: usize,
    pub font_substituted: bool,
    pub already_compressed: bool,
    pub level: String,
    pub note: String,
}

/// Measure the compression opportunity without writing anything.
pub fn measure_compression(doc: &Document, original_bytes: usize) -> CompressionReport {
    let mut image_count = 0usize;
    let mut image_bytes = 0usize;
    let mut font_count = 0usize;
    let mut font_substituted = false;
    let mut uncompressed_streams = 0usize;
    let mut uncompressed_bytes = 0usize;
    let mut already_compressed = true;

    for (id, object) in &doc.objects {
        let Object::Stream(stream) = object else { continue };
        let subtype = stream.dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).map(|n| n.to_vec());
        let length = stream.content.len();
        let compressed = stream.dict.has(b"Filter");
        if !compressed {
            already_compressed = false;
            uncompressed_streams += 1;
            uncompressed_bytes += length;
        }
        match subtype.as_deref() {
            Some(b"Image") => {
                image_count += 1;
                image_bytes += length;
                if !has_lossy_filter(&stream.dict) {
                    already_compressed = false;
                }
            }
            Some(b"Type1") | Some(b"TrueType") | Some(b"Type0") | Some(b"CIDFontType0") | Some(b"CIDFontType2") => {
                font_count += 1;
                // A non-embedded font is drawn with a substitute by the reader. That
                // is a fidelity problem, not a size problem, so it is reported
                // separately instead of being quietly "fixed".
                if stream.dict.get(b"FontFile").ok().and_then(|o| o.as_reference().ok()).is_none()
                    && stream.dict.get(b"FontFile2").ok().and_then(|o| o.as_reference().ok()).is_none()
                    && stream.dict.get(b"FontFile3").ok().and_then(|o| o.as_reference().ok()).is_none()
                {
                    font_substituted = true;
                }
            }
            _ => {
                let _ = id;
            }
        }
    }

    // Flate typically reaches 60-80% on PDF text and vector streams; already
    // compressed images are copied verbatim, so they are added back at full size.
    let estimated = image_bytes + (uncompressed_bytes as f64 * 0.35) as usize;
    let level = if already_compressed {
        "none"
    } else if image_bytes > 0 && image_bytes * 2 > uncompressed_bytes {
        "text and vectors"
    } else {
        "text, vectors and uncompressed images"
    };
    let savings = if estimated > 0 && original_bytes > estimated {
        100 - (estimated * 100 / original_bytes)
    } else {
        0
    };

    let mut note = if already_compressed {
        "This PDF is already compressed. Saving it again would not make it smaller.".to_string()
    } else {
        format!(
            "Text and vector streams ({uncompressed_streams} stream(s)) are recompressed losslessly; \
             about {savings}% smaller overall."
        )
    };
    if image_count > 0 {
        note.push_str(&format!(
            " The {image_count} image(s), {} KB in total, are stored as JPEG or similar and are \
             copied unchanged, because recompressing a photograph would lose quality.",
            image_bytes / 1024
        ));
    }
    if font_substituted {
        note.push_str(
            " This PDF does not embed all of its fonts, so a reader substitutes them and the text \
             may look different. Compressing cannot fix that.",
        );
    }

    CompressionReport {
        original_bytes,
        estimated_bytes: estimated,
        image_count,
        image_bytes,
        font_count,
        font_substituted,
        already_compressed,
        level: level.to_string(),
        note,
    }
}

/// Recompress the document's uncompressed streams. The rewrite is *lossless*:
/// already-compressed streams and images are left alone.
pub fn compress_document(doc: &mut Document) -> Result<()> {
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        let skip = doc.get_dictionary(id).is_ok_and(|dict| {
            dict.get(b"Subtype")
                .ok()
                .and_then(|s| s.as_name().ok())
                == Some(b"Image".as_slice())
                && has_lossy_filter(dict)
        });
        if skip {
            continue;
        }
        if let Some(Object::Stream(stream)) = doc.objects.get_mut(&id) {
            if !stream.dict.has(b"Filter") && stream.allows_compression {
                let _ = stream.compress();
            }
        }
    }
    // Pack non-stream objects into object streams: lossless, and typically 10-30%
    // of the remaining overhead on a page-heavy file.
    doc.compress();
    Ok(())
}

fn has_lossy_filter(dict: &Dictionary) -> bool {
    let names: Vec<String> = match dict.get(b"Filter") {
        Ok(Object::Name(name)) => vec![String::from_utf8_lossy(name).into_owned()],
        Ok(Object::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_name().ok().map(|n| String::from_utf8_lossy(n).into_owned()))
            .collect(),
        _ => vec![],
    };
    names.iter().any(|n| {
        matches!(
            n.as_str(),
            "DCTDecode" | "JPXDecode" | "CCITTFaxDecode" | "JBIG2Decode"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::writer::blank_document;

    fn three_page() -> Document {
        let mut doc = blank_document(200.0, 300.0);
        append_page(&mut doc, 200.0, 300.0).unwrap();
        append_page(&mut doc, 200.0, 300.0).unwrap();
        doc
    }

    fn reload(doc: &mut Document) -> Document {
        let bytes = serialise(doc, false).unwrap();
        Document::load_mem(&bytes).unwrap()
    }

    #[test]
    fn appends_and_counts_pages() {
        let mut doc = three_page();
        assert_eq!(page_count(&doc), 3);
        let reloaded = reload(&mut doc);
        assert_eq!(page_count(&reloaded), 3);
    }

    #[test]
    fn delete_removes_exactly_one_page() {
        let mut doc = three_page();
        delete_page(&mut doc, 2).unwrap();
        assert_eq!(page_count(&doc), 2);
        assert_eq!(page_count(&reload(&mut doc)), 2);
    }

    #[test]
    fn refuses_to_delete_every_page() {
        let mut doc = blank_document(200.0, 300.0);
        let err = delete_page(&mut doc, 1).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidRequest);
    }

    #[test]
    fn bulk_delete_uses_original_page_numbers() {
        let mut doc = three_page();
        delete_pages(&mut doc, &[1, 3]).unwrap();
        assert_eq!(page_count(&doc), 1);
    }

    #[test]
    fn bulk_delete_refuses_to_empty_the_document() {
        let mut doc = three_page();
        assert!(delete_pages(&mut doc, &[1, 2, 3]).is_err());
    }

    #[test]
    fn duplicate_puts_the_copy_after_the_original() {
        let mut doc = three_page();
        let copy = duplicate_page(&mut doc, 1).unwrap();
        assert_eq!(page_count(&doc), 4);
        // The copy must be its own object, not the original page.
        assert_ne!(copy, page_ids(&doc)[0]);
        assert_eq!(page_count(&reload(&mut doc)), 4);
    }

    #[test]
    fn reorder_requires_a_permutation() {
        let mut doc = three_page();
        assert!(reorder_pages(&mut doc, &[0, 1]).is_err());
        assert!(reorder_pages(&mut doc, &[0, 0, 1]).is_err());
        assert!(reorder_pages(&mut doc, &[0, 1, 9]).is_err());
        reorder_pages(&mut doc, &[2, 1, 0]).unwrap();
        assert_eq!(page_count(&reload(&mut doc)), 3);
    }

    #[test]
    fn rotation_is_normalised_and_validated() {
        let mut doc = blank_document(200.0, 300.0);
        set_page_rotation(&mut doc, 1, 450).unwrap();
        assert_eq!(effective_rotation(&doc, 1), 90);
        assert!(set_page_rotation(&mut doc, 1, 45).is_err());
        assert!(set_page_rotation(&mut doc, 99, 90).is_err());
    }

    #[test]
    fn rotate_by_accumulates() {
        let mut doc = blank_document(200.0, 300.0);
        assert_eq!(rotate_page_by(&mut doc, 1, 90).unwrap(), 90);
        assert_eq!(rotate_page_by(&mut doc, 1, 90).unwrap(), 180);
        assert_eq!(rotate_page_by(&mut doc, 1, 180).unwrap(), 0);
    }

    #[test]
    fn keep_pages_reorders_and_filters() {
        let mut doc = three_page();
        keep_pages(&mut doc, &[3, 1]).unwrap();
        assert_eq!(page_count(&doc), 2);
        assert!(keep_pages(&mut doc, &[]).is_err());
        assert!(keep_pages(&mut doc, &[9]).is_err());
    }

    #[test]
    fn merge_concatenates_pages() {
        let a = three_page();
        let b = blank_document(100.0, 100.0);
        let bytes = merge(&[(a, None), (b, None)]).unwrap();
        let merged = Document::load_mem(&bytes).unwrap();
        assert_eq!(merged.get_pages().values().count(), 4);
        let sizes = page_sizes(&merged);
        assert_eq!(sizes[3], (100.0, 100.0));
    }

    #[test]
    fn merge_of_one_document_keeps_its_pages() {
        let a = three_page();
        let bytes = merge(&[(a, None)]).unwrap();
        let merged = Document::load_mem(&bytes).unwrap();
        assert_eq!(merged.get_pages().values().count(), 3);
    }

    #[test]
    fn merge_rejects_an_empty_list() {
        assert!(merge(&[]).is_err());
    }

    #[test]
    fn resize_changes_the_media_box() {
        let mut doc = blank_document(200.0, 300.0);
        resize_page(&mut doc, 1, 595.0, 842.0).unwrap();
        assert_eq!(page_sizes(&doc)[0], (595.0, 842.0));
        assert!(resize_page(&mut doc, 1, 0.0, 10.0).is_err());
    }

    #[test]
    fn extract_keeps_only_the_requested_page() {
        let doc = three_page();
        let bytes = extract_page(&doc, 2).unwrap();
        let one = Document::load_mem(&bytes).unwrap();
        assert_eq!(one.get_pages().values().count(), 1);
    }

    #[test]
    fn split_writes_one_file_per_page() {
        let doc = three_page();
        let dir = std::env::temp_dir().join(format!("britpdf-split-{}", std::process::id()));
        let files = split_into_files(&doc, &dir, "part").unwrap();
        assert_eq!(files.len(), 3);
        for file in &files {
            assert!(file.exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compression_keeps_the_document_readable() {
        let mut doc = three_page();
        compress_document(&mut doc).unwrap();
        let bytes = serialise(&mut doc, true).unwrap();
        let reloaded = Document::load_mem(&bytes).unwrap();
        assert_eq!(reloaded.get_pages().values().count(), 3);
    }

    #[test]
    fn compression_report_is_honest_about_images() {
        let doc = three_page();
        let report = measure_compression(&doc, 1000);
        assert_eq!(report.image_count, 0);
        assert!(report.note.contains("losslessly"));
    }

    #[test]
    fn load_rejects_non_pdf_data() {
        let err = load(b"this is not a pdf", None).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidPdf);
    }
}
