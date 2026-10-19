// The only module that talks to Rust. Components import from here, never `invoke` directly.
// Outside Tauri (e.g. the Replit browser preview) every call falls back to a mock.
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { MIME } from "./mockFiles";

export interface Operation {
  id: string;
  label: string;
  /** lucide-react icon name. */
  icon: string;
  /** Sub-options (e.g. Resize -> 50% / 25%); shown as an outer ring. */
  children?: Operation[];
}

export type Options = Record<string, string>;

const op = (id: string, label: string, icon: string, children?: Operation[]): Operation => ({
  id, label, icon, ...(children && { children }),
});

const MOCK_OPERATIONS: Record<string, Operation[]> = {
  [MIME.pdf]: [
    op("pdf-images", "Images", "Images"),
    op("pdf-text", "Text", "Type"),
    op("pdf-docx", "Word", "FileType"),
    op("pdf-compress", "Compress", "Minimize2", [
      op("pdf-compress-light", "Light", "Feather"),
      op("pdf-compress-balanced", "Balanced", "Gauge"),
      op("pdf-compress-small", "Smallest", "Minimize2"),
    ]),
    op("pdf-split", "Split", "Scissors"),
    op("pdf-merge", "Merge", "Merge"),
  ],
  [MIME.docx]: [
    op("docx-pdf", "PDF", "FileText"),
    op("docx-md", "Markdown", "FileCode"),
    op("docx-html", "HTML", "Globe"),
    op("docx-txt", "Text", "Type"),
    op("docx-copy", "Duplicate", "Copy"),
  ],
  [MIME.png]: [
    op("img-jpg", "JPG", "FileImage"),
    op("img-webp", "WebP", "Image"),
    op("img-pdf", "PDF", "FileText"),
    op("img-resize", "Resize", "Maximize2", [
      op("img-resize-50", "50%", "Percent"),
      op("img-resize-25", "25%", "Percent"),
      op("img-resize-custom", "Custom", "Pencil"),
    ]),
    op("img-compress", "Compress", "Minimize2"),
    op("img-rotate", "Rotate", "RotateCw"),
    op("img-exif", "Strip EXIF", "EyeOff"),
    op("img-ico", "Icon", "Archive"),
  ],
  [MIME.xlsx]: [
    op("xlsx-csv", "CSV", "FileSpreadsheet"),
    op("xlsx-pdf", "PDF", "FileText"),
    op("xlsx-json", "JSON", "Code"),
    op("xlsx-md", "Markdown", "FileCode"),
    op("xlsx-html", "HTML", "Globe"),
  ],
  [MIME.md]: [
    op("md-pdf", "PDF", "FileText"),
    op("md-docx", "Word", "FileType"),
    op("md-html", "HTML", "Globe"),
    op("md-txt", "Text", "Type"),
  ],
};

const FALLBACK_OPERATIONS = [op("copy", "Duplicate", "Copy"), op("zip", "Zip", "Archive")];

export async function listOperations(mime: string): Promise<Operation[]> {
  if (isTauri()) return invoke<Operation[]>("list_operations", { mime });
  return MOCK_OPERATIONS[mime] ?? FALLBACK_OPERATIONS;
}

/** Resolves to the paths of the files written next to the inputs. */
export async function runOperation(
  mime: string,
  op: Operation,
  inputs: string[],
  options: Options = {},
): Promise<string[]> {
  if (isTauri()) return invoke<string[]>("run_operation", { mime, op, inputs, options });
  await new Promise((r) => setTimeout(r, 400));
  return inputs.map((p) => p.replace(/(\.[^./]+)?$/, `-${op.id}$1`));
}

// ---- Desktop shell ------------------------------------------------------------------------
// Browser mode (no Tauri) gets inert or in-memory stand-ins so the UI stays developable.

export const inTauri = (): boolean => isTauri();

export interface FileBatch {
  paths: string[];
  /** File name for one file, "N files" for several. */
  label: string;
  mime: string;
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
  const name = paths[0]?.split(/[\\/]/).pop() ?? "file";
  return { paths, label: paths.length > 1 ? `${paths.length} files` : name, mime: "application/octet-stream" };
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
