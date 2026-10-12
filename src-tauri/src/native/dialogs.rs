//! File picker plumbing and the paths BritPDF owns.
//!
//! The UI uses the Tauri dialog plugin for the actual native pickers; this module
//! owns the *policy*: default file names, per-app work folders, the temp folder used
//! by drag-and-drop and by image conversion, and the cleanup of those temp files.

use crate::error::{BritError, ErrorKind};
use std::path::PathBuf;

/// The folder BritPDF writes derived files into when the user picks "save into
/// this app's folder". Keeping generated artefacts separate from the user's own
/// documents is what makes "never overwrite the original" easy to guarantee.
pub fn default_output_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("BritPDF")
        .join("Output")
}

/// A sensible file name for a Save As dialog, e.g. `report (copy).pdf`.
pub fn suggest_save_name(existing: &str, add_suffix: Option<&str>) -> String {
    let base = std::path::Path::new(existing)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Document".into());
    match add_suffix {
        Some(suffix) if !suffix.is_empty() => format!("{base}{suffix}.pdf"),
        _ => format!("{base}.pdf"),
    }
}

/// Build a non-colliding path by appending " (2)", " (3)" and so on.
pub fn unique_path(dir: &std::path::Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = std::path::Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let ext = std::path::Path::new(file_name)
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pdf".into());
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({n}).{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    Err(BritError::new(ErrorKind::WriteFailed, "Could not find a free file name.")).unwrap()
}

/// Expand a Windows shortcut-style path or a bare file name against a base folder.
pub fn resolve_relative(base: &std::path::Path, input: &str) -> PathBuf {
    let trimmed = input.trim().trim_matches('"');
    let direct = PathBuf::from(trimmed);
    if direct.is_absolute() {
        return direct;
    }
    base.join(direct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_suffix_before_extension() {
        assert_eq!(suggest_save_name("report.pdf", Some(" (compressed)")), "report (compressed).pdf");
        assert_eq!(suggest_save_name("report.pdf", None), "report.pdf");
        assert_eq!(suggest_save_name("no-extension", Some("")), "no-extension.pdf");
    }

    #[test]
    fn resolves_relative_paths_against_a_base() {
        let base = PathBuf::from(r"C:\Users\me\Documents");
        assert_eq!(resolve_relative(&base, "a.pdf"), base.join("a.pdf"));
        assert_eq!(resolve_relative(&base, r"D:\x\a.pdf"), PathBuf::from(r"D:\x\a.pdf"));
    }
}
