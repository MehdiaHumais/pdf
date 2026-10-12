//! Reproduce the in-app "open a PDF" pipeline headlessly to expose real errors.
//! Run: cargo test --test open_flow --features ocr-windows -- --nocapture

use britpdf_lib::engine::{authoring, engine_info, text};
use britpdf_lib::engine::render::{render_page, RenderRequest};
use britpdf_lib::engine::session::{Dirs, Session};
use std::path::PathBuf;

fn temp_dirs(tag: &str) -> Dirs {
    let root = std::env::temp_dir().join(format!("britpdf-probe-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let dirs = Dirs::new(root.clone());
    dirs.ensure().expect("prepare dirs");
    dirs
}

#[test]
fn open_render_text_flow() {
    let dirs = temp_dirs("open-flow");
    println!("engine_info = {:?}", engine_info());
    println!(
        "cwd = {} (manifest={})",
        std::env::current_dir().expect("cwd").display(),
        env!("CARGO_MANIFEST_DIR")
    );

    // Make a real PDF with the writer.
    let sample = dirs.recovery_dir().join("probe.pdf");
    std::fs::write(&sample, authoring::create_blank_document(400.0, 600.0, None))
        .expect("write sample");

    let session = Session::new(dirs);
    let id = session.new_id();
    let mut doc = match session.open_from_path(id.clone(), &sample, None) {
        Ok(d) => d,
        Err(e) => panic!(
            "open_from_path failed: kind={:?} message={} hint={:?} detail={:?}",
            e.kind, e.message, e.hint, e.detail
        ),
    };
    println!("opened ok: pages={} version={}", doc.page_count(), doc.pdf_version);

    let g = doc.page_geometry(0).expect("geometry");
    println!("page0 geometry = {} x {}", g.width, g.height);

    // Exercise the pure-PDFium path exactly like the UI's render_page command.
    let render_dirs = session.dirs.clone();
    let rendered = render_page(
        &mut doc,
        &render_dirs,
        RenderRequest {
            page: 0,
            width: 800,
            rotation: 0,
            render_annotations: true,
            render_forms: true,
            grayscale: false,
            max_area: Some(20_000_000),
        },
    )
    .expect("render page");
    println!(
        "rendered {}x{} -> {} (cache={})",
        rendered.width, rendered.height, rendered.path, rendered.from_cache
    );
    assert!(std::path::Path::new(&rendered.path).exists(), "render png must exist");

    let all = text::document_text(&mut doc).expect("document_text");
    println!("document_text chars={}", all.1);

    session.close(&id).expect("close");
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("britpdf-probe-open-flow"));
}