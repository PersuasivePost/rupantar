import {
  ArrowRightToLine, BookOpen, Circle, Columns3, Copy, Feather, File, FileCode, FileDigit,
  FileImage, FileOutput, FilePen, FileSpreadsheet, FileText, FileType, Files, Gauge, Globe,
  Image, LayoutGrid, ListOrdered, Maximize2, Merge, Minimize2, Monitor, Percent, Printer,
  RectangleVertical, RotateCcw, RotateCw, Scaling, Scissors, Sheet,
  type LucideIcon,
} from "lucide-react";
import { LAYOUT_TEST_MIME, MIME } from "./mockFiles";

// Explicit map keeps the bundle small. Every icon named by an engine operation
// (src/generated/engine-operations.json) or a mock in api.ts must be here.
export const ICONS: Record<string, LucideIcon> = {
  ArrowRightToLine, BookOpen, Columns3, Copy, Feather, FileCode, FileDigit, FileImage, FileOutput,
  FilePen, FileSpreadsheet, FileText, FileType, Files, Gauge, Globe, Image, ListOrdered,
  Maximize2, Merge, Minimize2, Monitor, Percent, Printer, RectangleVertical, RotateCcw, RotateCw,
  Scaling, Scissors,
};

export const iconFor = (name: string): LucideIcon => ICONS[name] ?? Circle;

export function fileIcon(mime: string): LucideIcon {
  switch (mime) {
    case MIME.pdf: return FileText;
    case MIME.docx: return FileType;
    case MIME.md: return FileCode;
    case MIME.html: return Globe;
    case MIME.png: case MIME.jpeg: case MIME.webp: return FileImage;
    case MIME.xlsx: case MIME.xls: case MIME.ods: return FileSpreadsheet;
    case MIME.csv: return Sheet;
    case LAYOUT_TEST_MIME: return LayoutGrid;
    default: return File;
  }
}
