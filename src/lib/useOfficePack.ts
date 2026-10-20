// Office pack state shared by the consent dialog (radial window) and Settings: status, live
// progress (from either window's download), and actions.
import { useCallback, useEffect, useState } from "react";
import {
  PACK_CANCELLED, cancelOfficePack, installOfficePack, officePackStatus, onOfficePackChanged, onOfficePackProgress,
  removeOfficePack, type PackProgress, type PackStatus,
} from "../api";

export function useOfficePack() {
  const [status, setStatus] = useState<PackStatus | null>(null);
  const [progress, setProgress] = useState<PackProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);

  const refresh = useCallback(async () => setStatus(await officePackStatus()), []);

  useEffect(() => {
    let live = true;
    const offs: Array<() => void> = [];
    void officePackStatus().then((s) => live && setStatus(s));
    void onOfficePackProgress((p) => {
      setProgress(p);
      setStatus((s) => (s && s.state !== "installing" ? { ...s, state: "installing" } : s));
    }).then((off) => (live ? offs.push(off) : off()));
    void onOfficePackChanged((s) => {
      setStatus(s);
      setProgress(null);
    }).then((off) => (live ? offs.push(off) : off()));
    return () => {
      live = false;
      offs.forEach((off) => off());
    };
  }, []);

  /** Starts the download (the user just agreed to it). Resolves true once installed. */
  const install = useCallback(async () => {
    setError(null);
    setStatus((s) => s && { ...s, state: "installing" });
    try {
      setStatus(await installOfficePack());
      return true;
    } catch (e) {
      const message = String(e);
      if (message !== PACK_CANCELLED) setError(message);
      await refresh();
      return false;
    } finally {
      setProgress(null);
    }
  }, [refresh]);

  const cancel = useCallback(() => void cancelOfficePack(), []);

  const remove = useCallback(async () => {
    setError(null);
    setRemoving(true);
    try {
      setStatus(await removeOfficePack());
    } catch (e) {
      setError(String(e));
    } finally {
      setRemoving(false);
    }
  }, []);

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
