// The only module that talks to Rust. Components import from here, never `invoke` directly.
// Outside Tauri (e.g. the Replit browser preview) every call falls back to a mock.
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import ENGINE_OPERATIONS from "./generated/engine-operations.json";
import { operationsFor } from "./lib/operations";
import { LAYOUT_TEST_MIME, MIME, commonMime, mimeFromFile } from "./mockFiles";

export interface Operation {
  id: string;
  label: string;
  /** lucide-react icon name. */
  icon: string;
  /** Sub-options (e.g. Resize -> 50% / 25%); shown as an outer ring. */
  children?: Operation[];
  /** False when a required tool is missing or the op is only planned; the menu hides it. Absent = true. */
  available?: boolean;
  /** Menu position for this selection, lower first (top level only). The engine already sorts by it. */
  priority?: number;
  /** What the op does (convert, optimize, pages, edit, extract, data, security, file). */
  group?: string;
  /** The UI asks for options first (`crop`: a rectangle). Only offered for one file. */
  prompt?: string;
}

export type Options = Record<string, string>;

/** Operations for a selection, in menu order: `mimes` has one entry per file (batch ops like "Merge" included). */
export async function listOperations(mimes: string[]): Promise<Operation[]> {
  if (isTauri()) return invoke<Operation[]>("list_operations", { mimes });
  return operationsFor(mimes, mockOperations, mockBatchOperations);
}

/** Resolves to the paths of the files written next to the inputs. Rust detects each input's type. */
export async function runOperation(op: Operation, inputs: string[], options: Options = {}): Promise<string[]> {
  if (isTauri()) return invoke<string[]>("run_operation", { op, inputs, options });
  await new Promise((r) => setTimeout(r, 400));
  return inputs.map((p) => p.replace(/(\.[^./]+)?$/, ` (${op.id})$1`));
}

/** A downscaled image for picking a crop rectangle. `width`/`height` are the original's pixels. */
export interface ImagePreview {
  src: string;
  width: number;
  height: number;
}

export async function previewImage(path: string): Promise<ImagePreview> {
  if (isTauri()) return invoke<ImagePreview>("preview_image", { path });
  return mockPreview(path);
}

// ---- Mocks --------------------------------------------------------------------------------
// Operations, batch ones included, are the engine's own, from src/generated/engine-operations.json
// (written by `UPDATE_SNAPSHOT=1 cargo test -p engine`, which fails when it is stale).

export interface MockTools {
  pandoc: boolean;
  typst: boolean;
  pdfium: boolean;
}

let mockTools: MockTools = { pandoc: true, typst: true, pdfium: true };

/** Playground: pretend tools are missing, to see their operations disappear. */
export function setMockTools(tools: MockTools): void {
  mockTools = { ...tools };
}

let showPlanned = false;

/** Playground: treat planned (not yet implemented) operations as available, to preview full menus. */
export function setShowPlanned(show: boolean): void {
  showPlanned = show;
}

const op = (id: string, label: string, icon: string, children?: Operation[]): Operation => ({
  id, label, icon, ...(children && { children }),
});

const IMAGE_MIMES: string[] = [MIME.png, MIME.jpeg, MIME.webp];

/**
 * Mirrors `PdfConverter::batch_operations`: the rule for which selections get a batch op is
 * here, the operations themselves come from the snapshot's representative selections.
 */
function mockBatchOperations(mimes: string[]): Operation[] {
  const batch = ENGINE.batch;
  if (mimes.length >= 2 && mimes.every((m) => m === MIME.pdf)) {
    return withAvailability(batch[`${MIME.pdf},${MIME.pdf}`] ?? []);
  }
  if (mimes.length && mimes.every((m) => IMAGE_MIMES.includes(m))) {
    return withAvailability(batch[mimes.length > 1 ? `${MIME.png},${MIME.jpeg}` : MIME.png] ?? []);
  }
  return [];
}

/** Twenty operations (one with sub-options), for checking the paged More ring in the playground. */
const LAYOUT_TEST: Operation[] = [
  ["Compress", "Minimize2"], ["To Word", "FileType"], ["To image", "FileImage"], ["Split", "Scissors"],
  ["Rotate", "RotateCw"], ["Extract", "FileOutput"], ["To PowerPoint", "Presentation"], ["To Excel", "FileSpreadsheet"],
  ["To text", "Text"], ["Watermark", "Stamp"], ["Page numbers", "Hash"], ["Protect / unlock", "Lock"],
  ["OCR", "ScanText"], ["Merge", "Merge"], ["To Markdown", "FileCode"], ["To HTML", "Globe"],
  ["To EPUB", "BookOpen"], ["Strip metadata", "EyeOff"], ["Upscale", "ZoomIn"], ["Duplicate", "Copy"],
].map(([label, icon], i) =>
  op(`layout.${i}`, label, icon, i === 5 ? [op("layout.5.a", "Page 1", "FileDigit"), op("layout.5.b", "Odd", "ListOrdered"), op("layout.5.c", "Even", "ListOrdered")] : undefined),
);

const ENGINE = ENGINE_OPERATIONS as {
  /** Per mime. */
  operations: Record<string, Operation[]>;
  /** Per representative selection, keyed by its mimes joined with ",". */
  batch: Record<string, Operation[]>;
};

function mockOperations(mime: string): Operation[] {
  if (mime === LAYOUT_TEST_MIME) return LAYOUT_TEST;
  return withAvailability(ENGINE.operations[mime] ?? ENGINE.operations[MIME.unknown]);
}

/** Marks operations unavailable the way the engine does when a tool is missing. */
function withAvailability(ops: Operation[]): Operation[] {
  const needs = (id: string) =>
    ((id.startsWith("doc.") && id !== "doc.extract_images") && !mockTools.pandoc) ||
    (/^pdf\.to_(pptx|docx)$/.test(id) && !mockTools.pandoc) ||
    (id === "doc.to_pdf" && !mockTools.typst) ||
    (/^pdf\.to_(image|png|jpg|text|pptx|docx)/.test(id) && !mockTools.pdfium);
  // The snapshot is taken with every tool present, so `available: false` there means planned.
  return ops.map((o) => ({
    ...o,
    available: (showPlanned || o.available !== false) && !needs(o.id),
    ...(o.children && { children: withAvailability(o.children) }),
  }));
}

/** A drawn stand-in photo, reported at 4x its preview size like a real downscaled preview. */
function mockPreview(path: string): ImagePreview {
  const [w, h] = [800, 533];
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const g = canvas.getContext("2d")!;
  const sky = g.createLinearGradient(0, 0, 0, h);
  sky.addColorStop(0, "#7dd3fc");
  sky.addColorStop(0.6, "#fde68a");
  sky.addColorStop(1, "#f97316");
  g.fillStyle = sky;
  g.fillRect(0, 0, w, h);
  g.fillStyle = "#fff7d6";
  g.beginPath();
  g.arc(w * 0.68, h * 0.42, 54, 0, Math.PI * 2);
  g.fill();
  g.fillStyle = "#1e3a5f";
  g.beginPath();
  g.moveTo(0, h);
  g.lineTo(w * 0.22, h * 0.55);
  g.lineTo(w * 0.42, h * 0.78);
  g.lineTo(w * 0.62, h * 0.5);
  g.lineTo(w, h * 0.85);
  g.lineTo(w, h);
  g.fill();
  g.fillStyle = "rgba(255,255,255,0.85)";
  g.font = "600 22px system-ui, sans-serif";
  g.fillText(path.split(/[\\/]/).pop() ?? path, 18, 34);
  return { src: canvas.toDataURL("image/jpeg", 0.85), width: w * 4, height: h * 4 };
}

// ---- Desktop shell ------------------------------------------------------------------------
// Browser mode (no Tauri) gets inert or in-memory stand-ins so the UI stays developable.

export const inTauri = (): boolean => isTauri();

export interface FileBatch {
  paths: string[];
  /** File name for one file, "N files" for several. */
  label: string;
  /** The type shared by every file, or `application/octet-stream` for a mixed batch. */
  mime: string;
  /** One per file, in `paths` order. */
  mimes: string[];
}

/** Sent by the shell each time the radial window is shown. `cursor` is in CSS px inside the window. */
export interface SummonPayload {
  cursor: { x: number; y: number };
  paths: string[];
}

export interface DropEvent {
  type: "over" | "leave" | "drop";
  paths: string[];
}

export interface Settings {
  shortcut: string;
  autostart: boolean;
}

type Unlisten = () => void;

export async function describeFiles(paths: string[]): Promise<FileBatch> {
  if (isTauri()) return invoke<FileBatch>("describe_files", { paths });
  const names = paths.map((p) => p.split(/[\\/]/).pop() ?? "file");
  const mimes = names.map((name) => mimeFromFile({ name }));
  return { paths, label: paths.length > 1 ? `${paths.length} files` : names[0] ?? "file", mime: commonMime(mimes), mimes };
}

export async function onSummon(cb: (p: SummonPayload) => void): Promise<Unlisten> {
  if (!isTauri()) return () => {};
  return listen<SummonPayload>("summon", (e) => cb(e.payload));
}

/** Files dragged onto the window, via the OS (the webview never sees real paths itself). */
export async function onFilesDropped(cb: (e: DropEvent) => void): Promise<Unlisten> {
  if (!isTauri()) return () => {};
  return getCurrentWebview().onDragDropEvent(({ payload }) => {
    if (payload.type === "enter" || payload.type === "over") cb({ type: "over", paths: [] });
    else if (payload.type === "drop") cb({ type: "drop", paths: payload.paths });
    else cb({ type: "leave", paths: [] });
  });
}

/** Tell the shell our listeners are live; returns a summon that arrived before they were. */
export async function frontendReady(): Promise<SummonPayload | null> {
  return isTauri() ? invoke<SummonPayload | null>("frontend_ready") : null;
}

export async function hideRadial(): Promise<void> {
  if (isTauri()) await invoke("hide_radial");
}

/** While a menu is open, clicking elsewhere should dismiss it; while waiting for a drop it must not. */
export async function setHideOnBlur(enabled: boolean): Promise<void> {
  if (isTauri()) await invoke("set_hide_on_blur", { enabled });
}

const DEFAULT_SETTINGS: Settings = { shortcut: "CommandOrControl+Shift+Space", autostart: false };
let mockSettings = { ...DEFAULT_SETTINGS };

export async function getSettings(): Promise<Settings> {
  return isTauri() ? invoke<Settings>("get_settings") : { ...mockSettings };
}

/** Rejects with a readable message if the shortcut is invalid or taken. */
export async function setShortcut(accelerator: string): Promise<Settings> {
  if (isTauri()) return invoke<Settings>("set_shortcut", { accelerator });
  mockSettings = { ...mockSettings, shortcut: accelerator };
  return { ...mockSettings };
}

export async function setAutostart(enabled: boolean): Promise<Settings> {
  if (isTauri()) return invoke<Settings>("set_autostart", { enabled });
  mockSettings = { ...mockSettings, autostart: enabled };
  return { ...mockSettings };
}
