# Third-party notices

BritPDF is distributed with the following third-party software. Each component
keeps its own licence, reproduced or referenced below.

## Runtime components bundled with the installer

### PDFium

* Source: <https://pdfium.googlesource.com/pdfium/>
* Licence: BSD 3-Clause
* Copyright: The Chromium Authors. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the conditions in the BSD 3-Clause
licence are met. The full text is in `src-tauri/vendor/pdfium/licenses/`.

BritPDF uses PDFium as its PDF parser and renderer. The build is the official
Chromium release binary for Windows x64 (ABI 7881), loaded at runtime through
`pdfium-render`. No PDFium source is included or linked statically.

### libjpeg-turbo, libpng, zlib, brotli, libwebp, openjpeg, libtiff, freetype

PDFium is built by the Chromium project and statically links the libraries above.
Their licences are reproduced in `src-tauri/vendor/pdfium/licenses/`, as required by
each licence. BritPDF neither modifies nor redistributes their source.

## Rust dependencies compiled into `britpdf.exe`

| Component | Licence |
| --- | --- |
| lopdf | MIT |
| pdfium-render | MIT OR Apache-2.0 |
| image | MIT OR Apache-2.0 |
| flate2 | MIT OR Apache-2.0 |
| miniz_oxide | MIT OR Zlib OR Apache-2.0 |
| chrono | MIT OR Apache-2.0 |
| serde, serde_json | MIT OR Apache-2.0 |
| uuid | MIT OR Apache-2.0 |
| base64 | MIT OR Apache-2.0 |
| dirs | MIT OR Apache-2.0 |
| thiserror | MIT OR Apache-2.0 |
| parking_lot | MIT OR Apache-2.0 |
| rayon | MIT OR Apache-2.0 |
| log | MIT OR Apache-2.0 |
| pdf | MIT OR Apache-2.0 |
| tauri | MIT OR Apache-2.0 |
| tauri-build, tauri-codegen, tauri-macros, tauri-runtime, tauri-utils | MIT OR Apache-2.0 |
| tauri-plugin-dialog, tauri-plugin-opener | MIT OR Apache-2.0 |
| serde_json, raw-window-handle, webview2-com, windows | MIT OR Apache-2.0 |

Full licence texts for the Rust ecosystem components are available in each
crate's own repository. MIT and Apache-2.0 both require that this notice be
retained, which is why this file ships with the installer.

## Frontend dependencies bundled into the web view

| Component | Licence |
| --- | --- |
| React, React DOM | MIT |
| Zustand | MIT |
| Vite | MIT |
| TypeScript | Apache-2.0 |
| Tailwind CSS | MIT |
| Vitest | MIT |
| @tauri-apps/api | MIT OR Apache-2.0 |
| @tauri-apps/plugin-dialog | MIT OR Apache-2.0 |
| lucide-react (icons) | ISC |

## Platform components used but not redistributed

### Windows.Media.Ocr

Optical character recognition uses `Windows.Media.Ocr`, a component of the
operating system already installed on the user's PC. BritPDF calls it through
the documented WinRT API; no Microsoft code is redistributed. Recognition runs
entirely on the local machine and no page image is sent anywhere.

### Microsoft Edge WebView2

The user interface is displayed in the Microsoft Edge WebView2 runtime, which
Windows provides. BritPDF does not redistribute it. If WebView2 is missing, the
installer offers to install the Evergreen runtime.

## Fonts

Documents created by BritPDF reference only the PDF base-14 fonts (Helvetica,
Times, Courier and Symbol). These are defined by the PDF specification itself and
are not embedded, so BritPDF ships no font files. When importing a font into a
document, that font is embedded in the user's own file and the licence terms of
that font apply to it.
