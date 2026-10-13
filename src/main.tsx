import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

// BritPDF is a Tauri desktop application: the UI drives a native Rust PDF
// engine over Tauri's IPC, which is only injected into the app's WebView. When
// the built frontend is served as a plain website (`__TAURI_INTERNALS__` is
// absent) those calls throw, so we show a notice instead of a broken editor.
const isTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function WebNotice() {
  return (
    <div className="flex h-full w-full items-center justify-center bg-slate-50 p-6 text-slate-700">
      <div className="max-w-md rounded-lg border border-slate-200 bg-white p-6 shadow-sm">
        <h1 className="text-lg font-semibold text-slate-800">
          BritPDF is a desktop application
        </h1>
        <p className="mt-2 text-sm text-slate-500">
          BritPDF's PDF engine runs natively on your computer, so the editor isn't
          available in a web browser. Install and launch the BritPDF desktop app to
          open, edit and print PDF files.
        </p>
      </div>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isTauri ? <App /> : <WebNotice />}</React.StrictMode>
);