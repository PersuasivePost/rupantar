import { useEffect } from "react";
import { motion, useReducedMotion } from "framer-motion";
import { Download, X } from "lucide-react";
import type { Operation } from "../api";
import { formatBytes, progressFraction, progressLabel, useOfficePack } from "../lib/useOfficePack";

/**
 * Asks before downloading the Office pack for `op` (the first time an Office operation is
 * chosen), then shows progress. `onDone(true)` once it is installed, so the operation can run;
 * `onDone(false)` when the user declines or closes.
 */
export function OfficePackDialog({ op, onDone }: { op: Operation; onDone: (installed: boolean) => void }) {
  const reduced = !!useReducedMotion();
  const { status, progress, error, install, cancel } = useOfficePack();
  const installing = status?.state === "installing";
  const offer = status?.offer;

  // Already there (e.g. installed from Settings meanwhile): go straight on.
  useEffect(() => {
    if (status?.state === "installed") onDone(true);
  }, [status?.state, onDone]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      if (installing) cancel();
      else onDone(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [cancel, installing, onDone]);

  const start = async () => {
    if (await install()) onDone(true);
  };

  const button = "rounded-[10px] px-3 py-1.5 text-xs font-medium transition-colors";
  const resumeFrom = offer && offer.downloaded_bytes > 0 && !installing ? offer.downloaded_bytes : 0;
  return (
    <motion.div
      className="fixed inset-0 z-[60] flex items-center justify-center select-none"
      style={{ background: "var(--scrim)" }}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: reduced ? 0.1 : 0.18 }}
      onClick={(e) => e.stopPropagation()}
      role="dialog"
      aria-label="Office pack needed"
    >
      <motion.div
        className="flex w-[340px] flex-col gap-3 rounded-2xl border p-4 text-xs"
        style={{
          background: "linear-gradient(var(--shell-under, transparent), var(--shell-under, transparent)), var(--panel)",
          borderColor: "var(--panel-border)", backdropFilter: "blur(18px)", WebkitBackdropFilter: "blur(18px)",
        }}
        initial={{ scale: reduced ? 1 : 0.96 }}
        animate={{ scale: 1 }}
        transition={{ type: "spring", stiffness: 420, damping: 30 }}
      >
        <header className="flex items-center gap-2">
          <Download size={14} style={{ color: "var(--accent)" }} />
          <span className="flex-1 font-semibold">{op.label} needs the Office pack</span>
          {!installing && (
            <button aria-label="Close" onClick={() => onDone(false)} className="rounded-md p-1" style={{ color: "var(--text-dim)" }}>
              <X size={14} />
            </button>
          )}
        </header>

        {!status && <p style={{ color: "var(--text-dim)" }}>Checking…</p>}

        {status?.state === "unsupported" && (
          <p>There is no Office pack for this system yet, so Office conversions aren’t available here.</p>
        )}

        {offer && (status?.state === "not_installed" || installing) && (
          <>
            <p className="leading-relaxed">
              Word, Excel and PowerPoint conversions use <b>LibreOffice {offer.version}</b>, which isn’t included with
              Rupantar. Download it from {offer.source}?
            </p>
            <p className="tabular-nums" style={{ color: "var(--text-dim)" }}>
              {formatBytes(offer.download_bytes)} download · about {formatBytes(offer.installed_bytes)} on disk
              {resumeFrom > 0 && ` · resumes from ${formatBytes(resumeFrom)}`}
            </p>
            <p className="leading-relaxed" style={{ color: "var(--text-dim)" }}>
              This download is the only time Rupantar goes online. Conversions still happen on this computer. You can
              remove the pack in Settings.
            </p>
            {!offer.tested && (
              <p style={{ color: "#f59e0b" }}>The Office pack hasn’t been tested on this platform yet.</p>
            )}
          </>
        )}

        {installing && (
          <div className="flex flex-col gap-1.5">
            <div className="h-1.5 overflow-hidden rounded-full" style={{ background: "var(--track)" }}>
              <motion.div
                className="h-full rounded-full"
                style={{ background: "var(--accent)", originX: 0 }}
                animate={{ scaleX: progressFraction(progress) }}
                transition={{ duration: reduced ? 0 : 0.2 }}
              />
            </div>
            <span className="tabular-nums" style={{ color: "var(--text-dim)" }}>{progressLabel(progress)}</span>
          </div>
        )}

        {error && <p role="alert" style={{ color: "#f87171" }}>{error}</p>}

        <footer className="flex justify-end gap-2">
          {installing ? (
            <button className={button} style={{ color: "var(--text)", border: "1px solid var(--panel-border)" }} onClick={cancel}>
              Cancel download
            </button>
          ) : (
            <>
              <button className={button} style={{ color: "var(--text)", border: "1px solid var(--panel-border)" }} onClick={() => onDone(false)}>
                {status?.state === "unsupported" ? "Close" : "Not now"}
              </button>
              {status?.state === "not_installed" && (
                <button className={button} style={{ background: "var(--accent)", color: "#fff" }} onClick={() => void start()}>
                  {error ? "Try again" : op.label.startsWith("To ") ? `Download & convert to ${op.label.slice(3)}` : "Download"}
                </button>
              )}
            </>
          )}
        </footer>
      </motion.div>
    </motion.div>
  );
}
