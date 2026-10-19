// Mock file types for browser dev mode (no Tauri). Real mime detection happens in Rust
// (crates/engine/src/mime.rs); these strings must match it.
export const MIME = {
  pdf: "application/pdf",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  doc: "application/msword",
  odt: "application/vnd.oasis.opendocument.text",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  ppt: "application/vnd.ms-powerpoint",
  odp: "application/vnd.oasis.opendocument.presentation",
  md: "text/markdown",
  html: "text/html",
  png: "image/png",
  jpeg: "image/jpeg",
  webp: "image/webp",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  xls: "application/vnd.ms-excel",
  ods: "application/vnd.oasis.opendocument.spreadsheet",
  csv: "text/csv",
  unknown: "application/octet-stream",
} as const;

/** Playground-only: a fake type whose mock lists 20 operations, to check the paged More ring. */
export const LAYOUT_TEST_MIME = "x-rupantar/layout-test";

export interface SampleFile {
  label: string;
  name: string;
  mime: string;
}

export const SAMPLE_FILES: SampleFile[] = [
  { label: "PDF", name: "Quarterly report.pdf", mime: MIME.pdf },
  { label: "DOCX", name: "Project brief.docx", mime: MIME.docx },
  { label: "MD", name: "README.md", mime: MIME.md },
  { label: "HTML", name: "Landing page.html", mime: MIME.html },
  { label: "PNG", name: "Screenshot.png", mime: MIME.png },
  { label: "JPG", name: "Holiday.jpg", mime: MIME.jpeg },
  { label: "XLSX", name: "Budget 2026.xlsx", mime: MIME.xlsx },
  { label: "CSV", name: "Contacts.csv", mime: MIME.csv },
  { label: "PPTX", name: "Pitch deck.pptx", mime: MIME.pptx },
  { label: "20 ops", name: "Layout test", mime: LAYOUT_TEST_MIME },
];

const EXT_MIME: Record<string, string> = {
  pdf: MIME.pdf, docx: MIME.docx, doc: MIME.doc, odt: MIME.odt, md: MIME.md, markdown: MIME.md, html: MIME.html, htm: MIME.html,
  png: MIME.png, jpg: MIME.jpeg, jpeg: MIME.jpeg, webp: MIME.webp,
  xlsx: MIME.xlsx, xls: MIME.xls, ods: MIME.ods, csv: MIME.csv,
  pptx: MIME.pptx, ppt: MIME.ppt, odp: MIME.odp,
};

/** Best-effort mime for a dropped file in dev mode; falls back to the browser's guess. */
export function mimeFromFile(file: { name: string; type?: string }): string {
  const ext = file.name.split(".").pop()?.toLowerCase() ?? "";
  return EXT_MIME[ext] ?? (file.type || MIME.unknown);
}

/** The type shared by every file, or octet-stream for a mixed batch (mirrors `mime::common`). */
export function commonMime(mimes: string[]): string {
  return mimes.length && mimes.every((m) => m === mimes[0]) ? mimes[0] : MIME.unknown;
}
