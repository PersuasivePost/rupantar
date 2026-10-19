// Mock file types for browser dev mode (no Tauri). Real mime detection happens in Rust.
export const MIME = {
  pdf: "application/pdf",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  png: "image/png",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  md: "text/markdown",
} as const;

export interface SampleFile {
  label: string;
  name: string;
  mime: string;
}

export const SAMPLE_FILES: SampleFile[] = [
  { label: "PDF", name: "Quarterly report.pdf", mime: MIME.pdf },
  { label: "DOCX", name: "Project brief.docx", mime: MIME.docx },
  { label: "PNG", name: "Screenshot.png", mime: MIME.png },
  { label: "XLSX", name: "Budget 2026.xlsx", mime: MIME.xlsx },
  { label: "MD", name: "README.md", mime: MIME.md },
];

const EXT_MIME: Record<string, string> = {
  pdf: MIME.pdf, docx: MIME.docx, png: MIME.png, xlsx: MIME.xlsx, md: MIME.md,
};

/** Best-effort mime for a dropped file in dev mode; falls back to the browser's guess. */
export function mimeFromFile(file: { name: string; type: string }): string {
  const ext = file.name.split(".").pop()?.toLowerCase() ?? "";
  return EXT_MIME[ext] ?? (file.type || "application/octet-stream");
}
