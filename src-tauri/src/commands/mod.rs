//! Tauri command surface.
//!
//! Every command is a thin, well-typed boundary: it validates input, calls into the
//! engine or the native layer, and returns plain data. No command ever returns a
//! partially-applied result - either the whole operation succeeded or the user's
//! file is untouched.

pub mod app;
pub mod documents;
pub mod editing;
