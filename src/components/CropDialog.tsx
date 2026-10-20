import { useCallback, useEffect, useRef, useState } from "react";
import { motion, useReducedMotion } from "framer-motion";
import { Crop, X } from "lucide-react";
import { previewImage, type ImagePreview, type Operation, type Options } from "../api";
import { needsPack } from "../lib/operations";
import { PackDialog } from "./PackDialog";

/** In original-image pixels. */
interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

type Handle = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";
type Drag =
  | { mode: "new"; ax: number; ay: number }
  | { mode: "move"; px: number; py: number; start: Rect }
  | { mode: Handle; start: Rect };

const HANDLES: Handle[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];
const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

/**
 * Asks for whatever an operation needs before it can run and resolves to its options, or null
 * when cancelled: a missing pack (`needsPack`: the offer to download it; resolves `{}` once
 * installed) or a prompt (`op.prompt`, e.g. `crop`). Render `element` somewhere above the menu.
 */
export function usePrompt() {
  const [req, setReq] = useState<{ op: Operation; parent?: Operation; path: string; name: string; resolve: (o: Options | null) => void } | null>(null);
  /** `parent`: the operation `op` is a sub-option of, which the download dialog names. */
  const prompt = useCallback(
    (op: Operation, path: string, name: string, parent?: Operation) =>
      new Promise<Options | null>((resolve) => {
        if (!needsPack(op) && op.prompt !== "crop") return resolve(null); // unknown prompt: nothing to ask
        setReq({ op, parent, path, name, resolve });
      }),
    [],
  );
  const finish = useCallback((options: Options | null) => {
    setReq((r) => {
      r?.resolve(options);
      return null;
    });
  }, []);
  const onPack = useCallback((installed: boolean) => finish(installed ? {} : null), [finish]);
  const element = req && (needsPack(req.op)
    ? <PackDialog key={req.op.id} pack={req.op.pack === "ai" ? "ai" : "office"} op={req.parent ?? req.op} onDone={onPack} />
    : <CropDialog key={req.path} path={req.path} name={req.name} onDone={finish} />);
  /** A download dialog is up: the window must stay while the user switches away. */
  const busy = !!req && needsPack(req.op);
  return { prompt, element, busy };
}

/** Pick a crop rectangle over a preview: drag to draw, drag inside to move, handles to resize. */
export function CropDialog({ path, name, onDone }: { path: string; name: string; onDone: (o: Options | null) => void }) {
  const reduced = !!useReducedMotion();
  const [preview, setPreview] = useState<ImagePreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rect, setRect] = useState<Rect | null>(null);
  const drag = useRef<Drag | null>(null);
  const stage = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    previewImage(path).then(
      (p) => {
        if (!live) return;
        setPreview(p);
        const [mx, my] = [Math.round(p.width * 0.1), Math.round(p.height * 0.1)];
        setRect({ x: mx, y: my, w: p.width - 2 * mx, h: p.height - 2 * my });
      },
      (e) => live && setError(String(e)),
    );
    return () => {
      live = false;
    };
  }, [path]);

  // Fit the preview in the window, leaving room for the header and buttons.
  const box = { w: Math.min(760, window.innerWidth - 48), h: Math.min(520, window.innerHeight - 150) };
  const scale = preview ? Math.min(box.w / preview.width, box.h / preview.height) : 1;
  const view = preview ? { w: Math.round(preview.width * scale), h: Math.round(preview.height * scale) } : box;
  /** Smallest selection: about 8 screen px, but at least one image pixel. */
  const min = Math.max(1, Math.ceil(8 / scale));

  const confirm = useCallback(() => {
    if (!rect) return;
    onDone({ x: String(rect.x), y: String(rect.y), width: String(rect.w), height: String(rect.h) });
  }, [onDone, rect]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onDone(null);
      else if (e.key === "Enter") confirm();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [confirm, onDone]);

  /** Pointer position in original pixels, clamped to the image. */
  const toImage = (e: React.PointerEvent) => {
    const r = stage.current!.getBoundingClientRect();
    return {
      x: clamp(Math.round((e.clientX - r.left) / scale), 0, preview!.width),
      y: clamp(Math.round((e.clientY - r.top) / scale), 0, preview!.height),
    };
  };

  const onPointerDown = (e: React.PointerEvent) => {
    if (!preview || !rect) return;
    e.preventDefault();
    stage.current!.setPointerCapture(e.pointerId);
    const p = toImage(e);
    const handle = (e.target as HTMLElement).dataset.handle as Handle | undefined;
    const inside = p.x >= rect.x && p.x <= rect.x + rect.w && p.y >= rect.y && p.y <= rect.y + rect.h;
    drag.current = handle
      ? { mode: handle, start: rect }
      : inside
        ? { mode: "move", px: p.x, py: p.y, start: rect }
        : { mode: "new", ax: p.x, ay: p.y };
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d || !preview) return;
    const p = toImage(e);
    const { width: W, height: H } = preview;
    if (d.mode === "new") {
      const x0 = Math.min(d.ax, p.x);
      const y0 = Math.min(d.ay, p.y);
      setRect({ x: x0, y: y0, w: Math.max(min, Math.abs(p.x - d.ax)), h: Math.max(min, Math.abs(p.y - d.ay)) });
      return;
    }
    const s = d.start;
    if (d.mode === "move") {
      setRect({ ...s, x: clamp(s.x + p.x - d.px, 0, W - s.w), y: clamp(s.y + p.y - d.py, 0, H - s.h) });
      return;
    }
    let { x, y, w, h } = s;
    if (d.mode.includes("w")) {
      x = clamp(p.x, 0, s.x + s.w - min);
      w = s.x + s.w - x;
    }
    if (d.mode.includes("e")) w = clamp(p.x - s.x, min, W - s.x);
    if (d.mode.includes("n")) {
      y = clamp(p.y, 0, s.y + s.h - min);
      h = s.y + s.h - y;
    }
    if (d.mode.includes("s")) h = clamp(p.y - s.y, min, H - s.y);
    setRect({ x, y, w, h });
  };

  const onPointerUp = () => {
    drag.current = null;
    // A new rectangle drawn near an edge can stick out by the minimum size: pull it back in.
    setRect((r) => r && preview && {
      x: clamp(r.x, 0, preview.width - 1),
      y: clamp(r.y, 0, preview.height - 1),
      w: Math.min(r.w, preview.width - clamp(r.x, 0, preview.width - 1)),
      h: Math.min(r.h, preview.height - clamp(r.y, 0, preview.height - 1)),
    });
  };

  const button = "rounded-[10px] px-3 py-1.5 text-xs font-medium transition-colors";
  return (
    <motion.div
      className="fixed inset-0 z-[60] flex items-center justify-center select-none"
      style={{ background: "var(--scrim)" }}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: reduced ? 0.1 : 0.18 }}
      onClick={(e) => e.stopPropagation()}
      role="dialog"
      aria-label={`Crop ${name}`}
    >
      <motion.div
        className="flex flex-col gap-3 rounded-2xl border p-3"
        style={{
          background: "linear-gradient(var(--shell-under, transparent), var(--shell-under, transparent)), var(--panel)",
          borderColor: "var(--panel-border)", backdropFilter: "blur(18px)", WebkitBackdropFilter: "blur(18px)",
        }}
        initial={{ scale: reduced ? 1 : 0.96 }}
        animate={{ scale: 1 }}
        transition={{ type: "spring", stiffness: 420, damping: 30 }}
      >
        <header className="flex items-center gap-2 text-xs">
          <Crop size={14} style={{ color: "var(--accent)" }} />
          <span className="font-semibold">Crop</span>
          <span className="min-w-0 flex-1 truncate" style={{ color: "var(--text-dim)" }}>{name}</span>
          <button aria-label="Cancel" onClick={() => onDone(null)} className="rounded-md p-1" style={{ color: "var(--text-dim)" }}>
            <X size={14} />
          </button>
        </header>

        <div
          ref={stage}
          className="relative overflow-hidden rounded-lg"
          style={{
            width: view.w, height: view.h, cursor: "crosshair", touchAction: "none",
            background: "repeating-conic-gradient(#8884 0% 25%, transparent 0% 50%) 0 0 / 16px 16px",
          }}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
        >
          {preview && <img src={preview.src} alt="" draggable={false} className="absolute inset-0 h-full w-full" />}
          {!preview && (
            <div className="absolute inset-0 flex items-center justify-center text-xs" style={{ color: "var(--text-dim)" }}>
              {error ? `Couldn't open the image: ${error}` : "Loading preview…"}
            </div>
          )}
          {preview && rect && (
            <div
              className="absolute"
              style={{
                left: rect.x * scale, top: rect.y * scale, width: rect.w * scale, height: rect.h * scale,
                boxShadow: "0 0 0 9999px rgba(0, 0, 0, 0.5)", outline: "1.5px solid #fff", cursor: "move",
              }}
            >
              {/* Rule-of-thirds guides. */}
              {[1, 2].map((i) => (
                <div key={`v${i}`} className="pointer-events-none absolute top-0 bottom-0 w-px" style={{ left: `${(i * 100) / 3}%`, background: "#fff6" }} />
              ))}
              {[1, 2].map((i) => (
                <div key={`h${i}`} className="pointer-events-none absolute left-0 right-0 h-px" style={{ top: `${(i * 100) / 3}%`, background: "#fff6" }} />
              ))}
              {HANDLES.map((hd) => (
                <div
                  key={hd}
                  data-handle={hd}
                  className="absolute h-3 w-3 rounded-[3px]"
                  style={{
                    left: hd.includes("w") ? -6 : hd.includes("e") ? "calc(100% - 6px)" : "calc(50% - 6px)",
                    top: hd.includes("n") ? -6 : hd.includes("s") ? "calc(100% - 6px)" : "calc(50% - 6px)",
                    background: "#fff", boxShadow: "0 0 0 1.5px var(--accent)", cursor: `${hd}-resize`,
                  }}
                />
              ))}
            </div>
          )}
        </div>

        <footer className="flex items-center gap-2 text-xs">
          <span className="tabular-nums" style={{ color: "var(--text-dim)" }}>
            {rect && preview ? `${rect.w} × ${rect.h} px · from ${preview.width} × ${preview.height}` : " "}
          </span>
          <span className="flex-1" />
          <button
            className={button}
            style={{ color: "var(--text-dim)" }}
            disabled={!preview}
            onClick={() => preview && setRect({ x: 0, y: 0, w: preview.width, h: preview.height })}
          >
            Select all
          </button>
          <button className={button} style={{ color: "var(--text)", border: "1px solid var(--panel-border)" }} onClick={() => onDone(null)}>
            Cancel
          </button>
          <button
            className={button}
            style={{ background: "var(--accent)", color: "#fff", opacity: rect ? 1 : 0.5 }}
            disabled={!rect}
            onClick={confirm}
          >
            Crop
          </button>
        </footer>
      </motion.div>
    </motion.div>
  );
}
