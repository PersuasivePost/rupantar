import {
  Archive, Circle, Code, Copy, EyeOff, Feather, File, FileCode, FileImage, FileSpreadsheet,
  FileText, FileType, Film, Gauge, Globe, Image, Images, Maximize2, Merge, Minimize2, Music,
  Pencil, Percent, RotateCcw, RotateCw, Scissors, Type,
  type LucideIcon,
} from "lucide-react";
import { MIME } from "./mockFiles";

// Explicit map keeps the bundle small; add icons here as operations need them.
const ICONS: Record<string, LucideIcon> = {
  Archive, Code, Copy, EyeOff, Feather, FileCode, FileImage, FileSpreadsheet, FileText, FileType,
  Film, Gauge, Globe, Image, Images, Maximize2, Merge, Minimize2, Music, Pencil, Percent,
  RotateCcw, RotateCw, Scissors, Type,
};

export const iconFor = (name: string): LucideIcon => ICONS[name] ?? Circle;

export function fileIcon(mime: string): LucideIcon {
  switch (mime) {
    case MIME.pdf: return FileText;
    case MIME.docx: return FileType;
    case MIME.png: return FileImage;
    case MIME.xlsx: return FileSpreadsheet;
    case MIME.md: return FileCode;
    default: return File;
  }
}
