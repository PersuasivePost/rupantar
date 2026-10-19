// The only module that talks to Rust. Components import from here, never `invoke` directly.
// Outside Tauri (e.g. the Replit browser preview) every call falls back to a mock.
import { invoke, isTauri } from "@tauri-apps/api/core";
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
