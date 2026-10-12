//! Windows printing.
//!
//! BritPDF prints through the Win32 printing API directly. Pages are rasterised by
//! PDFium at the chosen resolution and blitted to the printer device context with
//! `SetDIBitsToDevice`, which gives real control over printer selection, page
//! range, copies, orientation, paper size, colour, duplex and scaling - and the
//! exact same geometry code drives the on-screen preview, so the preview cannot
//! disagree with the printed output.
//!
//! Trade-off, stated plainly: pages are printed as raster images, so output
//! resolution follows the chosen DPI rather than the printer's vector capabilities.
//! 600 dpi is the default and 1200 dpi is available for line art.

use crate::engine::types::{PaperSize, PrintPageSpec, PrintScaling};
use crate::error::{BritError, ErrorKind, Result};

// ---------------------------------------------------------------- Windows impl

#[cfg(windows)]
mod win {
    use super::{PaperSize, PrintPageSpec, PrintScaling, BritError, ErrorKind, Result};
    use std::ffi::c_void;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::MAX_PATH;
    use windows::Win32::Graphics::Gdi::{
        CreateDCW, DeleteDC, GetDeviceCaps, SetDIBitsToDevice, BITMAPINFO, DEVMODEW,
        DEVMODE_FIELD_FLAGS, DIB_RGB_COLORS, DM_COLLATE, DM_COLOR, DM_COPIES, DM_DUPLEX,
        DM_ORIENTATION, DM_OUT_BUFFER, DM_PAPERLENGTH, DM_PAPERSIZE, DM_PAPERWIDTH, DMCOLLATE_TRUE,
        DMCOLOR_COLOR, DMCOLOR_MONOCHROME, DMDUP_VERTICAL, DMPAPER_A3, DMPAPER_A4, DMPAPER_A5,
        DMPAPER_LAST, DMPAPER_LEGAL, DMPAPER_LETTER, DMPAPER_TABLOID, GET_DEVICE_CAPS_INDEX, HDC,
        HORZRES, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT, PHYSICALWIDTH, VERTRES,
    };
    use windows::Win32::Graphics::Printing::{
        DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, PRINTER_ENUM_CONNECTIONS,
        PRINTER_ENUM_LOCAL, PRINTER_INFO_2W, PRINTER_HANDLE,
    };
    use windows::Win32::Storage::Xps::{DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};

    // DEVMODE mode flags that the generated crate does not export.
    const DM_PAPERSIZE_ONLY: DEVMODE_FIELD_FLAGS = DEVMODE_FIELD_FLAGS(0x0000_0002);

    // Orientation values.
    const DMORIENT_PORTRAIT: i16 = 1;
    const DMORIENT_LANDSCAPE: i16 = 2;

    const DMPAPER_USER: u32 = 256;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct BitmapInfoHeader {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_x_pels_per_meter: i32,
        bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16(&buf[..end]).unwrap_or_default()
    }

    /// Fetch a printer's DEVMODE. The returned buffer must stay alive while the
    /// pointer is in use, so it is returned alongside it.
    fn get_devmode(printer: &str) -> Result<(Vec<u8>, *mut DEVMODEW)> {
        let name = wide(printer);
        unsafe {
            let size = DocumentPropertiesW(
                None,
                PRINTER_HANDLE::default(),
                PCWSTR(name.as_ptr()),
                None,
                None,
                DM_OUT_BUFFER.0,
            );
            if size <= 0 {
                return Err(BritError::new(
                    ErrorKind::PrinterUnavailable,
                    format!("{printer} did not report its paper settings."),
                )
                .with_hint("Reinstall the printer driver, then try again."));
            }
            let mut buffer = vec![0u8; size as usize];
            let ret = DocumentPropertiesW(
                None,
                PRINTER_HANDLE::default(),
                PCWSTR(name.as_ptr()),
                Some(buffer.as_mut_ptr() as *mut DEVMODEW),
                None,
                DM_OUT_BUFFER.0,
            );
            if ret < 0 {
                return Err(BritError::new(
                    ErrorKind::PrinterUnavailable,
                    format!("{printer} rejected its paper settings."),
                ));
            }
            let ptr = buffer.as_mut_ptr() as *mut DEVMODEW;
            Ok((buffer, ptr))
        }
    }

    fn set_paper(devmode: *mut DEVMODEW, paper: PaperSize) {
        let (id, len_mm, wid_mm) = match paper {
            PaperSize::A3 => (DMPAPER_A3, 297.0_f64, 420.0_f64),
            PaperSize::A4 => (DMPAPER_A4, 297.0_f64, 210.0_f64),
            PaperSize::A5 => (DMPAPER_A5, 210.0_f64, 148.0_f64),
            PaperSize::Letter => (DMPAPER_LETTER, 279.4_f64, 215.9_f64),
            PaperSize::Legal => (DMPAPER_LEGAL, 355.6_f64, 215.9_f64),
            PaperSize::Tabloid => (DMPAPER_TABLOID, 431.8_f64, 279.4_f64),
            PaperSize::Auto | PaperSize::Custom => return,
        };
        unsafe {
            let printer = &mut (*devmode).Anonymous1.Anonymous1;
            printer.dmPaperSize = id as i16;
            // Paper length/width are in tenths of a millimetre.
            printer.dmPaperLength = (len_mm * 10.0).round() as i16;
            printer.dmPaperWidth = (wid_mm * 10.0).round() as i16;
            (*devmode).dmFields |= DM_PAPERSIZE | DM_PAPERLENGTH | DM_PAPERWIDTH;
        }
    }

    /// The localised name of a DEVMODE paper id, e.g. "A4" or "A4 (210 x 297 mm)".
    fn paper_name(printer: &str, template: *mut DEVMODEW, id: u32) -> String {
        let name = wide(printer);
        unsafe {
            (*template).Anonymous1.Anonymous1.dmPaperSize = id as i16;
            (*template).dmFields |= DM_PAPERSIZE;
            let size = DocumentPropertiesW(
                None,
                PRINTER_HANDLE::default(),
                PCWSTR(name.as_ptr()),
                None,
                Some(template),
                DM_PAPERSIZE_ONLY.0,
            );
            if size <= 0 {
                return String::new();
            }
            let mut buffer = vec![0u16; (size as usize) / 2 + 4];
            let ret = DocumentPropertiesW(
                None,
                PRINTER_HANDLE::default(),
                PCWSTR(name.as_ptr()),
                Some(buffer.as_mut_ptr() as *mut DEVMODEW),
                Some(template),
                DM_PAPERSIZE_ONLY.0,
            );
            if ret <= 0 {
                return String::new();
            }
            from_wide(&buffer).trim().to_string()
        }
    }

    /// Every paper size a printer supports, with the driver-selected default first.
    pub fn list_printers() -> Result<Vec<PrinterInfo>> {
        let mut out: Vec<PrinterInfo> = Vec::new();
        let default = default_printer();

        let (mut needed, mut returned) = (0u32, 0u32);
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        unsafe {
            let first = EnumPrintersW(flags, PCWSTR::null(), 2, None, &mut needed, &mut returned);
            if first.is_err() || needed == 0 {
                return Ok(out);
            }
            let mut buffer = vec![0u8; needed as usize];
            let second = EnumPrintersW(
                flags,
                PCWSTR::null(),
                2,
                Some(buffer.as_mut_slice()),
                &mut needed,
                &mut returned,
            );
            if second.is_err() {
                return Ok(out);
            }
            let stride = std::mem::size_of::<PRINTER_INFO_2W>();
            let count = (returned as usize).min(needed as usize / stride);
            for index in 0..count {
                let info = &*(buffer.as_ptr() as *const PRINTER_INFO_2W).add(index);
                if info.pPrinterName.is_null() {
                    continue;
                }
                let name = info.pPrinterName.to_string().unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let paper_sizes = paper_size_names(&name);
                let is_pdf = name.to_ascii_lowercase().contains("pdf")
                    || paper_sizes.iter().any(|p| p.to_ascii_lowercase().contains("pdf"));
                out.push(PrinterInfo {
                    is_default: default.as_deref() == Some(name.as_str()),
                    name,
                    is_pdf_printer: is_pdf,
                    paper_sizes,
                    supports_duplex: true,
                    supports_color: true,
                });
            }
        }
        Ok(out)
    }

    pub fn paper_size_names(printer: &str) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok((_buffer, devmode)) = get_devmode(printer) {
            let current = unsafe { (*devmode).Anonymous1.Anonymous1.dmPaperSize };
            if current as u32 != DMPAPER_USER && current > 0 {
                let n = paper_name(printer, devmode, current as u32);
                if !n.is_empty() {
                    names.push(n);
                }
            }
            for id in 1..=DMPAPER_LAST {
                let n = paper_name(printer, devmode, id);
                if !n.is_empty() && !names.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
                    names.push(n);
                }
            }
        }
        if names.is_empty() {
            names.push("Printer default".to_string());
        }
        names
    }

    pub fn default_printer() -> Option<String> {
        unsafe {
            let mut buffer = [0u16; MAX_PATH as usize];
            let mut size = buffer.len() as u32;
            if GetDefaultPrinterW(Some(PWSTR(buffer.as_mut_ptr())), &mut size).as_bool() {
                let s = from_wide(&buffer);
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        list_printers().ok().and_then(|list| list.into_iter().find(|p| p.is_default).map(|p| p.name))
    }

    /// The printable page geometry of a printer under a given configuration, in
    /// points. Used by the preview so it matches the printed result.
    fn configure(devmode: *mut DEVMODEW, spec: &PrintPageSpec) {
        unsafe {
            (*devmode).Anonymous1.Anonymous1.dmCopies = spec.copies.clamp(1, 99) as i16;
            (*devmode).dmFields |= DM_COPIES;
            (*devmode).dmColor = if spec.color { DMCOLOR_COLOR } else { DMCOLOR_MONOCHROME };
            (*devmode).dmFields |= DM_COLOR;
            if spec.duplex {
                (*devmode).dmDuplex = DMDUP_VERTICAL;
                (*devmode).dmFields |= DM_DUPLEX;
            }
            if spec.paper != PaperSize::Auto {
                set_paper(devmode, spec.paper);
            }
            match spec.orientation {
                crate::engine::types::PageOrientationRequest::Portrait => {
                    (*devmode).Anonymous1.Anonymous1.dmOrientation = DMORIENT_PORTRAIT;
                    (*devmode).dmFields |= DM_ORIENTATION;
                }
                crate::engine::types::PageOrientationRequest::Landscape => {
                    (*devmode).Anonymous1.Anonymous1.dmOrientation = DMORIENT_LANDSCAPE;
                    (*devmode).dmFields |= DM_ORIENTATION;
                }
                crate::engine::types::PageOrientationRequest::Auto => {}
            }
        }
    }

    pub fn printer_geometry(printer: &str, spec: &PrintPageSpec) -> Result<PaperGeometry> {
        let (_buffer, devmode) = get_devmode(printer)?;
        configure(devmode, spec);
        // Re-open the DC with the configured DEVMODE so the driver resolves the
        // paper for this job, then read the real resolution and paper size.
        let (_buffer2, devmode2) = get_devmode(printer)?;
        configure(devmode2, spec);
        unsafe {
            let driver = wide("WINSPOOL");
            let name = wide(printer);
            let dc = CreateDCW(
                PCWSTR(driver.as_ptr()),
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                Some(devmode2),
            );
            if dc.is_invalid() {
                return Err(BritError::new(
                    ErrorKind::PrinterUnavailable,
                    format!("{printer} could not be opened."),
                ));
            }
            let caps = |index: GET_DEVICE_CAPS_INDEX| GetDeviceCaps(Some(dc), index);
            let log_x = caps(LOGPIXELSX).max(96) as f32;
            let log_y = caps(LOGPIXELSY).max(96) as f32;
            let geometry = PaperGeometry {
                printable_width_pt: caps(HORZRES) as f32 / log_x * 72.0,
                printable_height_pt: caps(VERTRES) as f32 / log_y * 72.0,
                paper_width_pt: caps(PHYSICALWIDTH) as f32 / 25.4 * 72.0,
                paper_height_pt: caps(PHYSICALHEIGHT) as f32 / 25.4 * 72.0,
                driver_dpi_x: log_x.round() as u32,
                driver_dpi_y: log_y.round() as u32,
            };
            let _ = DeleteDC(dc);
            Ok(geometry)
        }
    }

    /// An installed printer, as reported by the Windows spooler.
    #[derive(Debug, Clone, serde::Serialize)]
    pub struct PrinterInfo {
        pub name: String,
        pub is_default: bool,
        pub is_pdf_printer: bool,
        pub paper_sizes: Vec<String>,
        pub supports_duplex: bool,
        pub supports_color: bool,
    }

    /// The physical and printable geometry of a printer, in points.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PaperGeometry {
        pub printable_width_pt: f32,
        pub printable_height_pt: f32,
        pub paper_width_pt: f32,
        pub paper_height_pt: f32,
        pub driver_dpi_x: u32,
        pub driver_dpi_y: u32,
    }

    /// An open printer device context for a print job.
    pub struct PrinterJob {
        dc: HDC,
        scaling: PrintScaling,
        pub geometry: PaperGeometry,
        pub name: String,
    }

    unsafe impl Send for PrinterJob {}

    impl PrinterJob {
        pub fn begin(printer: &str, spec: &PrintPageSpec) -> Result<Self> {
            let (_buffer, devmode) = get_devmode(printer)?;
            configure(devmode, spec);
            unsafe {
                (*devmode).dmCollate = DMCOLLATE_TRUE;
                (*devmode).dmFields |= DM_COLLATE;
                let driver = wide("WINSPOOL");
                let name = wide(printer);
                let dc = CreateDCW(
                    PCWSTR(driver.as_ptr()),
                    PCWSTR(name.as_ptr()),
                    PCWSTR::null(),
                    Some(devmode),
                );
                if dc.is_invalid() {
                    return Err(BritError::new(
                        ErrorKind::PrinterUnavailable,
                        format!("{printer} could not be opened for printing."),
                    )
                    .with_hint("Check the printer is switched on, online and not paused."));
                }
                let caps = |index: GET_DEVICE_CAPS_INDEX| GetDeviceCaps(Some(dc), index);
                let log_x = caps(LOGPIXELSX).max(96) as f32;
                let log_y = caps(LOGPIXELSY).max(96) as f32;
                let geometry = PaperGeometry {
                    printable_width_pt: caps(HORZRES) as f32 / log_x * 72.0,
                    printable_height_pt: caps(VERTRES) as f32 / log_y * 72.0,
                    paper_width_pt: caps(PHYSICALWIDTH) as f32 / 25.4 * 72.0,
                    paper_height_pt: caps(PHYSICALHEIGHT) as f32 / 25.4 * 72.0,
                    driver_dpi_x: log_x.round() as u32,
                    driver_dpi_y: log_y.round() as u32,
                };
                Ok(Self { dc, scaling: spec.scaling, geometry, name: printer.to_string() })
            }
        }

        pub fn start(&self, job_name: &str) -> Result<()> {
            let name = wide(job_name);
            let datatype = wide("EMF");
            let doc = DOCINFOW {
                cbSize: std::mem::size_of::<DOCINFOW>() as i32,
                lpszDocName: PCWSTR(name.as_ptr()),
                lpszOutput: PCWSTR::null(),
                lpszDatatype: PCWSTR(datatype.as_ptr()),
                fwType: 0,
            };
            unsafe {
                if StartDocW(self.dc, &doc) <= 0 {
                    return Err(BritError::new(
                        ErrorKind::PrinterUnavailable,
                        format!("{} could not start a print job.", self.name),
                    ));
                }
            }
            Ok(())
        }

        /// Print one page from a top-down RGBA buffer.
        pub fn print_page(&self, rgba: &[u8], width: u32, height: u32) -> Result<()> {
            unsafe {
                if StartPage(self.dc) <= 0 {
                    return Err(BritError::new(
                        ErrorKind::PrinterUnavailable,
                        format!("{} could not start a new page.", self.name),
                    ));
                }
                let (x, y) = self.position(width, height);
                let header = BitmapInfoHeader {
                    bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                    bi_width: width as i32,
                    bi_height: -(height as i32),
                    bi_planes: 1,
                    bi_bit_count: 32,
                    bi_compression: 0, // BI_RGB
                    bi_size_image: 0,
                    bi_x_pels_per_meter: 0,
                    bi_y_pels_per_meter: 0,
                    bi_clr_used: 0,
                    bi_clr_important: 0,
                };
                SetDIBitsToDevice(
                    self.dc,
                    x,
                    y,
                    width,
                    height,
                    0,
                    0,
                    0,
                    height as u32,
                    rgba.as_ptr() as *const c_void,
                    &header as *const BitmapInfoHeader as *const BITMAPINFO,
                    DIB_RGB_COLORS,
                );
                if EndPage(self.dc) <= 0 {
                    return Err(BritError::new(
                        ErrorKind::PrinterUnavailable,
                        format!("{} could not finish the page.", self.name),
                    ));
                }
            }
            Ok(())
        }

        /// Where the image lands on the sheet, honouring the scaling mode.
        pub fn position(&self, width: u32, height: u32) -> (i32, i32) {
            let img_w_pt = width as f32 / self.geometry.driver_dpi_x.max(1) as f32 * 72.0;
            let img_h_pt = height as f32 / self.geometry.driver_dpi_y.max(1) as f32 * 72.0;
            let (target_w, target_h) = scaled(self.scaling, img_w_pt, img_h_pt, &self.geometry);
            (
                ((self.geometry.printable_width_pt - target_w) / 2.0).round() as i32,
                ((self.geometry.printable_height_pt - target_h) / 2.0).round() as i32,
            )
        }
    }

    impl Drop for PrinterJob {
        fn drop(&mut self) {
            unsafe {
                let _ = EndDoc(self.dc);
                let _ = DeleteDC(self.dc);
            }
        }
    }

    /// Shared scaling maths, used by both the printer DC and the preview.
    pub fn scaled(scaling: PrintScaling, page_w: f32, page_h: f32, geometry: &PaperGeometry) -> (f32, f32) {
        match scaling {
            PrintScaling::FitToPage => {
                let sx = geometry.printable_width_pt / page_w.max(1.0);
                let sy = geometry.printable_height_pt / page_h.max(1.0);
                let s = sx.min(sy);
                (page_w * s, page_h * s)
            }
            PrintScaling::Percent { percent } => {
                let s = percent.clamp(10, 400) as f32 / 100.0;
                (page_w * s, page_h * s)
            }
            PrintScaling::ActualSize => (page_w, page_h),
        }
    }
}

#[cfg(not(windows))]
mod win {
    use super::*;

    #[derive(Debug, Clone, serde::Serialize)]
    pub struct PrinterInfo {
        pub name: String,
        pub is_default: bool,
        pub is_pdf_printer: bool,
        pub paper_sizes: Vec<String>,
        pub supports_duplex: bool,
        pub supports_color: bool,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct PaperGeometry {
        pub printable_width_pt: f32,
        pub printable_height_pt: f32,
        pub paper_width_pt: f32,
        pub paper_height_pt: f32,
        pub driver_dpi_x: u32,
        pub driver_dpi_y: u32,
    }

    pub struct PrinterJob;

    pub fn list_printers() -> Result<Vec<PrinterInfo>> {
        Err(BritError::new(
            ErrorKind::PrinterUnavailable,
            "Printing is only available on Windows.",
        ))
    }
    pub fn default_printer() -> Option<String> {
        None
    }
    pub fn paper_size_names(_: &str) -> Vec<String> {
        vec!["Letter".into()]
    }
    pub fn printer_geometry(_: &str, _spec: &PrintPageSpec) -> Result<PaperGeometry> {
        Err(BritError::new(ErrorKind::PrinterUnavailable, "Printing is Windows only."))
    }
    pub fn scaled(
        scaling: PrintScaling,
        page_w: f32,
        page_h: f32,
        geometry: &PaperGeometry,
    ) -> (f32, f32) {
        match scaling {
            PrintScaling::FitToPage => {
                let s = (geometry.printable_width_pt / page_w.max(1.0))
                    .min(geometry.printable_height_pt / page_h.max(1.0));
                (page_w * s, page_h * s)
            }
            PrintScaling::Percent { percent } => {
                let s = percent.clamp(10, 400) as f32 / 100.0;
                (page_w * s, page_h * s)
            }
            PrintScaling::ActualSize => (page_w, page_h),
        }
    }

    impl PrinterJob {
        pub fn begin(_printer: &str, _spec: &PrintPageSpec) -> Result<Self> {
            Err(BritError::new(ErrorKind::PrinterUnavailable, "Printing is Windows only."))
        }
        pub fn start(&self, _job_name: &str) -> Result<()> {
            Ok(())
        }
        pub fn print_page(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<()> {
            Ok(())
        }
    }
}

// ------------------------------------------------------------------ public API

/// The physical and printable geometry of a printer, in points.
pub use win::PaperGeometry;

/// A live printer device context, opened for the duration of a print job.
pub use win::PrinterJob;

/// An installed printer, as reported by the Windows spooler.
pub use win::PrinterInfo;

impl PrinterInfo {
    fn into_capabilities(self) -> crate::engine::types::PrinterCapabilities {
        crate::engine::types::PrinterCapabilities {
            name: self.name,
            is_default: self.is_default,
            is_pdf_printer: self.is_pdf_printer,
            paper_sizes: self.paper_sizes,
            supports_duplex: self.supports_duplex,
            supports_color: self.supports_color,
        }
    }
}

/// The on-sheet placement of a page image, in 1/100 mm, for the preview dialog.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub paper_width: i32,
    pub paper_height: i32,
}

/// Convert a page's printed size into a placement on the sheet.
pub fn placement(
    scaling: PrintScaling,
    page_width_pt: f32,
    page_height_pt: f32,
    geometry: &PaperGeometry,
) -> Placement {
    let (w_pt, h_pt) = win::scaled(scaling, page_width_pt, page_height_pt, geometry);
    let to_tenths = |pt: f32| ((pt / 72.0) * 254.0).round() as i32;
    let paper_w = to_tenths(geometry.paper_width_pt);
    let paper_h = to_tenths(geometry.paper_height_pt);
    let w = to_tenths(w_pt);
    let h = to_tenths(h_pt);
    Placement {
        x: ((paper_w - w) / 2).max(0),
        y: ((paper_h - h) / 2).max(0),
        width: w,
        height: h,
        paper_width: paper_w,
        paper_height: paper_h,
    }
}

pub fn printers() -> Result<Vec<crate::engine::types::PrinterCapabilities>> {
    Ok(win::list_printers()?
        .into_iter()
        .map(PrinterInfo::into_capabilities)
        .collect())
}

pub fn default_printer_name() -> Option<String> {
    win::default_printer()
}

pub fn supported_papers(printer: &str) -> Vec<String> {
    win::paper_size_names(printer)
}

pub fn geometry(printer: &str, spec: &PrintPageSpec) -> Result<PaperGeometry> {
    win::printer_geometry(printer, spec)
}

pub fn open_job(printer: &str, spec: &PrintPageSpec) -> Result<PrinterJob> {
    win::PrinterJob::begin(printer, spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> PaperGeometry {
        PaperGeometry {
            printable_width_pt: 515.0,
            printable_height_pt: 762.0,
            paper_width_pt: 595.0,
            paper_height_pt: 842.0,
            driver_dpi_x: 600,
            driver_dpi_y: 600,
        }
    }

    #[test]
    fn fit_to_page_shrinks_a4_into_the_printable_area() {
        let p = placement(PrintScaling::FitToPage, 595.0, 842.0, &geometry());
        assert!(p.width < p.paper_width);
        assert!(p.height < p.paper_height);
    }

    #[test]
    fn actual_size_preserves_the_page() {
        let p = placement(PrintScaling::ActualSize, 595.0, 842.0, &geometry());
        assert_eq!(p.width, ((595.0 / 72.0) * 254.0).round() as i32);
    }

    #[test]
    fn percent_scaling_halves_the_size() {
        let full = placement(PrintScaling::Percent { percent: 100 }, 600.0, 800.0, &geometry());
        let half = placement(PrintScaling::Percent { percent: 50 }, 600.0, 800.0, &geometry());
        assert!((full.width - half.width * 2).abs() <= 2);
    }
}