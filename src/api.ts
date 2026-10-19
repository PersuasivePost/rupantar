// The only module that talks to Rust. Components import from here, never `invoke` directly.
// Outside Tauri (e.g. the Replit browser preview) every call falls back to a mock.
import { invoke, isTauri } from "@tauri-apps/api/core";

export interface Operation {
  id: string;
  label: string;
  /** lucide-react icon name. */
  icon: string;
}

export type Options = Record<string, string>;

const MOCK_OPERATIONS: Operation[] = [
  { id: "copy", label: "Duplicate", icon: "Copy" },
  { id: "image-png", label: "To PNG", icon: "Image" },
  { id: "doc-pdf", label: "To PDF", icon: "FileText" },
  { id: "audio-mp3", label: "To MP3", icon: "Music" },
  { id: "video-mp4", label: "To MP4", icon: "Film" },
  { id: "archive-zip", label: "Zip", icon: "Archive" },
];

export async function listOperations(mime: string): Promise<Operation[]> {
  if (isTauri()) return invoke<Operation[]>("list_operations", { mime });
  return MOCK_OPERATIONS;
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
