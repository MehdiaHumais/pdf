//! Headless text-extraction diagnostics.
//!
//! Opens a PDF straight through PDFium and prints the raw per-character metrics,
//! per-line thresholds, and reconstructed text, so extraction bugs can be studied
//! without the Tauri shell.
//!
//! Usage:
//!   cargo run --bin probe -- <path-to.pdf> [page [page ...]]   (pages are 1-based)
//!
//! Loads `pdfium.dll` from the same candidates as the app (the `vendor` dir under
//! the project root, or `BRITPDF_PDFIUM_PATH`).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: probe <pdf path> [page 1-based] [page ...]");
        std::process::exit(2);
    }
    let path = &args[0];
    let pages: Option<Vec<usize>> = if args.len() > 1 {
        Some(args[1..].iter().filter_map(|s| s.parse::<usize>().ok()).collect())
    } else {
        None
    };
    britpdf_lib::engine::text::probe_text_extraction(path, pages);
}