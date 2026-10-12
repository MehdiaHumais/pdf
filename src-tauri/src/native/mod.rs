//! BritPDF native layer: everything that touches the operating system.
//!
//! * [`system`] - settings, recent files, atomic writes, crash recovery
//! * [`printing`] - Windows printer enumeration, preview and printing via GDI
//! * [`dialogs`] - file picker plumbing and per-app-file paths
//!
//! None of this is coupled to React: the UI calls Tauri commands and receives
//! plain data back.

pub mod dialogs;
pub mod printing;
pub mod system;
