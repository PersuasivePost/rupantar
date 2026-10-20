import {
  ArrowDown, ArrowDownAZ, ArrowDownZA, ArrowDownRight, ArrowRightToLine, ArrowUpRight, Contrast, FlipHorizontal2, FlipVertical2, BookOpen, Braces, ChevronsRight, Circle, Columns3, Copy, CopyMinus,
  Crop, Ellipsis, EyeOff, Feather, File, FileCode, FileDigit, FileImage, FileOutput, FilePen,
  FileSpreadsheet, FileText, FileType, Files, Gauge, Globe, Hash, Image, ImageDown, Images,
  LayoutGrid, ListOrdered, Lock, Maximize2, Merge, Minimize2, Monitor, Percent, Presentation,
  Printer, RectangleVertical, RotateCcw, RotateCw, Rows3, Scaling, ScanText, Scissors, Sheet,
  SlidersHorizontal, Split, Stamp, Text, Undo2, WandSparkles, ZoomIn,
  type LucideIcon,
} from "lucide-react";
import { LAYOUT_TEST_MIME, MIME } from "./mockFiles";

// Explicit map keeps the bundle small. Every icon named by an engine operation
// (src/generated/engine-operations.json), a mock in api.ts, or a menu control
// (More / Back / page, in lib/operations.ts) must be here.
export const ICONS: Record<string, LucideIcon> = {
  ArrowDown, ArrowDownAZ, ArrowDownZA, ArrowDownRight, ArrowRightToLine, ArrowUpRight, Contrast, FlipHorizontal2, FlipVertical2, BookOpen, Braces, ChevronsRight, Columns3, Copy, CopyMinus, Crop,
  Ellipsis, EyeOff, Feather, FileCode, FileDigit, FileImage, FileOutput, FilePen,
  FileSpreadsheet, FileText, FileType, Files, Gauge, Globe, Hash, Image, ImageDown, Images,
  ListOrdered, Lock, Maximize2, Merge, Minimize2, Monitor, Percent, Presentation, Printer,
  RectangleVertical, RotateCcw, RotateCw, Rows3, Scaling, ScanText, Scissors, SlidersHorizontal,
  Split, Stamp, Text, Undo2, WandSparkles, ZoomIn,
};

export const iconFor = (name: string): LucideIcon => ICONS[name] ?? Circle;

export function fileIcon(mime: string): LucideIcon {
  switch (mime) {
    case MIME.pdf: return FileText;
    case MIME.docx: case MIME.doc: case MIME.odt: return FileType;
    case MIME.md: return FileCode;
    case MIME.html: return Globe;
    case MIME.png: case MIME.jpeg: case MIME.webp: return FileImage;
    case MIME.xlsx: case MIME.xls: case MIME.ods: return FileSpreadsheet;
    case MIME.csv: return Sheet;
    case MIME.pptx: case MIME.ppt: case MIME.odp: return Presentation;
    case LAYOUT_TEST_MIME: return LayoutGrid;
    default: return File;
  }
}
