import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

interface Metadata {
  title: string | null;
  author: string | null;
  subject: string | null;
  keywords: string | null;
  creator: string | null;
  producer: string | null;
  creation_date: string | null;
  modification_date: string | null;
}

interface DocSummary {
  id: string;
  fileName: string;
  path: string | null;
  fileSize: number;
  pageCount: number;
  pdfVersion: string;
  isEncrypted: boolean;
  isModified: boolean;
  revision: number;
  metadata: Metadata;
  firstPageLabel: string | null;
  canUndo: boolean;
  canRedo: boolean;
  undoLabel: string | null;
  redoLabel: string | null;
}

interface PageGeometry {
  width: number;
  height: number;
  raw_width: number;
  raw_height: number;
  rotation: number;
  orientation: string;
  label: string | null;
}

interface RenderedPage {
  path: string;
  data_url: string | null;
  width: number;
  height: number;
  unrotated_width: number;
  unrotated_height: number;
  render_rotation: number;
  content_kind: string;
  character_count: number;
  annotation_count: number;
  from_cache: boolean;
}

interface PageText {
  text: string;
  contentKind: "text" | "imageOnly" | "mostlyImage";
  isScannedCandidate: boolean;
}

interface RecentFile {
  path: string;
  fileName: string;
  folder: string;
  lastOpened: number;
  pageCount: number | null;
  fileSize: number | null;
  thumbnail: string | null;
  missing: boolean;
}

interface BackendError {
  kind: string;
  headline: string;
  message: string;
  hint: string | null;
}

interface AppInfo {
  name: string;
  version: string;
  engine: string;
  engine_version: string;
  os: string;
  arch: string;
  data_dir: string;
}

interface SearchHit {
  page: number;
  snippet: string;
}

type ZoomMode = "fitWidth" | "fitPage" | "pct";

interface ZoomSpec {
  mode: ZoomMode;
  pct: number;
}

type ToolId =
  | "select"
  | "hand"
  | "text"
  | "typewriter"
  | "highlight"
  | "underline"
  | "strikeout"
  | "squiggly"
  | "draw"
  | "eraser"
  | "rect"
  | "ellipse"
  | "triangle"
  | "line"
  | "arrow"
  | "polygon"
  | "cloud"
  | "callout"
  | "stamp"
  | "signature"
  | "comment"
  | "distance"
  | "area"
  | "perimeter";

interface Size {
  w: number;
  h: number;
}

/** One annotation on the drawing overlay, stored in UNROTATED page coordinates
 * (y grows down, origin top-left of the page). */
interface Ann {
  id: string;
  page: number;
  kind: string;
  rect: { left: number; top: number; right: number; bottom: number };
  points: { x: number; y: number }[];
  quads: { left: number; top: number; right: number; bottom: number }[];
  color: string;
  thickness: number;
  opacity: number;
  font: string;
  fontSize: number;
  textStyle?: { bold: boolean; italic: boolean; underline: boolean; strike: boolean };
  label?: string;
  contents?: string;
  filled?: boolean;
  rotation?: number;
  created: number;
}

/** Wire format for the backend `apply_annotations` command. */
interface AnnotationSpec {
  kind: string;
  page: number;
  rect: { left: number; top: number; right: number; bottom: number };
  points: { x: number; y: number }[];
  quads: { left: number; top: number; right: number; bottom: number }[];
  color: number[];
  thickness: number;
  opacity: number;
  font_size: number;
  contents?: string;
  label?: string;
  filled?: boolean;
  rotation: number;
}

interface OutlineEntry {
  title: string;
  page: number;
  depth: number;
  resolvable: boolean;
  external: boolean;
}

type PageNumberKind = "topLeft" | "topCenter" | "topRight" | "bottomLeft" | "bottomCenter" | "bottomRight";
type PageNumberFmt = "plain" | "dashed" | "slashed" | "pageXofY";

interface HeaderFooterOptions {
  header_left: string | null;
  header_center: string | null;
  header_right: string | null;
  footer_left: string | null;
  footer_center: string | null;
  footer_right: string | null;
  use_page_number: boolean;
  show_page_count: boolean;
  date_format: string;
  font_size: number;
  margin_points: number;
  color: number[];
  pages: number[] | null;
}

interface WatermarkOptions {
  text: string | null;
  image_path: string | null;
  position: "topLeft" | "topCenter" | "topRight" | "middleLeft" | "middleCenter" | "middleRight" | "bottomLeft" | "bottomCenter" | "bottomRight" | "tiled";
  font_size: number;
  color: number[];
  opacity: number;
  rotation_degrees: number;
  pages: number[] | null;
  layer_above: boolean;
}

let annCounter = 0;
const nextAnnId = () => `ann-${Date.now().toString(36)}-${(annCounter++).toString(36)}`;

function hexToRgb01(hex: string): number[] {
  const m = /^#([0-9a-fA-F]{6})$/.exec(hex.trim());
  if (!m) return [1.0, 0.85, 0.2];
  const n = parseInt(m[1], 16);
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

function annToSpec(a: Ann): AnnotationSpec {
  return {
    kind: a.kind,
    page: a.page,
    rect: { left: a.rect.left, top: a.rect.top, right: a.rect.right, bottom: a.rect.bottom },
    points: a.points,
    quads: a.quads,
    color: hexToRgb01(a.color),
    thickness: a.thickness,
    opacity: a.opacity,
    font_size: a.fontSize,
    contents: a.contents,
    label: a.label,
    filled: a.filled,
    rotation: a.rotation ?? 0,
  };
}

interface PageAnnotation {
  page: number;
  spec: AnnotationSpec;
}

/** A right-click on a page: where it happened (page space + viewport) and any
 *  annotation that was under the cursor. Drives the PDF context menu. */
interface SheetContextReq {
  page: number;
  px: number;
  py: number;
  clientX: number;
  clientY: number;
  annId: string | null;
}

function rgb01ToHex(c: number[]): string {
  const ch = (v: number) =>
    Math.max(0, Math.min(255, Math.round((v ?? 0) * 255)))
      .toString(16)
      .padStart(2, "0");
  return `#${ch(c[0])}${ch(c[1])}${ch(c[2])}`;
}

function specToAnn(spec: AnnotationSpec, page: number): Ann {
  return {
    id: nextAnnId(),
    page,
    kind: spec.kind,
    rect: spec.rect,
    points: spec.points ?? [],
    quads: spec.quads ?? [],
    color: rgb01ToHex(spec.color),
    thickness: spec.thickness ?? 2,
    opacity: spec.opacity ?? 1,
    font: "Inter, system-ui, sans-serif",
    fontSize: spec.font_size ?? 12,
    label: spec.label,
    contents: spec.contents,
    filled: spec.filled,
    rotation: spec.rotation ?? 0,
    created: Date.now(),
  };
}

/** Deep-copy an annotation onto `page`, nudged by (dx, dy) and given a fresh id. */
function cloneAnn(a: Ann, page: number, dx: number, dy: number): Ann {
  return {
    ...a,
    id: nextAnnId(),
    page,
    created: Date.now(),
    rect: { left: a.rect.left + dx, top: a.rect.top + dy, right: a.rect.right + dx, bottom: a.rect.bottom + dy },
    points: a.points.map((p) => ({ x: p.x + dx, y: p.y + dy })),
    quads: a.quads.map((q) => ({ left: q.left + dx, top: q.top + dy, right: q.right + dx, bottom: q.bottom + dy })),
  };
}

const box = (l: number, t: number, r: number, b: number) => ({ left: l, top: t, right: r, bottom: b });
const boxFrom = (a: { x: number; y: number }, c: { x: number; y: number }) =>
  box(Math.min(a.x, c.x), Math.min(a.y, c.y), Math.max(a.x, c.x), Math.max(a.y, c.y));

/** Rotated-render geometry helpers. Annotations are kept in unrotated page
 * coordinates; `rotation` is the combined view + page rotation in degrees. */
function localToPage(lx: number, ly: number, pw: number, ph: number, rot: number, sw: number, sh: number) {
  const r = ((rot % 360) + 360) % 360;
  const u = lx / sw;
  const v = ly / sh;
  switch (r) {
    case 90: return { x: v * pw, y: (1 - u) * ph };
    case 180: return { x: (1 - u) * pw, y: (1 - v) * ph };
    case 270: return { x: (1 - v) * pw, y: u * ph };
    default: return { x: u * pw, y: v * ph };
  }
}
function pageToLocal(x: number, y: number, pw: number, ph: number, rot: number, sw: number, sh: number) {
  const r = ((rot % 360) + 360) % 360;
  const u = x / pw;
  const v = y / ph;
  switch (r) {
    case 90: return { x: (1 - v) * sw, y: u * sh };
    case 180: return { x: (1 - u) * sw, y: (1 - v) * sh };
    case 270: return { x: v * sw, y: (1 - u) * sh };
    default: return { x: u * sw, y: v * sh };
  }
}

function pt(f: { x: number; y: number }) {
  return `${f.x.toFixed(2)},${f.y.toFixed(2)}`;
}
const mkWavy = (a: { x: number; y: number }, b: { x: number; y: number }) => {
  const len = Math.max(0.001, b.x - a.x);
  const steps = Math.max(3, Math.round(len / 4));
  let d = `M ${pt(a)}`;
  for (let i = 0; i < steps; i++) {
    const t1 = a.x + (len * (i + 2)) / (2 * steps);
    const t2 = a.x + (len * (i + 1)) / steps;
    d += ` Q ${t1.toFixed(2)},${(a.y - 2).toFixed(2)} ${t2.toFixed(2)},${a.y}`;
  }
  return d;
};

const errorMessage = (e: unknown): string => {
  const err = e as Partial<BackendError>;
  if (err && typeof err.message === "string") {
    return err.hint ? `${err.message} ${err.hint}` : err.message;
  }
  return String(e);
};

const isPasswordRequired = (e: unknown): boolean =>
  (e as Partial<BackendError>)?.kind === "PasswordRequired";

const fmtBytes = (n: number): string => {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
};

/** Expand a human page-range expression ("1-3, 7") into 0-based indices. */
function parsePageRange(spec: string, pageCount: number): number[] {
  const out: number[] = [];
  for (const part of spec.split(",")) {
    const seg = part.trim().toLowerCase();
    if (!seg) continue;
    const m = /^(\d+)(?:\s*-\s*(\d+))?$/.exec(seg);
    if (!m) continue;
    const a = Number(m[1]);
    const b = m[2] ? Number(m[2]) : a;
    if (a < 1 || a > pageCount || b < a || b > pageCount) continue;
    for (let p = a; p <= b; p++) out.push(p - 1);
  }
  return out;
}

/* ------------------------------- icons ------------------------------- */

const ICONS: Record<string, ReactNode> = {
  open: (
    <>
      <path d="M2 4h3.4l1.2 1.7H13a1 1 0 0 1 1 1V11a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5" />
      <path d="M2 5v7a1 1 0 0 0 1 1h.5" />
    </>
  ),
  saveAs: (
    <>
      <path d="M4 2.5h5l3 3v8H4z" />
      <path d="M9 2.5V6h3" />
      <path d="M11.5 10.5H13M12.25 9.75v1.5" />
    </>
  ),
  save: (
    <>
      <rect x="2.5" y="2.5" width="11" height="11" rx="1" />
      <path d="M5 2.5v4h6v-4" />
      <path d="M4.8 9.5h6.4v4H4.8z" />
      <path d="M6.8 9.5v2.4" />
    </>
  ),
  print: (
    <>
      <path d="M5 2.5h6v3.5H5z" />
      <path d="M4.5 7.5h7a1 1 0 0 1 1 1v2.5H14a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1v-2.5a1 1 0 0 1 1-1z" />
      <path d="M5.5 10h5v3.5h-5z" />
    </>
  ),
  undo: (
    <>
      <path d="M9 4.5H4.5l2-2" />
      <path d="M4 5.5a5.5 5.5 0 1 0 5.5-3.5" />
    </>
  ),
  redo: (
    <>
      <path d="M7 4.5h4.5l-2-2" />
      <path d="M12 5.5a5.5 5.5 0 1 1-5.5-3.5" />
    </>
  ),
  cursor: (
    <>
      <path d="M5 2.5l6.5 5.8-4 .6 3.4 4-1.4 1.2-3.4-4-.7 4z" />
    </>
  ),
  hand: (
    <>
      <path d="M6.5 8V4a1 1 0 0 1 2 0v3.5" />
      <path d="M8.5 7.5V3a1 1 0 0 1 2 0v4.5" />
      <path d="M10.5 7.6V3.4a1 1 0 0 1 2 0V8" />
      <path d="M12.5 8.2V5.3a1 1 0 0 1 2 0V10c0 3.5-2.2 4.5-5 4.5-2.6 0-4.3-1.2-5.4-3L2.8 9a1 1 0 0 1 1.4-1.4l2.3 1.6" />
    </>
  ),
  text: (
    <>
      <path d="M8 3v10" />
      <path d="M4.5 5.5h7" />
      <path d="M6 12.5h4" />
    </>
  ),
  highlight: (
    <>
      <path d="M3 13h10" />
      <path d="M4.5 13V10h7v3" />
      <path d="M4.5 10L6 7h4l1.5 3" />
    </>
  ),
  draw: (
    <>
      <path d="M11.2 2.8l2 2-7 7H4.2v-2z" />
      <path d="M9.5 3.5l2 2" />
    </>
  ),
  comment: (
    <>
      <path d="M3 3.5h10a1 1 0 0 1 1 1V10a1 1 0 0 1-1 1H7.5L4.5 13.5V11H3a1 1 0 0 1-1-1V4.5a1 1 0 0 1 1-1z" />
    </>
  ),
  shape: (
    <>
      <rect x="2.5" y="2.5" width="11" height="11" rx="1" />
    </>
  ),
  signature: (
    <>
      <path d="M2.5 11c1.5-4.5 2.5-4 3-.5s1.2 3 2.3.7-.3-3 .5-2.8c.6.2 1 1.2 1.8.6.7-.5.5-1.5 1.4-1.5" />
      <path d="M13 9v4" />
    </>
  ),
  chevronLeft: <path d="M10 3.5L5.5 8l4.5 4.5" />,
  chevronRight: <path d="M6 3.5L10.5 8 6 12.5" />,
  chevronDown: <path d="M3.5 6L8 10.5 12.5 6" />,
  check: <path d="M3.5 8.5L6.5 11.5 12.5 5" />,
  close: <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" />,
  search: <><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5L13.5 13.5" /></>,
  minus: <path d="M3 8h10" />,
  plus: <path d="M8 3v10M3 8h10" />,
  rotate: (
    <>
      <path d="M13 7a5.5 5.5 0 1 0 .4 4.6" />
      <path d="M13.5 2.5v4.5H9" />
    </>
  ),
  duplicate: (
    <>
      <rect x="5.5" y="5.5" width="8" height="8" rx="1" />
      <path d="M3 10.5V3.5a1 1 0 0 1 1-1h7" />
    </>
  ),
  trash: (
    <>
      <path d="M3 4h10" />
      <path d="M6 4V2.5h4V4" />
      <path d="M4.5 4l.7 9.5h5.6l.7-9.5" />
      <path d="M6.8 7v4M9.2 7v4" />
    </>
  ),
  windowMin: <path d="M2.5 8h11" />,
  windowMax: <rect x="2.5" y="3.5" width="11" height="9" />,
  windowRestore: (
    <>
      <rect x="3" y="5.5" width="7.5" height="7.5" />
      <path d="M6 5.5v-2h7.5v7.5h-2" />
    </>
  ),
  windowClose: <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" />,
  fullscreen: (
    <>
      <path d="M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10" />
    </>
  ),
  exitFullscreen: (
    <>
      <path d="M6 2.5V6H2.5M10 6V2.5h3.5M13.5 10H10v3.5M6 13.5V10H2.5" />
    </>
  ),
  zoomIn: (
    <>
      <circle cx="7" cy="7" r="4.5" />
      <path d="M10.5 10.5L13.5 13.5M7 5v4M5 7h4" />
    </>
  ),
  zoomOut: (
    <>
      <circle cx="7" cy="7" r="4.5" />
      <path d="M10.5 10.5L13.5 13.5M5 7h4" />
    </>
  ),
  fitWidth: (
    <>
      <rect x="2" y="3" width="12" height="10" rx="1" />
      <path d="M5.5 6.5L4 8l1.5 1.5M10.5 6.5L12 8l-1.5 1.5" />
    </>
  ),
  fitPage: (
    <>
      <rect x="3.5" y="2" width="9" height="12" rx="1" />
      <path d="M6 6L5 8l1 2M10 6l1 2-1 2" />
    </>
  ),
  onePage: (
    <>
      <rect x="4.5" y="2.5" width="7" height="11" rx="1" />
    </>
  ),
  continuous: (
    <>
      <rect x="2" y="1.5" width="12" height="5.5" rx="1" />
      <rect x="2" y="9" width="12" height="5.5" rx="1" />
    </>
  ),
  facing: (
    <>
      <rect x="1.5" y="3" width="5.5" height="10" rx="1" />
      <rect x="9" y="3" width="5.5" height="10" rx="1" />
    </>
  ),
  sidebar: (
    <>
      <rect x="2" y="3" width="12" height="10" rx="1" />
      <path d="M6.5 3v10" />
    </>
  ),
  properties: (
    <>
      <rect x="2.5" y="2.5" width="11" height="11" rx="1" />
      <path d="M5.5 6h5M5.5 8.5h5M5.5 11h3" />
    </>
  ),
  find: (
    <>
      <circle cx="7" cy="7" r="4.5" />
      <path d="M10.5 10.5L13.5 13.5" />
    </>
  ),
  copy: (
    <>
      <rect x="5.5" y="5.5" width="8" height="8" rx="1" />
      <path d="M3 10.5V3.5a1 1 0 0 1 1-1h7" />
    </>
  ),
  organize: (
    <>
      <rect x="2.5" y="2.5" width="4.5" height="11" rx="1" />
      <rect x="9" y="2.5" width="4.5" height="11" rx="1" />
    </>
  ),
  share: (
    <>
      <circle cx="12" cy="4" r="2" />
      <circle cx="4" cy="8" r="2" />
      <circle cx="12" cy="12" r="2" />
      <path d="M5.8 7L10.2 5M5.8 9l4.4 2" />
    </>
  ),
  doc: (
    <>
      <path d="M4 2.5h5l3 3v8H4z" />
      <path d="M9 2.5V6h3" />
    </>
  ),

  /* ---- office-365 style icon set ---- */
  textBox: (
    <>
      <rect x="2.5" y="3.5" width="11" height="9" rx="1" />
      <path d="M5.5 6.5h5M5.5 8.5h5M5.5 10.5h3" />
    </>
  ),
  image: (
    <>
      <rect x="2.5" y="3.5" width="11" height="9" rx="1" />
      <circle cx="6" cy="6.6" r="1" />
      <path d="M2.9 11.5l3.4-3 2.3 2 2.5-2.6 3.4 3.6" />
    </>
  ),
  link: (
    <>
      <path d="M6.6 9.4a2.6 2.6 0 0 1 0-3.7l1.8-1.8a2.6 2.6 0 0 1 3.7 3.7l-.9.9" />
      <path d="M9.4 6.6a2.6 2.6 0 0 1 0 3.7l-1.8 1.8a2.6 2.6 0 0 1-3.7-3.7l.9-.9" />
    </>
  ),
  watermark: (
    <>
      <path d="M8 2.5c2.2 2.6 3.4 4.4 3.4 6a3.4 3.4 0 1 1-6.8 0c0-1.6 1.2-3.4 3.4-6z" />
      <path d="M2.5 12.5h11" />
    </>
  ),
  headerFooter: (
    <>
      <path d="M4 2.5h5l3 3v8H4z" />
      <path d="M9 2.5V6h3" />
      <path d="M6 8.5h4M6 11h4" />
    </>
  ),
  pageNumber: (
    <>
      <rect x="3.5" y="2.5" width="9" height="11" rx="1" />
      <path d="M6.4 5.6 5.6 10.4M8.6 5.6 7.8 10.4M6 8h3" />
    </>
  ),
  table: (
    <>
      <rect x="2.5" y="3.5" width="11" height="9" rx="1" />
      <path d="M2.5 6.5h11M2.5 9.5h11M6.2 3.5v9M9.8 3.5v9" />
    </>
  ),
  rect: <rect x="2.5" y="4" width="11" height="8" rx="0.5" />,
  ellipse: <ellipse cx="8" cy="8" rx="5.5" ry="4" />,
  line: <path d="M3 13L13 3" />,
  arrow: (
    <>
      <path d="M3 13L12 4" />
      <path d="M8.4 4H12v3.6" />
    </>
  ),
  triangle: <path d="M8 3.5l5 8.5H3z" />,
  stamp: (
    <>
      <path d="M6 6.5a2 2 0 1 1 4 0c0 1.2-.6 1.9-.6 2.6h-2.8c0-.7-.6-1.4-.6-2.6z" />
      <rect x="3.5" y="10.5" width="9" height="2" rx="0.5" />
    </>
  ),
  note: (
    <>
      <path d="M3.5 3.5h9v6l-3 3h-6z" />
      <path d="M12.5 9.5h-3v3" />
    </>
  ),
  callout: (
    <>
      <path d="M2.5 4.5h11v6h-6l-2.5 2.5v-2.5h-2.5z" />
      <path d="M5 7h6M5 8.8h4" />
    </>
  ),
  cloud: <path d="M4.6 11.5a2.6 2.6 0 0 1 .3-5.1 3.4 3.4 0 0 1 6.5.9 2.2 2.2 0 0 1-.4 4.2z" />,
  squiggly: <path d="M2.5 8c1-1.6 2-1.6 3 0s2 1.6 3 0 2-1.6 3 0" />,
  underline: (
    <>
      <path d="M5 2.5v5a3 3 0 0 0 6 0v-5" />
      <path d="M4 13.5h8" />
    </>
  ),
  strikeout: (
    <>
      <path d="M5.5 5.5c0-1.2 1-1.8 2.5-1.8s2.5.6 2.5 1.8" />
      <path d="M5 12c0 1.3 1.1 1.9 3 1.9s3-.6 3-1.9" />
      <path d="M2.5 8.5h11" />
    </>
  ),
  pencil: (
    <>
      <path d="M11.2 2.8l2 2-7.6 7.6-2.7.7.7-2.7z" />
      <path d="M9.8 4.2l2 2" />
    </>
  ),
  eraser: (
    <>
      <path d="M6.4 13.5l-3-3a1.4 1.4 0 0 1 0-2l5.1-5.1a1.4 1.4 0 0 1 2 0l3 3a1.4 1.4 0 0 1 0 2L10.3 13.5z" />
      <path d="M4.6 8.2l3.2 3.3" />
    </>
  ),
  ruler: (
    <>
      <rect x="2" y="5.5" width="12" height="5" rx="1" transform="rotate(-45 8 8)" />
      <path d="M4.6 6.2l1.2 1.2M6.9 3.9l1.2 1.2M9.2 6.2l1.2 1.2" />
    </>
  ),
  gridlines: (
    <>
      <rect x="2.5" y="3.5" width="11" height="9" rx="1" />
      <path d="M2.5 6.5h11M2.5 9.5h11M6.2 3.5v9M9.8 3.5v9" />
    </>
  ),
  bookmarks: (
    <>
      <path d="M4 2.5h3v11l-1.5-1.2L4 13.5z" />
      <path d="M9 2.5h3v11l-1.5-1.2L9 13.5z" />
    </>
  ),
  layers: (
    <>
      <path d="M8 2.5l5.5 3L8 8.5 2.5 5.5z" />
      <path d="M2.5 8.5L8 11.5l5.5-3" />
      <path d="M2.5 11.5L8 14.5l5.5-3" />
    </>
  ),
  newPage: (
    <>
      <path d="M4 2.5h5l3 3v8H4z" />
      <path d="M9 2.5V6h3" />
      <path d="M8 8.5v4M6 10.5h4" />
    </>
  ),
  insertPages: (
    <>
      <rect x="2.5" y="2.5" width="6.5" height="9" rx="1" />
      <path d="M11.5 4.5v7" />
      <path d="M9.5 9.5l2 2 2-2" />
    </>
  ),
  extract: (
    <>
      <rect x="7.5" y="2.5" width="6" height="8" rx="1" />
      <path d="M4 13.5h6" />
      <path d="M6.5 8.5V3M4.5 5l2-2 2 2" />
    </>
  ),
  area: (
    <>
      <path d="M3 12.5l2.5-6 4 2.5 3.5-4.5" />
      <path d="M9.5 4.5h3.5V8" />
    </>
  ),
  distance: (
    <>
      <path d="M2.5 8h11" />
      <path d="M4.5 5.5L2.5 8l2 2.5M11.5 5.5L13.5 8l-2 2.5" />
    </>
  ),
  perimeter: (
    <>
      <path d="M4 4.5h8v7H4z" />
      <path d="M2.5 2.5h3v4h-3zM13.5 9.5h-3v4h3z" />
    </>
  ),
  typewriter: (
    <>
      <rect x="2.5" y="4.5" width="11" height="6" rx="1" />
      <path d="M5 7.5h.01M7.5 7.5h.01M10 7.5h.01M5.5 10.5h5" />
    </>
  ),
  object: (
    <>
      <path d="M8 2.5l5 2.6v5.8L8 13.5 3 10.9V5.1z" />
      <path d="M3 5.1l5 2.6 5-2.6M8 7.7v5.8" />
    </>
  ),
  statusBar: (
    <>
      <rect x="2.5" y="3.5" width="11" height="9" rx="1" />
      <path d="M2.5 10h11" />
    </>
  ),
  ribbonUp: (
    <>
      <path d="M3.5 10l4.5-4.5 4.5 4.5" />
      <path d="M3.5 13h9" />
    </>
  ),
  ribbonDown: (
    <>
      <path d="M3.5 6.5l4.5 4.5 4.5-4.5" />
      <path d="M3.5 3.5h9" />
    </>
  ),
  newWindow: (
    <>
      <rect x="2.5" y="2.5" width="8" height="8" rx="1" />
      <path d="M6 13.5h7.5V6" />
    </>
  ),
  splitWindow: (
    <>
      <rect x="2.5" y="3" width="11" height="10" rx="1" />
      <path d="M8 3v10" />
    </>
  ),
  pageUp: (
    <>
      <path d="M8 13V3.5" />
      <path d="M4.5 7L8 3.5 11.5 7" />
    </>
  ),
  pageDown: (
    <>
      <path d="M8 3v9.5" />
      <path d="M4.5 9L8 12.5 11.5 9" />
    </>
  ),
  zoom100: (
    <>
      <circle cx="7" cy="7" r="4.2" />
      <path d="M10.2 10.2l3 3M5.6 7h2.8M7 5.6v2.8" />
    </>
  ),
  bold: <path d="M5.5 3.5h3a2 2 0 0 1 0 4h-3zM5.5 7.5h3.4a2.2 2.2 0 0 1 0 4.4H5.5z" />,
  italic: (
    <>
      <path d="M6.5 3.5h4M5.5 12.5h4" />
      <path d="M9 3.5l-2 9" />
    </>
  ),
  underlineIcon: (
    <>
      <path d="M5 2.5v5a3 3 0 0 0 6 0v-5" />
      <path d="M4 13.5h8" />
    </>
  ),
  strikeIcon: (
    <>
      <path d="M4.5 9h7" />
      <path d="M8 3.5c-1.8-.6-3.4.4-3.4 1.9 0 2.4 3.6 2 3.6 4.1 0 1.5-1.6 2.4-3.4 1.6" />
    </>
  ),
  fontColor: (
    <>
      <path d="M5 2.5h6M8 2.5v8M6.5 10.5h3" />
      <path d="M3.5 13.5h9" />
    </>
  ),
  alignLeft: (
    <>
      <path d="M3 4h10M3 7h6.5M3 10h10M3 13h6.5" />
    </>
  ),
  alignCenter: (
    <>
      <path d="M3 4h10M4.8 7h6.5M3 10h10M4.8 13h6.5" />
    </>
  ),
  alignRight: (
    <>
      <path d="M3 4h10M6.5 7h6.5M3 10h10M6.5 13h6.5" />
    </>
  ),
  commentsPane: (
    <>
      <rect x="2.5" y="3" width="11" height="8" rx="1" />
      <path d="M5 11v2.5l3-2.5" />
    </>
  ),
};

function Icon({ name, className }: { name: keyof typeof ICONS | string; className?: string }) {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className ?? "h-4 w-4"}
      aria-hidden="true"
    >
      {ICONS[name]}
    </svg>
  );
}

/* --------------------------- window helpers --------------------------- */

const win = () => getCurrentWindow();

async function toggleMaximize(setMax: (v: boolean) => void) {
  const w = win();
  try {
    if (await w.isMaximized()) {
      await w.unmaximize();
      setMax(false);
    } else {
      await w.maximize();
      setMax(true);
    }
  } catch {
    /* window API unavailable in browser test context */
  }
}

/* ------------------------------- sheets ------------------------------- */

function pageDisplaySize(geo: PageGeometry, zoom: ZoomSpec, cw: number, ch: number): Size {
  const pad = 44;
  const availW = Math.max(140, cw - pad);
  const availH = Math.max(140, ch - 100);
  if (zoom.mode === "fitWidth") {
    const w = availW;
    return { w, h: (geo.height / geo.width) * w };
  }
  if (zoom.mode === "fitPage") {
    const s = Math.min(availW / geo.width, availH / geo.height);
    return { w: geo.width * s, h: geo.height * s };
  }
  const w = (geo.width * 4 * zoom.pct) / 300;
  return { w, h: (geo.height / geo.width) * w };
}

interface AnnSettings {
  color: string;
  thickness: number;
  font: string;
  fontSize: number;
  bold: boolean;
  italic: boolean;
  underline: boolean;
  strike: boolean;
  stampLabel: string;
  calloutLabel: string;
}

const DEFAULT_ANN_SETTINGS: AnnSettings = {
  color: "#1667f4",
  thickness: 2,
  font: "Helvetica",
  fontSize: 12,
  bold: false,
  italic: false,
  underline: false,
  strike: false,
  stampLabel: "Approved",
  calloutLabel: "Note",
};

/** Custom pointer shown while the eraser tool is active. */
const ERASER_CURSOR = `url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='28' height='28' viewBox='0 0 28 28'%3E%3Cpath d='M5.5 17 14 8.5a2.2 2.2 0 0 1 3.1 0l4.4 4.4a2.2 2.2 0 0 1 0 3.1L14 23.5H9.4z' fill='%23ffffff' stroke='%23334155' stroke-width='1.6' stroke-linejoin='round'/%3E%3Cpath d='M11.5 23.5 7 19' stroke='%23334155' stroke-width='1.6'/%3E%3Cpath d='M12 11 19.5 18.5' stroke='%23f472b6' stroke-width='2.2'/%3E%3C/svg%3E") 5.5 21, auto`;

const DEFAULT_HF_OPTIONS: HeaderFooterOptions = {
  header_left: null,
  header_center: null,
  header_right: null,
  footer_left: null,
  footer_center: null,
  footer_right: "Page ",
  use_page_number: true,
  show_page_count: true,
  date_format: "%Y-%m-%d",
  font_size: 9,
  margin_points: 24,
  color: [0.35, 0.35, 0.35],
  pages: null,
};

interface SheetProps {
  index: number;
  geo: PageGeometry;
  size: Size;
  rotation: number;
  isVisible: boolean;
  id: string;
  revision: number;
  tool: ToolId;
  anns: Ann[];
  settings: AnnSettings;
  onCommitAnn: (page: number, ann: Ann) => void;
  onUpdateAnn: (page: number, ann: Ann) => void;
  onDeleteAnn: (page: number, annId: string) => void;
  selectedIds: string[];
  onSelectIds: (ids: string[]) => void;
  onContextMenu: (req: SheetContextReq) => void;
  onSelect: (page: number) => void;
  onToast: (msg: string) => void;
  onMeasure: (msg: string | null) => void;
  registerRef: (index: number, el: HTMLElement | null) => void;
}

type Draft =
  | { kind: "drag"; tool: ToolId; start: { x: number; y: number }; end: { x: number; y: number } }
  | { kind: "freehand"; points: { x: number; y: number }[] }
  | { kind: "poly"; preview: { x: number; y: number } };

const SHAPE_TOOLS: ToolId[] = ["rect", "ellipse", "triangle", "line", "arrow", "cloud", "callout"];
const CLICK_TOOLS: ToolId[] = ["polygon", "area", "perimeter"];

/** One annotation in sheet (local) coordinates, ready to draw. */
function annLocal(ann: Ann, pw: number, ph: number, rot: number, sw: number, sh: number) {
  const tl = (x: number, y: number) => pageToLocal(x, y, pw, ph, rot, sw, sh);
  const corners = (q: { left: number; top: number; right: number; bottom: number }) => {
    const c = [tl(q.left, q.top), tl(q.right, q.top), tl(q.right, q.bottom), tl(q.left, q.bottom)];
    return [
      Math.min(...c.map((p) => p.x)),
      Math.min(...c.map((p) => p.y)),
      Math.max(...c.map((p) => p.x)),
      Math.max(...c.map((p) => p.y)),
    ] as [number, number, number, number];
  };
  return { rect: box(...corners(ann.rect)), points: ann.points.map((p) => tl(p.x, p.y)), quads: ann.quads.map((q) => box(...corners(q))) };
}

function Sheet({
  index,
  geo,
  size,
  rotation,
  isVisible,
  id,
  revision,
  tool,
  anns,
  settings,
  onCommitAnn,
  onUpdateAnn,
  onDeleteAnn,
  selectedIds,
  onSelectIds,
  onContextMenu,
  onSelect,
  onToast,
  onMeasure,
  registerRef,
}: SheetProps) {
  const [rp, setRp] = useState<RenderedPage | null>(null);
  const [failed, setFailed] = useState(false);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [poly, setPoly] = useState<{ x: number; y: number }[]>([]);
  const [editing, setEditing] = useState<{ l: number; t: number; w: number; h: number; initial: string; editId?: string } | null>(null);
  const reqId = useRef(0);
  const overlayRef = useRef<HTMLDivElement>(null);
  const lastClick = useRef(0);
  const moveState = useRef<{ id: string; origin: { x: number; y: number }; rect: Ann["rect"]; points: { x: number; y: number }[] } | null>(null);
  const pageAnns = useMemo(() => anns.filter((a) => a.page === index), [anns, index]);

  const pw = geo.raw_width || geo.width;
  const ph = geo.raw_height || geo.height;
  const rot = ((rotation % 360) + 360) % 360;
  const scale = size.w / (rot % 180 === 90 ? ph : pw);

  const neededW = Math.max(120, Math.min(2600, Math.round(size.w * 2)));

  useEffect(() => {
    if (!isVisible || neededW <= 0) return;
    if (rp && Math.abs(rp.width - neededW) / neededW < 0.06 && rp.width !== 0) return;
    const token = ++reqId.current;
    let cancelled = false;
    invoke<RenderedPage>("render_page", { id, page: index, width: neededW, rotation: rot })
      .then((img) => {
        if (cancelled || token !== reqId.current) return;
        setRp(img);
        setFailed(false);
      })
      .catch((e) => {
        if (cancelled || token !== reqId.current) return;
        setFailed(true);
        onToast(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isVisible, neededW, id, index, revision, rot]);

  const localFromEvent = (e: { clientX: number; clientY: number }) => {
    const el = overlayRef.current;
    if (!el) return { x: 0, y: 0 };
    const r = el.getBoundingClientRect();
    const lx = e.clientX - r.left;
    const ly = e.clientY - r.top;
    return localToPage(lx, ly, pw, ph, rot, size.w, size.h);
  };

  const isShape = SHAPE_TOOLS.includes(tool);
  const isClickTool = CLICK_TOOLS.includes(tool);
  const isDrawMode =
    tool === "draw" || tool === "text" || tool === "typewriter" || tool === "highlight" || tool === "underline" ||
    tool === "strikeout" || tool === "squiggly" || isShape || tool === "stamp" || tool === "signature" ||
    tool === "comment" || tool === "distance" || isClickTool;
  const overlayActive = isDrawMode;
  const svgInteractive = tool === "select" || tool === "eraser";

  /* ------------------------------ drawing ------------------------------ */
  const beginDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const p = localFromEvent(e);
    overlayRef.current?.setPointerCapture(e.pointerId);
    if (tool === "draw") {
      setDraft({ kind: "freehand", points: [p] });
    } else {
      setDraft({ kind: "drag", tool, start: p, end: p });
    }
  };

  const moveDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!draft) return;
    if (draft.kind === "freehand") {
      const p = localFromEvent(e);
      setDraft({ kind: "freehand", points: [...draft.points, p] });
      return;
    }
    if (draft.kind === "drag") {
      const p = localFromEvent(e);
      setDraft({ kind: "drag", tool: draft.tool, start: draft.start, end: p });
      if (draft.tool === "distance") {
        const pxDist = Math.sqrt((p.x - draft.start.x) ** 2 + (p.y - draft.start.y) ** 2);
        onMeasure(`${(((pxDist * 25.4) / 72)).toFixed(2)} mm`);
      }
    }
  };

  const finishDrag = () => {
    if (!draft) return;
    if (draft.kind === "poly") {
      setDraft(null);
      return;
    }
    if (draft.kind === "freehand") {
      const pts = draft.points;
      if (pts.length < 2) {
        setDraft(null);
        return;
      }
      const b = boxFrom(pts[0], pts[pts.length - 1]);
      const pad = settings.thickness + 1;
      const padded = box(
        Math.max(0, b.left - pad),
        Math.max(0, b.top - pad),
        b.right + pad,
        b.bottom + pad
      );
      commit({
        kind: "freehand",
        rect: padded,
        points: pts,
        quads: [],
        color: settings.color,
        thickness: settings.thickness,
        opacity: 1,
        font: settings.font,
        fontSize: settings.fontSize,
      });
    } else {
      const a = draft.start;
      const b = draft.end;
      const boxed = boxFrom(a, b);
      switch (tool) {
        case "highlight":
        case "underline":
        case "strikeout":
        case "squiggly":
          commit({
            kind: tool,
            rect: boxed,
            points: [],
            quads: [boxed],
            color: settings.color.toLowerCase() === "transparent" ? "#fde047" : settings.color,
            thickness: Math.max(2, settings.thickness),
            opacity: 0.45,
            font: settings.font,
            fontSize: settings.fontSize,
          });
          break;
        case "text":
        case "typewriter": {
          const w = Math.max(60, boxed.right - boxed.left);
          const h = Math.max(settings.fontSize * 1.6, boxed.bottom - boxed.top);
          const rect = box(boxed.left, boxed.top, boxed.left + w, boxed.top + h);
          setEditing({ l: rect.left, t: rect.top, w: rect.right - rect.left, h: rect.bottom - rect.top, initial: "" });
          break;
        }
        case "distance":
          commitMeasure(a, b, "distance");
          break;
        case "comment": {
          setEditing({ l: boxed.left, t: boxed.top, w: 150, h: 60, initial: "" });
          break;
        }
        default: {
          if (isShape || tool === "stamp" || tool === "signature") {
            const kind =
              tool === "stamp" || tool === "signature" ? "stamp"
                : tool === "rect" ? "rectangle"
                  : tool === "ellipse" ? "circle"
                    : tool;
            if (tool === "callout") {
              setEditing({ l: boxed.left, t: boxed.top, w: 120, h: 40, initial: settings.calloutLabel });
            } else {
              commit({
                kind: kind === "stamp" ? "stamp" : kind,
                rect: boxed,
                points: [a, b],
                quads: [],
                color: kind === "stamp" ? "#e11d48" : settings.color,
                thickness: settings.thickness,
                opacity: kind === "stamp" ? 1 : 0.9,
                font: settings.font,
                fontSize: settings.fontSize,
                filled: false,
                ...(kind === "stamp" ? { label: tool === "signature" ? "Signature" : settings.stampLabel, filled: true } : {}),
              });
            }
          }
        }
      }
    }
    setDraft(null);
    onMeasure(null);
  };

  const commitMeasure = (a: { x: number; y: number }, b: { x: number; y: number }, mode: string) => {
    const pts = [a, b];
    const label = `${((Math.sqrt((b.x - a.x) ** 2 + (b.y - a.y) ** 2) * 25.4) / 72).toFixed(2)} mm`;
    commit({
      kind: mode === "distance" ? "arrow" : "polygon",
      rect: boxFrom(a, b),
      points: pts,
      quads: [],
      color: "#16a34a",
      thickness: 1.5,
      opacity: 1,
      font: settings.font,
      fontSize: Math.max(9, settings.fontSize - 2),
      label,
    });
  };

  const commit = (base: Omit<Ann, "id" | "page" | "created">) => {
    const full: Ann = { ...base, id: nextAnnId(), page: index, created: Date.now() };
    onCommitAnn(index, full);
  };

  /* polygon/area/perimeter: click to add vertices, double-click to finish */
  const onPolyClick = (p: { x: number; y: number }) => {
    if (!isClickTool) return;
    const now = Date.now();
    if (now - lastClick.current < 320 && poly.length >= 2) {
      const pts = [...poly, p];
      const xs = pts.map((q) => q.x);
      const ys = pts.map((q) => q.y);
      commit({
        kind: "polygon",
        rect: box(Math.min(...xs), Math.min(...ys), Math.max(...xs), Math.max(...ys)),
        points: pts,
        quads: [],
        color: "#16a34a",
        thickness: 1.5,
        opacity: 1,
        font: settings.font,
        fontSize: Math.max(9, settings.fontSize - 2),
        ...(tool === "area" || tool === "perimeter"
          ? (() => {
              let areaMM2 = 0;
              for (let i = 0; i < pts.length; i++) {
                const j = (i + 1) % pts.length;
                areaMM2 += pts[i].x * pts[j].y - pts[j].x * pts[i].y;
              }
              const area = Math.abs(areaMM2) / 2;
              const perim = pts.reduce((acc, q, i) => {
                const j = (i + 1) % pts.length;
                return acc + Math.sqrt((pts[j].x - q.x) ** 2 + (pts[j].y - q.y) ** 2);
              }, 0);
              const toMM = (v: number) => ((v * 25.4) / 72);
              const toMM2 = (v: number) => (v * Math.pow(25.4 / 72, 2));
              return {
                label: tool === "area" ? `${toMM2(area).toFixed(0)} mm²` : `${toMM(perim).toFixed(2)} mm`,
              };
            })()
          : { label: `${((bboxLen(poly, p) * 25.4) / 72).toFixed(2)} mm` }),
      });
      setPoly([]);
      setDraft(null);
      onMeasure(null);
      return;
    }
    lastClick.current = now;
    setPoly((prev) => [...prev, p]);
  };

  const bboxLen = (pts: { x: number; y: number }[], p: { x: number; y: number }) => {
    if (pts.length === 0) return 0;
    const a = pts[0];
    return Math.sqrt((p.x - a.x) ** 2 + (p.y - a.y) ** 2);
  };

  /** Hit-test a page point against committed text bearing annotations. */
  const textAnnAt = (p: { x: number; y: number }) => {
    for (let i = pageAnns.length - 1; i >= 0; i--) {
      const a = pageAnns[i];
      if (a.kind === "textBox" || a.kind === "stickyNote" || a.kind === "callout") {
        const r = a.rect;
        if (p.x >= r.left && p.x <= r.right && p.y >= r.top && p.y <= r.bottom) return a;
      }
    }
    return null;
  };

  /** Hit-test a page point against every annotation, topmost first. */
  const hitAnnAt = (p: { x: number; y: number }) => {
    for (let i = pageAnns.length - 1; i >= 0; i--) {
      const r = pageAnns[i].rect;
      if (p.x >= r.left && p.x <= r.right && p.y >= r.top && p.y <= r.bottom) return pageAnns[i];
    }
    return null;
  };

  /** Re-open the inline editor for an already committed annotation. */
  const openEditorFor = (a: Ann) => {
    const r = a.rect;
    const initial = a.kind === "callout" ? (a.label ?? "") : (a.contents ?? "");
    setEditing({
      l: r.left,
      t: r.top,
      w: Math.max(60, r.right - r.left),
      h: Math.max(28, r.bottom - r.top),
      initial,
      editId: a.id,
    });
  };

  /** Drag the whole annotation (its outline) to a new position. */
  const beginMove = (e: React.PointerEvent<SVGElement>, sel: Ann, el: SVGElement) => {
    e.stopPropagation();
    if (e.button !== 0) return;
    if (tool === "select") onSelectIds([sel.id]);
    try {
      el.setPointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    moveState.current = {
      id: sel.id,
      origin: localFromEvent(e),
      rect: { ...sel.rect },
      points: sel.points.map((p) => ({ x: p.x, y: p.y })),
    };
    const onMove = (ev: PointerEvent) => {
      const m = moveState.current;
      if (!m) return;
      const p = localFromEvent(ev);
      const w = m.rect.right - m.rect.left;
      const h = m.rect.bottom - m.rect.top;
      const left = Math.max(0, Math.min(Math.max(0, pw - w), m.rect.left + (p.x - m.origin.x)));
      const top = Math.max(0, Math.min(Math.max(0, ph - h), m.rect.top + (p.y - m.origin.y)));
      const dx = left - m.rect.left;
      const dy = top - m.rect.top;
      const base = pageAnns.find((a) => a.id === m.id);
      if (!base) return;
      onUpdateAnn(index, {
        ...base,
        rect: box(left, top, left + w, top + h),
        points: m.points.map((q) => ({ x: q.x + dx, y: q.y + dy })),
      });
    };
    const onUp = () => {
      moveState.current = null;
      el.removeEventListener("pointermove", onMove);
      el.removeEventListener("pointerup", onUp);
      el.removeEventListener("pointercancel", onUp);
    };
    el.addEventListener("pointermove", onMove);
    el.addEventListener("pointerup", onUp);
    el.addEventListener("pointercancel", onUp);
  };

  const closeEditor = (text: string) => {
    if (editing?.editId) {
      const base = pageAnns.find((a) => a.id === editing.editId);
      if (base) {
        if (text.trim().length === 0) {
          onDeleteAnn(index, base.id);
        } else {
          onUpdateAnn(index, base.kind === "callout" ? { ...base, label: text } : { ...base, contents: text });
        }
      }
      setEditing(null);
      return;
    }
    if (editing && text.trim().length > 0) {
      const l = editing.l;
      const t = editing.t;
      const w = editing.w;
      const h = editing.h;
      const rect = box(l, t, l + w, t + h);
      if (tool === "text" || tool === "typewriter") {
        commit({
          kind: "textBox",
          rect,
          points: [],
          quads: [],
          color: settings.color.toLowerCase() === "transparent" ? "#1e293b" : settings.color,
          thickness: 1,
          opacity: 1,
          font: settings.font,
          fontSize: settings.fontSize,
          contents: text,
          textStyle: { bold: settings.bold, italic: settings.italic, underline: settings.underline, strike: settings.strike },
        });
      } else if (tool === "comment") {
        commit({
          kind: "stickyNote",
          rect: { left: l, top: t, right: l + 24, bottom: t + 24 },
          points: [],
          quads: [],
          color: "#fde047",
          thickness: 1,
          opacity: 1,
          font: settings.font,
          fontSize: settings.fontSize,
          contents: text,
        });
      } else if (tool === "callout") {
        commit({
          kind: "callout",
          rect,
          points: [
            { x: l + w / 2, y: t + h },
            { x: l + w + 30, y: t + h + 80 },
          ],
          quads: [],
          color: "#1667f4",
          thickness: 1.5,
          opacity: 1,
          font: settings.font,
          fontSize: settings.fontSize,
          label: text,
        });
      }
    }
    setEditing(null);
  };

  /* ------------------------------ rendering ------------------------------ */

  return (
    <div
      ref={(el) => registerRef(index, el)}
      className="sheet relative"
      style={{ width: size.w, height: size.h, cursor: tool === "eraser" ? ERASER_CURSOR : undefined }}
      data-page={index}
      onClick={() => {
        if (!overlayActive && !svgInteractive) onSelect(index);
        if (tool === "select" || tool === "hand") onSelect(index);
        if (selectedIds.length > 0) onSelectIds([]);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        const p = localFromEvent(e);
        const hit = hitAnnAt(p);
        if (hit && !selectedIds.includes(hit.id)) onSelectIds([hit.id]);
        onContextMenu({ page: index, px: p.x, py: p.y, clientX: e.clientX, clientY: e.clientY, annId: hit ? hit.id : null });
      }}
    >
      {failed ? (
        <div className="flex h-full w-full items-center justify-center rounded-sm border border-slate-300 bg-white text-xs text-slate-400">
          Render failed
        </div>
      ) : rp?.data_url ? (
        <img
          src={rp.data_url}
          alt={`Page ${index + 1}`}
          draggable={false}
          className="h-full w-full rounded-sm bg-white object-fill shadow-[0_1px_3px_rgba(0,0,0,0.25),0_6px_16px_-4px_rgba(0,0,0,0.18)]"
          onError={() => onToast("The rendered page image failed to load.")}
        />
      ) : (
        <div className="flex h-full w-full items-center justify-center rounded-sm border border-slate-200 bg-white text-xs text-slate-300">
          <span>Loading…</span>
        </div>
      )}

      {/* annotation overlay */}
      <div
        ref={overlayRef}
        className={`absolute inset-0 ${overlayActive ? "touch-none" : "pointer-events-none"}`}
        style={{ cursor: overlayActive ? "crosshair" : undefined }}
        onPointerDown={(e) => {
          if (!overlayActive) return;
          if (e.button !== 0) return;
          if (isClickTool) {
            onPolyClick(localFromEvent(e));
            return;
          }
          if (tool === "typewriter" || tool === "text") {
            const existing = textAnnAt(localFromEvent(e));
            if (existing) {
              openEditorFor(existing);
              return;
            }
          }
          beginDrag(e);
        }}
        onPointerMove={(e) => {
          if (!overlayActive) return;
          if (isClickTool) {
            if (poly.length > 0) setDraft({ kind: "poly", preview: localFromEvent(e) });
            return;
          }
          moveDrag(e);
        }}
        onPointerUp={() => {
          if (overlayActive && !isClickTool) finishDrag();
        }}
        onPointerCancel={() => {
          setDraft(null);
          onMeasure(null);
        }}
        onDoubleClick={(e) => {
          if (isClickTool) {
            onPolyClick(localFromEvent(e));
            return;
          }
          const existing = textAnnAt(localFromEvent(e));
          if (existing) openEditorFor(existing);
        }}
      >
        <svg
          className="h-full w-full"
          viewBox={`0 0 ${size.w} ${size.h}`}
          style={{ pointerEvents: svgInteractive ? "auto" : "none" }}
        >
          {pageAnns.map((ann) => (
            <AnnGlyph
              key={ann.id}
              ann={ann}
              pw={pw}
              ph={ph}
              rot={rot}
              sw={size.w}
              sh={size.h}
              selected={selectedIds.includes(ann.id)}
              interactive={svgInteractive}
              labelIndex={pageAnns.findIndex((a) => a.id === ann.id)}
              onPointerDownHit={(e) => {
                e.stopPropagation();
                if (!svgInteractive) return;
                onSelect(index);
                if (tool === "select") beginMove(e, ann, e.currentTarget);
                else if (tool === "eraser") onDeleteAnn(index, ann.id);
              }}
              onHit={(e) => {
                e.stopPropagation();
                if (svgInteractive) {
                  onSelect(index);
                  if (tool === "eraser") onDeleteAnn(index, ann.id);
                }
              }}
              onEdit={() => openEditorFor(ann)}
            />
          ))}

          {/* draft preview images */}
          {draft?.kind === "freehand" && draft.points.length > 1 && (
            <polyline
              points={draft.points.map((p) => pt(pageToLocal(p.x, p.y, pw, ph, rot, size.w, size.h))).join(" ")}
              fill="none"
              stroke={settings.color}
              strokeWidth={settings.thickness}
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          )}
          {draft?.kind === "drag" && (isShape && draft.tool !== "callout") && (
            <BrowserShape
              tool={draft.tool}
              a={pageToLocal(draft.start.x, draft.start.y, pw, ph, rot, size.w, size.h)}
              b={pageToLocal(draft.end.x, draft.end.y, pw, ph, rot, size.w, size.h)}
              stroke={settings.color}
              thickness={settings.thickness}
              filled={false}
            />
          )}
          {draft?.kind === "drag" && draft.tool === "stamp" && (() => {
            const a = pageToLocal(draft.start.x, draft.start.y, pw, ph, rot, size.w, size.h);
            const b = pageToLocal(draft.end.x, draft.end.y, pw, ph, rot, size.w, size.h);
            const l = Math.min(a.x, b.x);
            const t = Math.min(a.y, b.y);
            const w = Math.max(1, Math.abs(b.x - a.x));
            const h = Math.max(1, Math.abs(b.y - a.y));
            return (
              <g>
                <rect x={l} y={t} width={w} height={h} rx={3} fill="#e11d48" fillOpacity={0.75} stroke="#e11d48" strokeWidth={1.5} />
                <text x={l + w / 2} y={t + h / 2} textAnchor="middle" fontSize={Math.min(h * 0.7, w / settings.stampLabel.length)} fill="#fff" fontWeight={700}>
                  {settings.stampLabel}
                </text>
              </g>
            );
          })()}
          {draft?.kind === "drag" && (draft.tool === "highlight" || draft.tool === "underline" || draft.tool === "strikeout" || draft.tool === "squiggly") && (() => {
            const a = pageToLocal(draft.start.x, draft.start.y, pw, ph, rot, size.w, size.h);
            const b = pageToLocal(draft.end.x, draft.end.y, pw, ph, rot, size.w, size.h);
            return (
              <rect
                x={Math.min(a.x, b.x)}
                y={Math.min(a.y, b.y)}
                width={Math.abs(b.x - a.x)}
                height={Math.abs(b.y - a.y)}
                fill={settings.color}
                opacity={0.35}
              />
            );
          })()}
          {draft?.kind === "drag" && tool === "distance" && (
            <MeasureLine from={draft.start} to={draft.end} pw={pw} ph={ph} rot={rot} sw={size.w} sh={size.h} />
          )}
          {draft?.kind === "drag" && (tool === "text" || tool === "typewriter") && (() => {
            const a = pageToLocal(draft.start.x, draft.start.y, pw, ph, rot, size.w, size.h);
            const b = pageToLocal(draft.end.x, draft.end.y, pw, ph, rot, size.w, size.h);
            return (
              <rect
                x={Math.min(a.x, b.x)}
                y={Math.min(a.y, b.y)}
                width={Math.max(10, Math.abs(b.x - a.x))}
                height={Math.max(10, Math.abs(b.y - a.y))}
                fill="#1667f4"
                fillOpacity={0.06}
                stroke="#1667f4"
                strokeWidth={1}
                strokeDasharray="3 2"
              />
            );
          })()}
          {draft?.kind === "poly" && isClickTool && poly.length > 0 && (
            <PreviewPoly
              vertices={[...poly, draft.preview]}
              pw={pw}
              ph={ph}
              rot={rot}
              sw={size.w}
              sh={size.h}
            />
          )}

          {/* selection chrome */}
          {selectedIds.length === 1 && tool === "select" && pageAnns.find((a) => a.id === selectedIds[0]) && (() => {
            const sel = pageAnns.find((a) => a.id === selectedIds[0])!;
            const l = annLocal(sel, pw, ph, rot, size.w, size.h).rect;
            return (
              <g className="pointer-events-auto">
                <rect
                  x={l.left}
                  y={l.top}
                  width={Math.max(1, l.right - l.left)}
                  height={Math.max(1, l.bottom - l.top)}
                  fill="transparent"
                  style={{ cursor: "move", pointerEvents: "all" }}
                  onPointerDown={(e) => beginMove(e, sel, e.currentTarget)}
                  onClick={(e) => e.stopPropagation()}
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    if (sel.kind === "textBox" || sel.kind === "stickyNote" || sel.kind === "callout") openEditorFor(sel);
                  }}
                />
                <rect x={l.left - 4} y={l.top - 4} width={l.right - l.left + 8} height={l.bottom - l.top + 8} fill="none" stroke="#1667f4" strokeWidth={1} strokeDasharray="4 3" />
                {[l.left, (l.left + l.right) / 2, l.right].map((x, i) => (
                  [l.top, l.bottom].map((y, j) => (
                    <rect key={`${i}-${j}`} x={x - 3} y={y - 3} width={6} height={6} fill="#fff" stroke="#1667f4" strokeWidth={1.2} />
                  ))
                ))}
                <g transform={`translate(${l.right - 6} ${l.top - 6})`}>
                  <circle r={5} fill="#ef4444" stroke="#fff" strokeWidth={1} onClick={(e) => { e.stopPropagation(); onDeleteAnn(index, sel.id); }} />
                  <path d="M-2.5 -2.5 L2.5 2.5 M2.5 -2.5 L-2.5 2.5" stroke="#fff" strokeWidth={1.4} />
                </g>
                {(sel.kind === "stamp" || sel.kind === "textBox") && (
                  <g transform={`translate(${(l.left + l.right) / 2} ${l.top - 18})`}>
                    <circle r={7} fill="#fff" stroke="#1667f4" strokeWidth={1.2} onClick={(e) => {
                      e.stopPropagation();
                      onUpdateAnn(index, { ...sel, rotation: ((sel.rotation ?? 0) + 90) % 360 });
                    }} />
                    <path d="M-1.5 -2 L1.5 1.5 M-1.5 -2 h3" stroke="#1667f4" strokeWidth={1.2} fill="none" />
                  </g>
                )}
              </g>
            );
          })()}
        </svg>

        {/* text / note editor */}
        {editing && (() => {
          const o = pageToLocal(editing.l, editing.t, pw, ph, rot, size.w, size.h);
          const base = editing.editId ? pageAnns.find((a) => a.id === editing.editId) : undefined;
          const fontSize = base?.fontSize ?? settings.fontSize;
          const style = base?.textStyle;
          const fontFamily = base?.font ?? settings.font;
          const bold = style ? style.bold : settings.bold;
          const italic = style ? style.italic : settings.italic;
          const underline = style ? style.underline : settings.underline;
          const strike = style ? !!style.strike : settings.strike;
          return (
            <div className="pointer-events-auto absolute z-10" style={{ left: o.x, top: o.y }}>
              <textarea
                autoFocus
                defaultValue={editing.initial}
                onBlur={(e) => closeEditor(e.target.value)}
                onPointerDown={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) closeEditor((e.target as HTMLTextAreaElement).value);
                  if (e.key === "Escape") setEditing(null);
                }}
                style={{
                  width: editing.w * scale,
                  height: editing.h * scale,
                  fontSize: fontSize * scale,
                  fontFamily,
                  fontWeight: bold ? 700 : 400,
                  fontStyle: italic ? "italic" : "normal",
                  textDecoration: `${underline ? "underline " : ""}${strike ? "line-through" : ""}`.trim() || undefined,
                }}
                className="resize rounded-sm border border-brand-400 bg-white/95 p-1 text-left text-slate-800 shadow outline-none"
                placeholder={tool === "comment" ? "Type a note…" : "Type text…"}
              />
            </div>
          );
        })()}

        {/* helper hint for click tools */}
        {isClickTool && (
          <div className="pointer-events-none absolute bottom-1.5 left-1/2 z-10 -translate-x-1/2 rounded bg-slate-800/80 px-2 py-0.5 text-[10px] text-white">
            {poly.length > 0 ? "Double-click to finish the shape" : "Click to add points · double-click to finish"}
          </div>
        )}
      </div>
    </div>
  );
}

function PreviewPoly({ vertices, pw, ph, rot, sw, sh }: { vertices: { x: number; y: number }[]; pw: number; ph: number; rot: number; sw: number; sh: number }) {
  const pts = vertices.map((v) => pageToLocal(v.x, v.y, pw, ph, rot, sw, sh));
  return (
    <g>
      <polygon points={pts.map(pt).join(" ")} fill="#16a34a" fillOpacity={0.08} stroke="#16a34a" strokeWidth={1.5} strokeDasharray="4 3" />
      {pts.map((p, i) => (
        <circle key={i} cx={p.x} cy={p.y} r={2.5} fill="#16a34a" />
      ))}
    </g>
  );
}

function MeasureLine({ from, to, pw, ph, rot, sw, sh }: { from: { x: number; y: number }; to: { x: number; y: number }; pw: number; ph: number; rot: number; sw: number; sh: number }) {
  const a = pageToLocal(from.x, from.y, pw, ph, rot, sw, sh);
  const b = pageToLocal(to.x, to.y, pw, ph, rot, sw, sh);
  const px = Math.sqrt((to.x - from.x) ** 2 + (to.y - from.y) ** 2);
  const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  return (
    <g>
      <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} stroke="#16a34a" strokeWidth={1.5} strokeDasharray="5 3" />
      <text x={mid.x} y={mid.y - 4} textAnchor="middle" fontSize={10} fill="#16a34a" fontFamily="Segoe UI, sans-serif">
        {(((px * 25.4) / 72)).toFixed(2)} mm
      </text>
    </g>
  );
}

/** A drag-preview shape in local coordinates (no rotation transform needed). */
function BrowserShape({ tool, a, b, stroke, thickness, filled }: { tool: string; a: { x: number; y: number }; b: { x: number; y: number }; stroke: string; thickness: number; filled: boolean }) {
  const l = Math.min(a.x, b.x);
  const t = Math.min(a.y, b.y);
  const w = Math.abs(b.x - a.x);
  const h = Math.abs(b.y - a.y);
  const fill = filled ? stroke : "none";
  const fillOpacity = filled ? 0.85 : 1;
  if (tool === "rect") return <rect x={l} y={t} width={w} height={h} fill={fill} fillOpacity={fillOpacity} stroke={stroke} strokeWidth={thickness} />;
  if (tool === "ellipse") {
    const cx = (a.x + b.x) / 2;
    const cy = (a.y + b.y) / 2;
    return <ellipse cx={cx} cy={cy} rx={w / 2} ry={h / 2} fill={fill} fillOpacity={fillOpacity} stroke={stroke} strokeWidth={thickness} />;
  }
  if (tool === "triangle") return <polygon points={`${(a.x + b.x) / 2},${t} ${b.x},${b.y} ${a.x},${b.y}`} fill={fill} fillOpacity={fillOpacity} stroke={stroke} strokeWidth={thickness} />;
  if (tool === "line") return <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} stroke={stroke} strokeWidth={thickness} />;
  if (tool === "arrow") {
    return (
      <g>
        <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} stroke={stroke} strokeWidth={thickness} />
        <ArrowHead a={a} b={b} color={stroke} />
      </g>
    );
  }
  if (tool === "cloud") return <CloudPath points={[a, { x: (a.x + b.x) / 2, y: a.y }, b, { x: (a.x + b.x) / 2, y: b.y }]} stroke={stroke} thickness={thickness} />;
  return <polygon points={`${a.x},${a.y} ${b.x},${a.y} ${b.x},${b.y} ${a.x},${b.y}`} fill="none" stroke={stroke} strokeWidth={thickness} />;
}

function ArrowHead({ a, b, color }: { a: { x: number; y: number }; b: { x: number; y: number }; color: string }) {
  const ang = Math.atan2(b.y - a.y, b.x - a.x);
  const s = 9;
  const p1 = { x: b.x - s * Math.cos(ang - 0.45), y: b.y - s * Math.sin(ang - 0.45) };
  const p2 = { x: b.x - s * Math.cos(ang + 0.45), y: b.y - s * Math.sin(ang + 0.45) };
  return <polygon points={`${pt(b)} ${pt(p1)} ${pt(p2)}`} fill={color} />;
}

function CloudPath({ points, stroke, thickness }: { points: { x: number; y: number }[]; stroke: string; thickness: number }) {
  let d = `M ${pt(points[0])}`;
  for (let i = 0; i < points.length; i++) {
    const p = points[i];
    const q = points[(i + 1) % points.length];
    const mx = (p.x + q.x) / 2;
    const my = (p.y + q.y) / 2;
    const dx = q.x - p.x;
    const dy = q.y - p.y;
    const len = Math.sqrt(dx * dx + dy * dy);
    const nx = -dy / (len || 1);
    const ny = dx / (len || 1);
    const r = Math.max(6, Math.min(16, len / 4));
    d += ` Q ${(p.x + nx * r).toFixed(2)},${(p.y + ny * r).toFixed(2)} ${mx.toFixed(2)},${my.toFixed(2)}`;
  }
  d += " Z";
  return <path d={d} fill="#fff" fillOpacity={0.6} stroke={stroke} strokeWidth={thickness} strokeLinejoin="round" />;
}

/** Renders one committed annotation into the overlay SVG. */
function AnnGlyph({
  ann,
  pw,
  ph,
  rot,
  sw,
  sh,
  selected,
  interactive,
  labelIndex,
  onHit,
  onPointerDownHit,
  onEdit,
}: {
  ann: Ann;
  pw: number;
  ph: number;
  rot: number;
  sw: number;
  sh: number;
  selected: boolean;
  interactive: boolean;
  labelIndex: number;
  onHit: (e: React.MouseEvent<SVGElement>) => void;
  onPointerDownHit?: (e: React.PointerEvent<SVGElement>) => void;
  onEdit?: () => void;
}) {
  const l = annLocal(ann, pw, ph, rot, sw, sh);
  const color = ann.color;
  const editable = ann.kind === "textBox" || ann.kind === "stickyNote" || ann.kind === "callout";
  const hit = {
    pointerEvents: interactive ? "auto" : undefined,
    onPointerDown: interactive && onPointerDownHit ? onPointerDownHit : interactive ? onHit : undefined,
    onClick: interactive ? onHit : undefined,
    onDoubleClick: interactive && editable && onEdit ? (e: React.MouseEvent<SVGElement>) => { e.stopPropagation(); onEdit(); } : undefined,
  };
  const ring = selected ? (
    <rect
      x={l.rect.left - 3}
      y={l.rect.top - 3}
      width={l.rect.right - l.rect.left + 6}
      height={l.rect.bottom - l.rect.top + 6}
      rx={2}
      fill="none"
      stroke="#1667f4"
      strokeWidth={1}
      strokeDasharray="4 3"
      pointerEvents="none"
    />
  ) : null;

  let node: ReactNode;
  switch (ann.kind) {
    case "highlight":
      node = (
        <g>
          {l.quads.map((q, i) => (
            <rect key={i} x={q.left} y={q.top} width={q.right - q.left} height={q.bottom - q.top} fill={color} opacity={0.4} {...hit} />
          ))}
        </g>
      );
      break;
    case "underline":
    case "strikeout":
      node = (
        <g>
          {l.quads.map((q, i) => (
            <g key={i} {...hit}>
              <rect x={q.left} y={q.top} width={q.right - q.left} height={q.bottom - q.top} fill={color} opacity={0.2} />
              <line
                x1={q.left}
                y1={ann.kind === "underline" ? q.bottom : (q.top + q.bottom) / 2}
                x2={q.right}
                y2={ann.kind === "underline" ? q.bottom : (q.top + q.bottom) / 2}
                stroke={color}
                strokeWidth={Math.max(1.5, ann.thickness * 0.8)}
              />
            </g>
          ))}
        </g>
      );
      break;
    case "squiggly":
      node = (
        <g>
          {l.quads.map((q, i) => (
            <g key={i} {...hit}>
              <path d={mkWavy({ x: q.left, y: q.bottom }, { x: q.right, y: q.bottom })} fill="none" stroke={color} strokeWidth={Math.max(1.5, ann.thickness * 0.8)} />
            </g>
          ))}
        </g>
      );
      break;
    case "freehand":
    case "pen":
      node = (
        <polyline
          points={l.points.map(pt).join(" ")}
          fill="none"
          stroke={color}
          strokeWidth={ann.thickness}
          strokeLinecap="round"
          strokeLinejoin="round"
          opacity={ann.opacity}
          {...hit}
        />
      );
      break;
    case "rectangle":
      node = <rect x={l.rect.left} y={l.rect.top} width={l.rect.right - l.rect.left} height={l.rect.bottom - l.rect.top} fill={ann.filled ? color : "none"} stroke={color} strokeWidth={ann.thickness} opacity={ann.opacity} {...hit} />;
      break;
    case "circle": {
      const cx = (l.rect.left + l.rect.right) / 2;
      const cy = (l.rect.top + l.rect.bottom) / 2;
      node = <ellipse cx={cx} cy={cy} rx={Math.max(1, (l.rect.right - l.rect.left) / 2)} ry={Math.max(1, (l.rect.bottom - l.rect.top) / 2)} fill={ann.filled ? color : "none"} stroke={color} strokeWidth={ann.thickness} opacity={ann.opacity} {...hit} />;
      break;
    }
    case "triangle":
      node = (
        <polygon
          points={`${((l.rect.left + l.rect.right) / 2).toFixed(2)},${l.rect.top} ${l.rect.right},${l.rect.bottom} ${l.rect.left},${l.rect.bottom}`}
          fill={ann.filled ? color : "none"}
          stroke={color}
          strokeWidth={ann.thickness}
          opacity={ann.opacity}
          {...hit}
        />
      );
      break;
    case "polygon":
      if (l.points.length >= 2) {
        node = <polygon points={l.points.map(pt).join(" ")} fill={ann.filled ? color : "none"} stroke={color} strokeWidth={ann.thickness} opacity={ann.opacity} strokeLinejoin="round" {...hit} />;
      } else {
        node = null;
      }
      break;
    case "cloud":
      if (l.points.length >= 2) {
        node = (
          <g fill="#fff" fillOpacity={0.55} {...hit}>
            <CloudPath points={l.points} stroke={color} thickness={ann.thickness} />
          </g>
        );
      } else {
        node = null;
      }
      break;
    case "line":
    case "arrow":
    case "rectangle-dotted":
      node = (
        <g opacity={ann.opacity}>
          {l.points.length >= 2 ? (
            <>
              <line x1={l.points[0].x} y1={l.points[0].y} x2={l.points[1].x} y2={l.points[1].y} stroke={color} strokeWidth={ann.thickness} {...hit} />
              {ann.kind === "arrow" && <ArrowHead a={l.points[0]} b={l.points[1]} color={ann.opacity < 1 ? "#16a34a" : color} />}
            </>
          ) : (
            <line x1={l.rect.left} y1={l.rect.top} x2={l.rect.right} y2={l.rect.bottom} stroke={color} strokeWidth={ann.thickness} {...hit} />
          )}
          {ann.label && (
            <text
              x={(l.points[0] ? (l.points[0].x + (l.points[1]?.x ?? l.rect.right)) / 2 : (l.rect.left + l.rect.right) / 2)}
              y={(l.points[0] ? (l.points[0].y + (l.points[1]?.y ?? l.rect.bottom)) / 2 : (l.rect.top + l.rect.bottom) / 2) - 4}
              textAnchor="middle"
              fontSize={ann.fontSize}
              fill="#16a34a"
              fontFamily="Segoe UI, sans-serif"
              fontWeight={600}
            >
              {ann.label}
            </text>
          )}
        </g>
      );
      break;
    case "callout": {
      const rect = l.rect;
      const p0 = l.points[0] ?? { x: rect.left, y: rect.top };
      const p1 = l.points[1] ?? { x: rect.right, y: rect.bottom };
      node = (
        <g>
          <rect x={rect.left} y={rect.top} width={rect.right - rect.left} height={rect.bottom - rect.top} rx={3} fill="#fff" stroke={color} strokeWidth={1.2} {...hit} />
          <line x1={p0.x} y1={p0.y} x2={p1.x} y2={p1.y} stroke={color} strokeWidth={1.2} strokeLinejoin="round" />
          <circle cx={p1.x} cy={p1.y} r={3} fill={color} />
          <text
            x={(rect.left + rect.right) / 2}
            y={(rect.top + rect.bottom) / 2 + ann.fontSize * 0.35}
            textAnchor="middle"
            fontSize={ann.fontSize}
            fill="#334155"
            fontFamily="Segoe UI, sans-serif"
          >
            {ann.label ?? ""}
          </text>
        </g>
      );
      break;
    }
    case "textBox": {
      const cx = (l.rect.left + l.rect.right) / 2;
      const cy = (l.rect.top + l.rect.bottom) / 2;
      const rotDeg = (((ann.rotation ?? 0) % 360) + 360) % 360;
      const textStyle = ann.textStyle ?? { bold: false, italic: false, underline: false, strike: false };
      node = (
        <g transform={`rotate(${rotDeg} ${cx} ${cy})`}>
          <rect x={l.rect.left} y={l.rect.top} width={l.rect.right - l.rect.left} height={l.rect.bottom - l.rect.top} fill="#fff" fillOpacity={0.08} stroke={color} strokeWidth={1} strokeDasharray="3 2" opacity={0.9} {...hit} />
          <text
            x={l.rect.left + 3}
            y={l.rect.top + (ann.fontSize || 12) * 0.8}
            fontSize={ann.fontSize ?? 12}
            fill={color}
            fontFamily={ann.font || "Helvetica"}
            fontWeight={textStyle.bold ? 700 : 400}
            fontStyle={textStyle.italic ? "italic" : "normal"}
            textDecoration={`${textStyle.underline ? "underline " : ""}${textStyle.strike ? "line-through" : ""}`.trim() || undefined}
          >
            {ann.contents ?? ""}
          </text>
        </g>
      );
      break;
    }
    case "stamp": {
      const cx = (l.rect.left + l.rect.right) / 2;
      const cy = (l.rect.top + l.rect.bottom) / 2;
      const rotDeg = (((ann.rotation ?? 0) % 360) + 360) % 360;
      const fontSize = ann.fontSize || Math.max(10, (l.rect.right - l.rect.left) / 10);
      node = (
        <g transform={`rotate(${rotDeg} ${cx} ${cy})`}>
          <rect x={l.rect.left} y={l.rect.top} width={Math.max(1, l.rect.right - l.rect.left)} height={Math.max(1, l.rect.bottom - l.rect.top)} rx={3} fill={color} fillOpacity={0.9} stroke={color} strokeWidth={1.4} {...hit} />
          <text
            x={cx}
            y={cy + fontSize * 0.35}
            textAnchor="middle"
            fontSize={fontSize}
            fill="#fff"
            fontWeight={700}
            fontFamily="Segoe UI, sans-serif"
            letterSpacing={1}
          >
            {ann.label ?? ""}
          </text>
        </g>
      );
      break;
    }
    case "stickyNote": {
      const n = labelIndex;
      node = (
        <g>
          <rect x={l.rect.left} y={l.rect.top} width={l.rect.right - l.rect.left} height={l.rect.bottom - l.rect.top} fill="#fde047" stroke="#ca8a04" strokeWidth={0.8} {...hit} />
          <path d={`M ${l.rect.right} ${l.rect.top} L ${l.rect.right} ${l.rect.top + Math.min(10, (l.rect.bottom - l.rect.top) * 0.35)} L ${l.rect.right - Math.min(10, l.rect.right - l.rect.left) * 0.35} ${l.rect.top} Z`} fill="#eab308" />
          <text x={l.rect.left + 5} y={l.rect.top + 15} fontSize={11} fill="#713f12" fontWeight={700} fontFamily="Segoe UI, sans-serif">
            {n + 1}
          </text>
        </g>
      );
      break;
    }
    default:
      node = <rect x={l.rect.left} y={l.rect.top} width={l.rect.right - l.rect.left} height={l.rect.bottom - l.rect.top} stroke={color} strokeWidth={1} fill="none" {...hit} />;
  }
  return <g>{ring}{node}</g>;
}

/* ----------------------------- title bar ----------------------------- */

interface TitleBarProps {
  appInfo: AppInfo | null;
  doc: DocSummary | null;
  dirty: boolean;
  searchRef: RefObject<HTMLInputElement>;
  searchQuery: string;
  searchStatus: "idle" | "searching" | "done" | "none";
  searchResults: SearchHit[];
  onSearchChange: (q: string) => void;
  onSearchEnter: () => void;
  onSearchHit: (hit: SearchHit) => void;
  onCloseSearch: () => void;
}

function TitleBar({
  appInfo,
  doc,
  dirty,
  searchRef,
  searchQuery,
  searchStatus,
  searchResults,
  onSearchChange,
  onSearchEnter,
  onSearchHit,
  onCloseSearch,
}: TitleBarProps) {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    void win()
      .isMaximized()
      .then((m) => setMaximized(m))
      .catch(() => setMaximized(false));
    return () => undefined;
  }, []);

  const searchActive = searchQuery.length > 0;

  return (
    <div className="relative z-30 flex h-[54px] shrink-0 items-center border-b border-slate-200 bg-white">
      <div
        data-tauri-drag-region
        className="absolute inset-0"
        title="Drag to move · double-click to maximize"
        onDoubleClick={() => void toggleMaximize(setMaximized)}
      />
      <div className="relative z-10 flex h-full shrink-0 items-center gap-2.5 px-3" data-tauri-drag-region>
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-gradient-to-br from-brand-500 to-brand-700 text-sm font-bold text-white shadow">
          B
        </div>
        <div className="flex flex-col leading-none" data-tauri-drag-region>
          <div className="flex items-center gap-1.5" data-tauri-drag-region>
            <span className="text-[12.5px] font-semibold tracking-tight text-slate-800">
              {appInfo?.name ?? "BritPDF"}
            </span>
            <span className="text-[10px] font-medium uppercase tracking-wide text-slate-400">
              {appInfo ? `v${appInfo.version}` : ""}
            </span>
          </div>
          <div className="mt-0.5 flex items-center gap-1.5" data-tauri-drag-region>
            <span className="max-w-[240px] truncate text-[11.5px] text-slate-500" title={doc?.path ?? undefined}>
              {doc ? doc.fileName : "No document open"}
            </span>
            {doc && (
              <span
                className={`h-2 w-2 shrink-0 rounded-full ${dirty ? "bg-amber-400" : "bg-emerald-400"}`}
                title={dirty ? "Unsaved changes" : "Saved"}
              />
            )}
          </div>
        </div>
      </div>

      <div className="relative z-10 mx-auto w-[440px] max-w-[45vw]">
        <div className="flex h-8 items-center gap-2 rounded-md border border-slate-300 bg-slate-50 px-2.5 focus-within:border-brand-400 focus-within:bg-white focus-within:ring-2 focus-within:ring-brand-100">
          <Icon name="search" className="h-3.5 w-3.5 text-slate-400" />
          <input
            ref={searchRef}
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") onSearchEnter();
              if (e.key === "Escape") onCloseSearch();
            }}
            placeholder="Search in document…"
            disabled={!doc}
            className="w-full bg-transparent text-[12.5px] text-slate-700 outline-none placeholder:text-slate-400 disabled:cursor-not-allowed"
          />
          {searchActive && (
            <button className="text-slate-400 hover:text-slate-600" onClick={onCloseSearch} title="Clear search">
              <Icon name="close" className="h-3.5 w-3.5" />
            </button>
          )}
        </div>

        {searchActive && searchStatus !== "idle" && searchStatus !== "searching" && (
          <div className="absolute left-0 right-0 top-9 max-h-[320px] overflow-auto rounded-md border border-slate-200 bg-white py-1 shadow-lg">
            {searchStatus === "none" ? (
              <p className="px-3 py-2 text-[12px] text-slate-500">
                No matches for “{searchQuery}”.
              </p>
            ) : (
              searchResults.map((r, i) => (
                <button
                  key={`${r.page}-${i}`}
                  className="block w-full px-3 py-1.5 text-left hover:bg-brand-50"
                  onClick={() => onSearchHit(r)}
                >
                  <span className="text-[11px] font-semibold text-brand-600">Page {r.page + 1}</span>
                  <span className="mt-0.5 block truncate text-[11.5px] text-slate-600">{r.snippet}</span>
                </button>
              ))
            )}
          </div>
        )}
      </div>

      <div className="relative z-10 ml-auto flex h-full shrink-0">
        <button
          className="flex h-8 w-10 items-center justify-center rounded-md text-slate-500 hover:bg-slate-100 active:bg-slate-200"
          onClick={() => {
            void win()
              .minimize()
              .catch(() => undefined);
          }}
          title="Minimize"
        >
          <Icon name="windowMin" className="h-3.5 w-3.5" />
        </button>
        <button
          className="flex h-8 w-10 items-center justify-center rounded-md text-slate-500 hover:bg-slate-100 active:bg-slate-200"
          onClick={() => void toggleMaximize(setMaximized)}
          title={maximized ? "Restore" : "Maximize"}
        >
          <Icon name={maximized ? "windowRestore" : "windowMax"} className="h-3.5 w-3.5" />
        </button>
        <button
          className="mr-1.5 flex h-8 w-10 items-center justify-center rounded-md text-slate-500 hover:bg-red-600 hover:text-white active:bg-red-700"
          onClick={() => {
            void win()
              .close()
              .catch(() => undefined);
          }}
          title="Close"
        >
          <Icon name="windowClose" className="h-3.5 w-3.5" />
        </button>
      </div>
    </div>
  );
}

/* ------------------------------ toolbar ------------------------------ */

type TabId = "home" | "insert" | "annotate" | "view";

const OMIT_GROUPS_KEY = "britpdf.omitRibbonGroups";

const TABS: { id: TabId; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "insert", label: "Insert" },
  { id: "annotate", label: "Annotate" },
  { id: "view", label: "View" },
];

type PageLayout = "onePage" | "continuous" | "facing";

interface ToolbarProps {
  doc: DocSummary | null;
  dirty: boolean;
  tool: ToolId;
  onTool: (t: ToolId) => void;
  tab: TabId;
  onTab: (t: TabId) => void;
  onOpen: () => void;
  onSave: () => void;
  onSaveAs: () => void;
  onPrint: () => void;
  onUndo: () => void;
  onRedo: () => void;
  thumbOpen: boolean;
  propsOpen: boolean;
  onToggleThumbs: () => void;
  onToggleProps: () => void;
  zoom: ZoomSpec;
  onZoom: (z: ZoomSpec) => void;
  current: number;
  onGoToPage: (p: number) => void;
  onPrevPage: () => void;
  onNextPage: () => void;
  layout: PageLayout;
  onLayout: (l: PageLayout) => void;
  fullscreen: boolean;
  onToggleFullscreen: () => void;
  onFocusSearch: () => void;
  onToast: (m: string) => void;
  settings: AnnSettings;
  onSettings: (s: Partial<AnnSettings>) => void;
  canUndo: boolean;
  canRedo: boolean;
  undoLabel: string | null;
  redoLabel: string | null;
  onInsertBlank: () => void;
  onInsertFromFile: () => void;
  onDuplicatePage: () => void;
  onDeletePage: () => void;
  onRotatePage: () => void;
  onExtractPages: () => void;
  onInsertImage: () => void;
  onHeaderFooter: () => void;
  onPageNumbers: () => void;
  onWatermark: () => void;
  onRotateView: () => void;
  onToggleBookmarks: () => void;
  onToggleLayers: () => void;
  onToggleNotes: () => void;
  onToggleStatusBar: () => void;
}

/** Assigns a popover anchor ref without fighting React's Ref variance. */
function assignRef<T extends HTMLElement>(ref: RefObject<T | null> | undefined, el: T | null) {
  if (ref) (ref as { current: T | null }).current = el;
}

/**
 * A labelled ribbon group. Everything sits on ONE horizontal row, sized so the
 * whole Home ribbon fits a 1280px window without scrolling — captions are pinned
 * to a fixed-height footer so every group lines up.
 */
function RibbonGroup({ label, title, children }: { label: string; title?: string; children: React.ReactNode }) {
  return (
    <div className="group/grp flex min-h-[64px] shrink-0 flex-col rounded-sm border-r border-slate-200/80 px-1 transition-colors hover:bg-slate-100/50" title={title}>
      <div className="flex h-[46px] items-stretch gap-0.5">{children}</div>
      <div className="flex h-[15px] items-center justify-center text-[9.5px] font-semibold uppercase tracking-wide text-slate-400">
        {label}
      </div>
    </div>
  );
}

/** Big stacked icon+caption button used inside ribbon groups. */
function RibbonButton({
  icon,
  label,
  onClick,
  active,
  disabled,
  title,
  badge,
}: {
  icon: string;
  label: string;
  onClick?: () => void;
  active?: boolean;
  disabled?: boolean;
  title?: string;
  badge?: string;
}) {
  return (
    <button
      className={`flex w-[48px] shrink-0 flex-col items-center justify-center gap-0.5 rounded px-0.5 transition-colors disabled:cursor-not-allowed disabled:opacity-35 ${
        active ? "bg-brand-50 text-brand-700 ring-1 ring-inset ring-brand-200" : "text-slate-600 hover:bg-slate-200/60"
      }`}
      onClick={onClick}
      disabled={disabled}
      title={title ?? label}
    >
      <span className="relative">
        <Icon name={icon} className="h-[18px] w-[18px]" />
        {badge && <span className="absolute -bottom-1.5 left-1/2 h-[3px] w-4 -translate-x-1/2 rounded-full" style={{ background: badge }} />}
      </span>
      <span className="max-w-[46px] truncate text-center text-[9.5px] font-medium leading-none">{label}</span>
    </button>
  );
}

/** Office "large" button: bigger glyph, used for the primary action of a group. */
function RibbonBigButton({
  icon,
  label,
  onClick,
  disabled,
  title,
  active,
}: {
  icon: string;
  label: string;
  onClick?: () => void;
  disabled?: boolean;
  title?: string;
  active?: boolean;
}) {
  return (
    <button
      className={`flex w-[62px] shrink-0 flex-col items-center justify-center gap-1 rounded px-1 transition-colors disabled:cursor-not-allowed disabled:opacity-35 ${
        active ? "bg-brand-50 text-brand-700 ring-1 ring-inset ring-brand-200" : "text-slate-700 hover:bg-slate-200/60"
      }`}
      onClick={onClick}
      disabled={disabled}
      title={title ?? label}
    >
      <Icon name={icon} className="h-[26px] w-[26px]" />
      <span className="max-w-[60px] truncate text-center text-[10px] font-semibold leading-none">{label}</span>
    </button>
  );
}

/**
 * Office split button: a wide action plus a narrow caret that opens a gallery.
 * `anchorRef` is attached to the caret so the popover can be positioned.
 */
function RibbonSplitButton({
  icon,
  label,
  onClick,
  onMenu,
  anchorRef,
  disabled,
  menuDisabled,
  title,
}: {
  icon: string;
  label: string;
  onClick?: () => void;
  onMenu?: () => void;
  anchorRef?: RefObject<HTMLButtonElement | null>;
  disabled?: boolean;
  menuDisabled?: boolean;
  title?: string;
}) {
  return (
    <div className="flex h-[42px] w-[62px] shrink-0 items-stretch overflow-hidden rounded border border-slate-300 bg-white transition-colors hover:bg-slate-100 disabled:opacity-35">
      <button
        className="flex flex-1 flex-col items-center justify-center gap-0.5 text-slate-700 disabled:cursor-not-allowed"
        onClick={onClick}
        disabled={disabled}
        title={title ?? label}
      >
        <Icon name={icon} className="h-[19px] w-[19px]" />
        <span className="max-w-[44px] truncate text-center text-[9.5px] font-medium leading-none">{label}</span>
      </button>
      <button
        ref={(el) => assignRef(anchorRef, el)}
        className="flex w-[13px] shrink-0 items-center justify-center border-l border-slate-300 text-slate-500 hover:bg-slate-200 disabled:cursor-not-allowed"
        onClick={onMenu}
        disabled={menuDisabled ?? (disabled || !onMenu)}
        title={`${label} options`}
      >
        <Icon name="chevronDown" className="h-3 w-3" />
      </button>
    </div>
  );
}

/** Office segmented (toggle) control. */
function RibbonSegmented({
  value,
  options,
  onChange,
  width = 46,
}: {
  value: string;
  options: { value: string; icon: string; label: string; title?: string }[];
  onChange: (v: string) => void;
  width?: number;
}) {
  return (
    <div className="flex h-[30px] shrink-0 items-stretch overflow-hidden rounded border border-slate-300 bg-white">
      {options.map((o, i) => (
        <button
          key={o.value}
          className={`flex shrink-0 items-center justify-center gap-1 px-1 text-slate-600 transition-colors hover:bg-slate-100 ${
            i > 0 ? "border-l border-slate-200" : ""
          } ${value === o.value ? "bg-brand-50 text-brand-700 ring-1 ring-inset ring-brand-200" : ""}`}
          style={{ width }}
          onClick={() => onChange(o.value)}
          title={o.title ?? o.label}
        >
          <Icon name={o.icon} className="h-[16px] w-[16px]" />
          <span className="text-[10px] font-medium leading-none">{o.label}</span>
        </button>
      ))}
    </div>
  );
}

/** Compact horizontal control (page box, font size, zoom stepper) inside a group. */
function RibbonField({
  children,
  title,
}: {
  children: ReactNode;
  title?: string;
}) {
  return (
    <div className="flex h-[30px] shrink-0 items-center justify-center rounded border border-slate-300 bg-white px-1" title={title}>
      {children}
    </div>
  );
}

/** Office-style icon button with a colour bar underneath (highlight / font colour). */
function RibbonSwatch({
  icon,
  label,
  color,
  anchorRef,
  onClick,
  onMenu,
  active,
  disabled,
  title,
}: {
  icon: string;
  label: string;
  color: string;
  anchorRef?: RefObject<HTMLButtonElement | null>;
  onClick?: () => void;
  onMenu?: () => void;
  active?: boolean;
  disabled?: boolean;
  title?: string;
}) {
  return (
    <div className="flex h-[42px] w-[52px] shrink-0 items-stretch overflow-hidden rounded border border-slate-300 bg-white transition-colors hover:bg-slate-100 disabled:opacity-35">
      <button
        className={`flex flex-1 flex-col items-center justify-center gap-0.5 text-slate-700 ${
          active ? "bg-brand-50 text-brand-700" : ""
        }`}
        onClick={onClick}
        disabled={disabled}
        title={title ?? label}
      >
        <Icon name={icon} className="h-[17px] w-[17px]" />
        <span className="max-w-[34px] truncate text-center text-[9px] font-medium leading-none">{label}</span>
      </button>
      <button
        ref={(el) => assignRef(anchorRef, el)}
        className="flex w-[13px] shrink-0 flex-col items-center justify-center gap-0.5 border-l border-slate-300 text-slate-500 hover:bg-slate-200"
        onClick={onMenu}
        disabled={disabled || !onMenu}
        title={`${label} colours`}
      >
        <Icon name="chevronDown" className="h-2.5 w-2.5" />
        <span className="h-[4px] w-[9px] rounded-sm" style={{ background: color }} />
      </button>
    </div>
  );
}

/**
 * Portal-rendered popover. Rendered into <body> so ribbon scroll containers and
 * group overflow can never clip a gallery or colour picker.
 */
function RibbonPopover({
  anchor,
  width = 240,
  onClose,
  children,
}: {
  anchor: RefObject<HTMLElement | null>;
  width?: number;
  onClose: () => void;
  children: ReactNode;
}) {
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);

  useLayoutEffect(() => {
    const el = anchor.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      top: Math.round(r.bottom + 3),
      left: Math.round(Math.max(6, Math.min(r.right - width, window.innerWidth - width - 6))),
    });
  }, [anchor, width]);

  if (!pos) return null;

  return createPortal(
    <>
      <div className="fixed inset-0 z-40" onClick={onClose} />
      <div
        className="fixed z-50 rounded-md border border-slate-200 bg-white py-1 shadow-xl"
        style={{ top: pos.top, left: pos.left, width }}
      >
        {children}
      </div>
    </>,
    document.body,
  );
}

/** Small label + grid entry used inside galleries. */
function GalleryItem({
  icon,
  label,
  onClick,
  disabled,
  active,
}: {
  icon: string;
  label: string;
  onClick?: () => void;
  disabled?: boolean;
  active?: boolean;
}) {
  return (
    <button
      className={`flex w-[72px] shrink-0 flex-col items-center gap-1 rounded p-1 transition-colors hover:bg-brand-50 ${
        active ? "bg-brand-50 text-brand-700" : "text-slate-600 hover:text-brand-700"
      } disabled:cursor-not-allowed disabled:opacity-35`}
      onClick={onClick}
      disabled={disabled}
      title={label}
    >
      <Icon name={icon} className="h-[20px] w-[20px]" />
      <span className="max-w-[68px] truncate text-center text-[9.5px] leading-none">{label}</span>
    </button>
  );
}

function RibbonMenuRow({
  icon,
  label,
  hint,
  onClick,
  disabled,
}: {
  icon?: string;
  label: string;
  hint?: string;
  onClick?: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-[12px] text-slate-700 transition-colors hover:bg-brand-50 disabled:cursor-not-allowed disabled:opacity-40"
      onClick={onClick}
      disabled={disabled}
    >
      {icon && <Icon name={icon} className="h-3.5 w-3.5 shrink-0 text-slate-500" />}
      <span className="flex-1 truncate">{label}</span>
      {hint && <span className="shrink-0 text-[10px] text-slate-400">{hint}</span>}
    </button>
  );
}

/** Description of one ribbon group; `key` is stable and used for hide/show. */
interface RibbonGroupSpec {
  key: string;
  label: string;
  title: string;
  content: ReactNode;
}

/**
 * The shared ribbon surface used by every tab: a single horizontal row of
 * groups, captions pinned to a fixed footer so they always line up, no wrapping
 * and no vertical stacking, plus the Office group show/hide menu.
 */
function RibbonSurface({
  groups,
  hidden,
  onToggleGroup,
}: {
  groups: RibbonGroupSpec[];
  hidden: string[];
  onToggleGroup: (key: string) => void;
}) {
  const menuBtn = useRef<HTMLButtonElement>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const visible = groups.filter((g) => !hidden.includes(g.key));

  return (
    <div className="relative">
      <div className="flex flex-wrap items-stretch gap-y-1 pl-1 pr-9 py-1">
        {visible.map((g) => (
          <RibbonGroup key={g.key} label={g.label} title={g.title}>
            {g.content}
          </RibbonGroup>
        ))}
      </div>

      <div className="absolute right-1 top-1">
        <button
          ref={menuBtn}
          className="flex h-7 w-7 items-center justify-center rounded text-slate-500 transition-colors hover:bg-slate-200/70"
          title="Customise the ribbon: show or hide groups"
          onClick={() => setMenuOpen((v) => !v)}
        >
          <Icon name="chevronDown" className="h-4 w-4" />
        </button>
        {menuOpen && (
          <RibbonPopover anchor={menuBtn} width={272} onClose={() => setMenuOpen(false)}>
            <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">
              Show ribbon groups
            </p>
            {groups.map((g) => {
              const on = !hidden.includes(g.key);
              return (
                <button
                  key={g.key}
                  className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-[12px] transition-colors hover:bg-brand-50"
                  onClick={() => onToggleGroup(g.key)}
                >
                  <span
                    className={`flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-[3px] border ${
                      on ? "border-brand-600 bg-brand-600 text-white" : "border-slate-300 bg-white"
                    }`}
                  >
                    {on && <Icon name="close" className="h-2.5 w-2.5 rotate-45" />}
                  </span>
                  <span className="flex-1 text-slate-700">{g.label}</span>
                  <span className="max-w-[120px] truncate text-[10px] text-slate-400">{g.title}</span>
                </button>
              );
            })}
          </RibbonPopover>
        )}
      </div>
    </div>
  );
}

function Toolbar({
  doc,
  dirty,
  tool,
  onTool,
  tab,
  onTab,
  onOpen,
  onSave,
  onSaveAs,
  onPrint,
  onUndo,
  onRedo,
  thumbOpen,
  propsOpen,
  onToggleThumbs,
  onToggleProps,
  zoom,
  onZoom,
  current,
  onGoToPage,
  onPrevPage,
  onNextPage,
  layout,
  onLayout,
  fullscreen,
  onToggleFullscreen,
  onFocusSearch,
  onToast,
  settings,
  onSettings,
  canUndo,
  canRedo,
  undoLabel,
  redoLabel,
  onInsertBlank,
  onInsertFromFile,
  onDuplicatePage,
  onDeletePage,
  onRotatePage,
  onExtractPages,
  onInsertImage,
  onHeaderFooter,
  onPageNumbers,
  onWatermark,
  onRotateView,
  onToggleBookmarks,
  onToggleLayers,
  onToggleNotes,
  onToggleStatusBar,
}: ToolbarProps) {
  const [fileOpen, setFileOpen] = useState(false);
  const hasDoc = doc !== null;

  // Which groups the user has hidden per tab via the ribbon group menu.
  const [hidden, setHidden] = useState<Record<string, string[]>>(() => {
    try {
      return JSON.parse(window.localStorage.getItem(OMIT_GROUPS_KEY) ?? "{}") as Record<string, string[]>;
    } catch {
      return {};
    }
  });
  const toggleGroup = (tabId: TabId, key: string) =>
    setHidden((prev) => {
      const cur = prev[tabId] ?? [];
      const next = { ...prev, [tabId]: cur.includes(key) ? cur.filter((k) => k !== key) : [...cur, key] };
      try {
        window.localStorage.setItem(OMIT_GROUPS_KEY, JSON.stringify(next));
      } catch {
        /* ignore quota / private-mode failures */
      }
      return next;
    });

  const [ribbonExpanded, setRibbonExpanded] = useState(true);
  const [popover, setPopover] = useState<string | null>(null);
  const shapesBtn = useRef<HTMLButtonElement>(null);
  const hlBtn = useRef<HTMLButtonElement>(null);
  const penBtn = useRef<HTMLButtonElement>(null);
  const pagesBtn = useRef<HTMLButtonElement>(null);
  const hfBtn = useRef<HTMLButtonElement>(null);
  const winBtn = useRef<HTMLButtonElement>(null);
  const annBtn = useRef<HTMLButtonElement>(null);
  const fileBtn = useRef<HTMLButtonElement>(null);
  const fontColorBtn = useRef<HTMLButtonElement>(null);
  const fontFamilyBtn = useRef<HTMLButtonElement>(null);
  const fontSizeBtn = useRef<HTMLButtonElement>(null);
  const penSizeBtn = useRef<HTMLButtonElement>(null);

  const fileItem =
    "flex w-full items-center gap-2 px-3 py-1.5 text-left text-[12px] hover:bg-brand-50 disabled:opacity-40";

  const action = (t: ToolId) => () => onTool(t);
  const soon = (what: string) => () => onToast(`${what} is coming next in the build.`);

  const togglePop = (id: string) => setPopover((p) => (p === id ? null : id));

  const PEN_SIZES = [1, 2, 3, 5, 8];

  /** Pencil size picker, shared by the Home and Annotate ribbons. */
  const penSizeField = (
    <RibbonField title="Pencil line thickness">
      <Icon name="pencil" className="mr-1 h-3.5 w-3.5 shrink-0 text-slate-500" />
<button
        ref={penSizeBtn}
        className="flex h-[22px] w-[52px] cursor-pointer items-center justify-between gap-0.5 px-1 text-[11px] font-semibold text-slate-700 transition-colors hover:text-brand-600"
        onClick={() => togglePop("penSize")}
        title="Pencil line thickness"
      >
        <span>{settings.thickness} px</span>
        <Icon name="chevronDown" className="h-2 w-2 shrink-0 opacity-60" />
      </button>
    </RibbonField>
  );

  /** Pencil / eraser / typewriter buttons, shared by the Home and Annotate ribbons. */
  const drawToolButtons = (
    <>
      <RibbonSwatch
        icon="pencil"
        label="Pencil"
        color={settings.color}
        anchorRef={penBtn}
        onClick={action("draw")}
        onMenu={() => togglePop("penColor")}
        active={tool === "draw"}
        disabled={!hasDoc}
        title="Draw freehand"
      />
      <RibbonButton icon="eraser" label="Eraser" active={tool === "eraser"} disabled={!hasDoc} onClick={action("eraser")} title="Erase markup" />
      <RibbonButton icon="typewriter" label="Typewriter" active={tool === "typewriter"} disabled={!hasDoc} onClick={action("typewriter")} title="Type text; click existing text to edit it" />
    </>
  );

  /** Text box + character formatting, shared by the Home and Insert ribbons. */
  const textTools = (
    <>
      <RibbonButton icon="textBox" label="Text Box" active={tool === "text"} disabled={!hasDoc} onClick={action("text")} title="Draw a text box" />
      <RibbonField title="Font family">
        <button
          ref={fontFamilyBtn}
          className="flex h-[22px] w-[74px] cursor-pointer items-center justify-between gap-1 px-1 text-[11px] font-medium text-slate-700 transition-colors hover:text-brand-600"
          onClick={() => togglePop("fontFamily")}
          title="Font family"
        >
          <span className="truncate">{settings.font}</span>
          <Icon name="chevronDown" className="h-2.5 w-2.5 shrink-0 opacity-60" />
        </button>
      </RibbonField>
      <RibbonField title="Font size">
        <button
          ref={fontSizeBtn}
          className="flex h-[22px] w-[40px] cursor-pointer items-center justify-between gap-0.5 px-0.5 text-[11px] font-semibold text-slate-700 transition-colors hover:text-brand-600"
          onClick={() => togglePop("fontSize")}
          title="Font size"
        >
          <span>{settings.fontSize}</span>
          <Icon name="chevronDown" className="h-2 w-2 shrink-0 opacity-60" />
        </button>
      </RibbonField>
      <RibbonField title="Font style">
        <button
          className="flex h-6 w-6 items-center justify-center rounded text-slate-600 hover:bg-slate-200/70"
          onClick={() => onSettings({ bold: !settings.bold })}
          style={{ fontWeight: settings.bold ? 700 : 400, color: settings.bold ? "#1667f4" : undefined }}
          title="Bold"
        >
          <Icon name="bold" className="h-3.5 w-3.5" />
        </button>
        <button
          className="flex h-6 w-6 items-center justify-center rounded text-slate-600 hover:bg-slate-200/70"
          onClick={() => onSettings({ italic: !settings.italic })}
          style={{ fontStyle: settings.italic ? "italic" : "normal", color: settings.italic ? "#1667f4" : undefined }}
          title="Italic"
        >
          <Icon name="italic" className="h-3.5 w-3.5" />
        </button>
        <button
          className="flex h-6 w-6 items-center justify-center rounded text-slate-600 hover:bg-slate-200/70"
          onClick={() => onSettings({ underline: !settings.underline })}
          style={{ color: settings.underline ? "#1667f4" : undefined, textDecoration: settings.underline ? "underline" : undefined }}
          title="Underline"
        >
          <Icon name="underlineIcon" className="h-3.5 w-3.5" />
        </button>
        <button
          className="flex h-6 w-6 items-center justify-center rounded text-slate-600 hover:bg-slate-200/70"
          onClick={() => onSettings({ strike: !settings.strike })}
          style={{ color: settings.strike ? "#1667f4" : undefined }}
          title="Strikethrough"
        >
          <Icon name="strikeIcon" className="h-3.5 w-3.5" />
        </button>
      </RibbonField>
      <RibbonSwatch
        icon="fontColor"
        label="Colour"
        color={settings.color}
        anchorRef={fontColorBtn}
        onClick={() => onTool("text")}
        onMenu={() => togglePop("fontColor")}
        disabled={!hasDoc}
        title="Text colour"
      />
    </>
  );

  const zoomLabel = zoom.mode === "pct" ? `${zoom.pct}%` : zoom.mode === "fitWidth" ? "Fit W" : "Fit P";

  /* ------------------------------ home ------------------------------ */
  const homeGroups: RibbonGroupSpec[] = [
    {
      key: "file",
      label: "File",
      title: "Open, save, save as and print",
      content: (
        <>
          <RibbonButton icon="open" label="Open" onClick={onOpen} title="Open a PDF (Ctrl+O)" />
          <RibbonSplitButton
            icon="save"
            label="Save"
            anchorRef={fileBtn}
            onClick={onSave}
            onMenu={() => togglePop("file")}
            disabled={!hasDoc || !dirty}
            menuDisabled={!hasDoc}
            title="Save (Ctrl+S)"
          />
        </>
      ),
    },
    {
      key: "history",
      label: "History",
      title: "Undo and redo",
      content: (
        <>
          <RibbonButton
            icon="undo"
            label="Undo"
            onClick={onUndo}
            disabled={!hasDoc || !canUndo}
            title={canUndo ? `Undo ${undoLabel ?? "last change"}` : "Nothing to undo"}
          />
          <RibbonButton
            icon="redo"
            label="Redo"
            onClick={onRedo}
            disabled={!hasDoc || !canRedo}
            title={canRedo ? `Redo ${redoLabel ?? "last change"}` : "Nothing to redo"}
          />
        </>
      ),
    },
    {
      key: "tools",
      label: "Tools",
      title: "Select, pan and find",
      content: (
        <>
          <RibbonButton icon="cursor" label="Select" active={tool === "select"} disabled={!hasDoc} onClick={action("select")} title="Select text and objects" />
          <RibbonButton icon="hand" label="Hand" active={tool === "hand"} disabled={!hasDoc} onClick={action("hand")} title="Pan the page" />
          <RibbonButton icon="search" label="Find" onClick={onFocusSearch} disabled={!hasDoc} title="Search text (Ctrl+F)" />
        </>
      ),
    },
    {
      key: "draw",
      label: "Draw",
      title: "Freehand pen, eraser and typewriter",
      content: (
        <>
          {drawToolButtons}
          {penSizeField}
        </>
      ),
    },
    {
      key: "text",
      label: "Text",
      title: "Text boxes and character formatting",
      content: <>{textTools}</>,
    },
    {
      key: "zoom",
      label: "Zoom",
      title: "Zoom level",
      content: (
        <>
          <RibbonField title="Zoom out / level / zoom in">
            <button
              className="flex h-6 w-6 items-center justify-center rounded text-slate-600 hover:bg-slate-200/70"
              onClick={() => onZoom({ mode: "pct", pct: Math.max(10, zoom.pct - 10) })}
              title="Zoom out (Ctrl+-)"
            >
              <Icon name="zoomOut" className="h-3.5 w-3.5" />
            </button>
            <button
              className="w-[42px] text-center text-[10.5px] font-semibold text-slate-700 hover:text-brand-600"
              onClick={() => onZoom({ mode: "pct", pct: 100 })}
              title="Reset to 100%"
            >
              {zoomLabel}
            </button>
            <button
              className="flex h-6 w-6 items-center justify-center rounded text-slate-600 hover:bg-slate-200/70"
              onClick={() => onZoom({ mode: "pct", pct: Math.min(400, zoom.pct + 10) })}
              title="Zoom in (Ctrl++)"
            >
              <Icon name="zoomIn" className="h-3.5 w-3.5" />
            </button>
          </RibbonField>
        </>
      ),
    },
    {
      key: "goto",
      label: "Go To",
      title: "Page navigation",
      content: (
        <>
          <RibbonButton icon="chevronLeft" label="Previous" onClick={onPrevPage} disabled={!hasDoc || current <= 0} title="Previous page" />
          <RibbonField title="Jump to page">
            <input
              type="text"
              inputMode="numeric"
              value={hasDoc ? current + 1 : 1}
              onChange={(e) => {
                const n = Number(e.target.value);
                if (!doc) return;
                onGoToPage(Math.max(1, Math.min(doc.pageCount, Number.isFinite(n) ? n : 1)) - 1);
              }}
              onFocus={(e) => e.currentTarget.select()}
              className="h-[22px] w-[40px] text-center text-[11px] font-semibold text-slate-700 focus:text-brand-700 focus:outline-none"
              title="Current page"
            />
            <span className="text-[10px] text-slate-400">of {doc?.pageCount ?? 0}</span>
          </RibbonField>
          <RibbonButton icon="chevronRight" label="Next" onClick={onNextPage} disabled={!hasDoc || current >= (doc?.pageCount ?? 1) - 1} title="Next page" />
        </>
      ),
    },
  ];

  /* ----------------------------- insert ----------------------------- */
  const shapeItems = [
    { icon: "rect", label: "Rectangle", tool: "rect" as ToolId },
    { icon: "ellipse", label: "Ellipse", tool: "ellipse" as ToolId },
    { icon: "triangle", label: "Triangle", tool: "triangle" as ToolId },
    { icon: "line", label: "Line", tool: "line" as ToolId },
    { icon: "arrow", label: "Arrow", tool: "arrow" as ToolId },
    { icon: "shape", label: "Polygon", tool: "polygon" as ToolId },
  ];

  const insertGroups: RibbonGroupSpec[] = [
    {
      key: "pages",
      label: "Pages",
      title: "Add, insert, rotate and remove pages",
      content: (
        <>
          <RibbonBigButton icon="newPage" label="Blank Page" onClick={onInsertBlank} disabled={!hasDoc} title="Insert a blank page" />
          <RibbonSplitButton icon="insertPages" label="Insert" anchorRef={pagesBtn} onMenu={() => togglePop("pages")} onClick={onInsertFromFile} disabled={!hasDoc} title="Insert pages from another file" />
          <RibbonButton icon="trash" label="Delete" onClick={onDeletePage} disabled={!hasDoc} title="Delete current page" />
          <RibbonButton icon="rotate" label="Rotate" onClick={onRotatePage} disabled={!hasDoc} title="Rotate current page" />
          <RibbonButton icon="extract" label="Extract" onClick={onExtractPages} disabled={!hasDoc} title="Extract pages to a new file" />
        </>
      ),
    },
    {
      key: "text",
      label: "Text",
      title: "Text boxes and character formatting",
      content: <>{textTools}</>,
    },
    {
      key: "media",
      label: "Images & Shapes",
      title: "Pictures, shapes and links",
      content: (
        <>
          <RibbonButton icon="image" label="Image" onClick={onInsertImage} disabled={!hasDoc} title="Insert an image" />
          <RibbonSplitButton icon="shape" label="Shapes" anchorRef={shapesBtn} onMenu={() => togglePop("shapes")} onClick={action("rect")} disabled={!hasDoc} title="Draw a shape" />
          <RibbonButton icon="link" label="Link" onClick={soon("Adding a link")} disabled={!hasDoc} title="Insert a hyperlink" />
          <RibbonButton icon="object" label="Object" onClick={soon("Embedding an object")} disabled={!hasDoc} title="Embed an OLE object" />
        </>
      ),
    },
    {
      key: "hf",
      label: "Header & Footer",
      title: "Headers, footers, page numbers and watermarks",
      content: (
        <>
          <RibbonSplitButton icon="headerFooter" label="Header" anchorRef={hfBtn} onMenu={() => togglePop("hf")} onClick={onHeaderFooter} disabled={!hasDoc} title="Add or edit header and footer" />
          <RibbonButton icon="pageNumber" label="Page Number" onClick={onPageNumbers} disabled={!hasDoc} title="Insert a page number" />
          <RibbonSplitButton icon="watermark" label="Watermark" anchorRef={annBtn} onMenu={() => togglePop("watermark")} onClick={onWatermark} disabled={!hasDoc} title="Add a watermark" />
        </>
      ),
    },
    {
      key: "sign",
      label: "Signature",
      title: "Digital and electronic signatures",
      content: (
        <>
          <RibbonButton icon="signature" label="Signature" active={tool === "signature"} disabled={!hasDoc} onClick={action("signature")} title="Place a signature" />
          <RibbonButton icon="stamp" label="Certificate" onClick={soon("Applying a certificate")} disabled={!hasDoc} title="Sign with a digital certificate" />
        </>
      ),
    },
  ];

  /* ---------------------------- annotate ---------------------------- */
  const HIGHLIGHT_COLORS = ["#fde047", "#86efac", "#7dd3fc", "#fca5a5", "#c4b5fd", "#d1fae5", "#fb923c", "#94a3b8"];
  const STAMPS = ["Approved", "Reviewed", "Confidential", "Draft", "Urgent", "Not Approved", "Signature"];

  const annotateGroups: RibbonGroupSpec[] = [
    {
      key: "comment",
      label: "Comment",
      title: "Notes, callouts and the comment pane",
      content: (
        <>
          <RibbonBigButton icon="comment" label="Add Note" active={tool === "comment"} disabled={!hasDoc} onClick={action("comment")} title="Add a sticky note" />
          <RibbonButton icon="callout" label="Callout" active={tool === "callout"} disabled={!hasDoc} onClick={action("callout")} title="Add a callout balloon" />
          <RibbonButton icon="commentsPane" label="Show Notes" disabled={!hasDoc} onClick={onToggleNotes} title="Show or hide all notes" />
        </>
      ),
    },
    {
      key: "annotate",
      label: "Annotate",
      title: "Highlight, markup and shapes",
      content: (
        <>
          <RibbonSwatch
            icon="highlight"
            label="Highlight"
            color={settings.color}
            anchorRef={hlBtn}
            onClick={action("highlight")}
            onMenu={() => togglePop("hlColor")}
            active={tool === "highlight"}
            disabled={!hasDoc}
            title="Highlight text"
          />
          <RibbonButton icon="underline" label="Underline" active={tool === "underline"} disabled={!hasDoc} onClick={action("underline")} title="Underline text" />
          <RibbonButton icon="strikeout" label="Strikeout" active={tool === "strikeout"} disabled={!hasDoc} onClick={action("strikeout")} title="Strike out text" />
          <RibbonButton icon="squiggly" label="Squiggly" active={tool === "squiggly"} disabled={!hasDoc} onClick={action("squiggly")} title="Squiggly underline" />
          <RibbonButton icon="cloud" label="Cloud" active={tool === "cloud"} disabled={!hasDoc} onClick={action("cloud")} title="Draw a cloud shape" />
          <RibbonButton icon="note" label="Note Shape" active={tool === "comment"} disabled={!hasDoc} onClick={action("comment")} title="Add a note shape" />
        </>
      ),
    },
    {
      key: "draw",
      label: "Draw",
      title: "Freehand drawing, stamps and typewriter",
      content: (
        <>
          {drawToolButtons}
          {penSizeField}
          <RibbonSplitButton icon="stamp" label="Stamp" anchorRef={winBtn} onMenu={() => togglePop("stamp")} onClick={action("stamp")} disabled={!hasDoc} title="Apply a stamp" />
        </>
      ),
    },
    {
      key: "measure",
      label: "Measure",
      title: "Area, distance and perimeter",
      content: (
        <>
          <RibbonButton icon="area" label="Area" active={tool === "area"} disabled={!hasDoc} onClick={action("area")} title="Measure an area" />
          <RibbonButton icon="distance" label="Distance" active={tool === "distance"} disabled={!hasDoc} onClick={action("distance")} title="Measure a distance" />
          <RibbonButton icon="perimeter" label="Perimeter" active={tool === "perimeter"} disabled={!hasDoc} onClick={action("perimeter")} title="Measure a perimeter" />
        </>
      ),
    },
  ];

  /* ------------------------------ view ------------------------------ */
  const viewGroups: RibbonGroupSpec[] = [
    {
      key: "pageview",
      label: "Page View",
      title: "Page view mode",
      content: (
        <>
          <RibbonSegmented
            value={layout}
            onChange={(v) => onLayout(v as PageLayout)}
            width={48}
            options={[
              { value: "onePage", icon: "onePage", label: "Single", title: "One page at a time" },
              { value: "continuous", icon: "continuous", label: "Scroll", title: "Scroll pages continuously" },
              { value: "facing", icon: "facing", label: "Facing", title: "Two pages side by side" },
            ]}
          />
          <RibbonButton icon="rotate" label="Rotate" onClick={onRotateView} title="Rotate the page view" />
        </>
      ),
    },
    {
      key: "zoom",
      label: "Zoom",
      title: "Magnification and fit modes",
      content: (
        <>
          <RibbonField title="Zoom out / level / zoom in">
            <button
              className="flex h-6 w-6 items-center justify-center rounded text-slate-600 hover:bg-slate-200/70"
              onClick={() => onZoom({ mode: "pct", pct: Math.max(10, zoom.pct - 10) })}
              title="Zoom out"
            >
              <Icon name="zoomOut" className="h-3.5 w-3.5" />
            </button>
            <button
              className="w-[42px] text-center text-[10.5px] font-semibold text-slate-700 hover:text-brand-600"
              onClick={() => onZoom({ mode: "pct", pct: 100 })}
              title="Actual size"
            >
              {zoomLabel}
            </button>
            <button
              className="flex h-6 w-6 items-center justify-center rounded text-slate-600 hover:bg-slate-200/70"
              onClick={() => onZoom({ mode: "pct", pct: Math.min(400, zoom.pct + 10) })}
              title="Zoom in"
            >
              <Icon name="zoomIn" className="h-3.5 w-3.5" />
            </button>
          </RibbonField>
          <RibbonButton icon="zoom100" label="Actual" onClick={() => onZoom({ mode: "pct", pct: 100 })} active={zoom.mode === "pct" && zoom.pct === 100} title="100%" />
          <RibbonButton icon="fitWidth" label="Fit Width" active={zoom.mode === "fitWidth"} onClick={() => onZoom({ mode: "fitWidth", pct: zoom.pct })} title="Fit page width" />
          <RibbonButton icon="fitPage" label="Fit Page" active={zoom.mode === "fitPage"} onClick={() => onZoom({ mode: "fitPage", pct: zoom.pct })} title="Fit whole page" />
        </>
      ),
    },
    {
      key: "navigation",
      label: "Navigation",
      title: "Navigation panes",
      content: (
        <>
          <RibbonButton icon="sidebar" label={thumbOpen ? "Hide Thumbs" : "Thumbnails"} active={thumbOpen} onClick={onToggleThumbs} title="Thumbnail pane" />
          <RibbonButton icon="bookmarks" label="Bookmarks" onClick={onToggleBookmarks} title="Bookmark pane" />
          <RibbonButton icon="layers" label="Layers" onClick={onToggleLayers} title="Layer pane" />
          <RibbonButton icon="properties" label={propsOpen ? "Hide Props" : "Properties"} active={propsOpen} onClick={onToggleProps} title="Properties pane" />
        </>
      ),
    },
    {
      key: "window",
      label: "Window",
      title: "Window and ribbon display",
      content: (
        <>
          <RibbonButton
            icon={fullscreen ? "exitFullscreen" : "fullscreen"}
            label={fullscreen ? "Exit Full" : "Full Screen"}
            active={fullscreen}
            onClick={onToggleFullscreen}
            title="Toggle full screen (F11)"
          />
          <RibbonButton
            icon={ribbonExpanded ? "ribbonUp" : "ribbonDown"}
            label={ribbonExpanded ? "Hide Ribbon" : "Show Ribbon"}
            active={!ribbonExpanded}
            onClick={() => setRibbonExpanded((v) => !v)}
            title="Collapse or expand the ribbon"
          />
          <RibbonButton icon="statusBar" label="Status Bar" active={false} onClick={onToggleStatusBar} title="Show or hide the status bar" />
        </>
      ),
    },
  ];

  return (
    <div className="relative z-20 shrink-0 border-b border-slate-200 bg-[#f7f8fa] select-none">
      {/* tab strip */}
      <div className="flex h-9 items-end gap-0.5 border-b border-slate-200 px-1">
        <div className="relative self-stretch">
          <button
            className="flex h-full items-center gap-1 px-2.5 text-[12.5px] font-semibold text-slate-700 hover:bg-white hover:shadow-[inset_0_2px_0_0_#1667f4]"
            onClick={() => setFileOpen((v) => !v)}
          >
            <span>File</span>
            <Icon name="chevronDown" className="h-3 w-3 text-slate-400" />
          </button>
          {fileOpen && (
            <>
              <div className="fixed inset-0 z-0" onClick={() => setFileOpen(false)} />
              <div className="absolute left-0 top-8 z-10 w-52 rounded-b-md rounded-tr-md border border-slate-200 bg-white py-1 shadow-lg">
                <button className={fileItem} onClick={() => { setFileOpen(false); onOpen(); }}>
                  <Icon name="open" className="h-3.5 w-3.5 text-slate-500" /> Open… <span className="ml-auto text-[10px] text-slate-400">Ctrl+O</span>
                </button>
                <button className={fileItem} disabled={!hasDoc} onClick={() => { setFileOpen(false); onSave(); }}>
                  <Icon name="save" className="h-3.5 w-3.5 text-slate-500" /> Save <span className="ml-auto text-[10px] text-slate-400">Ctrl+S</span>
                </button>
                <button className={fileItem} disabled={!hasDoc} onClick={() => { setFileOpen(false); onSaveAs(); }}>
                  <Icon name="save" className="h-3.5 w-3.5 text-slate-500" /> Save As… <span className="ml-auto text-[10px] text-slate-400">Ctrl+Shift+S</span>
                </button>
                <button className={fileItem} disabled={!hasDoc} onClick={() => { setFileOpen(false); onPrint(); }}>
                  <Icon name="print" className="h-3.5 w-3.5 text-slate-500" /> Print <span className="ml-auto text-[10px] text-slate-400">Ctrl+P</span>
                </button>
                <div className="my-1 border-t border-slate-100" />
                <button className={fileItem} disabled={!hasDoc || !canUndo} onClick={() => { setFileOpen(false); onUndo(); }}>
                  <Icon name="undo" className="h-3.5 w-3.5 text-slate-500" /> Undo <span className="ml-auto text-[10px] text-slate-400">Ctrl+Z</span>
                </button>
                <button className={fileItem} disabled={!hasDoc || !canRedo} onClick={() => { setFileOpen(false); onRedo(); }}>
                  <Icon name="redo" className="h-3.5 w-3.5 text-slate-500" /> Redo <span className="ml-auto text-[10px] text-slate-400">Ctrl+Y</span>
                </button>
              </div>
            </>
          )}
        </div>
        {TABS.map((t) => {
          const active = tab === t.id;
          return (
            <button
              key={t.id}
              className={`flex h-full items-center px-3 text-[12.5px] font-medium transition-colors ${
                active
                  ? "bg-white font-semibold text-slate-800 shadow-[inset_0_2px_0_0_#1667f4]"
                  : "text-slate-500 hover:bg-slate-100 hover:text-slate-700"
              }`}
              onClick={() => onTab(t.id)}
            >
              {t.label}
            </button>
          );
        })}
        <div className="ml-auto flex items-center gap-0.5 pr-1">
          <button
            className="flex h-6 w-6 items-center justify-center rounded text-slate-400 transition-colors hover:bg-slate-200/70 hover:text-slate-600"
            title={ribbonExpanded ? "Collapse the ribbon" : "Expand the ribbon"}
            onClick={() => setRibbonExpanded((v) => !v)}
          >
            <Icon name={ribbonExpanded ? "ribbonUp" : "ribbonDown"} className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

{/* active tab's ribbon: one horizontal row of groups, like Office 365 */}
      {ribbonExpanded && (
        <RibbonSurface
          groups={tab === "home" ? homeGroups : tab === "insert" ? insertGroups : tab === "annotate" ? annotateGroups : viewGroups}
          hidden={hidden[tab] ?? []}
          onToggleGroup={(key) => toggleGroup(tab, key)}
        />
      )}

      {/* ribbon galleries and colour pickers */}
      {popover === "file" && (
        <RibbonPopover anchor={fileBtn} width={220} onClose={() => setPopover(null)}>
            <RibbonMenuRow icon="save" label="Save" hint="Ctrl+S" onClick={() => { setPopover(null); onSave(); }} disabled={!hasDoc || !dirty} />
          <RibbonMenuRow icon="saveAs" label="Save As…" hint="Ctrl+Shift+S" onClick={() => { setPopover(null); onSaveAs(); }} disabled={!hasDoc} />
          <RibbonMenuRow icon="print" label="Print…" hint="Ctrl+P" onClick={() => { setPopover(null); onPrint(); }} disabled={!hasDoc} />
          <div className="my-1 border-t border-slate-100" />
          <RibbonMenuRow icon="close" label="Close document" onClick={soon("Closing the document")} disabled={!hasDoc} />
        </RibbonPopover>
      )}
      {popover === "shapes" && (
        <RibbonPopover anchor={shapesBtn} width={264} onClose={() => setPopover(null)}>
          <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">Basic shapes</p>
          <div className="flex flex-wrap gap-0.5 px-1.5 pb-1">
            {shapeItems.map((s) => {
              const t = s.tool as ToolId;
              return <GalleryItem key={s.label} icon={s.icon} label={s.label} active={tool === t} disabled={!hasDoc} onClick={() => { setPopover(null); onTool(t); }} />;
            })}
          </div>
        </RibbonPopover>
      )}
      {popover === "hlColor" && (
        <RibbonPopover anchor={hlBtn} width={168} onClose={() => setPopover(null)}>
          <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">Highlight colour</p>
          <div className="grid grid-cols-4 gap-1 px-3 pb-1">
            {HIGHLIGHT_COLORS.map((c) => (
              <button
                key={c}
                className={`h-6 w-6 rounded border-2 transition-transform hover:scale-110 ${settings.color === c ? "border-brand-600" : "border-slate-200"}`}
                style={{ background: c }}
                title={c}
                onClick={() => {
                  onSettings({ color: c });
                  onTool("highlight");
                  setPopover(null);
                }}
              />
            ))}
          </div>
          <div className="my-1 border-t border-slate-100" />
          <RibbonMenuRow icon="highlight" label="No colour" onClick={() => onSettings({ color: "transparent" })} />
        </RibbonPopover>
      )}
      {popover === "penColor" && (
        <RibbonPopover anchor={penBtn} width={168} onClose={() => setPopover(null)}>
          <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">Pen colour</p>
          <div className="grid grid-cols-4 gap-1 px-3 pb-1">
            {HIGHLIGHT_COLORS.map((c) => (
              <button
                key={c}
                className={`h-6 w-6 rounded border-2 transition-transform hover:scale-110 ${settings.color === c ? "border-brand-600" : "border-slate-200"}`}
                style={{ background: c }}
                title={c}
                onClick={() => {
                  onSettings({ color: c });
                  onTool("draw");
                  setPopover(null);
                }}
              />
            ))}
          </div>
        </RibbonPopover>
      )}
      {popover === "fontColor" && (
        <RibbonPopover anchor={fontColorBtn} width={168} onClose={() => setPopover(null)}>
          <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">Text colour</p>
          <div className="grid grid-cols-4 gap-1 px-3 pb-1">
            {["#1e293b", "#000000", "#1667f4", "#e11d48", "#16a34a", "#ca8a04", "#7c3aed", "#94a3b8"].map((c) => (
              <button
                key={c}
                className={`h-6 w-6 rounded border-2 transition-transform hover:scale-110 ${settings.color === c ? "border-brand-600" : "border-slate-200"}`}
                style={{ background: c }}
                title={c}
                onClick={() => {
                  onSettings({ color: c });
                  onTool("text");
                  setPopover(null);
                }}
              />
            ))}
          </div>
        </RibbonPopover>
      )}
      {popover === "fontFamily" && (
        <RibbonPopover anchor={fontFamilyBtn} width={120} onClose={() => setPopover(null)}>
          <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">Font family</p>
          {["Helvetica", "Times", "Courier"].map((f) => (
            <button
              key={f}
              className="flex w-full items-center justify-between px-3 py-1.5 text-left text-[12px] text-slate-700 transition-colors hover:bg-brand-50"
              title={`${f} font`}
              onClick={() => {
                onSettings({ font: f });
                setPopover(null);
              }}
            >
              <span>{f}</span>
              {settings.font === f && <Icon name="check" className="h-3.5 w-3.5 text-brand-600" />}
            </button>
          ))}
        </RibbonPopover>
      )}
      {popover === "fontSize" && (
        <RibbonPopover anchor={fontSizeBtn} width={96} onClose={() => setPopover(null)}>
          <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">Font size</p>
          {[8, 9, 10, 12, 14, 18, 24, 36].map((s) => (
            <button
              key={s}
              className="flex w-full items-center justify-between px-3 py-1.5 text-left text-[12px] text-slate-700 transition-colors hover:bg-brand-50"
              title={`${s} pt`}
              onClick={() => {
                onSettings({ fontSize: s });
                setPopover(null);
              }}
            >
              <span>{s}</span>
              {settings.fontSize === s && <Icon name="check" className="h-3.5 w-3.5 text-brand-600" />}
            </button>
          ))}
        </RibbonPopover>
      )}
      {popover === "penSize" && (
        <RibbonPopover anchor={penSizeBtn} width={104} onClose={() => setPopover(null)}>
          <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">Thickness</p>
          {PEN_SIZES.map((s) => (
            <button
              key={s}
              className="flex w-full items-center justify-between px-3 py-1.5 text-left text-[12px] text-slate-700 transition-colors hover:bg-brand-50"
              title={`${s} px`}
              onClick={() => {
                onSettings({ thickness: s });
                setPopover(null);
              }}
            >
              <span>{s} px</span>
              {settings.thickness === s && <Icon name="check" className="h-3.5 w-3.5 text-brand-600" />}
            </button>
          ))}
        </RibbonPopover>
      )}
      {popover === "pages" && (
        <RibbonPopover anchor={pagesBtn} width={228} onClose={() => setPopover(null)}>
          <RibbonMenuRow icon="open" label="From file…" onClick={() => { setPopover(null); onInsertFromFile(); }} disabled={!hasDoc} />
          <RibbonMenuRow icon="copy" label="Duplicate page" onClick={() => { setPopover(null); onDuplicatePage(); }} disabled={!hasDoc} />
          <RibbonMenuRow icon="extract" label="Extract to new file" onClick={() => { setPopover(null); onExtractPages(); }} disabled={!hasDoc} />
        </RibbonPopover>
      )}
      {popover === "hf" && (
        <RibbonPopover anchor={hfBtn} width={216} onClose={() => setPopover(null)}>
          <RibbonMenuRow icon="headerFooter" label="Header & Footer…" onClick={() => { setPopover(null); onHeaderFooter(); }} disabled={!hasDoc} />
          <RibbonMenuRow icon="pageNumber" label="Page Number…" onClick={() => { setPopover(null); onPageNumbers(); }} disabled={!hasDoc} />
          <div className="my-1 border-t border-slate-100" />
          <RibbonMenuRow icon="watermark" label="Watermark…" onClick={() => { setPopover(null); onWatermark(); }} disabled={!hasDoc} />
        </RibbonPopover>
      )}
      {popover === "watermark" && (
        <RibbonPopover anchor={annBtn} width={216} onClose={() => setPopover(null)}>
          <RibbonMenuRow icon="watermark" label="Text watermark…" onClick={() => { setPopover(null); onWatermark(); }} disabled={!hasDoc} />
          <RibbonMenuRow icon="image" label="Image watermark…" onClick={() => { setPopover(null); onWatermark(); }} disabled={!hasDoc} />
        </RibbonPopover>
      )}
      {popover === "stamp" && (
        <RibbonPopover anchor={winBtn} width={240} onClose={() => setPopover(null)}>
          <p className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">Stamps</p>
          <div className="flex flex-wrap gap-0.5 px-1.5 pb-1">
            {STAMPS.slice(0, 6).map((s) => (
              <GalleryItem
                key={s}
                icon="stamp"
                label={s}
                active={settings.stampLabel === s}
                disabled={!hasDoc}
                onClick={() => {
                  onSettings({ stampLabel: s });
                  onTool("stamp");
                  setPopover(null);
                }}
              />
            ))}
          </div>
        </RibbonPopover>
      )}
    </div>
  );
}

/* ------------------------- thumbnail panel ------------------------- */

interface ThumbPanelProps {
  id: string;
  geometries: PageGeometry[];
  current: number;
  onGo: (page: number) => void;
  onClose: () => void;
  onToast: (msg: string) => void;
  onDuplicate: (page: number) => void;
  onDelete: (page: number) => void;
  onRotate: (page: number) => void;
}

function ThumbPanel({ id, geometries, current, onGo, onClose, onToast, onDuplicate, onDelete, onRotate }: ThumbPanelProps) {
  const [thumbs, setThumbs] = useState<Record<number, RenderedPage>>({});
  const [hover, setHover] = useState<number | null>(null);

  useEffect(() => {
    const n = Math.min(geometries.length, 60);
    const load = async () => {
      const fresh: Record<number, RenderedPage> = {};
      for (let i = 0; i < n; i++) {
        if (!(i in thumbs)) {
          try {
            fresh[i] = await invoke<RenderedPage>("render_thumbnail", { id, page: i, maxWidth: 150 });
          } catch {
            /* individual thumb failures are non-fatal */
          }
        }
      }
      setThumbs((prev) => ({ ...prev, ...fresh }));
    };
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, geometries.length]);

  return (
    <aside className="flex w-[205px] shrink-0 flex-col border-r border-slate-200 bg-[#f4f5f7]">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-slate-200 px-2.5">
        <Icon name="doc" className="h-3.5 w-3.5 text-slate-500" />
        <span className="text-[11.5px] font-semibold text-slate-600">Pages</span>
        <span className="text-[11px] text-slate-400">· {geometries.length}</span>
        <button
          className="ml-auto rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-600"
          onClick={onClose}
          title="Collapse thumbnails"
        >
          <Icon name="chevronLeft" className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        <div className="flex flex-col items-stretch gap-2.5">
          {geometries.map((g, i) => {
            const t = thumbs[i];
            const aspect = g.height / g.width;
            const active = i === current;
            return (
              <div
                key={i}
                className="group relative"
                onMouseEnter={() => setHover(i)}
                onMouseLeave={() => setHover(null)}
              >
                <button
                  className={`block w-full overflow-hidden rounded border-2 bg-white transition-colors ${
                    active ? "border-brand-500 shadow-sm" : "border-transparent hover:border-brand-300"
                  }`}
                  onClick={() => onGo(i)}
                  title={`Go to page ${i + 1}`}
                >
                  <div className="m-1.5 overflow-hidden rounded-sm border border-slate-200 bg-slate-50" style={{ height: 150 * aspect }}>
                    {t?.data_url ? (
                      <img src={t.data_url} alt={`Page ${i + 1} thumbnail`} className="h-full w-full object-fill" />
                    ) : (
                      <div className="flex h-full w-full items-center justify-center text-[10px] text-slate-300">…</div>
                    )}
                  </div>
                  <div className="pb-1 text-center text-[10.5px] font-medium text-slate-500">{i + 1}</div>
                </button>
                {hover === i && (
                  <div className="absolute right-1.5 top-1.5 flex gap-0.5 rounded-md bg-white/95 p-0.5 shadow">
                    {(["rotate", "duplicate", "trash"] as const).map((act) => (
                      <button
                        key={act}
                        className="rounded p-1 text-slate-500 hover:bg-brand-50 hover:text-brand-600"
                        title={act === "rotate" ? "Rotate page" : act === "duplicate" ? "Duplicate page" : "Delete page"}
                        onClick={(e) => {
                          e.stopPropagation();
                          if (act === "rotate") {
                            onRotate(i);
                            void onGo(i);
                          } else if (act === "duplicate") {
                            onDuplicate(i);
                            void onGo(i + 1);
                          } else if (act === "trash") {
                            if (geometries.length <= 1) {
                              onToast("A document must keep at least one page.");
                            } else {
                              onDelete(i);
                              void onGo(Math.max(0, i - 1));
                            }
                          }
                        }}
                      >
                        <Icon name={act} className="h-3 w-3" />
                      </button>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </aside>
  );
}

/* ---------------------------- workspace ---------------------------- */

interface WorkspaceProps {
  doc: DocSummary;
  geometries: PageGeometry[];
  zoom: ZoomSpec;
  tool: ToolId;
  layout: PageLayout;
  currentPage: number;
  viewRot: number;
  annotations: Ann[];
  settings: AnnSettings;
  selectedIds: string[];
  onSelectIds: (ids: string[]) => void;
  onContextMenu: (req: SheetContextReq) => void;
  onSelect: (page: number) => void;
  onCurrent: (page: number) => void;
  onCommitAnn: (page: number, ann: Ann) => void;
  onUpdateAnn: (page: number, ann: Ann) => void;
  onDeleteAnn: (page: number, annId: string) => void;
  onToast: (msg: string) => void;
  onMeasure: (msg: string | null) => void;
}

function Workspace({
  doc,
  geometries,
  zoom,
  tool,
  layout,
  currentPage: externalPage,
  viewRot,
  annotations,
  settings,
  selectedIds,
  onSelectIds,
  onContextMenu,
  onSelect,
  onCurrent,
  onCommitAnn,
  onUpdateAnn,
  onDeleteAnn,
  onToast,
  onMeasure,
}: WorkspaceProps) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const sheetEls = useRef(new Map<number, HTMLElement>());
  const [cw, setCw] = useState(1);
  const [ch, setCh] = useState(1);
  const [visible, setVisible] = useState<Set<number>>(new Set());
  const rafRef = useRef(0);

  // Page currently in view; single-page layout follows it directly.
  const [currentPage, setCurrentPage] = useState(0);
  useEffect(() => {
    if (currentPage >= 0 && currentPage < geometries.length) onCurrent(currentPage);
  }, [currentPage, geometries.length, onCurrent]);

  // Single-page layout renders exactly one sheet, so external navigation (Next,
  // Go To box, thumbnail click) must drive it instead of waiting for a scroll.
  useEffect(() => {
    if (layout !== "onePage") return;
    if (externalPage < 0 || externalPage >= geometries.length) return;
    setCurrentPage((prev) => (prev === externalPage ? prev : externalPage));
  }, [layout, externalPage, geometries.length]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      for (const entry of entries) {
        setCw(entry.contentRect.width);
        setCh(entry.contentRect.height);
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Sheets mount *after* this observer would first run (the workspace starts on
  // the welcome screen), so it has to be rebuilt whenever the document or page
  // count changes. Otherwise no page is ever observed, `visible` stays empty and
  // every sheet is stuck on "Loading…" with a blank canvas.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    setVisible(new Set());
    const io = new IntersectionObserver(
      (entries) => {
        setVisible((prev) => {
          const next = new Set(prev);
          for (const entry of entries) {
            const i = Number((entry.target as HTMLElement).dataset.page ?? -1);
            if (i < 0) continue;
            if (entry.isIntersecting) next.add(i);
            else next.delete(i);
          }
          return next;
        });
      },
      { root: el, rootMargin: "400px 0px 400px 0px" }
    );
    for (const elx of sheetEls.current.values()) io.observe(elx);
    return () => io.disconnect();
  }, [doc.id, geometries.length]);

  const onScroll = () => {
    cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(() => {
      const el = scrollRef.current;
      if (!el) return;
      const elTop = el.getBoundingClientRect().top;
      let current = -1;
      for (let i = 0; i < geometries.length; i++) {
        const node = sheetEls.current.get(i);
        if (!node) continue;
        const r = node.getBoundingClientRect();
        if (r.top - elTop < el.clientHeight * 0.55 && r.bottom - elTop > 0) current = i;
      }
      if (current >= 0 && current !== currentPage) {
        setCurrentPage(current);
        onCurrent(current);
      }
    });
  };

  // Facing pages need half the available width per page.
  const effCw = layout === "facing" ? Math.max(160, (cw - 24) / 2) : cw;
  const effCh = layout === "onePage" ? ch : ch;

  const sizes = useMemo(
    () =>
      geometries.map((g) => {
        const rot = ((((viewRot % 360) + 360) % 360) + g.rotation) % 360;
        const base = pageDisplaySize({ ...g, width: g.raw_width, height: g.raw_height }, zoom, effCw, effCh);
        return rot % 180 === 90 ? { w: base.h, h: base.w } : base;
      }),
    [geometries, zoom, effCw, effCh, viewRot]
  );

  const registerRef = useCallback((index: number, el: HTMLElement | null) => {
    if (el) sheetEls.current.set(index, el);
    else sheetEls.current.delete(index);
  }, []);

  // Hand tool: dragging the canvas pans the document, like every other reader.
  const panRef = useRef<{ x: number; y: number; left: number; top: number } | null>(null);

  const onPanStart = (e: React.PointerEvent<HTMLDivElement>) => {
    if (tool !== "hand") return;
    const el = scrollRef.current;
    if (!el) return;
    el.setPointerCapture(e.pointerId);
    panRef.current = { x: e.clientX, y: e.clientY, left: el.scrollLeft, top: el.scrollTop };
  };

  const onPanMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const pan = panRef.current;
    const el = scrollRef.current;
    if (!pan || !el) return;
    el.scrollLeft = pan.left - (e.clientX - pan.x);
    el.scrollTop = pan.top - (e.clientY - pan.y);
  };

  const onPanEnd = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!panRef.current) return;
    panRef.current = null;
    scrollRef.current?.releasePointerCapture(e.pointerId);
  };

  return (
    <div
      ref={scrollRef}
      onScroll={onScroll}
      onPointerDown={onPanStart}
      onPointerMove={onPanMove}
      onPointerUp={onPanEnd}
      onPointerCancel={onPanEnd}
      className={`min-w-0 flex-1 overflow-auto bg-[#eef0f3] ${tool === "hand" ? "cursor-grab active:cursor-grabbing" : ""}`}
    >
      <div
        className={
          layout === "continuous"
            ? "flex flex-col items-center gap-5 px-6 py-7"
            : layout === "onePage"
              ? "flex flex-col items-center px-6 py-7"
              : "flex flex-wrap items-start justify-center gap-x-4 gap-y-5 px-6 py-7"
        }
      >
        {geometries.map((g, i) => {
          // Single-page mode only ever shows the current page.
          if (layout === "onePage" && i !== currentPage) return null;
          const rot = ((((viewRot % 360) + 360) % 360) + g.rotation) % 360;
          return (
            <Sheet
              key={i}
              index={i}
              geo={g}
              size={sizes[i]}
              rotation={rot}
              isVisible={visible.has(i)}
              id={doc.id}
              revision={doc.revision}
              tool={tool}
              anns={annotations}
              settings={settings}
              selectedIds={selectedIds}
              onSelectIds={onSelectIds}
              onContextMenu={onContextMenu}
              onCommitAnn={onCommitAnn}
              onUpdateAnn={onUpdateAnn}
              onDeleteAnn={onDeleteAnn}
              onSelect={onSelect}
              onToast={onToast}
              onMeasure={onMeasure}
              registerRef={registerRef}
            />
          );
        })}
        {layout === "continuous" && (
          <div className="flex flex-col items-center gap-2 pb-4">
            <Icon name="doc" className="h-4 w-4 text-slate-300" />
            <span className="text-[11px] text-slate-400">
              End of document · {geometries.length} page{geometries.length === 1 ? "" : "s"}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}

/* -------------------------- properties panel -------------------------- */

interface PropsPanelProps {
  doc: DocSummary;
  dirty: boolean;
  selected: { kind: "doc" } | { kind: "page"; page: number };
  geometries: PageGeometry[];
  onClose: () => void;
}

function Row({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-3 px-3 py-[5px] text-[11.5px]">
      <span className="shrink-0 text-slate-500">{label}</span>
      <span className="truncate text-right font-medium text-slate-700">{value ?? "—"}</span>
    </div>
  );
}

function PropsPanel({ doc, dirty, selected, geometries, onClose }: PropsPanelProps) {
  const m: Metadata = doc.metadata ?? {
    title: null,
    author: null,
    subject: null,
    keywords: null,
    creator: null,
    producer: null,
    creation_date: null,
    modification_date: null,
  };
  const isPage = selected.kind === "page";
  const title = isPage ? `Page Properties` : `Document Properties`;

  return (
    <aside className="flex w-[290px] shrink-0 flex-col border-l border-slate-200 bg-white">
      <div className="flex h-9 shrink-0 items-center border-b border-slate-200 px-3">
        <span className="text-[11.5px] font-semibold text-slate-600">{title}</span>
        <button className="ml-auto rounded p-1 text-slate-400 hover:bg-slate-100" onClick={onClose} title="Hide properties">
          <Icon name="chevronRight" className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="min-h-0 flex-1 divide-y divide-slate-100 overflow-y-auto py-1">
        {isPage ? (
          (() => {
            const g = geometries[selected.page];
            if (!g) return null;
            const wIn = g.width / 72;
            const hIn = g.height / 72;
            return (
              <div>
                <Row label="Page number" value={`Page ${selected.page + 1} of ${doc.pageCount}`} />
                <Row label="Page size" value={`${wIn.toFixed(2)}″ × ${hIn.toFixed(2)}″ (${wIn * 25.4 | 0} × ${hIn * 25.4 | 0} mm)`} />
                <Row label="Orientation" value={g.orientation} />
                <Row label="Rotation" value={`${g.rotation}°`} />
                <Row label="Raw size" value={`${g.raw_width} × ${g.raw_height} pt`} />
                {g.label && <Row label="Label" value={g.label} />}
              </div>
            );
          })()
        ) : (
          <div>
            <Row label="File name" value={doc.fileName} />
            <Row label="Location" value={doc.path ? truncatePath(doc.path) : "Unsaved new document"} />
            <Row label="File size" value={fmtBytes(doc.fileSize)} />
            <Row label="Pages" value={doc.pageCount} />
            <Row label="PDF version" value={doc.pdfVersion} />
            <Row label="Encryption" value={doc.isEncrypted ? "Password protected" : "None"} />
            <Row label="Modified" value={dirty ? "Yes — unsaved changes" : "Saved"} />
            {(m.title || m.author || m.subject || m.creator || m.producer || m.creation_date) && (
              <div>
                <p className="px-3 pt-2 pb-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400">
                  Document metadata
                </p>
                <Row label="Title" value={m.title} />
                <Row label="Author" value={m.author} />
                <Row label="Subject" value={m.subject} />
                <Row label="Creator" value={m.creator} />
                <Row label="Producer" value={m.producer} />
                <Row label="Created" value={m.creation_date} />
                <Row label="Modified" value={m.modification_date} />
              </div>
            )}
          </div>
        )}
        <div className="px-3 py-2.5">
          <p className="text-[11px] leading-relaxed text-slate-400">
            The properties panel reflects real data from the open PDF. It becomes contextual for text,
            highlight, shape and annotation selections in the next build step.
          </p>
        </div>
      </div>
    </aside>
  );
}

function truncatePath(p: string): string {
  return p.length > 42 ? `…${p.slice(-42)}` : p;
}

/* ----------------------------- status bar ----------------------------- */

interface StatusBarProps {
  current: number;
  pageCount: number;
  zoom: ZoomSpec;
  busy: string | null;
  message: string | null;
  onZoom: (z: ZoomSpec) => void;
  onPrev: () => void;
  onNext: () => void;
}

function StatusBar({ current, pageCount, zoom, busy, message, onZoom, onPrev, onNext }: StatusBarProps) {
  const slider = (e: React.ChangeEvent<HTMLInputElement>) => {
    onZoom({ mode: "pct", pct: Number(e.target.value) });
  };
  const pctLabel =
    zoom.mode === "fitWidth" ? "Fit width" : zoom.mode === "fitPage" ? "Fit page" : `${zoom.pct}%`;

  return (
    <footer className="flex h-9 shrink-0 select-none items-center gap-2 border-t border-slate-200 bg-[#f7f8fa] px-3 text-[11.5px] text-slate-600">
      <span className="font-medium">
        Page {pageCount === 0 ? 0 : current + 1} of {pageCount}
      </span>
      <button className="flex h-6 w-6 items-center justify-center rounded hover:bg-slate-200/70 disabled:opacity-40" onClick={onPrev} disabled={current <= 0} title="Previous page">
        <Icon name="chevronLeft" className="h-3.5 w-3.5" />
      </button>
      <button className="flex h-6 w-6 items-center justify-center rounded hover:bg-slate-200/70 disabled:opacity-40" onClick={onNext} disabled={current >= pageCount - 1} title="Next page">
        <Icon name="chevronRight" className="h-3.5 w-3.5" />
      </button>

      {busy && <span className="ml-2 text-slate-400">{busy}</span>}
      {message && !busy && <span className="ml-2 font-medium text-emerald-700">{message}</span>}

      <div className="ml-auto flex items-center gap-1.5">
        <button
          className={`flex h-6 items-center gap-1 rounded px-2 text-[11px] font-medium ${
            zoom.mode === "fitWidth" ? "bg-brand-50 text-brand-700 ring-1 ring-inset ring-brand-200" : "hover:bg-slate-200/70"
          }`}
          onClick={() => onZoom({ mode: "fitWidth", pct: zoom.pct })}
          title="Fit page width"
        >
          Fit Width
        </button>
        <button
          className={`flex h-6 items-center gap-1 rounded px-2 text-[11px] font-medium ${
            zoom.mode === "fitPage" ? "bg-brand-50 text-brand-700 ring-1 ring-inset ring-brand-200" : "hover:bg-slate-200/70"
          }`}
          onClick={() => onZoom({ mode: "fitPage", pct: zoom.pct })}
          title="Fit whole page in the window"
        >
          Fit Page
        </button>
        <div className="mx-0.5 h-4 w-px bg-slate-300/70" />
        <button
          className="flex h-6 w-6 items-center justify-center rounded hover:bg-slate-200/70"
          onClick={() => onZoom({ mode: "pct", pct: Math.max(10, zoom.pct - 10) })}
          title="Zoom out"
        >
          <Icon name="minus" className="h-3.5 w-3.5" />
        </button>
        <input
          type="range"
          min={10}
          max={400}
          step={5}
          value={zoom.pct}
          onChange={slider}
          className="w-36 accent-brand-600"
          title="Zoom"
        />
        <button
          className="flex h-6 w-6 items-center justify-center rounded hover:bg-slate-200/70"
          onClick={() => onZoom({ mode: "pct", pct: Math.min(400, zoom.pct + 10) })}
          title="Zoom in"
        >
          <Icon name="plus" className="h-3.5 w-3.5" />
        </button>
        <button
          className="w-14 text-right text-[11.5px] font-medium text-slate-700 hover:text-brand-600"
          onClick={() => onZoom({ mode: "pct", pct: 100 })}
          title="Reset to 100%"
        >
          {pctLabel}
        </button>
      </div>
    </footer>
  );
}

/* ---------------------------- welcome view ---------------------------- */

interface WelcomeProps {
  appInfo: AppInfo | null;
  recents: RecentFile[];
  onOpen: () => void;
  onRecent: (path: string) => void;
  onClearRecents: () => void;
}

function Welcome({ appInfo, recents, onOpen, onRecent, onClearRecents }: WelcomeProps) {
  return (
    <section className="flex min-w-0 flex-1 flex-col items-center justify-center gap-5 bg-[#eef0f3]">
      <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-gradient-to-br from-brand-500 to-brand-700 text-3xl font-bold text-white shadow-panel">
        B
      </div>
      <div className="text-center">
        <h1 className="text-xl font-semibold text-slate-700">{appInfo ? `${appInfo.name} ${appInfo.version}` : "BritPDF"}</h1>
        <p className="mt-1 max-w-md text-[12.5px] leading-relaxed text-slate-500">
          A professional PDF editor and reader for Windows. Everything happens on your computer — your files never
          leave it.
        </p>
      </div>
      <button
        className="flex h-9 items-center gap-2 rounded-md bg-brand-600 px-4 text-[13px] font-medium text-white shadow hover:bg-brand-700"
        onClick={onOpen}
      >
        <Icon name="open" className="h-4 w-4" />
        Open PDF…
      </button>
      {recents.length > 0 && (
        <div className="w-full max-w-sm rounded-md border border-slate-200 bg-white p-3 shadow-sm">
          <div className="mb-1.5 flex items-center justify-between">
            <span className="text-[11.5px] font-semibold text-slate-600">Recent files</span>
            <button className="text-[11px] text-slate-400 hover:text-slate-600" onClick={onClearRecents}>
              Clear
            </button>
          </div>
          <ul className="flex flex-col gap-0.5">
            {recents.map((r) => (
              <li key={r.path}>
                <button
                  className="flex w-full items-center gap-2 rounded px-2 py-1 text-left text-[12px] text-slate-600 hover:bg-brand-50"
                  title={r.missing ? `${r.path} (missing)` : r.path}
                  onClick={() => onRecent(r.path)}
                >
                  <Icon name="doc" className={`h-3.5 w-3.5 shrink-0 ${r.missing ? "text-amber-500" : "text-slate-400"}`} />
                  <span className="flex min-w-0 flex-1 flex-col">
                    <span className={`truncate ${r.missing ? "text-slate-400" : ""}`}>{r.fileName || r.path}</span>
                    <span className="truncate text-[10.5px] text-slate-400">
                      {[
                        r.pageCount != null ? `${r.pageCount} page${r.pageCount === 1 ? "" : "s"}` : null,
                        r.fileSize != null ? fmtBytes(r.fileSize) : null,
                        r.missing ? "Missing" : null,
                      ]
                        .filter(Boolean)
                        .join(" \u00b7 ")}
                    </span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
      {appInfo && (
        <p className="text-[11px] text-slate-400">
          {appInfo.engine} {appInfo.engine_version} · {appInfo.os} {appInfo.arch}
        </p>
      )}
    </section>
  );
}

/* --------------------------------- app --------------------------------- */

export default function App() {
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [doc, setDoc] = useState<DocSummary | null>(null);
  const [geometries, setGeometries] = useState<PageGeometry[]>([]);
  const [current, setCurrent] = useState(0);
  const [zoom, setZoom] = useState<ZoomSpec>({ mode: "fitPage", pct: 100 });
  const [tool, setTool] = useState<ToolId>("select");
  const [tab, setTab] = useState<TabId>("home");
  const [thumbOpen, setThumbOpen] = useState(true);
  const [propsOpen, setPropsOpen] = useState(true);
  const [layout, setLayout] = useState<PageLayout>("continuous");
  const [fullscreen, setFullscreen] = useState(false);
  const [selected, setSelected] = useState<{ kind: "doc" } | { kind: "page"; page: number }>({ kind: "doc" });
  const [recents, setRecents] = useState<RecentFile[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchStatus, setSearchStatus] = useState<"idle" | "searching" | "done" | "none">("idle");
  const [searchResults, setSearchResults] = useState<SearchHit[]>([]);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const searchCache = useRef<{ boundaries: number[]; full: string } | null>(null);
  // Monotonic token so a slow search can't overwrite the results of a newer one.
  const searchSeq = useRef(0);
  const toastTimer = useRef<number | null>(null);

  const showToast = useCallback((msg: string) => {
    setToast(msg);
    if (toastTimer.current) window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 2400);
  }, []);

  const showError = useCallback((e: unknown) => setError(errorMessage(e)), []);

  /* ---- annotation overlay state (committed to the PDF on save / structural edits) ---- */
  const [annotations, setAnnotations] = useState<Ann[]>([]);
  const [annSettings, setAnnSettings] = useState<AnnSettings>(DEFAULT_ANN_SETTINGS);
  const [viewRot, setViewRot] = useState(0);
  const [measureMsg, setMeasureMsg] = useState<string | null>(null);
  const [bookmarksOpen, setBookmarksOpen] = useState(false);
  const [layersOpen, setLayersOpen] = useState(false);
  const [notesOpen, setNotesOpen] = useState(false);
  const [statusBarVisible, setStatusBarVisible] = useState(true);
  const [outline, setOutline] = useState<OutlineEntry[]>([]);
  const [dialog, setDialog] = useState<"hf" | "pageNumbers" | "watermark" | "none">("none");
  const annotationsRef = useRef<Ann[]>([]);
  const annUndoRef = useRef<Ann[][]>([]);
  const annRedoRef = useRef<Ann[][]>([]);
  const annClipboard = useRef<Ann[]>([]);
  const annBaselineRef = useRef<Ann[]>([]);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [ctxMenu, setCtxMenu] = useState<SheetContextReq | null>(null);

  /** Reload the overlay from the annotations already embedded in the working
   *  PDF, so reopens, saves and undo/redo always show what is on disk. */
  const refreshAnnotations = useCallback(
    async (id: string) => {
      try {
        const items = await invoke<PageAnnotation[]>("get_annotations", { id });
        const next = items.map((it) => specToAnn(it.spec, it.page));
        annotationsRef.current = next;
        annBaselineRef.current = next;
        annUndoRef.current = [];
        annRedoRef.current = [];
        setAnnotations(next);
      } catch (e) {
        showError(e);
      }
    },
    [showError]
  );

  const annCommit = useCallback((_page: number, ann: Ann) => {
    annUndoRef.current.push(annotationsRef.current);
    annRedoRef.current = [];
    const next = [...annotationsRef.current, ann];
    annotationsRef.current = next;
    setAnnotations(next);
  }, []);

  const annUpdate = useCallback((_page: number, updated: Ann) => {
    const prev = annotationsRef.current;
    const old = prev.find((a) => a.id === updated.id);
    if (!old || JSON.stringify(old) === JSON.stringify(updated)) return;
    annUndoRef.current.push(prev);
    annRedoRef.current = [];
    const next = prev.map((a) => (a.id === updated.id ? updated : a));
    annotationsRef.current = next;
    setAnnotations(next);
  }, []);

  const annDelete = useCallback((_page: number, id: string) => {
    const prev = annotationsRef.current;
    if (!prev.some((a) => a.id === id)) return;
    annUndoRef.current.push(prev);
    annRedoRef.current = [];
    const next = prev.filter((a) => a.id !== id);
    annotationsRef.current = next;
    setAnnotations(next);
  }, []);

  const annUndoLocal = useCallback(() => {
    const stack = annUndoRef.current;
    if (stack.length === 0) return;
    const prev = stack[stack.length - 1];
    annUndoRef.current = stack.slice(0, -1);
    annRedoRef.current.push(annotationsRef.current);
    annotationsRef.current = prev;
    setAnnotations(prev);
  }, []);

  const annRedoLocal = useCallback(() => {
    const stack = annRedoRef.current;
    if (stack.length === 0) return;
    const next = stack[stack.length - 1];
    annRedoRef.current = stack.slice(0, -1);
    annUndoRef.current.push(annotationsRef.current);
    annotationsRef.current = next;
    setAnnotations(next);
  }, []);

  /** Append several annotations in one undo step (paste / duplicate many). */
  const annCommitMany = useCallback((list: Ann[]) => {
    if (list.length === 0) return;
    annUndoRef.current.push(annotationsRef.current);
    annRedoRef.current = [];
    const next = [...annotationsRef.current, ...list];
    annotationsRef.current = next;
    setAnnotations(next);
  }, []);

  /** Remove several annotations in one undo step. */
  const annDeleteMany = useCallback((ids: string[]) => {
    const prev = annotationsRef.current;
    if (!prev.some((a) => ids.includes(a.id))) return;
    annUndoRef.current.push(prev);
    annRedoRef.current = [];
    const next = prev.filter((a) => !ids.includes(a.id));
    annotationsRef.current = next;
    setAnnotations(next);
  }, []);

  /** Re-stack annotations so the given ids draw last (front) or first (back). */
  const annReorder = useCallback((ids: string[], place: "front" | "back") => {
    const prev = annotationsRef.current;
    const moving = prev.filter((a) => ids.includes(a.id));
    if (moving.length === 0) return;
    const rest = prev.filter((a) => !ids.includes(a.id));
    const next = place === "front" ? [...rest, ...moving] : [...moving, ...rest];
    if (next.every((a, i) => a === prev[i])) return;
    annUndoRef.current.push(prev);
    annRedoRef.current = [];
    annotationsRef.current = next;
    setAnnotations(next);
  }, []);

  /** Flush pending overlay annotations into the working PDF. */
  const applyAnnotationsNow = useCallback(async (): Promise<DocSummary | null> => {
    const specs = annotationsRef.current;
    if (!doc || specs.length === 0) return null;
    const updated = await invoke<DocSummary>("apply_annotations", {
      id: doc.id,
      annotations: specs.map(annToSpec),
    });
    setDoc((prev) => (prev && prev.id === updated.id ? updated : prev));
    await refreshAnnotations(updated.id);
    return updated;
  }, [doc, refreshAnnotations]);

  const effectiveCanUndo = (doc?.canUndo ?? false) || annUndoRef.current.length > 0;
  const effectiveCanRedo = (doc?.canRedo ?? false) || annRedoRef.current.length > 0;
  const effectiveUndoLabel = annUndoRef.current.length > 0 ? "annotation" : (doc?.undoLabel ?? null);
  const effectiveRedoLabel = annRedoRef.current.length > 0 ? "annotation" : (doc?.redoLabel ?? null);

  /** True while the drawing overlay differs from what is saved in the PDF, so
   *  annotation-only edits still light up Save. */
  const annotationsDirty = useMemo(
    () => JSON.stringify(annotations) !== JSON.stringify(annBaselineRef.current),
    [annotations]
  );
  const effectiveModified = (doc?.isModified ?? false) || annotationsDirty;

  const refreshRecents = useCallback(async () => {
    try {
      setRecents(await invoke<RecentFile[]>("get_recent_files"));
    } catch (e) {
      showError(e);
    }
  }, [showError]);

  useEffect(() => {
    let unlistenFn: (() => void) | null = null;

    void (async () => {
      try {
        setAppInfo(await invoke<AppInfo>("app_info"));
      } catch (e) {
        showError(e);
      }
    })();

    void refreshRecents();

    // Guard: listen() requires the Tauri IPC bridge to be ready.
    // Capturing the unlisten handle in an async IIFE prevents an unhandled
    // rejection from the transformCallback race during Vite HMR fast-refresh.
    void (async () => {
      try {
        unlistenFn = await listen<string[]>("britpdf:open-requested", (event) => {
          const paths = event.payload ?? [];
          if (paths.length > 0) void openPath(paths[0]);
        });
      } catch (e) {
        // IPC bridge not yet ready (HMR edge case) — non-fatal, the listener
        // will be re-registered on the next mount.
        console.warn("[BritPDF] Could not attach open-requested listener:", e);
      }
    })();

    return () => {
      if (unlistenFn) unlistenFn();
      if (toastTimer.current) window.clearTimeout(toastTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadDocument = useCallback(async (summary: DocSummary) => {
    setDoc(summary);
    setCurrent(0);
    setSelected({ kind: "doc" });
    setZoom({ mode: "fitPage", pct: 100 });
    await refreshAnnotations(summary.id);
    setViewRot(0);
    setOutline([]);
    searchCache.current = null;
    setSearchQuery("");
    setSearchStatus("idle");
    setSearchResults([]);
    setError(null);
    try {
      const geos = await invoke<PageGeometry[]>("list_pages", { id: summary.id });
      setGeometries(geos);
    } catch (e) {
      showError(e);
    }
  }, [refreshAnnotations, showError]);

  const openPath = useCallback(
    async (path: string) => {
      setBusy("Opening…");
      setError(null);
      try {
        let summary: DocSummary;
        try {
          summary = await invoke<DocSummary>("open_document", { path });
        } catch (e) {
          if (!isPasswordRequired(e)) throw e;
          const password = window.prompt("This PDF is password protected. Enter its open password:", "") ?? "";
          if (!password) {
            setBusy(null);
            return;
          }
          summary = await invoke<DocSummary>("open_document_with_password", { path, password });
        }
        await loadDocument(summary);
        void refreshRecents();
      } catch (e) {
        showError(e);
      } finally {
        setBusy(null);
      }
    },
    [loadDocument, refreshRecents, showError]
  );

  const pickAndOpen = useCallback(async () => {
    try {
      const picked = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "PDF files", extensions: ["pdf"] }],
      });
      if (typeof picked === "string" && picked.length > 0) {
        await openPath(picked);
      }
    } catch (e) {
      showError(e);
    }
  }, [openPath, showError]);

  const doSaveAs = useCallback(async () => {
    if (!doc) return;
    setBusy("Saving…");
    setError(null);
    try {
      await applyAnnotationsNow();
      const picked = await save({
        defaultPath: doc.fileName.endsWith(".pdf") || doc.fileName.endsWith(".PDF") ? doc.fileName : `${doc.fileName}.pdf`,
        filters: [{ name: "PDF files", extensions: ["pdf"] }],
      });
      if (typeof picked === "string" && picked.length > 0) {
        const updated = await invoke<DocSummary>("save_document_as", { id: doc.id, path: picked });
        setDoc(updated);
        showToast(`Saved to ${updated.fileName}`);
      }
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, applyAnnotationsNow, showError, showToast]);

  const doSave = useCallback(async () => {
    if (!doc) return;
    if (!doc.path) {
      await doSaveAs();
      return;
    }
    setBusy("Saving…");
    setError(null);
    try {
      await applyAnnotationsNow();
      const updated = await invoke<DocSummary>("save_document", { id: doc.id });
      setDoc(updated);
      showToast("Document saved");
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, doSaveAs, applyAnnotationsNow, showError, showToast]);

  const doPrint = useCallback(async () => {
    if (!doc) return;
    setBusy("Printing…");
    setError(null);
    try {
      const res = await invoke<{ jobName: string; pagesSent: number; sheets: number }>("print_document", {
        id: doc.id,
      });
      showToast(`Sent ${res.sheets} sheet${res.sheets === 1 ? "" : "s"} to the printer`);
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, showError, showToast]);

  /** After undo/redo the engine reloads the PDF, so page geometry is refetched. */
  const reloadAfterHistory = useCallback(
    async (summary: DocSummary) => {
      setDoc(summary);
      searchCache.current = null;
      setSearchQuery("");
      setSearchStatus("idle");
      setSearchResults([]);
      try {
        const geos = await invoke<PageGeometry[]>("list_pages", { id: summary.id });
        setGeometries(geos);
        await refreshAnnotations(summary.id);
      } catch (e) {
        showError(e);
      }
    },
    [showError, refreshAnnotations]
  );

  const doUndo = useCallback(async () => {
    if (annUndoRef.current.length > 0) {
      annUndoLocal();
      return;
    }
    if (!doc?.canUndo) return;
    setBusy("Undoing…");
    setError(null);
    try {
      const summary = await invoke<DocSummary>("undo_document", { id: doc.id });
      await reloadAfterHistory(summary);
      showToast(`Undid ${summary.undoLabel ? `“${summary.undoLabel}”` : "the last change"}`);
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, annUndoLocal, reloadAfterHistory, showError, showToast]);

  const doRedo = useCallback(async () => {
    if (annRedoRef.current.length > 0) {
      annRedoLocal();
      return;
    }
    if (!doc?.canRedo) return;
    setBusy("Redoing…");
    setError(null);
    try {
      const summary = await invoke<DocSummary>("redo_document", { id: doc.id });
      await reloadAfterHistory(summary);
      showToast("Redid the last change");
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, annRedoLocal, reloadAfterHistory, showError, showToast]);

  const ensureSearchCache = useCallback(async () => {
    if (!doc || searchCache.current) return;
    const bounds: number[] = [];
    const texts: string[] = [];
    for (let i = 0; i < geometries.length; i++) {
      try {
        const t = await invoke<PageText>("page_text", { id: doc.id, page: i });
        texts.push(t.text);
      } catch {
        texts.push("");
      }
    }
    const full = texts.join("\n\n");
    let acc = 0;
    for (const t of texts) {
      bounds.push(acc);
      acc += t.length + 2;
    }
    searchCache.current = { boundaries: bounds, full };
  }, [doc, geometries.length]);

  const runSearch = useCallback(
    async (raw: string) => {
      const q = raw.trim().toLowerCase();
      if (!doc || q.length === 0) {
        setSearchStatus("idle");
        setSearchResults([]);
        return;
      }
      const token = ++searchSeq.current;
      setSearchStatus("searching");
      try {
        await ensureSearchCache();
        if (searchSeq.current !== token) return;
        const { boundaries, full } = searchCache.current ?? { boundaries: [], full: "" };
        const hay = full.toLowerCase();
        const hits: SearchHit[] = [];
        let from = 0;
        let count = 0;
        while (count < 200) {
          const idx = hay.indexOf(q, from);
          if (idx < 0) break;
          let page = 0;
          for (let i = boundaries.length - 1; i >= 0; i--) {
            if (boundaries[i] <= idx) {
              page = i;
              break;
            }
          }
          const start = Math.max(0, idx - 36);
          const snippet = full
            .slice(start, idx + q.length + 72)
            .replace(/\s+/g, " ")
            .trim();
          hits.push({ page, snippet });
          from = idx + q.length;
          count++;
        }
        if (searchSeq.current !== token) return;
        setSearchResults(hits);
        setSearchStatus(hits.length > 0 ? "done" : "none");
      } catch (e) {
        if (searchSeq.current !== token) return;
        setSearchStatus("none");
        showError(e);
      }
    },
    [doc, ensureSearchCache, showError]
  );

  const onSearchChange = (v: string) => {
    setSearchQuery(v);
    setSearchStatus("idle");
    if (v.trim().length === 0) {
      setSearchResults([]);
      return;
    }
    window.setTimeout(() => void runSearch(v), 320);
  };

  const onSearchHit = (hit: SearchHit) => {
    goToPage(hit.page);
  };

  const searchNext = () => {
    if (searchResults.length > 0) {
      const idx = searchResults.findIndex((r) => r.page >= current);
      void onSearchHit(searchResults[idx >= 0 ? idx : 0]);
    } else {
      void runSearch(searchQuery);
    }
  };

  const goToPage = useCallback((page: number) => {
    setCurrent(page);
    setSelected({ kind: "page", page });
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLElement>(`[data-page="${page}"]`);
      el?.scrollIntoView({ behavior: "smooth", block: "start" });
    });
  }, []);

  const prevPage = useCallback(() => {
    if (current > 0) goToPage(current - 1);
  }, [current, goToPage]);

  const nextPage = useCallback(() => {
    if (doc && current < doc.pageCount - 1) goToPage(current + 1);
  }, [current, doc, goToPage]);

  /* ---------------------- page operations & dialogs ---------------------- */
  const runMutation = useCallback(
    async (fn: () => Promise<DocSummary>, done: string) => {
      await applyAnnotationsNow();
      const updated = await fn();
      setDoc((prev) => (prev && prev.id === updated.id ? updated : prev));
      const geos = await invoke<PageGeometry[]>("list_pages", { id: updated.id });
      setGeometries(geos);
      await refreshAnnotations(updated.id);
      if (geos.length === 0) {
        setDoc(null);
        return;
      }
      setCurrent((c) => (c >= geos.length ? geos.length - 1 : c));
      setSelected((s) => (s.kind === "page" && s.page >= geos.length ? { kind: "page", page: geos.length - 1 } : s));
      showToast(done);
    },
    [applyAnnotationsNow, refreshAnnotations, showToast]
  );

  const insertBlank = useCallback(async () => {
    if (!doc) return;
    setBusy("Inserting a blank page…");
    setError(null);
    try {
      await runMutation(
        () => invoke<DocSummary>("insert_blank_page", { id: doc.id, before: current + 1 }),
        "Inserted a blank page"
      );
      setCurrent((c) => c + 1);
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, current, runMutation, showError]);

  const insertFromFile = useCallback(async () => {
    if (!doc) return;
    try {
      const picked = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "PDF files", extensions: ["pdf"] }],
      });
      if (typeof picked !== "string" || picked.length === 0) return;
      setBusy("Inserting pages…");
      setError(null);
      try {
        await runMutation(
          () => invoke<DocSummary>("insert_pdf_from", { id: doc.id, path: picked, before: current + 1 }),
          "Inserted pages from file"
        );
      } catch (e) {
        showError(e);
      } finally {
        setBusy(null);
      }
    } catch (e) {
      showError(e);
    }
  }, [doc, current, runMutation, showError]);

  const duplicateAt = useCallback(async (page: number) => {
    if (!doc) return;
    setBusy("Duplicating page…");
    setError(null);
    try {
      await runMutation(
        () => invoke<DocSummary>("duplicate_page", { id: doc.id, page }),
        "Duplicated the page"
      );
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, runMutation, showError]);

  const deletePage = useCallback(
    async (page?: number) => {
      if (!doc) return;
      const target = page ?? current;
      if (doc.pageCount <= 1) {
        showToast("A document must keep at least one page.");
        return;
      }
      setBusy("Deleting page…");
      setError(null);
      try {
        await runMutation(
          () => invoke<DocSummary>("delete_pages", { id: doc.id, pages: [target] }),
          "Deleted the page"
        );
      } catch (e) {
        showError(e);
      } finally {
        setBusy(null);
      }
    },
    [doc, current, runMutation, showError, showToast]
  );

  const rotateAt = useCallback(async (page: number) => {
    if (!doc) return;
    setBusy("Rotating page…");
    setError(null);
    try {
      await runMutation(
        () => invoke<DocSummary>("rotate_pages", { id: doc.id, pages: [page], degrees: 90 }),
        "Rotated the page"
      );
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, runMutation, showError]);

  const extractPages = useCallback(async () => {
    if (!doc) return;
    const input = window.prompt("Pages to extract (e.g. 1-3, 7):", String(current + 1));
    if (!input) return;
    const indices = parsePageRange(input, doc.pageCount);
    if (indices.length === 0) {
      showToast("No valid pages in that range.");
      return;
    }
    try {
      const picked = await save({
        defaultPath: "extracted-pages.pdf",
        filters: [{ name: "PDF files", extensions: ["pdf"] }],
      });
      if (typeof picked !== "string" || picked.length === 0) return;
      setBusy("Extracting pages…");
      setError(null);
      const res = await invoke<{ path: string; pages: number }>("extract_pages_to", { id: doc.id, pages: indices, path: picked });
      showToast(`Extracted ${res.pages} page${res.pages === 1 ? "" : "s"} to ${res.path.split(/[\\/]/).pop()}`);
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, current, showError, showToast]);

  const rotateView = useCallback(() => {
    setViewRot((r) => (r + 90) % 360);
  }, []);

  const insertImage = useCallback(async () => {
    if (!doc || geometries.length === 0) return;
    const geo = geometries[current] ?? geometries[0];
    const pw = geo.raw_width || geo.width;
    const ph = geo.raw_height || geo.height;
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/*";
    input.onchange = () => {
      const file = input.files?.[0];
      input.value = "";
      if (!file) return;
      const reader = new FileReader();
      reader.onload = () => {
        const dataUrl = typeof reader.result === "string" ? reader.result : null;
        if (!dataUrl) return;
        setBusy("Placing image…");
        setError(null);
        void (async () => {
          try {
            await runMutation(
              () =>
                invoke<DocSummary>("place_image", { id: doc.id, page: current, dataUrl, x: pw * 0.2, y: ph * 0.4, w: pw * 0.6, h: 0 }),
              "Placed image"
            );
          } catch (e) {
            showError(e);
          } finally {
            setBusy(null);
          }
        })();
      };
      reader.readAsDataURL(file);
    };
    input.click();
  }, [doc, current, geometries, runMutation, showError]);

  const refreshOutline = useCallback(async () => {
    if (!doc) return;
    try {
      setOutline(await invoke<OutlineEntry[]>("get_outline", { id: doc.id }));
    } catch {
      setOutline([]);
    }
  }, [doc]);

  const toggleBookmarks = useCallback(() => {
    setBookmarksOpen((v) => {
      if (!v) void refreshOutline();
      return !v;
    });
  }, [refreshOutline]);

  const noteAnns = useMemo(
    () => annotations.filter((a) => a.kind === "stickyNote" || a.kind === "comment" || a.kind === "callout" || a.kind === "TextBox"),
    [annotations]
  );

  const applyHeaderFooter = useCallback(async (options: HeaderFooterOptions) => {
    if (!doc) return;
    setBusy("Adding header & footer…");
    setError(null);
    try {
      await runMutation(
        () => invoke<DocSummary>("add_header_footer", { id: doc.id, options }),
        "Added header & footer"
      );
      setDialog("none");
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, runMutation, showError]);

  const applyPageNumbers = useCallback(
    async (position: PageNumberKind, format: PageNumberFmt, pages: number[] | null) => {
      if (!doc) return;
      setBusy("Adding page numbers…");
      setError(null);
      try {
        await runMutation(
          () => invoke<DocSummary>("add_page_numbers", { id: doc.id, position, format, pages }),
          "Added page numbers"
        );
        setDialog("none");
      } catch (e) {
        showError(e);
      } finally {
        setBusy(null);
      }
    },
    [doc, runMutation, showError]
  );

  const applyWatermark = useCallback(async (options: WatermarkOptions) => {
    if (!doc) return;
    setBusy("Adding watermark…");
    setError(null);
    try {
      await runMutation(
        () => invoke<DocSummary>("add_watermark", { id: doc.id, options }),
        "Added watermark"
      );
      setDialog("none");
    } catch (e) {
      showError(e);
    } finally {
      setBusy(null);
    }
  }, [doc, runMutation, showError]);

  const toggleFullscreen = useCallback(async () => {
    try {
      const entering = !fullscreen;
      if (entering) await win().setFullscreen(true);
      else await win().setFullscreen(false);
      setFullscreen(entering);
    } catch {
      showToast("Full screen is not available right now.");
    }
  }, [fullscreen, showToast]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "o") {
        e.preventDefault();
        void pickAndOpen();
      } else if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void doSave();
      } else if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void doSaveAs();
      } else if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "p") {
        e.preventDefault();
        void doPrint();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
        e.preventDefault();
        void doUndo();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "y") {
        e.preventDefault();
        void doRedo();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "g") {
        e.preventDefault();
        searchNext();
      } else if (e.key === "ArrowDown" || e.key === "PageDown") {
        nextPage();
      } else if (e.key === "ArrowUp" || e.key === "PageUp") {
        prevPage();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [current, doc, doSave, doSaveAs, doPrint, doUndo, doRedo, pickAndOpen]);

  const onSelectPage = useCallback((page: number) => {
    setCurrent(page);
    setSelected({ kind: "page", page });
  }, []);

  const onTool = (t: ToolId) => {
    setTool(t);
  };

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-[#eef0f3] text-slate-800">
      <TitleBar
        appInfo={appInfo}
        doc={doc}
        dirty={effectiveModified}
        searchRef={searchRef}
        searchQuery={searchQuery}
        searchStatus={searchStatus}
        searchResults={searchResults}
        onSearchChange={onSearchChange}
        onSearchEnter={searchNext}
        onSearchHit={onSearchHit}
        onCloseSearch={() => {
          setSearchQuery("");
          setSearchStatus("idle");
          setSearchResults([]);
        }}
      />

      <Toolbar
        doc={doc}
        dirty={effectiveModified}
        tool={tool}
        onTool={onTool}
        tab={tab}
        onTab={setTab}
        onOpen={() => void pickAndOpen()}
        onSave={() => void doSave()}
        onSaveAs={() => void doSaveAs()}
        onPrint={() => void doPrint()}
        onUndo={() => void doUndo()}
        onRedo={() => void doRedo()}
        thumbOpen={thumbOpen}
        propsOpen={propsOpen}
        onToggleThumbs={() => setThumbOpen((v) => !v)}
        onToggleProps={() => setPropsOpen((v) => !v)}
        zoom={zoom}
        onZoom={setZoom}
        current={current}
        onGoToPage={goToPage}
        onPrevPage={prevPage}
        onNextPage={nextPage}
        layout={layout}
        onLayout={setLayout}
        fullscreen={fullscreen}
        onToggleFullscreen={toggleFullscreen}
        onFocusSearch={() => {
          searchRef.current?.focus();
          searchRef.current?.select();
        }}
        onToast={showToast}
        settings={annSettings}
        onSettings={(p) => setAnnSettings((s) => ({ ...s, ...p }))}
        canUndo={effectiveCanUndo}
        canRedo={effectiveCanRedo}
        undoLabel={effectiveUndoLabel}
        redoLabel={effectiveRedoLabel}
        onInsertBlank={() => void insertBlank()}
        onInsertFromFile={() => void insertFromFile()}
        onDuplicatePage={() => void duplicateAt(current)}
        onDeletePage={() => void deletePage()}
        onRotatePage={() => void rotateAt(current)}
        onExtractPages={() => void extractPages()}
        onInsertImage={() => void insertImage()}
        onHeaderFooter={() => setDialog("hf")}
        onPageNumbers={() => setDialog("pageNumbers")}
        onWatermark={() => setDialog("watermark")}
        onRotateView={rotateView}
        onToggleBookmarks={toggleBookmarks}
        onToggleLayers={() => setLayersOpen((v) => !v)}
        onToggleNotes={() => setNotesOpen((v) => !v)}
        onToggleStatusBar={() => setStatusBarVisible((v) => !v)}
      />

      {error && (
        <div className="flex shrink-0 items-center gap-2 border-b border-red-200 bg-red-50 px-4 py-1.5 text-[12px] text-red-800">
          <span className="min-w-0 flex-1 truncate">{error}</span>
          <button
            className="rounded px-1.5 py-0.5 text-[11px] font-medium hover:bg-red-100"
            onClick={() => setError(null)}
          >
            Dismiss
          </button>
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        {!doc ? (
          <Welcome
            appInfo={appInfo}
            recents={recents}
            onOpen={() => void pickAndOpen()}
            onRecent={(p) => void openPath(p)}
            onClearRecents={() => {
              void (async () => {
                try {
                  await invoke("clear_recent_files");
                } catch (e) {
                  showError(e);
                }
                void refreshRecents();
              })();
            }}
          />
        ) : (
          <>
            {thumbOpen ? (
              <ThumbPanel
                id={doc.id}
                geometries={geometries}
                current={current}
                onGo={goToPage}
                onClose={() => setThumbOpen(false)}
                onToast={showToast}
                onDuplicate={(p) => void duplicateAt(p)}
                onDelete={(p) => void deletePage(p)}
                onRotate={(p) => void rotateAt(p)}
              />
            ) : (
              <button
                className="flex w-8 shrink-0 flex-col items-center gap-1 border-r border-slate-200 bg-[#f4f5f7] py-2 text-slate-400 hover:text-brand-600"
                onClick={() => setThumbOpen(true)}
                title="Show thumbnails"
              >
                <Icon name="chevronRight" className="h-3.5 w-3.5" />
                <Icon name="doc" className="h-3 w-3" />
              </button>
            )}

{bookmarksOpen && (
              <BookmarksPanel outline={outline} onGoToPage={(p) => goToPage(p)} onClose={() => setBookmarksOpen(false)} />
            )}
            {layersOpen && <LayersPanel onClose={() => setLayersOpen(false)} />}
            {notesOpen && <NotesPanel notes={noteAnns} onGoToPage={goToPage} onClose={() => setNotesOpen(false)} />}

            <Workspace
              doc={doc}
              geometries={geometries}
              zoom={zoom}
              tool={tool}
              layout={layout}
              currentPage={current}
              viewRot={viewRot}
              annotations={annotations}
              settings={annSettings}
              selectedIds={selectedIds}
              onSelectIds={setSelectedIds}
              onContextMenu={setCtxMenu}
              onSelect={onSelectPage}
              onCurrent={setCurrent}
              onCommitAnn={annCommit}
              onUpdateAnn={annUpdate}
              onDeleteAnn={annDelete}
              onToast={showToast}
              onMeasure={setMeasureMsg}
            />

            {propsOpen ? (
              <PropsPanel
                doc={doc}
                dirty={effectiveModified}
                selected={selected}
                geometries={geometries}
                onClose={() => setPropsOpen(false)}
              />
            ) : (
              <button
                className="flex w-8 shrink-0 flex-col items-center gap-1 border-l border-slate-200 bg-white py-2 text-slate-400 hover:text-brand-600"
                onClick={() => setPropsOpen(true)}
                title="Show properties"
              >
                <Icon name="chevronLeft" className="h-3.5 w-3.5" />
                <span className="text-[9px] font-medium">Info</span>
              </button>
            )}
          </>
        )}
      </div>

      {doc && statusBarVisible && (
        <StatusBar
          current={current}
          pageCount={doc.pageCount}
          zoom={zoom}
          busy={busy}
          message={measureMsg}
          onZoom={setZoom}
          onPrev={prevPage}
          onNext={nextPage}
        />
      )}

      {doc && dialog === "hf" && (
        <HeaderFooterDialog
          pageCount={doc.pageCount}
          initial={DEFAULT_HF_OPTIONS}
          onApply={applyHeaderFooter}
          onClose={() => setDialog("none")}
        />
      )}
      {doc && dialog === "pageNumbers" && (
        <PageNumbersDialog
          pageCount={doc.pageCount}
          onApply={applyPageNumbers}
          onClose={() => setDialog("none")}
        />
      )}
      {doc && dialog === "watermark" && (
        <WatermarkDialog
          pageCount={doc.pageCount}
          onApply={applyWatermark}
          onClose={() => setDialog("none")}
        />
      )}

      {ctxMenu && (() => {
        const pageIds = annotationsRef.current.filter((a) => a.page === ctxMenu.page).map((a) => a.id);
        const targetIds = selectedIds.length ? selectedIds : ctxMenu.annId ? [ctxMenu.annId] : [];
        const targets = annotationsRef.current.filter((a) => targetIds.includes(a.id));
        const clip = annClipboard.current;
        const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;
        const textColour = annSettings.color.toLowerCase() === "transparent" ? "#1e293b" : annSettings.color;
        const items: CtxItem[] = [
          { key: "undo", label: effectiveUndoLabel ? `Undo ${effectiveUndoLabel}` : "Undo", icon: "undo", shortcut: "Ctrl+Z", disabled: !effectiveCanUndo, onSelect: () => void doUndo() },
          { key: "redo", label: effectiveRedoLabel ? `Redo ${effectiveRedoLabel}` : "Redo", icon: "redo", shortcut: "Ctrl+Y", disabled: !effectiveCanRedo, onSelect: () => void doRedo() },
          { key: "s1", sep: true },
          {
            key: "cut", label: "Cut", shortcut: "Ctrl+X", disabled: targets.length === 0,
            onSelect: () => { annClipboard.current = targets.map((a) => ({ ...a })); annDeleteMany(targetIds); setSelectedIds([]); showToast(`Cut ${plural(targets.length, "annotation")}`); },
          },
          {
            key: "copy", label: "Copy", icon: "copy", shortcut: "Ctrl+C", disabled: targets.length === 0,
            onSelect: () => { annClipboard.current = targets.map((a) => ({ ...a })); showToast(`Copied ${plural(targets.length, "annotation")}`); },
          },
          {
            key: "paste", label: "Paste", shortcut: "Ctrl+V", disabled: clip.length === 0,
            onSelect: () => { const clones = clip.map((a) => cloneAnn(a, ctxMenu.page, 12, 12)); annCommitMany(clones); setSelectedIds(clones.map((c) => c.id)); showToast(`Pasted ${plural(clones.length, "annotation")}`); },
          },
          {
            key: "dup", label: "Duplicate", icon: "duplicate", disabled: targets.length === 0,
            onSelect: () => { const clones = targets.map((a) => cloneAnn(a, a.page, 12, 12)); annCommitMany(clones); setSelectedIds(clones.map((c) => c.id)); showToast(`Duplicated ${plural(clones.length, "annotation")}`); },
          },
          {
            key: "del", label: "Delete", icon: "trash", shortcut: "Del", disabled: targets.length === 0, danger: true,
            onSelect: () => { annDeleteMany(targetIds); setSelectedIds([]); showToast(`Deleted ${plural(targets.length, "annotation")}`); },
          },
          { key: "s2", sep: true },
          { key: "front", label: "Bring to Front", disabled: targets.length === 0, onSelect: () => annReorder(targetIds, "front") },
          { key: "back", label: "Send to Back", disabled: targets.length === 0, onSelect: () => annReorder(targetIds, "back") },
          { key: "s3", sep: true },
          {
            key: "tbox", label: "Add Text Box", icon: "textBox",
            onSelect: () => {
              const ann: Ann = {
                id: nextAnnId(), page: ctxMenu.page, kind: "textBox",
                rect: box(ctxMenu.px, ctxMenu.py, ctxMenu.px + 180, ctxMenu.py + 44),
                points: [], quads: [], color: textColour, thickness: 1, opacity: 1,
                font: annSettings.font, fontSize: annSettings.fontSize,
                textStyle: { bold: annSettings.bold, italic: annSettings.italic, underline: annSettings.underline, strike: annSettings.strike },
                contents: "", created: Date.now(),
              };
              annCommitMany([ann]);
              setSelectedIds([ann.id]);
              showToast("Added text box");
            },
          },
          {
            key: "hl", label: "Highlight", icon: "highlight",
            onSelect: () => {
              const ann: Ann = {
                id: nextAnnId(), page: ctxMenu.page, kind: "highlight",
                rect: box(ctxMenu.px - 90, ctxMenu.py - 8, ctxMenu.px + 90, ctxMenu.py + 8),
                points: [], quads: [], color: textColour.toLowerCase() === "transparent" ? "#fde047" : textColour,
                thickness: Math.max(2, annSettings.thickness), opacity: 0.45,
                font: annSettings.font, fontSize: annSettings.fontSize, created: Date.now(),
              };
              annCommitMany([ann]);
              setSelectedIds([ann.id]);
              showToast("Added highlight");
            },
          },
          { key: "selall", label: "Select All", disabled: pageIds.length === 0, onSelect: () => setSelectedIds(pageIds) },
          { key: "s4", sep: true },
          { key: "zin", label: "Zoom In", icon: "zoomIn", shortcut: "Ctrl++", onSelect: () => setZoom({ mode: "pct", pct: Math.min(400, zoom.pct + 10) }) },
          { key: "zout", label: "Zoom Out", icon: "zoomOut", shortcut: "Ctrl+-", onSelect: () => setZoom({ mode: "pct", pct: Math.max(10, zoom.pct - 10) }) },
          { key: "fitw", label: "Fit Width", icon: "fitWidth", onSelect: () => setZoom({ mode: "fitWidth", pct: zoom.pct }) },
          { key: "fitp", label: "Fit Page", icon: "fitPage", onSelect: () => setZoom({ mode: "fitPage", pct: zoom.pct }) },
          { key: "s5", sep: true },
          { key: "rotl", label: "Rotate Left", icon: "rotate", onSelect: () => setViewRot((r) => (r - 90 + 360) % 360) },
          { key: "rotr", label: "Rotate Right", icon: "rotate", onSelect: () => setViewRot((r) => (r + 90) % 360) },
          { key: "s6", sep: true },
          { key: "save", label: "Save", icon: "save", shortcut: "Ctrl+S", disabled: !effectiveModified, onSelect: () => void doSave() },
          { key: "saveas", label: "Save As…", icon: "saveAs", onSelect: () => void doSaveAs() },
          { key: "print", label: "Print…", icon: "print", shortcut: "Ctrl+P", onSelect: () => void doPrint() },
        ];
        return <ContextMenu x={ctxMenu.clientX} y={ctxMenu.clientY} items={items} onClose={() => setCtxMenu(null)} />;
      })()}

      {toast && (
        <div className="pointer-events-none fixed bottom-12 right-4 z-50 rounded-md border border-slate-200 bg-slate-800 px-3 py-2 text-[12px] font-medium text-white shadow-lg">
          {toast}
        </div>
      )}
    </div>
  );
}

/* --------------------------- dialogs & panes --------------------------- */

interface CtxItem {
  key: string;
  label?: string;
  icon?: string;
  shortcut?: string;
  disabled?: boolean;
  danger?: boolean;
  sep?: boolean;
  onSelect?: () => void;
}

/** The PDF right-click menu: a portal-positioned list clamped to the viewport. */
function ContextMenu({ x, y, items, onClose }: { x: number; y: number; items: CtxItem[]; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      x: Math.max(8, Math.min(x, window.innerWidth - r.width - 8)),
      y: Math.max(8, Math.min(y, window.innerHeight - r.height - 8)),
    });
  }, [x, y]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onAway = () => onClose();
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("resize", onAway);
    window.addEventListener("scroll", onAway, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("resize", onAway);
      window.removeEventListener("scroll", onAway, true);
    };
  }, [onClose]);

  return createPortal(
    <div
      ref={ref}
      className="fixed z-[80] min-w-[232px] rounded-md border border-slate-200 bg-white py-1 shadow-xl"
      style={{ left: pos.x, top: pos.y }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) =>
        it.sep ? (
          <div key={`sep-${i}`} className="my-1 h-px bg-slate-100" />
        ) : (
          <button
            key={it.key}
            disabled={it.disabled}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => {
              if (it.disabled) return;
              it.onSelect?.();
              onClose();
            }}
            className={`flex w-full items-center gap-2.5 px-3 py-[6px] text-left text-[12px] ${
              it.disabled
                ? "cursor-default text-slate-300"
                : it.danger
                  ? "text-red-600 hover:bg-red-50"
                  : "text-slate-700 hover:bg-slate-100"
            }`}
          >
            <span className="flex h-4 w-4 shrink-0 items-center justify-center">
              {it.icon ? <Icon name={it.icon} className="h-4 w-4" /> : null}
            </span>
            <span className="flex-1 truncate">{it.label}</span>
            {it.shortcut ? <span className="ml-4 shrink-0 text-[10.5px] text-slate-400">{it.shortcut}</span> : null}
          </button>
        )
      )}
    </div>,
    document.body
  );
}

function DialogShell({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  return createPortal(
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-slate-900/30" onClick={onClose}>
      <div
        className="max-h-[86vh] w-[560px] overflow-y-auto rounded-lg border border-slate-200 bg-white shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-slate-100 px-4 py-2.5">
          <h2 className="text-[13px] font-semibold text-slate-800">{title}</h2>
          <button className="rounded p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600" onClick={onClose} title="Close">
            <Icon name="close" className="h-4 w-4" />
          </button>
        </div>
        <div className="px-4 py-3">{children}</div>
      </div>
    </div>,
    document.body
  );
}

const dialField = "h-[24px] rounded border border-slate-300 bg-white px-2 text-[11.5px] text-slate-700 focus:border-brand-400 focus:outline-none";
const dialLabel = "mb-0.5 block text-[10.5px] font-medium text-slate-500";

function parsePages(v: string, count: number): number[] | null {
  const trimmed = v.trim();
  if (!trimmed) return null;
  const parsed = parsePageRange(trimmed, count);
  return parsed.length > 0 ? parsed : null;
}

function HexSwatch({ value, onChange }: { value: string; onChange: (hex: string) => void }) {
  return (
    <div className="flex items-center gap-2">
      <input type="color" className="h-6 w-9 cursor-pointer rounded border border-slate-300 bg-white p-0.5" value={value} onChange={(e) => onChange(e.target.value)} title="Colour" />
      <input className={`${dialField} w-[74px]`} value={value} onChange={(e) => onChange(e.target.value)} spellCheck={false} />
    </div>
  );
}

function HeaderFooterDialog({
  pageCount,
  initial,
  onApply,
  onClose,
}: {
  pageCount: number;
  initial: HeaderFooterOptions;
  onApply: (o: HeaderFooterOptions) => void;
  onClose: () => void;
}) {
  const [opt, setOpt] = useState<HeaderFooterOptions>({
    ...initial,
    header_left: initial.header_left ?? "",
    header_center: initial.header_center ?? "",
    header_right: initial.header_right ?? "",
    footer_left: initial.footer_left ?? "",
    footer_center: initial.footer_center ?? "",
    footer_right: initial.footer_right ?? "",
  });
  const [pages, setPages] = useState("");
  const [colorHex, setColorHex] = useState("#595959");
  const set = <K extends keyof HeaderFooterOptions>(k: K, v: HeaderFooterOptions[K]) => setOpt((o) => ({ ...o, [k]: v }));
  const slot = (label: string, key: keyof HeaderFooterOptions) => (
    <div>
      <label className={dialLabel}>{label}</label>
      <input className={`${dialField} w-full`} value={(opt[key] as string) ?? ""} onChange={(e) => set(key, e.target.value)} placeholder="Leave blank to skip" />
    </div>
  );
  return (
    <DialogShell title="Header & Footer" onClose={onClose}>
      <p className="mb-2 text-[11px] text-slate-400">Tick “Page number” to draw the current page number at the end of each filled slot. An empty range adds the header/footer to every page.</p>
      <div className="grid grid-cols-3 gap-3">
        {slot("Header — left", "header_left")}
        {slot("Header — center", "header_center")}
        {slot("Header — right", "header_right")}
        {slot("Footer — left", "footer_left")}
        {slot("Footer — center", "footer_center")}
        {slot("Footer — right", "footer_right")}
      </div>
      <div className="my-3 border-t border-slate-100" />
      <div className="grid grid-cols-2 gap-x-4 gap-y-2">
        <label className="flex items-center gap-2 text-[11.5px] text-slate-700">
          <input type="checkbox" className="accent-brand-600" checked={opt.use_page_number} onChange={(e) => set("use_page_number", e.target.checked)} /> Page number
        </label>
        <label className="flex items-center gap-2 text-[11.5px] text-slate-700">
          <input type="checkbox" className="accent-brand-600" checked={opt.show_page_count} onChange={(e) => set("show_page_count", e.target.checked)} /> Page X of Y
        </label>
        <div>
          <label className={dialLabel}>Date format</label>
          <select className={`${dialField} w-full`} value={opt.date_format} onChange={(e) => set("date_format", e.target.value)}>
            <option value="">No date</option>
            <option value="%Y-%m-%d">2026-10-09</option>
            <option value="%m/%d/%Y">10/09/2026</option>
            <option value="%B %d, %Y">October 9, 2026</option>
          </select>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className={dialLabel}>Font size</label>
            <input type="number" min={6} max={72} className={`${dialField} w-full`} value={opt.font_size} onChange={(e) => set("font_size", Number(e.target.value))} />
          </div>
          <div>
            <label className={dialLabel}>Margin (pt)</label>
            <input type="number" min={8} max={200} className={`${dialField} w-full`} value={opt.margin_points} onChange={(e) => set("margin_points", Number(e.target.value))} />
          </div>
        </div>
        <div>
          <label className={dialLabel}>Colour</label>
          <HexSwatch value={colorHex} onChange={setColorHex} />
        </div>
        <div>
          <label className={dialLabel}>Pages (e.g. 1-3, 7)</label>
          <input className={`${dialField} w-full`} value={pages} onChange={(e) => setPages(e.target.value)} placeholder={`All ${pageCount} pages`} />
        </div>
      </div>
      <DialogActions
        onCancel={onClose}
        onConfirm={() => {
          onApply({
            ...opt,
            color: hexToRgb01(colorHex),
            pages: parsePages(pages, pageCount),
          });
        }}
        confirmLabel="Add"
      />
    </DialogShell>
  );
}

function PageNumbersDialog({
  pageCount,
  onApply,
  onClose,
}: {
  pageCount: number;
  onApply: (position: PageNumberKind, format: PageNumberFmt, pages: number[] | null) => void;
  onClose: () => void;
}) {
  const [position, setPosition] = useState<PageNumberKind>("bottomCenter");
  const [format, setFormat] = useState<PageNumberFmt>("plain");
  const [pages, setPages] = useState("");
  return (
    <DialogShell title="Page Numbers" onClose={onClose}>
      <div className="grid grid-cols-3 gap-3">
        <div className="col-span-2">
          <label className={dialLabel}>Position</label>
          <select className={`${dialField} w-full`} value={position} onChange={(e) => setPosition(e.target.value as PageNumberKind)}>
            <option value="topLeft">Top left</option>
            <option value="topCenter">Top center</option>
            <option value="topRight">Top right</option>
            <option value="bottomLeft">Bottom left</option>
            <option value="bottomCenter">Bottom center</option>
            <option value="bottomRight">Bottom right</option>
          </select>
        </div>
        <div>
          <label className={dialLabel}>Format</label>
          <select className={`${dialField} w-full`} value={format} onChange={(e) => setFormat(e.target.value as PageNumberFmt)}>
            <option value="plain">1, 2, 3…</option>
            <option value="dashed">-1-, -2-…</option>
            <option value="slashed">1 / 9</option>
            <option value="pageXofY">Page 1 of 9</option>
          </select>
        </div>
        <div className="col-span-3">
          <label className={dialLabel}>Pages (empty = every page)</label>
          <input className={`${dialField} w-full`} value={pages} onChange={(e) => setPages(e.target.value)} placeholder={`All ${pageCount} pages`} />
        </div>
      </div>
      <DialogActions
        onCancel={onClose}
        onConfirm={() => onApply(position, format, parsePages(pages, pageCount))}
        confirmLabel="Add"
      />
    </DialogShell>
  );
}

function WatermarkDialog({
  pageCount,
  onApply,
  onClose,
}: {
  pageCount: number;
  onApply: (o: WatermarkOptions) => void;
  onClose: () => void;
}) {
  const [text, setText] = useState("CONFIDENTIAL");
  const [position, setPosition] = useState<WatermarkOptions["position"]>("middleCenter");
  const [fontSize, setFontSize] = useState(48);
  const [colorHex, setColorHex] = useState("#808080");
  const [opacity, setOpacity] = useState(0.25);
  const [rotation, setRotation] = useState(45);
  const [pages, setPages] = useState("");
  return (
    <DialogShell title="Watermark" onClose={onClose}>
      <div className="grid grid-cols-2 gap-x-4 gap-y-2">
        <div className="col-span-2">
          <label className={dialLabel}>Text</label>
          <input className={`${dialField} w-full`} value={text} onChange={(e) => setText(e.target.value)} placeholder="Watermark text" />
        </div>
        <div>
          <label className={dialLabel}>Position</label>
          <select className={`${dialField} w-full`} value={position} onChange={(e) => setPosition(e.target.value as WatermarkOptions["position"])}>
            <option value="topLeft">Top left</option>
            <option value="topCenter">Top center</option>
            <option value="topRight">Top right</option>
            <option value="middleLeft">Middle left</option>
            <option value="middleCenter">Middle center</option>
            <option value="middleRight">Middle right</option>
            <option value="bottomLeft">Bottom left</option>
            <option value="bottomCenter">Bottom center</option>
            <option value="bottomRight">Bottom right</option>
            <option value="tiled">Tiled</option>
          </select>
        </div>
        <div>
          <label className={dialLabel}>Font size</label>
          <input type="number" min={6} max={200} className={`${dialField} w-full`} value={fontSize} onChange={(e) => setFontSize(Number(e.target.value))} />
        </div>
        <div>
          <label className={dialLabel}>Opacity</label>
          <input type="range" min={0.05} max={1} step={0.05} value={opacity} onChange={(e) => setOpacity(Number(e.target.value))} className="w-full accent-brand-600" />
        </div>
        <div>
          <label className={dialLabel}>Rotation</label>
          <input type="number" min={0} max={360} className={`${dialField} w-full`} value={rotation} onChange={(e) => setRotation(Number(e.target.value))} />
        </div>
        <div>
          <label className={dialLabel}>Colour</label>
          <HexSwatch value={colorHex} onChange={setColorHex} />
        </div>
        <div>
          <label className={dialLabel}>Pages (empty = every page)</label>
          <input className={`${dialField} w-full`} value={pages} onChange={(e) => setPages(e.target.value)} placeholder={`All ${pageCount} pages`} />
        </div>
      </div>
      <DialogActions
        onCancel={onClose}
        onConfirm={() =>
          onApply({
            text: text.trim() ? text : null,
            image_path: null,
            position,
            font_size: fontSize,
            color: hexToRgb01(colorHex),
            opacity,
            rotation_degrees: rotation,
            pages: parsePages(pages, pageCount),
            layer_above: true,
          })
        }
        confirmLabel="Add"
      />
    </DialogShell>
  );
}

function DialogActions({ onCancel, onConfirm, confirmLabel }: { onCancel: () => void; onConfirm: () => void; confirmLabel: string }) {
  return (
    <div className="mt-4 flex items-center justify-end gap-2 border-t border-slate-100 pt-3">
      <button className="rounded-md border border-slate-300 px-3 py-1.5 text-[12px] font-medium text-slate-600 hover:bg-slate-50" onClick={onCancel}>
        Cancel
      </button>
      <button className="rounded-md bg-brand-600 px-3 py-1.5 text-[12px] font-medium text-white hover:bg-brand-700" onClick={onConfirm}>
        {confirmLabel}
      </button>
    </div>
  );
}

function BookmarksPanel({ outline, onGoToPage, onClose }: { outline: OutlineEntry[]; onGoToPage: (page: number) => void; onClose: () => void }) {
  return (
    <div className="flex w-60 shrink-0 flex-col border-r border-slate-200 bg-white">
      <div className="flex items-center justify-between px-3 py-2">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-slate-400">Bookmarks</span>
        <button className="rounded p-0.5 text-slate-400 hover:bg-slate-100 hover:text-slate-600" onClick={onClose} title="Close">
          <Icon name="close" className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        {outline.length === 0 ? (
          <p className="px-1 py-4 text-[11.5px] text-slate-400">No bookmarks in this document.</p>
        ) : (
          outline.map((e, i) => (
            <button
              key={i}
              className="block w-full truncate rounded px-2 py-1 text-left text-[12px] text-slate-700 hover:bg-brand-50 hover:text-brand-700"
              style={{ paddingLeft: 8 + Math.min(e.depth, 6) * 14 }}
              onClick={() => onGoToPage(Math.max(0, e.page - 1))}
              title={e.external ? `${e.title} (external)${e.resolvable ? "" : " — page not found"}` : e.title}
            >
              {e.title}
            </button>
          ))
        )}
      </div>
    </div>
  );
}

function LayersPanel({ onClose }: { onClose: () => void }) {
  return (
    <div className="flex w-52 shrink-0 flex-col border-r border-slate-200 bg-white">
      <div className="flex items-center justify-between px-3 py-2">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-slate-400">Layers</span>
        <button className="rounded p-0.5 text-slate-400 hover:bg-slate-100 hover:text-slate-600" onClick={onClose} title="Close">
          <Icon name="close" className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        <p className="px-1 py-4 text-[11.5px] text-slate-400">This document has no optional-content layers.</p>
      </div>
    </div>
  );
}

function NotesPanel({ notes, onGoToPage, onClose }: { notes: Ann[]; onGoToPage: (page: number) => void; onClose: () => void }) {
  return (
    <div className="flex w-56 shrink-0 flex-col border-r border-slate-200 bg-white">
      <div className="flex items-center justify-between px-3 py-2">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-slate-400">Notes</span>
        <button className="rounded p-0.5 text-slate-400 hover:bg-slate-100 hover:text-slate-600" onClick={onClose} title="Close">
          <Icon name="close" className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        {notes.length === 0 ? (
          <p className="px-1 py-4 text-[11.5px] text-slate-400">No notes on the canvas. Use “Add Note”, “Callout” or a text box to start commenting.</p>
        ) : (
          notes.map((n) => (
            <button
              key={n.id}
              className="mb-1 w-full rounded border border-brand-100 bg-white px-2 py-1.5 text-left hover:border-brand-300"
              onClick={() => onGoToPage(n.page)}
            >
              <div className="flex items-center justify-between gap-2">
                <span className="text-[10px] font-medium uppercase tracking-wide text-brand-600">
                  Page {n.page + 1}
                </span>
                <span className="text-[10px] text-slate-400">
                  {new Date(n.created).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                </span>
              </div>
              <p className="mt-0.5 truncate text-[11.5px] text-slate-700">{n.contents?.trim() || n.label || "—"}</p>
            </button>
          ))
        )}
      </div>
    </div>
  );
}
