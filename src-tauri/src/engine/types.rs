//! Serialisable data-transfer objects shared by the Rust engine and the React UI.
//!
//! The UI never touches PDFium or lopdf types directly; it only ever sees these
//! structs. This is the boundary that keeps React decoupled from PDF internals.

use serde::{Deserialize, Serialize};

/// A rectangle in **PDF points** (1/72 inch) unless stated otherwise.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self { left, top, right, bottom }
    }

    pub fn width(&self) -> f32 {
        (self.right - self.left).abs()
    }

    pub fn height(&self) -> f32 {
        (self.bottom - self.top).abs()
    }

    /// A small overlap test used to build search/selection rectangles.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.left < other.right
            && other.left < self.right
            && self.top < other.bottom
            && other.top < self.bottom
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PageOrientation {
    Portrait,
    Landscape,
}

/// Deliberately **not** `Copy`: `label` is a `String`, and cloning a whole page
/// geometry on every render request is a measurable cost on large documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageGeometry {
    /// Width in PDF points *after* applying the page's own /Rotate.
    pub width: f32,
    pub height: f32,
    /// Width in points ignoring rotation (the raw MediaBox width).
    pub raw_width: f32,
    pub raw_height: f32,
    /// The page's permanent /Rotate value, 0/90/180/270.
    pub rotation: u32,
    pub orientation: PageOrientation,
    /// The page label from the document's page-label tree, when present.
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
}

impl DocumentMetadata {
    pub fn empty() -> Self {
        Self {
            title: None,
            author: None,
            subject: None,
            keywords: None,
            creator: None,
            producer: None,
            creation_date: None,
            modification_date: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentInfo {
    pub id: String,
    pub file_name: String,
    /// `None` for documents that have never been saved to disk (blank/new).
    pub path: Option<String>,
    pub file_size: u64,
    pub page_count: usize,
    pub pdf_version: String,
    pub metadata: DocumentMetadata,
    pub is_encrypted: bool,
    pub is_modified: bool,
    /// Estimated memory footprint of the currently resident page cache, in bytes.
    pub cache_bytes: u64,
    /// A hint from a quick structural inspection, used to drive the scanned-PDF notice.
    pub has_text_layer: bool,
    pub page_label_first: Option<String>,
    pub supports_javascript: bool,
}

/// The kind of body text on a page, derived from the PDF text layer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PageContentKind {
    /// The page contains real, selectable, searchable text.
    Text,
    /// The page has no text layer: it is a scan or a pure image.
    ImageOnly,
    /// The page has very little text relative to its size - probably mostly imagery.
    MostlyImage,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageInfo {
    pub index: usize,
    pub geometry: PageGeometry,
    pub content_kind: PageContentKind,
    pub character_count: usize,
    pub annotation_count: usize,
    pub is_scanned_candidate: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderedPage {
    /// Absolute path to the rendered PNG.
    pub path: String,
    /// The PNG as a `data:image/png;base64,...` URL, so the webview can display
    /// the page without depending on the asset protocol or filesystem scope.
    pub data_url: Option<String>,
    pub width: u32,
    pub height: u32,
    /// Pixel size of the page *before* the render rotation, so the UI can lay out
    /// unscaled overlays correctly.
    pub unrotated_width: u32,
    pub unrotated_height: u32,
    pub render_rotation: u32,
    pub content_kind: PageContentKind,
    pub character_count: usize,
    pub annotation_count: usize,
    pub from_cache: bool,
}

/// A single character of the PDF text layer, with its tight bounding box.
/// Coordinates are in PDF points with the origin at the **top-left** of the page,
/// matching how the viewer canvas is laid out.
#[derive(Debug, Clone, Serialize)]
pub struct TextChar {
    pub c: String,
    pub rect: Rect,
    pub font_size: f32,
    pub font_is_serif: bool,
    pub font_is_italic: bool,
    pub font_is_bold: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageText {
    pub text: String,
    pub chars: Vec<TextChar>,
    pub content_kind: PageContentKind,
    pub is_scanned_candidate: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    pub page: usize,
    /// The matched text exactly as it appears on the page.
    pub text: String,
    /// One or more rectangles covering the match, in PDF points.
    pub rects: Vec<Rect>,
    /// Char offsets within `PageText.chars` for the page, used for highlighting.
    pub start_index: usize,
    pub end_index: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResults {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub matches: Vec<SearchMatch>,
    pub pages_searched: usize,
    pub truncated: bool,
    /// How long the search took, for the status bar.
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnnotationKind {
    Highlight,
    Underline,
    Strikeout,
    Squiggly,
    Freehand,
    Pen,
    TextBox,
    Rectangle,
    Circle,
    Arrow,
    Line,
    Triangle,
    Polygon,
    Cloud,
    Callout,
    StickyNote,
    Redaction,
    Stamp,
}

/// One annotation read back out of the working PDF, tagged with the page it
/// lives on (0-based, in the current page order).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageAnnotation {
    pub page: usize,
    pub spec: AnnotationSpec,
}

/// The complete description of an annotation to be written into the PDF.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationSpec {
    pub kind: AnnotationKind,
    pub page: usize,
    /// The annotation's outer rectangle in PDF points.
    pub rect: Rect,
    /// Stroke/drag path, in PDF points. Used by freehand, pen, line and arrow.
    #[serde(default)]
    pub points: Vec<Point>,
    /// Per-character or per-word quad points for text markup, in PDF points.
    #[serde(default)]
    pub quads: Vec<Rect>,
    /// Note body for sticky notes and text boxes.
    #[serde(default)]
    pub contents: Option<String>,
    /// Author recorded in the annotation's /T field.
    #[serde(default)]
    pub author: Option<String>,
    /// RGB 0..=1.
    #[serde(default = "default_color")]
    pub color: [f32; 3],
    #[serde(default = "default_thickness")]
    pub thickness: f32,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// For stamp annotations: a text stamp or a static label.
    #[serde(default)]
    pub label: Option<String>,
    /// Colour fill for shapes; opaque objects are stroked only.
    #[serde(default)]
    pub filled: bool,
    /// Rotation in degrees for stamp / free-text appearances.
    #[serde(default)]
    pub rotation: f32,
}

fn default_color() -> [f32; 3] {
    [1.0, 0.85, 0.2]
}
fn default_thickness() -> f32 {
    2.0
}
fn default_opacity() -> f32 {
    1.0
}
fn default_font_size() -> f32 {
    12.0
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An annotation read back out of a saved PDF, for listing/erasing existing notes.
#[derive(Debug, Clone, Serialize)]
pub struct ExistingAnnotation {
    /// The annotation's `/NM` name, which is how we identify it for deletion.
    pub name: String,
    pub page: usize,
    pub kind: Option<&'static str>,
    pub rect: Rect,
    pub contents: Option<String>,
    pub author: Option<String>,
    pub color: Option<[f32; 3]>,
    pub opacity: Option<f32>,
    pub width: Option<f32>,
    pub created: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageAnnotationList {
    pub page: usize,
    pub annotations: Vec<ExistingAnnotation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SaveResult {
    pub path: String,
    pub bytes_written: u64,
    /// True when the user picked a new destination ("Save As").
    pub renamed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RotationDegrees {
    Rotate90,
    Rotate180,
    Rotate270,
}

impl RotationDegrees {
    pub fn delta(self) -> i32 {
        match self {
            RotationDegrees::Rotate90 => 90,
            RotationDegrees::Rotate180 => 180,
            RotationDegrees::Rotate270 => 270,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PaperSize {
    A3,
    A4,
    A5,
    Letter,
    Legal,
    Tabloid,
    /// Match the first page of the source document.
    Auto,
    /// A custom size in points.
    Custom,
}

impl PaperSize {
    /// (width, height) in points for the portrait variant.
    pub fn points(self) -> Option<(f32, f32)> {
        match self {
            PaperSize::A3 => Some((841.89, 1190.55)),
            PaperSize::A4 => Some((595.28, 841.89)),
            PaperSize::A5 => Some((419.53, 595.28)),
            PaperSize::Letter => Some((612.0, 792.0)),
            PaperSize::Legal => Some((612.0, 1008.0)),
            PaperSize::Tabloid => Some((792.0, 1224.0)),
            PaperSize::Auto | PaperSize::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageOrientationRequest {
    Auto,
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    /// Keep the image's aspect ratio, letterboxing inside the page area.
    Fit,
    /// Stretch to fill the page area, ignoring aspect ratio.
    Fill,
    /// Place the image at its natural size, centred.
    Actual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarginMode {
    None,
    Small,
    Big,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageToPdfOptions {
    pub paper: PaperSize,
    pub orientation: PageOrientationRequest,
    pub margin: MarginMode,
    /// Margin in points, used when `margin == Custom`.
    #[serde(default)]
    pub custom_margin_points: f32,
    pub fit: ImageFit,
    /// Optional single image placed in the centre of every page.
    #[serde(default)]
    pub watermark_image_path: Option<String>,
    #[serde(default)]
    pub watermark_opacity: f32,
}

impl Default for ImageToPdfOptions {
    fn default() -> Self {
        Self {
            paper: PaperSize::A4,
            orientation: PageOrientationRequest::Auto,
            margin: MarginMode::Small,
            custom_margin_points: 36.0,
            fit: ImageFit::Fit,
            watermark_image_path: None,
            watermark_opacity: 0.3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompressionLevel {
    /// Re-pack object/xref streams only. Lossless: text stays text, images stay images.
    Lossless,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompressionEstimate {
    pub original_bytes: u64,
    pub estimated_output_bytes: u64,
    pub page_count: usize,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WatermarkPosition {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
    Tiled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatermarkOptions {
    pub text: Option<String>,
    pub image_path: Option<String>,
    pub position: WatermarkPosition,
    /// Font size in points for text watermarks.
    pub font_size: f32,
    pub color: [f32; 3],
    pub opacity: f32,
    pub rotation_degrees: f32,
    /// `None` means every page.
    pub pages: Option<Vec<usize>>,
    pub layer_above: bool,
}

impl Default for WatermarkOptions {
    fn default() -> Self {
        Self {
            text: Some("CONFIDENTIAL".to_string()),
            image_path: None,
            position: WatermarkPosition::MiddleCenter,
            font_size: 48.0,
            color: [0.5, 0.5, 0.5],
            opacity: 0.25,
            rotation_degrees: 45.0,
            pages: None,
            layer_above: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderFooterOptions {
    pub header_left: Option<String>,
    pub header_center: Option<String>,
    pub header_right: Option<String>,
    pub footer_left: Option<String>,
    pub footer_center: Option<String>,
    pub footer_right: Option<String>,
    pub use_page_number: bool,
    /// Show `Page X of Y` instead of just `X`.
    pub show_page_count: bool,
    pub date_format: String,
    pub font_size: f32,
    pub margin_points: f32,
    pub color: [f32; 3],
    pub pages: Option<Vec<usize>>,
}

impl Default for HeaderFooterOptions {
    fn default() -> Self {
        Self {
            header_left: None,
            header_center: None,
            header_right: None,
            footer_left: None,
            footer_center: None,
            footer_right: Some("Page ".to_string()),
            use_page_number: true,
            show_page_count: true,
            date_format: "%Y-%m-%d".to_string(),
            font_size: 9.0,
            margin_points: 24.0,
            color: [0.35, 0.35, 0.35],
            pages: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SecurityAlgorithm {
    /// AES-128 (PDF 2.0, revision 4).
    Aes128,
    /// AES-256 (PDF 2.0, revision 6) - the modern default.
    Aes256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityOptions {
    pub user_password: Option<String>,
    pub owner_password: Option<String>,
    pub algorithm: SecurityAlgorithm,
    pub allow_printing: bool,
    pub allow_high_resolution_printing: bool,
    pub allow_copying: bool,
    pub allow_modifying: bool,
    pub allow_annotations: bool,
    pub allow_form_filling: bool,
    pub allow_assembly: bool,
    pub allow_accessibility_extract: bool,
}

impl Default for SecurityOptions {
    fn default() -> Self {
        Self {
            user_password: None,
            owner_password: None,
            algorithm: SecurityAlgorithm::Aes256,
            allow_printing: true,
            allow_high_resolution_printing: true,
            allow_copying: true,
            allow_modifying: true,
            allow_annotations: true,
            allow_form_filling: true,
            allow_assembly: true,
            allow_accessibility_extract: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageOutputFormat {
    Png,
    Jpeg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfToImageOptions {
    pub pages: Option<Vec<usize>>,
    pub format: ImageOutputFormat,
    /// Rasterisation resolution.
    pub dpi: u32,
    pub jpeg_quality: u8,
    pub prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitMode {
    /// Explicit ranges, e.g. "1-3, 4, 7-9".
    Ranges,
    /// Every N pages.
    EveryN,
    /// One file per page.
    Individual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitOptions {
    pub mode: SplitMode,
    pub ranges: Option<String>,
    pub every_n: u32,
    /// Optional 1-based page labels, used when `ranges` is empty.
    pub pages: Option<Vec<usize>>,
}

impl Default for SplitOptions {
    fn default() -> Self {
        Self { mode: SplitMode::Ranges, ranges: Some("1-5".into()), every_n: 10, pages: None }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SplitResult {
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OcrLanguage {
    Auto,
    English,
    French,
    German,
    Spanish,
    Italian,
    Portuguese,
    Dutch,
    Polish,
    Russian,
    Japanese,
    Korean,
    ChineseSimplified,
    ChineseTraditional,
    Hindi,
    Arabic,
}

impl OcrLanguage {
    /// Windows BCP-47 language tag used by `Windows.Media.Ocr`.
    pub fn tag(self) -> &'static str {
        match self {
            OcrLanguage::Auto => "en-US",
            OcrLanguage::English => "en-US",
            OcrLanguage::French => "fr-FR",
            OcrLanguage::German => "de-DE",
            OcrLanguage::Spanish => "es-ES",
            OcrLanguage::Italian => "it-IT",
            OcrLanguage::Portuguese => "pt-BR",
            OcrLanguage::Dutch => "nl-NL",
            OcrLanguage::Polish => "pl-PL",
            OcrLanguage::Russian => "ru-RU",
            OcrLanguage::Japanese => "ja-JP",
            OcrLanguage::Korean => "ko-KR",
            OcrLanguage::ChineseSimplified => "zh-Hans-CN",
            OcrLanguage::ChineseTraditional => "zh-Hant-TW",
            OcrLanguage::Hindi => "hi-IN",
            OcrLanguage::Arabic => "ar-SA",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrOptions {
    pub language: OcrLanguage,
    pub pages: Option<Vec<usize>>,
    /// Rasterisation DPI used before recognition. Higher = more accurate, slower.
    pub dpi: u32,
    /// Write the recognised text as an invisible selectable text layer.
    pub add_invisible_text_layer: bool,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            language: OcrLanguage::Auto,
            pages: None,
            dpi: 200,
            add_invisible_text_layer: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OcrPageResult {
    pub page: usize,
    pub recognized_text: String,
    pub word_count: usize,
    pub line_count: usize,
    pub confidence_note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OcrReport {
    pub engine: String,
    pub engine_version: String,
    pub pages: Vec<OcrPageResult>,
    pub added_text_layer: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BatchOperation {
    Compress,
    Watermark,
    ImagesToPdf,
    Ocr,
    Merge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchOptions {
    pub operation: BatchOperation,
    pub files: Vec<String>,
    pub output_folder: String,
    pub suffix: String,
    pub compression: CompressionLevel,
    pub watermark: WatermarkOptions,
    pub image_options: ImageToPdfOptions,
    pub ocr: OcrOptions,
    pub overwrite: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchItemResult {
    pub source: String,
    pub output: Option<String>,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchResult {
    pub operation: BatchOperation,
    pub items: Vec<BatchItemResult>,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub elapsed_ms: u64,
}

/// Progress payload emitted to the UI while a long operation runs.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    /// Stable id so the UI can ignore events from superseded jobs.
    pub job_id: String,
    pub label: String,
    pub current: usize,
    pub total: usize,
    pub fraction: f64,
    pub cancellable: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrinterCapabilities {
    pub name: String,
    pub is_default: bool,
    pub is_pdf_printer: bool,
    pub paper_sizes: Vec<String>,
    pub supports_duplex: bool,
    pub supports_color: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrintPageSpec {
    /// 1-based page numbers, already expanded from the user's range expression.
    pub pages: Vec<usize>,
    pub copies: u32,
    /// `Auto` follows each page's own orientation.
    pub orientation: PageOrientationRequest,
    pub paper: PaperSize,
    /// Fit to the printable area, or print at the requested percentage.
    pub scaling: PrintScaling,
    pub color: bool,
    pub duplex: bool,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum PrintScaling {
    /// Shrink/grow each page to the whole printable area.
    FitToPage,
    /// Print at a percentage of the page's natural size.
    Percent { percent: u32 },
    /// Print at 100% of the page's natural size.
    ActualSize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrintResult {
    pub job_name: String,
    pub pages_sent: usize,
    pub copies: usize,
    pub sheets: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitOrientation {
    Portrait,
    Landscape,
}
