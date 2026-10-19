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
  /** False when a required tool (pandoc, typst) is missing; the menu doesn't render it. Absent = true. */
  available?: boolean;
}

export type Options = Record<string, string>;

/** Operations for a selection: `mimes` has one entry per file (batch ops like "Merge PDFs" included). */
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

// ---- Mocks --------------------------------------------------------------------------------
// Per-type operations are the engine's own, from src/generated/engine-operations.json
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

const op = (id: string, label: string, icon: string, children?: Operation[]): Operation => ({
  id, label, icon, ...(children && { children }),
});

const rotations = (base: string) => [
  op(`${base}.90`, "90\u00b0 CW", "RotateCw"),
  op(`${base}.270`, "90\u00b0 CCW", "RotateCcw"),
  op(`${base}.180`, "180\u00b0", "RotateCw"),
];
const dpis = (base: string) => [
  op(`${base}.72`, "72 DPI", "Monitor"),
  op(`${base}.150`, "150 DPI", "Gauge"),
  op(`${base}.300`, "300 DPI", "Printer"),
];

// Planned: the engine has no PDF converter yet, so these only exist in mock mode. Ids follow
// the engine scheme (`pdf.<action>.<value>`; `options` will be able to override the value).
const PLANNED_PDF: Operation[] = [
  op("pdf.split", "Split", "Scissors"),
  op("pdf.extract", "Extract", "FileOutput", [
    op("pdf.extract.1", "Page 1", "FileDigit"),
    op("pdf.extract.1-3", "Pages 1\u20133", "Files"),
    op("pdf.extract.odd", "Odd", "ListOrdered"),
    op("pdf.extract.even", "Even", "ListOrdered"),
  ]),
  op("pdf.rotate", "Rotate", "RotateCw", rotations("pdf.rotate")),
  op("pdf.to_png", "PNG", "FileImage", dpis("pdf.to_png")),
  op("pdf.to_jpg", "JPG", "FileImage", dpis("pdf.to_jpg")),
];

const IMAGE_MIMES: string[] = [MIME.png, MIME.jpeg, MIME.webp];

/** Mirrors `Converter::batch_operations` of the (planned) PDF converter. */
function mockBatchOperations(mimes: string[]): Operation[] {
  const out: Operation[] = [];
  if (mimes.length >= 2 && mimes.every((m) => m === MIME.pdf)) out.push(op("pdf.merge", "Merge PDFs", "Merge"));
  if (mimes.length && mimes.every((m) => IMAGE_MIMES.includes(m))) {
    out.push(op("pdf.from_images", mimes.length > 1 ? "Images \u2192 PDF" : "Image \u2192 PDF", "FileText", [
      op("pdf.from_images.a4", "Fit A4", "RectangleVertical"),
      op("pdf.from_images.original", "Original size", "Scaling"),
    ]));
  }
  return withAvailability(out);
}

/** Eight plain wedges, for checking the fullest layout in the playground. */
const LAYOUT_TEST: Operation[] = [
  ["Convert", "FileImage"], ["Resize", "Maximize2"], ["Compress", "Minimize2"], ["Rotate", "RotateCw"],
  ["Merge PDFs", "Merge"], ["Images \u2192 PDF", "FileText"], ["Markdown", "FileCode"], ["Duplicate", "Copy"],
].map(([label, icon], i) => op(`layout.${i}`, label, icon));

const ENGINE = ENGINE_OPERATIONS as Record<string, Operation[]>;

function mockOperations(mime: string): Operation[] {
  if (mime === LAYOUT_TEST_MIME) return LAYOUT_TEST;
  const engine = ENGINE[mime] ?? ENGINE[MIME.unknown];
  return withAvailability(mime === MIME.pdf ? [...PLANNED_PDF, ...engine] : engine);
}

/** Marks operations unavailable the way the engine does when a tool is missing. */
function withAvailability(ops: Operation[]): Operation[] {
  const needs = (id: string) =>
    (id.startsWith("doc.") && !mockTools.pandoc) ||
    (id === "doc.to_pdf" && !mockTools.typst) ||
    (/^pdf\.to_(png|jpg)/.test(id) && !mockTools.pdfium);
  return ops.map((o) => ({
    ...o,
    available: o.available !== false && !needs(o.id),
    ...(o.children && { children: withAvailability(o.children) }),
  }));
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
