//! Unified, serialisable error model for every BritPDF command.
//!
//! Every failure path in the application funnels through [`BritError`]. The UI can
//! therefore always show the user *why* something failed and offer a recovery action,
//! instead of silently failing. No error text is ever logged to the console, so
//! document contents and passwords never leak into logs.

use serde::{Serialize, Serializer};
use std::path::Path;

/// Stable machine-readable error categories. The frontend switches on these to pick
/// the right dialog copy and recovery buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// The file does not exist, or was moved/deleted between selection and open.
    FileNotFound,
    /// The path exists but could not be read (permissions, sharing violation, ...).
    AccessDenied,
    /// The path is not a valid filesystem path, or is of the wrong kind (dir vs file).
    InvalidPath,
    /// The bytes are not a parseable PDF.
    CorruptDocument,
    /// The document is encrypted and no/incorrect password was supplied.
    PasswordRequired,
    /// The document is encrypted and the supplied password was rejected.
    InvalidPassword,
    /// The document uses a PDF feature this build cannot process.
    UnsupportedFeature,
    /// The document has a valid structure but cannot be saved in the requested way.
    NotSupported,
    /// A write to the destination failed (read-only volume, disk full, ...).
    WriteFailed,
    /// The operation was cancelled by the user.
    Cancelled,
    /// PDFium (the rendering engine) could not be initialised.
    EngineUnavailable,
    /// A PDFium call failed at runtime.
    EngineFailure,
    /// Page index out of range.
    InvalidPage,
    /// A tool rejected its input (bad page range, empty merge list, ...).
    InvalidRequest,
    /// OCR is unavailable on this machine.
    OcrUnavailable,
    /// No printer is installed / the chosen printer cannot be opened.
    PrinterUnavailable,
    /// Recovery data could not be created or restored.
    RecoveryFailure,
    /// An unexpected internal error.
    Internal,
}

impl ErrorKind {
    /// Default user-facing headline for this category.
    pub fn headline(self) -> &'static str {
        match self {
            ErrorKind::FileNotFound => "File not found",
            ErrorKind::AccessDenied => "Permission denied",
            ErrorKind::InvalidPath => "Invalid file path",
            ErrorKind::CorruptDocument => "Unable to open this PDF",
            ErrorKind::PasswordRequired => "Password required",
            ErrorKind::InvalidPassword => "Incorrect password",
            ErrorKind::UnsupportedFeature => "Unsupported PDF feature",
            ErrorKind::NotSupported => "Operation not supported",
            ErrorKind::WriteFailed => "Could not write the file",
            ErrorKind::Cancelled => "Operation cancelled",
            ErrorKind::EngineUnavailable => "PDF engine unavailable",
            ErrorKind::EngineFailure => "PDF engine error",
            ErrorKind::InvalidPage => "Invalid page",
            ErrorKind::InvalidRequest => "Invalid request",
            ErrorKind::OcrUnavailable => "OCR is not available",
            ErrorKind::PrinterUnavailable => "No usable printer",
            ErrorKind::RecoveryFailure => "Recovery data unavailable",
            ErrorKind::Internal => "Unexpected error",
        }
    }
}

/// A single error value that is safe to send to the UI.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct BritError {
    pub kind: ErrorKind,
    pub message: String,
    /// Optional, user-actionable hint ("Try a different password.", ...).
    pub hint: Option<String>,
    /// The underlying library's message, kept for diagnostics. It is deliberately
    /// *not* serialised to the UI: a low-level error can contain a file path or a
    /// fragment of a password, and neither belongs in a dialog a user can screenshot.
    pub detail: Option<String>,
}

impl BritError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), hint: None, detail: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Attach the underlying cause for the diagnostics log only.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }
}

impl Serialize for BritError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let headline = self.kind.headline();
        let mut st = serializer.serialize_struct("BritError", 4)?;
        st.serialize_field("kind", &self.kind)?;
        st.serialize_field("headline", &headline)?;
        st.serialize_field("message", &self.message)?;
        st.serialize_field("hint", &self.hint)?;
        st.end()
    }
}

pub type Result<T> = std::result::Result<T, BritError>;

/// Wrap an IO failure with a precise category based on the `io::ErrorKind`.
pub fn io_err(context: &str, path: &Path, err: &std::io::Error) -> BritError {
    use std::io::ErrorKind as IoKind;
    let kind = match err.kind() {
        IoKind::NotFound => ErrorKind::FileNotFound,
        IoKind::PermissionDenied => ErrorKind::AccessDenied,
        _ => ErrorKind::WriteFailed,
    };
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let display = path.display().to_string();
    BritError::new(kind, format!("{context}: {}", file.as_deref().unwrap_or(&display)))
        .with_hint(match kind {
            ErrorKind::FileNotFound => {
                "The file may have been moved, renamed or deleted.".to_string()
            }
            ErrorKind::AccessDenied => {
                "Close the file in another program or check the folder permissions.".to_string()
            }
            _ => format!("{err}"),
        })
}

pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    if !path.exists() {
        return Err(BritError::new(
            ErrorKind::FileNotFound,
            format!("{} does not exist", path.display()),
        ));
    }
    if path.is_dir() {
        return Err(BritError::new(
            ErrorKind::InvalidPath,
            format!("{} is a folder, not a PDF file", path.display()),
        ));
    }
    std::fs::read(path).map_err(|e| io_err("Could not read", path, &e))
}

pub fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            return Err(BritError::new(
                ErrorKind::InvalidPath,
                format!("The folder {} does not exist", parent.display()),
            ));
        }
    }
    std::fs::write(path, bytes).map_err(|e| io_err("Could not save", path, &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_with_stable_headline() {
        let err = BritError::new(ErrorKind::CorruptDocument, "bad trailer");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"kind\":\"corruptDocument\""));
        assert!(json.contains("\"headline\":\"Unable to open this PDF\""));
    }

    #[test]
    fn missing_file_is_file_not_found() {
        let err = read_file(Path::new("Z:/definitely/not/here.pdf")).unwrap_err();
        assert_eq!(err.kind, ErrorKind::FileNotFound);
    }
}
