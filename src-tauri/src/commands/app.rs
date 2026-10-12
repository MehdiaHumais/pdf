//! Application-level commands: identity, settings, recents, shell integration.

use crate::error::{BritError, ErrorKind, Result};
use crate::native::system::Settings;
use crate::SharedState;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub engine: String,
    pub engine_version: String,
    pub os: String,
    pub arch: String,
    pub data_dir: String,
    /// What BritPDF will and will not do, so the About screen is honest.
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// A PDF opened in a browser: BritPDF is deliberately local-only, so this is
    /// always false. The field exists so the UI can state it rather than imply it.
    pub network_access: bool,
    /// PDF JavaScript actions are not executed. The PDF specification allows
    /// scripts; BritPDF treats every PDF as untrusted input and never runs them.
    pub executes_pdf_javascript: bool,
    pub annotations: bool,
    pub text_layer_ocr: bool,
    pub encryption: bool,
    pub redaction_removes_content: bool,
    pub vector_printing: bool,
    pub ocr_engine: String,
}

#[tauri::command]
pub fn app_info(state: SharedState) -> Result<AppInfo> {
    let (engine, engine_version) = crate::engine::engine_info();
    let settings = state.system.settings();
    let ocr_engine = if settings.ocr_engine == crate::native::system::OcrEngineChoice::Disabled {
        "Disabled".to_string()
    } else {
        crate::engine::ocr::available_engine()
            .map(|(name, version)| format!("{name} {version}"))
            .unwrap_or_else(|| "Not available on this PC".to_string())
    };
    Ok(AppInfo {
        name: "BritPDF".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        engine,
        engine_version,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        data_dir: state.system.dirs().data.display().to_string(),
        capabilities: Capabilities {
            network_access: false,
            executes_pdf_javascript: false,
            annotations: true,
            text_layer_ocr: !ocr_engine.starts_with("Not available"),
            encryption: true,
            redaction_removes_content: true,
            // Pages are rasterised for printing at the chosen DPI.
            vector_printing: false,
            ocr_engine,
        },
    })
}

#[tauri::command]
pub fn get_settings(state: SharedState) -> Result<Settings> {
    Ok(state.system.settings())
}

#[tauri::command]
pub fn set_settings(state: SharedState, settings: Settings) -> Result<Settings> {
    state.system.set_settings(settings)
}

#[tauri::command]
pub fn get_recent_files(state: SharedState) -> Result<Vec<crate::native::system::RecentEntry>> {
    state.system.prune_recents();
    Ok(state.system.recents())
}

#[tauri::command]
pub fn clear_recent_files(state: SharedState) -> Result<()> {
    state.system.clear_recents()
}

#[tauri::command]
pub fn remove_recent_file(state: SharedState, path: String) -> Result<()> {
    state.system.remove_recent(&path)
}

/// Show a file in Windows Explorer, selecting it.
#[tauri::command]
pub fn reveal_in_explorer(path: String) -> Result<()> {
    reveal(&path)
}

fn reveal(path: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
        let result = std::process::Command::new("explorer.exe")
            .arg(format!("/select,\"{}\"", path))
            .creation_flags(SEE_MASK_NOCLOSEPROCESS)
            .spawn();
        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(BritError::new(
                ErrorKind::Internal,
                format!("Windows Explorer could not be opened: {e}"),
            )),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(BritError::new(ErrorKind::Internal, "Explorer is Windows only."))
    }
}

/// Open a path or URL with the system default handler. Used for http(s) links only;
/// PDF paths always go through the document pipeline.
#[tauri::command]
pub fn open_external(url: String) -> Result<()> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")) {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            "Only web links can be opened outside BritPDF.",
        ));
    }
    open_url(&url)
}

fn open_url(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(SEE_MASK_NOCLOSEPROCESS)
            .spawn()
            .map(|_| ())
            .map_err(|e| BritError::new(ErrorKind::Internal, format!("Could not open the link: {e}")))
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err(BritError::new(ErrorKind::Internal, "Unsupported platform."))
    }
}

/// Update checking is opt-in and never automatic. BritPDF is local-first, so this
/// only reports whether an update *service* has been configured; with none
/// configured the answer is "not available", and no network call is made.
#[tauri::command]
pub fn check_for_updates() -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "available": false,
        "currentVersion": env!("CARGO_PKG_VERSION"),
        "channel": "stable",
        "message": "BritPDF is a local-first application and does not contact the network for updates. \
                    Install a new version with the installer supplied by your administrator.",
    }))
}

#[tauri::command]
pub fn get_third_party_notices() -> Result<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("THIRD-PARTY-NOTICES.md"))
        .unwrap_or_else(|| PathBuf::from("THIRD-PARTY-NOTICES.md"));
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(_) => Ok(
            "Third-party notices are bundled with the installer. \
             BritPDF uses PDFium (BSD-3-Clause), lopdf (MIT), pdfium-render (MIT OR Apache-2.0), \
             the image crate (MIT OR Apache-2.0) and the React/Tauri ecosystem."
                .to_string(),
        ),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryState {
    pub has_undocumented_work: bool,
    pub entries: Vec<crate::native::system::RecoveryEntry>,
}

#[tauri::command]
pub fn get_recovery_state(state: SharedState) -> Result<RecoveryState> {
    state.system.prune_recovery();
    let entries = state.system.recovery();
    Ok(RecoveryState { has_undocumented_work: !entries.is_empty(), entries })
}

#[tauri::command]
pub fn discard_recovery(state: SharedState, doc_id: String) -> Result<()> {
    state.system.clear_recovery(&doc_id);
    let _ = std::fs::remove_file(state.session.dirs.work_path(&doc_id));
    Ok(())
}

/// A self test the user (or QA) can run from Help ▸ Check the installation. It
/// proves the engine, the file system and the PDF writer all work on this machine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfTest {
    pub passed: bool,
    pub checks: Vec<CheckResult>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[tauri::command]
pub fn run_self_test(state: SharedState) -> Result<SelfTest> {
    let mut checks = Vec::new();

    checks.push(match crate::engine::pdfium() {
        Ok(_) => CheckResult {
            name: "PDF rendering engine".into(),
            passed: true,
            detail: "PDFium loaded".into(),
        },
        Err(e) => CheckResult {
            name: "PDF rendering engine".into(),
            passed: false,
            detail: e.message,
        },
    });

    // Create, render and re-open a one page PDF: exercises the writer and renderer.
    let probe = state.session.dirs.recovery_dir().join("selftest.pdf");
    let doc = crate::engine::authoring::create_blank_document(300.0, 300.0, None);
    let write_ok = std::fs::write(&probe, doc).is_ok();
    checks.push(CheckResult {
        name: "PDF writer".into(),
        passed: write_ok,
        detail: if write_ok { "Wrote a test PDF".into() } else { "Could not write".into() },
    });

    if write_ok {
        let id = state.session.new_id();
        match state.session.open_from_path(id, &probe, None) {
            Ok(mut opened) => {
                let count = opened.page_count();
                let dirs = state.session.dirs.clone();
                let render = crate::engine::render::render_page(
                    &mut opened,
                    &dirs,
                    crate::engine::render::RenderRequest {
                        page: 0,
                        width: 200,
                        rotation: 0,
                        render_annotations: true,
                        render_forms: true,
                        grayscale: false,
                        max_area: Some(200_000),
                    },
                );
                checks.push(CheckResult {
                    name: "PDF reader and renderer".into(),
                    passed: count == 1 && render.is_ok(),
                    detail: format!(
                        "Read {count} page(s); {}",
                        match render {
                            Ok(r) => format!("rendered {}x{}", r.width, r.height),
                            Err(e) => e.message,
                        }
                    ),
                });
                state.session.close(&opened.id).ok();
            }
            Err(e) => checks.push(CheckResult {
                name: "PDF reader and renderer".into(),
                passed: false,
                detail: e.message,
            }),
        }
    }

    let _ = std::fs::remove_file(&probe);

    checks.push(CheckResult {
        name: "Printers".into(),
        passed: true,
        detail: match crate::native::printing::default_printer_name() {
            Some(name) => format!("Default printer: {name}"),
            None => "No default printer installed".to_string(),
        },
    });

    checks.push(CheckResult {
        name: "OCR".into(),
        passed: true,
        detail: match crate::engine::ocr::available_engine() {
            Some((name, version)) => format!("{name} {version}"),
            None => "No OCR engine on this PC".to_string(),
        },
    });

    let passed = checks.iter().all(|c| c.passed);
    Ok(SelfTest { passed, checks })
}
