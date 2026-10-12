//! BritPDF's Tauri application: state, setup, and the command surface.

pub mod commands;
pub mod engine;
pub mod error;
pub mod native;

use error::{BritError, Result};
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;

/// Everything BritPDF needs to live in one place, handed to every command.
pub struct AppState {
    /// Open documents and their working files.
    pub session: engine::session::Session,
    /// Settings, recents, and crash-recovery bookkeeping.
    pub system: native::system::System,
}

impl AppState {
    pub fn new(app: &tauri::AppHandle) -> Result<Self> {
        let data = app
            .path()
            .app_local_data_dir()
            .map_err(|e| BritError::internal(format!("BritPDF has no application data folder: {e}")))?
            .join("BritPDF");
        let dirs = engine::session::Dirs::new(data);
        dirs.ensure()
            .map_err(|e| BritError::internal(format!("The BritPDF data folder could not be prepared: {e}")))?;
        let system = native::system::System::new(dirs.clone())?;
        Ok(Self {
            session: engine::session::Session::new(dirs),
            system,
        })
    }
}

/// The managed application state, as a command parameter.
///
/// The lifetime is a parameter on purpose. A Tauri command that declares
/// `State<'static, _>` forces the command wrapper to borrow its ACL table for
/// `'static`, which does not compile; `State<'_, _>` is the correct signature and
/// is what the Tauri macro is built to generate.
pub type SharedState<'a> = tauri::State<'a, Arc<AppState>>;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let state = Arc::new(AppState::new(app.handle())?);
            app.manage(state);

            // The window is created hidden and shown only once the frontend has
            // painted, so the user never sees a white flash before the UI exists.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
            }

            // A file passed on the command line - a double-click in Explorer, a
            // "Open with BritPDF" shell action, or a BritOffice dashboard launch -
            // is reported to the UI, which opens it as a normal document. Passing
            // the path as data rather than opening it here keeps all document
            // handling in one place, with one set of error messages.
            let pending: Vec<String> = std::env::args()
                .skip(1)
                .filter(|a| !a.starts_with('-'))
                .map(|a| a.trim_matches('"').to_string())
                .filter(|a| a.to_ascii_lowercase().ends_with(".pdf"))
                .collect();
            let _ = app.emit("britpdf:open-requested", pending);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // application
            commands::app::app_info,
            commands::app::get_settings,
            commands::app::set_settings,
            commands::app::get_recent_files,
            commands::app::clear_recent_files,
            commands::app::remove_recent_file,
            commands::app::reveal_in_explorer,
            commands::app::open_external,
            commands::app::check_for_updates,
            commands::app::get_third_party_notices,
            commands::app::get_recovery_state,
            commands::app::discard_recovery,
            commands::app::run_self_test,
            // documents
            commands::documents::open_document,
            commands::documents::open_document_with_password,
            commands::documents::list_documents,
            commands::documents::doc_info,
            commands::documents::close_document,
            commands::documents::list_pages,
            commands::documents::render_page,
            commands::documents::render_thumbnail,
            commands::documents::save_document,
            commands::documents::save_document_as,
            commands::documents::undo_document,
            commands::documents::redo_document,
            commands::documents::print_document,
            commands::documents::page_text,
            commands::documents::document_text,
            // editing: annotations, page surgery, decoration
            commands::editing::apply_annotations,
            commands::editing::get_annotations,
            commands::editing::insert_blank_page,
            commands::editing::insert_pdf_from,
            commands::editing::duplicate_page,
            commands::editing::delete_pages,
            commands::editing::rotate_pages,
            commands::editing::extract_pages_to,
            commands::editing::place_image,
            commands::editing::add_header_footer,
            commands::editing::add_page_numbers,
            commands::editing::add_watermark,
            commands::editing::get_outline,
        ])
        .run(tauri::generate_context!())
        .expect("BritPDF could not start. Reinstalling the application usually fixes this.");
}
