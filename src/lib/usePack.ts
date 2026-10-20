// Pack state shared by the consent dialog (radial window) and Settings: status, live progress
// (from either window's download), and actions. One hook per pack (`office`, `ai`).
import { useCallback, useEffect, useState } from "react";
import {
  PACK_CANCELLED, cancelPack, installPack, onPackChanged, onPackProgress, packStatus, removePack,
  type PackId, type PackProgress, type PackStatus,
} from "../api";

/** What the dialog and Settings say about each pack. */
export const PACK_INFO: Record<PackId, {
  name: string;
  /** "needs Office pack" under a dimmed wedge. */
  note: string;
  /** One sentence for the consent dialog: what it contains, `{version}` and `{source}` filled in. */
  offer: (version: string, source: string) => string;
  /** Settings: what it is for. */
  about: (source: string) => string;
  /** Settings, once installed: "LibreOffice 26.2.6". */
  installed: (version: string) => string;
}> = {
  office: {
    name: "Office pack",
    note: "needs Office pack",
    offer: (version, source) =>
      `Word, Excel and PowerPoint conversions use LibreOffice ${version}, which isn’t included with Rupantar. Download it from ${source}?`,
    about: (source) =>
      `LibreOffice, for Word, Excel and PowerPoint conversions (to PDF, between formats, slides to images). Optional, and downloaded from ${source} only when you ask. Conversions stay on this computer.`,
    installed: (version) => `LibreOffice ${version}`,
  },
  ai: {
    name: "AI pack",
    note: "needs AI pack",
    offer: (_version, source) =>
      `Remove background and Upscale use AI models that aren’t included with Rupantar: BiRefNet lite (run by ONNX Runtime) and Real-ESRGAN. Download them from ${source}?`,
    about: (source) =>
      `Remove background (BiRefNet lite: on the GPU where ONNX Runtime supports it, otherwise the CPU) and Upscale (Real-ESRGAN, needs a graphics card with Vulkan). Optional, and downloaded from ${source} only when you ask. Your images never leave this computer.`,
    installed: (version) => `AI pack ${version}`,
  },
};

export function usePack(pack: PackId) {
  const [status, setStatus] = useState<PackStatus | null>(null);
  const [progress, setProgress] = useState<PackProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);

  const refresh = useCallback(async () => setStatus(await packStatus(pack)), [pack]);

  useEffect(() => {
    let live = true;
    const offs: Array<() => void> = [];
    void packStatus(pack).then((s) => live && setStatus(s));
    void onPackProgress(pack, (p) => {
      setProgress(p);
      setStatus((s) => (s && s.state !== "installing" ? { ...s, state: "installing" } : s));
    }).then((off) => (live ? offs.push(off) : off()));
    void onPackChanged(pack, (s) => {
      setStatus(s);
      setProgress(null);
    }).then((off) => (live ? offs.push(off) : off()));
    return () => {
      live = false;
      offs.forEach((off) => off());
    };
  }, [pack]);

  /** Starts the download (the user just agreed to it). Resolves true once installed. */
  const install = useCallback(async () => {
    setError(null);
    setStatus((s) => s && { ...s, state: "installing" });
    try {
      setStatus(await installPack(pack));
      return true;
    } catch (e) {
      const message = String(e);
      if (message !== PACK_CANCELLED) setError(message);
      await refresh();
      return false;
    } finally {
      setProgress(null);
    }
  }, [pack, refresh]);

  const cancel = useCallback(() => void cancelPack(pack), [pack]);

  const remove = useCallback(async () => {
    setError(null);
    setRemoving(true);
    try {
      setStatus(await removePack(pack));
    } catch (e) {
      setError(String(e));
    } finally {
      setRemoving(false);
    }
  }, [pack]);

  return { status, progress, error, removing, install, cancel, remove };
}

export function formatBytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${Math.round(n / 1e6)} MB`;
  return `${Math.max(1, Math.round(n / 1e3))} KB`;
}

/** "Downloading… 45 of 217 MB" / "Checking the download…" / "Unpacking…". */
export function progressLabel(p: PackProgress | null): string {
  if (!p) return "Starting…";
  if (p.phase === "download") return `Downloading… ${formatBytes(p.done)} of ${formatBytes(p.total)}`;
  if (p.phase === "verify") return "Checking the download…";
  return "Unpacking…";
}

/** 0..1 across all phases (download dominates). */
export function progressFraction(p: PackProgress | null): number {
  if (!p || !p.total) return 0;
  const f = Math.min(1, p.done / p.total);
  const [start, span] = p.phase === "download" ? [0, 0.8] : p.phase === "verify" ? [0.8, 0.05] : [0.85, 0.15];
  return start + f * span;
}
