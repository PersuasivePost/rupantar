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
  /**
   * An optional download this needs (`office`: LibreOffice; `ai`: the AI pack). With
   * `available: false` the menu still shows it, and choosing it offers the download.
   */
  pack?: string;
  /** Always under "More", never a top wedge (Duplicate; Office-pack ops while the pack is missing). */
  more?: boolean;
}

export type Options = Record<string, string>;

/** Operations for a selection, in menu order: `mimes` has one entry per file (batch ops like "Merge" included). */
export async function listOperations(mimes: string[]): Promise<Operation[]> {
  if (isTauri()) return invoke<Operation[]>("list_operations", { mimes });
  return operationsFor(mimes, mockOperations, mockBatchOperations);
}

/** How far a long run has got (only some operations report it, e.g. the AI pack's). */
export interface RunProgress {
  /** 0..1. */
  fraction: number;
  /** What it is doing, e.g. "Removing background on the CPU…". */
  label: string;
}

export interface RunControl {
  onProgress?: (p: RunProgress) => void;
  /** Aborting cancels the run, which then rejects with `RUN_CANCELLED`. */
  signal?: AbortSignal;
}

/** Rejection message of a run the user cancelled. */
export const RUN_CANCELLED = "cancelled";

/**
 * Resolves to the paths of the files written next to the inputs. Rust detects each input's type.
 * Long runs report progress through `control.onProgress` and stop when `control.signal` aborts.
 */
export async function runOperation(op: Operation, inputs: string[], options: Options = {}, control: RunControl = {}): Promise<string[]> {
  if (!isTauri()) return mockRun(op, inputs, control);
  const runId = crypto.randomUUID();
  const { onProgress, signal } = control;
  const off = onProgress
    ? await listen<RunProgress & { run_id: string }>("operation-progress", (e) => {
      if (e.payload.run_id === runId) onProgress({ fraction: e.payload.fraction, label: e.payload.label });
    })
    : undefined;
  const cancel = () => void invoke("cancel_operation", { runId });
  signal?.addEventListener("abort", cancel);
  try {
    return await invoke<string[]>("run_operation", { op, inputs, options, runId });
  } finally {
    off?.();
    signal?.removeEventListener("abort", cancel);
  }
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
  /** The Office pack (LibreOffice) is installed. */
  office: boolean;
  /** The AI pack is installed. */
  ai: boolean;
}

let mockTools: MockTools = { pandoc: true, typst: true, pdfium: true, office: false, ai: false };

/** Playground: pretend tools are missing, to see their operations disappear. */
export function setMockTools(tools: MockTools): void {
  const changed = PACK_IDS.filter((p) => tools[p] !== mockTools[p]);
  mockTools = { ...tools };
  changed.forEach(mockPackChanged);
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
  const needs = (id: string, pack?: string): boolean => {
    if (pack === "office") return !mockTools.office || (id === "slides.to_images" && !mockTools.pdfium);
    if (pack === "ai") return !mockTools.ai;
    return (id.startsWith("doc.") && !/^doc\.(extract_images|compress)/.test(id) && !mockTools.pandoc) ||
      (/^pdf\.to_(pptx|docx)$/.test(id) && !mockTools.pandoc) ||
      (id === "doc.to_pdf" && !mockTools.typst) ||
      (/^pdf\.to_(image|png|jpg|text|pptx|docx)/.test(id) && !mockTools.pdfium);
  };
  // The snapshot is taken with every tool present, so `available: false` there means planned.
  return ops.map((o) => ({
    ...o,
    available: (showPlanned || o.available !== false) && !needs(o.id, o.pack),
    // Like the engine: Office ops wait under More until their pack is there.
    ...(o.pack === "office" && !mockTools.office && { more: true }),
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
  /** Folder the file picker opened last. */
  last_dir?: string | null;
}

type Unlisten = () => void;

export async function describeFiles(paths: string[]): Promise<FileBatch> {
  if (isTauri()) return invoke<FileBatch>("describe_files", { paths });
  const names = paths.map((p) => p.split(/[\\/]/).pop() ?? "file");
  const mimes = names.map((name) => mimeFromFile({ name }));
  return { paths, label: paths.length > 1 ? `${paths.length} files` : names[0] ?? "file", mime: commonMime(mimes), mimes };
}

/**
 * Opens the native file picker (multi-select) and resolves to the chosen paths, empty when
 * cancelled. In the browser a hidden `<input type="file">` stands in; it only knows file names,
 * which then serve as the paths. Needs a user gesture, like any file input.
 */
export async function pickFiles(): Promise<string[]> {
  if (isTauri()) return invoke<string[]>("pick_files");
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    input.hidden = true;
    const done = (names: string[]) => {
      input.remove();
      resolve(names);
    };
    input.addEventListener("change", () => done(Array.from(input.files ?? [], (f) => f.name)));
    input.addEventListener("cancel", () => done([]));
    document.body.append(input);
    input.click();
  });
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

// ---- Packs --------------------------------------------------------------------------------
// Optional downloads, only after the user agreed (see src-tauri/src/packs.rs): the Office pack
// (LibreOffice) and the AI pack (ONNX Runtime + BiRefNet lite, Real-ESRGAN).

export type PackId = "office" | "ai";
export const PACK_IDS: PackId[] = ["office", "ai"];

export interface PackOffer {
  version: string;
  download_bytes: number;
  installed_bytes: number;
  /** Already on disk from an interrupted download; a retry resumes after it. */
  downloaded_bytes: number;
  /** False when this platform's build hasn't been tried on a real machine yet. */
  tested: boolean;
  /** The sites it downloads from, e.g. "github.com, nuget.org". */
  source: string;
}

export interface PackStatus {
  pack: PackId;
  state: "installed" | "installing" | "not_installed" | "unsupported";
  installed: { version: string; size: number } | null;
  offer: PackOffer | null;
}

export interface PackProgress {
  phase: "download" | "verify" | "extract";
  done: number;
  total: number;
}

export interface Notice {
  name: string;
  text: string;
}

/** Rejection message when the user cancelled the download. */
export const PACK_CANCELLED = "cancelled";

export async function packStatus(pack: PackId): Promise<PackStatus> {
  return isTauri() ? invoke<PackStatus>("pack_status", { pack }) : mockPackStatus(pack);
}

/**
 * Downloads and installs a pack. Call it only from a button the user pressed after seeing the
 * size: this is the app's one network access. Rejects with `PACK_CANCELLED` or a readable error.
 */
export async function installPack(pack: PackId): Promise<PackStatus> {
  return isTauri() ? invoke<PackStatus>("install_pack", { pack }) : mockInstall(pack);
}

export async function cancelPack(pack: PackId): Promise<void> {
  if (isTauri()) await invoke("cancel_pack", { pack });
  else mockCancel.add(pack);
}

export async function removePack(pack: PackId): Promise<PackStatus> {
  if (isTauri()) return invoke<PackStatus>("remove_pack", { pack });
  mockTools = { ...mockTools, [pack]: false };
  mockDownloaded[pack] = 0;
  mockPackChanged(pack);
  return mockPackStatus(pack);
}

export async function onPackProgress(pack: PackId, cb: (p: PackProgress) => void): Promise<Unlisten> {
  if (isTauri()) {
    return listen<PackProgress & { pack: PackId }>("pack-progress", (e) => {
      if (e.payload.pack === pack) cb({ phase: e.payload.phase, done: e.payload.done, total: e.payload.total });
    });
  }
  const listener = (p: PackId, progress: PackProgress) => p === pack && cb(progress);
  mockProgressListeners.add(listener);
  return () => void mockProgressListeners.delete(listener);
}

/** Fires when the pack was installed or removed (from any window). */
export async function onPackChanged(pack: PackId, cb: (s: PackStatus) => void): Promise<Unlisten> {
  if (isTauri()) return listen<PackStatus>("pack-changed", (e) => e.payload.pack === pack && cb(e.payload));
  const listener = (s: PackStatus) => s.pack === pack && cb(s);
  mockChangedListeners.add(listener);
  return () => void mockChangedListeners.delete(listener);
}

/** License/notice files shipped with a component: `pdfium` (bundled) or a pack's (when installed). */
export async function thirdPartyNotices(component: "pdfium" | PackId): Promise<Notice[]> {
  if (isTauri()) return invoke<Notice[]>("third_party_notices", { component });
  return [{ name: "NOTICE", text: `(${component} notices are read from disk in the desktop app)` }];
}

// Browser mock: pretend downloads that take a few seconds, resumable and cancellable.
const MOCK_OFFERS: Record<PackId, Omit<PackOffer, "downloaded_bytes">> = {
  office: { version: "26.2.6", download_bytes: 217_285_735, installed_bytes: 560_000_000, tested: true, source: "documentfoundation.org" },
  ai: { version: "1", download_bytes: 280_066_660, installed_bytes: 295_000_000, tested: true, source: "github.com" },
};
const mockInstalling = new Set<PackId>();
const mockCancel = new Set<PackId>();
const mockDownloaded: Record<PackId, number> = { office: 0, ai: 0 };
const mockProgressListeners = new Set<(pack: PackId, p: PackProgress) => void>();
const mockChangedListeners = new Set<(s: PackStatus) => void>();

function mockPackStatus(pack: PackId): PackStatus {
  const offer = MOCK_OFFERS[pack];
  const installed = mockTools[pack] ? { version: offer.version, size: offer.installed_bytes * 0.97 } : null;
  const state = mockInstalling.has(pack) ? "installing" : installed ? "installed" : "not_installed";
  return { pack, state, installed, offer: { ...offer, downloaded_bytes: mockDownloaded[pack] } };
}

function mockPackChanged(pack: PackId): void {
  const s = mockPackStatus(pack);
  mockChangedListeners.forEach((cb) => cb(s));
}

async function mockInstall(pack: PackId): Promise<PackStatus> {
  if (mockInstalling.has(pack)) throw `the ${pack} pack is already being downloaded`;
  mockInstalling.add(pack);
  mockCancel.delete(pack);
  const emit = (p: PackProgress) => mockProgressListeners.forEach((cb) => cb(pack, p));
  const total = MOCK_OFFERS[pack].download_bytes;
  try {
    for (const [phase, ms] of [["download", 3200], ["verify", 500], ["extract", 900]] as const) {
      let done = phase === "download" ? mockDownloaded[pack] : 0;
      const step = total / (ms / 50);
      while (done < total) {
        await new Promise((r) => setTimeout(r, 50));
        if (mockCancel.has(pack)) throw PACK_CANCELLED;
        done = Math.min(total, done + step);
        if (phase === "download") mockDownloaded[pack] = done;
        emit({ phase, done, total });
      }
    }
    mockTools = { ...mockTools, [pack]: true };
    mockDownloaded[pack] = 0;
  } finally {
    mockInstalling.delete(pack);
  }
  mockPackChanged(pack);
  return mockPackStatus(pack);
}

/** Instant for most operations; the AI ones take a few seconds and report progress, like the real ones. */
async function mockRun(op: Operation, inputs: string[], { onProgress, signal }: RunControl): Promise<string[]> {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const slow = op.id.startsWith("image.remove_bg") ? "Removing background on the CPU…" : op.id.startsWith("image.upscale") ? "Upscaling…" : null;
  if (!slow) await sleep(400);
  for (let i = 0; slow && i <= 30; i++) {
    if (signal?.aborted) throw RUN_CANCELLED;
    onProgress?.({ fraction: i / 30, label: slow });
    await sleep(100);
  }
  return inputs.map((p) => p.replace(/(\.[^./]+)?$/, ` (${op.id})$1`));
}
