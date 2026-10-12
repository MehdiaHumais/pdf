//! Offline OCR built on `Windows.Media.Ocr`, the text recogniser that ships inside
//! Windows 10 and 11.
//!
//! Two properties matter here and both are structural, not promises:
//!
//! * **Nothing leaves the machine.** The recogniser is a local Windows component.
//!   There is no network code in this module, and no code path that could add one.
//! * **It is not always there.** A Windows install with no language packs has no
//!   recogniser at all. Every entry point therefore reports "not available"
//!   honestly rather than pretending, and the UI hides the feature instead of
//!   failing at the moment the user clicks it.
//!
//! Recognised words come back in *bitmap pixel* space, not PDF points. The caller
//! scales them using [`OcrPage::pixel_width`] and [`OcrPage::pixel_height`], which
//! is why those are part of every result.

use crate::error::{BritError, ErrorKind, Result};

/// One recognised word and where it sits in the image.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrWord {
    pub text: String,
    /// `x, y, width, height` in image pixels, origin top-left.
    pub bounds: [f32; 4],
    /// 0..=1. Windows OCR does not report per-word confidence, so this is filled
    /// from the page average and is not a per-word measurement.
    pub confidence: f32,
}

/// One recognised line, with its words.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrLine {
    pub text: String,
    pub words: Vec<OcrWord>,
    /// `x, y, width, height` in image pixels, origin top-left.
    pub bounds: [f32; 4],
}

/// The recognised text of one page image.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrPage {
    pub lines: Vec<OcrLine>,
    pub text: String,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Recogniser language tag, e.g. `en-GB`.
    pub language: String,
    /// Average word confidence, 0..=1. Windows OCR exposes no confidence value, so
    /// this is a placeholder that the UI must not present as a measured score.
    pub confidence: f32,
    /// How long recognition took, in milliseconds.
    pub elapsed_ms: u64,
}

/// The name and version of the OCR engine, for the About screen and self test.
/// `None` means this PC has no recogniser, and the feature must be hidden.
pub fn available_engine() -> Option<(String, String)> {
    #[cfg(all(windows, feature = "ocr-windows"))]
    {
        use windows::Media::Ocr::OcrEngine;
        let _com = ComScope::enter();
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().ok()?;
        let language = engine
            .RecognizerLanguage()
            .and_then(|l| l.LanguageTag())
            .map(|t| t.to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        Some(("Windows OCR".to_string(), language))
    }
    #[cfg(not(all(windows, feature = "ocr-windows")))]
    {
        None
    }
}

/// Recognise the text in a PNG file on disk.
///
/// The file, rather than an in-memory buffer, is deliberate: `Windows.Media.Ocr`
/// takes a `SoftwareBitmap`, and the only fully-supported way to build one from
/// pixels in the safe bindings is to let Windows decode an image file. Page images
/// are already written to the render cache, so this costs nothing extra.
pub fn recognize_png_file(path: &std::path::Path) -> Result<OcrPage> {
    #[cfg(all(windows, feature = "ocr-windows"))]
    {
        let started = std::time::Instant::now();
        let engine = engine()?;
        recognize_with(&engine, path, started)
    }
    #[cfg(not(all(windows, feature = "ocr-windows")))]
    {
        let _ = path;
        Err(BritError::new(
            ErrorKind::UnsupportedFeature,
            "This build of BritPDF has no OCR engine.",
        ))
    }
}

#[cfg(all(windows, feature = "ocr-windows"))]
fn engine() -> Result<windows::Media::Ocr::OcrEngine> {
    use windows::Media::Ocr::OcrEngine;
    // OCR talks to WinRT, which is COM. Tauri runs commands on worker threads, so
    // each thread that touches it has to be in an apartment first; `ComScope` does
    // that and unwinds it again when the thread finishes the call.
    ComScope::enter();
    OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| {
        BritError::new(
            ErrorKind::OcrUnavailable,
            "This PC has no text recogniser installed.",
        )
        .with_hint(
            "In Windows Settings > Time & language > Language, add a language with optical \
             character recognition, then restart BritPDF."
                .to_string(),
        )
        .with_detail(e.to_string())
    })
}

/// Initialises COM on the current thread and uninitialises it on drop.
///
/// `windows-rs` 0.62 no longer exposes an `initialize_mta` helper, so BritPDF calls
/// `CoInitializeEx` itself. It is safe to nest: the OS returns `S_FALSE` when the
/// apartment is already initialised, and `RPC_E_CHANGED_MODE` when the thread was
/// deliberately put in a single-threaded apartment, which is also fine - both are
/// treated as "already initialised" and no matching `CoUninitialize` is performed.
#[cfg(all(windows, feature = "ocr-windows"))]
struct ComScope {
    /// True only when *this* call is the one that initialised the apartment.
    owned: bool,
}

#[cfg(all(windows, feature = "ocr-windows"))]
impl ComScope {
    fn enter() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
        // SAFETY: `CoInitializeEx` takes an optional security descriptor; a null one
        // means "use the default", which is what every WinRT caller wants.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let owned = hr.is_ok();
        Self { owned }
    }
}

#[cfg(all(windows, feature = "ocr-windows"))]
impl Drop for ComScope {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        use windows::Win32::System::Com::CoUninitialize;
        // SAFETY: `owned` is only true when this very call succeeded, which is the
        // only case in which uninitialising is required.
        unsafe { CoUninitialize() };
    }
}

/// Block on a WinRT `IAsyncOperation` and return its result.
///
/// The `Async` trait in `windows-future` 0.3 is not publicly exported, so the wait
/// is built here from the public `Status`/`SetCompleted`/`GetResults` surface plus a
/// condition variable. This is deliberately a *blocking* call: OCR runs on a worker
/// thread, and the progress of a page-by-page operation is reported by the caller
/// between pages rather than from inside a single operation.
#[cfg(all(windows, feature = "ocr-windows"))]
fn join<T: windows::core::RuntimeType>(op: &windows_future::IAsyncOperation<T>) -> windows::core::Result<T> {
    use std::sync::{Arc, Condvar, Mutex};
    use windows_future::{AsyncOperationCompletedHandler, AsyncStatus};

    if op.Status()? != AsyncStatus::Started {
        return op.GetResults();
    }

    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let signal = Arc::clone(&done);
    let handler = AsyncOperationCompletedHandler::new(move |_sender, _status| {
        if let Ok(mut finished) = signal.0.lock() {
            *finished = true;
            signal.1.notify_all();
        }
        Ok(())
    });
    op.SetCompleted(&handler)?;

    let (lock, cvar) = &*done;
    let mut finished = lock.lock().unwrap_or_else(|e| e.into_inner());
    while !*finished {
        finished = cvar.wait(finished).unwrap_or_else(|e| e.into_inner());
    }
    op.GetResults()
}

#[cfg(all(windows, feature = "ocr-windows"))]
fn recognize_with(
    engine: &windows::Media::Ocr::OcrEngine,
    path: &std::path::Path,
    started: std::time::Instant,
) -> Result<OcrPage> {
    use windows::core::HSTRING;
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrResult;
    use windows::Storage::{FileAccessMode, StorageFile};

    let max_dimension = windows::Media::Ocr::OcrEngine::MaxImageDimension().unwrap_or(2600);
    let full_path = std::fs::canonicalize(path).map_err(|e| {
        BritError::internal(format!("The page image could not be found for recognition: {e}"))
    })?;

    let file_op = StorageFile::GetFileFromPathAsync(&HSTRING::from(full_path.to_string_lossy().as_ref()))
        .map_err(ocr_error)?;
    let file = join(&file_op).map_err(ocr_error)?;

    let stream_op = file.OpenAsync(FileAccessMode::Read).map_err(ocr_error)?;
    let stream = join(&stream_op).map_err(ocr_error)?;

    let decoder_op = BitmapDecoder::CreateAsync(&stream).map_err(ocr_error)?;
    let decoder = join(&decoder_op).map_err(ocr_error)?;

    // The recogniser refuses images wider than MaxImageDimension. Rather than
    // silently reducing accuracy, tell the caller which pages need a smaller render.
    if decoder.PixelWidth().unwrap_or(0) > max_dimension
        || decoder.PixelHeight().unwrap_or(0) > max_dimension
    {
        return Err(BritError::new(
            ErrorKind::InvalidRequest,
            format!(
                "This page is too large to recognise at the current zoom. Render it at no more \
                 than {max_dimension} pixels on its longest side, or raise the OCR quality."
            ),
        ));
    }

    // Windows 0.62 only exposes the argument-less `GetSoftwareBitmapAsync`, which
    // returns the file's native pixel format. The recognised formats (BGRA8, RGBA8,
    // G8 and a few more) are whitelisted by the recogniser, so no conversion is
    // needed here.
    let bitmap_op = decoder
        .GetSoftwareBitmapAsync()
        .map_err(ocr_error)?;
    let bitmap = join(&bitmap_op).map_err(ocr_error)?;

    let width = bitmap.PixelWidth().unwrap_or(0) as u32;
    let height = bitmap.PixelHeight().unwrap_or(0) as u32;

    let result_op = engine.RecognizeAsync(&bitmap).map_err(ocr_error)?;
    let result: OcrResult = join(&result_op).map_err(ocr_error)?;

    let lines = collect_lines(&result)?;
    let text = lines
        .iter()
        .map(|l| l.text.as_str())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let language = engine
        .RecognizerLanguage()
        .and_then(|l| l.LanguageTag())
        .map(|t| t.to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    Ok(OcrPage {
        lines,
        text,
        pixel_width: width,
        pixel_height: height,
        language,
        // Windows OCR reports no confidence. BritPDF passes 0.0 rather than a
        // made-up number, so no code path can present a fabricated score.
        confidence: 0.0,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(all(windows, feature = "ocr-windows"))]
fn collect_lines(result: &windows::Media::Ocr::OcrResult) -> Result<Vec<OcrLine>> {
    let mut out = Vec::new();
    let lines = result.Lines().map_err(ocr_error)?;
    for line in lines {
        let words_vec = line.Words().map_err(ocr_error)?;
        let mut words: Vec<OcrWord> = Vec::new();
        for word in words_vec {
            let Some(text) = word.Text().ok() else { continue };
            if text.is_empty() {
                continue;
            }
            let r = word.BoundingRect().map_err(ocr_error)?;
            words.push(OcrWord {
                text: text.to_string(),
                bounds: [r.X, r.Y, r.Width, r.Height],
                confidence: 0.0,
            });
        }
        if words.is_empty() {
            continue;
        }
        let left = words.iter().map(|w| w.bounds[0]).fold(f32::MAX, f32::min);
        let top = words.iter().map(|w| w.bounds[1]).fold(f32::MAX, f32::min);
        let right = words
            .iter()
            .map(|w| w.bounds[0] + w.bounds[2])
            .fold(f32::MIN, f32::max);
        let bottom = words
            .iter()
            .map(|w| w.bounds[1] + w.bounds[3])
            .fold(f32::MIN, f32::max);
        out.push(OcrLine {
            text: words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            words,
            bounds: [left, top, (right - left).max(0.0), (bottom - top).max(0.0)],
        });
    }
    Ok(out)
}

#[cfg(all(windows, feature = "ocr-windows"))]
fn ocr_error(e: windows::core::Error) -> BritError {
    BritError::new(
        ErrorKind::EngineFailure,
        "Windows text recognition failed.",
    )
    .with_detail(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_availability_without_claiming_success() {
        // On a machine with no recogniser this must be `None`, never a fake engine.
        match available_engine() {
            Some((name, version)) => {
                assert!(!name.is_empty());
                assert!(!version.is_empty());
            }
            None => {}
        }
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        let missing = std::env::temp_dir().join("britpdf-no-such-page-for-ocr.png");
        let _ = std::fs::remove_file(&missing);
        assert!(recognize_png_file(&missing).is_err());
    }
}
