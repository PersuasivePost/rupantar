import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import {
  AnimatePresence, animate, motion, useMotionValue, useReducedMotion, useSpring,
  type Variants,
} from "framer-motion";
import { X, type LucideIcon } from "lucide-react";
import type { Operation } from "../api";
import { fileIcon, iconFor } from "../icons";
import { TAU, angleFromUp, pickInSpan, pickWedge, polar, sectorPath } from "../lib/radial";

// Geometry (px). Everything is laid out once in a SIZE x SIZE box centered on the cursor;
// all animation is transform/opacity on those fixed layers.
const SIZE = 540;
const C = SIZE / 2;
const R_DISC = 52;
const R_IN = 64;
const R_OUT = 168;
const S_IN = 178;
const S_OUT = 240;
const DEAD = 34;
const GAP = 3;

type Phase = "choosing" | "running" | "done" | "error";

interface Selection {
  hi: number | null;
  sub: number | null;
  expanded: number | null;
  dismiss: boolean;
}

export interface RadialMenuProps {
  /** Cursor position; the menu is centered here (clamped to stay on screen). */
  origin: { x: number; y: number };
  file: { name: string; mime: string };
  /** 4-8 operations for this file type. */
  operations: Operation[];
  /** Runs the chosen operation; `parent` is set when it came from the sub ring. */
  onConfirm: (op: Operation, parent?: Operation) => Promise<unknown>;
  onDismiss: () => void;
  /** Called after the success animation finishes. */
  onDone: () => void;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const spring = { type: "spring", stiffness: 420, damping: 28, mass: 0.8 } as const;

export function RadialMenu({ origin, file, operations, onConfirm, onDismiss, onDone }: RadialMenuProps) {
  const reduced = !!useReducedMotion();
  const uid = useId().replace(/:/g, "");
  const n = operations.length;
  const step = TAU / n;

  const center = useMemo(() => {
    const m = R_OUT + 24;
    return {
      x: Math.min(Math.max(origin.x, m), Math.max(m, window.innerWidth - m)),
      y: Math.min(Math.max(origin.y, m), Math.max(m, window.innerHeight - m)),
    };
  }, [origin.x, origin.y]);

  const [hi, setHi] = useState<number | null>(null);
  const [sub, setSub] = useState<number | null>(null);
  const [expanded, setExpanded] = useState<number | null>(null);
  const [phase, setPhase] = useState<Phase>("choosing");
  const [chosen, setChosen] = useState("");

  // Handlers read the latest state through a ref so they never act on stale closures.
  const latest = useRef({ hi, sub, expanded, phase });
  latest.current = { hi, sub, expanded, phase };
  const timers = useRef<number[]>([]);
  useEffect(() => () => timers.current.forEach(clearTimeout), []);

  const progress = useMotionValue(0);
  const aimX = useSpring(0, { stiffness: 500, damping: 34 });
  const aimY = useSpring(0, { stiffness: 500, damping: 34 });

  const resolve = useCallback(
    (x: number, y: number, exp: number | null): Selection => {
      const dx = x - center.x;
      const dy = y - center.y;
      const dist = Math.hypot(dx, dy);
      if (dist > (exp !== null ? S_OUT + 60 : R_OUT + 70)) {
        return { hi: null, sub: null, expanded: null, dismiss: true };
      }
      if (dist < DEAD) return { hi: null, sub: null, expanded: exp, dismiss: false };
      const a = angleFromUp(dx, dy);
      if (exp !== null && dist > R_OUT + 4) {
        const kids = operations[exp].children ?? [];
        return { hi: exp, sub: pickInSpan(a, exp * step - step / 2, step, kids.length), expanded: exp, dismiss: false };
      }
      const h = pickWedge(a, n);
      return { hi: h, sub: null, expanded: h === exp ? exp : null, dismiss: false };
    },
    [center, n, operations, step],
  );

  const apply = useCallback(
    (s: Selection) => {
      if (s.dismiss) return onDismiss();
      setHi(s.hi);
      setSub(s.sub);
      setExpanded(s.expanded);
    },
    [onDismiss],
  );

  const confirm = useCallback(
    async (op: Operation, parent?: Operation) => {
      setChosen(op.label);
      setPhase("running");
      progress.set(0);
      const ramp = animate(progress, 0.85, { duration: 2.4, ease: [0.2, 0.8, 0.3, 1] });
      const later = (fn: () => void, ms: number) => void timers.current.push(window.setTimeout(fn, ms));
      try {
        // Hold the ring on screen long enough to read even when the work is instant.
        await Promise.all([onConfirm(op, parent), sleep(reduced ? 250 : 700)]);
        ramp.stop();
        await animate(progress, 1, { duration: reduced ? 0.1 : 0.25 });
        setPhase("done");
        later(onDone, reduced ? 500 : 950);
      } catch {
        ramp.stop();
        setPhase("error");
        later(onDismiss, 1100);
      }
    },
    [onConfirm, onDismiss, onDone, progress, reduced],
  );

  const activate = useCallback(
    (s: Selection) => {
      if (s.dismiss || s.hi === null) return onDismiss();
      const op = operations[s.hi];
      if (s.sub !== null) return void confirm(op.children![s.sub], op);
      if (op.children?.length) {
        setExpanded(s.hi);
        setSub(null);
        return;
      }
      void confirm(op);
    },
    [confirm, onDismiss, operations],
  );

  const onPointerMove = (e: React.PointerEvent) => {
    if (latest.current.phase !== "choosing") return;
    const s = resolve(e.clientX, e.clientY, latest.current.expanded);
    apply(s);
    const aimed = s.hi !== null && !s.dismiss;
    const p = polar(R_DISC + 9, angleFromUp(e.clientX - center.x, e.clientY - center.y));
    aimX.set(aimed ? p.x : 0);
    aimY.set(aimed ? p.y : 0);
  };

  const onClick = (e: React.MouseEvent) => {
    e.stopPropagation(); // the click that confirms must not reach the page underneath and reopen the menu
    if (latest.current.phase !== "choosing") return;
    activate(resolve(e.clientX, e.clientY, latest.current.expanded));
  };

  // Keyboard: arrows move the highlight, Enter confirms, Esc steps back or dismisses.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const { hi: h, sub: s, expanded: exp, phase: ph } = latest.current;
      if (ph !== "choosing") return;
      const dir = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
      if (dir) {
        e.preventDefault();
        if (exp !== null && h === exp) {
          const k = operations[exp].children!.length;
          setSub((((s ?? (dir > 0 ? -1 : 0)) + dir) % k + k) % k);
        } else {
          setHi((((h ?? (dir > 0 ? -1 : 0)) + dir) % n + n) % n);
          setSub(null);
          setExpanded(null);
        }
      } else if (e.key === "Enter") {
        e.preventDefault();
        activate({ hi: h, sub: s, expanded: exp, dismiss: false });
      } else if (e.key === "Escape" || e.key === "Backspace") {
        if (exp !== null) {
          setExpanded(null);
          setSub(null);
        } else if (e.key === "Escape") onDismiss();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activate, n, onDismiss, operations]);

  // ---- variants -----------------------------------------------------------------------------
  // Reduced motion keeps the same states but swaps spring/scale for short opacity fades.
  const fade = { duration: 0.14 };
  const rootV: Variants = {
    hidden: { opacity: 0, transition: reduced ? fade : { duration: 0.2, delay: 0.1 } },
    open: { opacity: 1, transition: { duration: 0.12 } },
    folded: { opacity: 1 },
  };
  const ringV: Variants = {
    hidden: { scale: reduced ? 1 : 0.55, opacity: 0, transition: { duration: 0.22 } },
    open: { scale: 1, opacity: 1, transition: reduced ? fade : spring },
    folded: { scale: reduced ? 1 : 0.55, opacity: 0, transition: { duration: 0.2 } },
  };
  const wedgeV: Variants = {
    hidden: (i: number) => ({
      scale: reduced ? 1 : 0.35, opacity: 0,
      transition: { duration: 0.2, delay: reduced ? 0 : i * 0.012 },
    }),
    open: (i: number) => ({
      scale: 1, opacity: 1,
      transition: reduced ? fade : { ...spring, delay: 0.05 + i * 0.045 },
    }),
    folded: (i: number) => ({
      scale: reduced ? 1 : 0.35, opacity: 0,
      transition: { duration: 0.2, ease: "easeIn", delay: reduced ? 0 : (n - 1 - i) * 0.015 },
    }),
  };
  const discV: Variants = {
    hidden: { scale: reduced ? 1 : 0.6, opacity: 0, transition: { duration: 0.2 } },
    open: { scale: 1, opacity: 1, transition: reduced ? fade : spring },
    folded: { scale: 1, opacity: 1 },
  };
  const scrimV: Variants = {
    hidden: { opacity: 0, transition: { duration: 0.2 } },
    open: { opacity: 1, transition: { duration: 0.25 } },
    folded: { opacity: 1 },
  };
  const subGroupV: Variants = {
    hidden: { scale: reduced ? 1 : 0.82, opacity: 0, transition: { duration: 0.16 } },
    open: { scale: 1, opacity: 1, transition: reduced ? fade : spring },
  };
  const subSegV: Variants = {
    hidden: { scale: reduced ? 1 : 0.6, opacity: 0, transition: { duration: 0.14 } },
    open: (j: number) => ({
      scale: 1, opacity: 1,
      transition: reduced ? fade : { ...spring, delay: 0.03 + j * 0.04 },
    }),
  };

  const FileIcon = fileIcon(file.mime);
  const subOps = expanded !== null ? operations[expanded].children ?? [] : [];
  const activeOp = sub !== null && expanded !== null ? subOps[sub] : hi !== null ? operations[hi] : null;
  const subStart = expanded !== null ? expanded * step - step / 2 : 0;

  return (
    <motion.div
      className="fixed inset-0 z-50 select-none"
      role="menu"
      aria-label={`Convert ${file.name}`}
      variants={rootV}
      initial="hidden"
      animate={phase === "choosing" ? "open" : "folded"}
      exit="hidden"
      onPointerMove={onPointerMove}
      onClick={onClick}
      onContextMenu={(e) => { e.preventDefault(); if (phase === "choosing") onDismiss(); }}
    >
      <motion.div className="absolute inset-0" style={{ background: "var(--scrim)" }} variants={scrimV} />

      {/* Shared SVG defs (gradients + glow filter), referenced by every segment. */}
      <svg width="0" height="0" className="absolute" aria-hidden>
        <defs>
          <linearGradient id={`${uid}-edge`} x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" style={{ stopColor: "var(--stroke-a)" }} />
            <stop offset="1" style={{ stopColor: "var(--stroke-b)" }} />
          </linearGradient>
          <linearGradient id={`${uid}-hot`} x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" style={{ stopColor: "var(--accent)", stopOpacity: 0.55 }} />
            <stop offset="1" style={{ stopColor: "var(--accent-2)", stopOpacity: 0.4 }} />
          </linearGradient>
          <linearGradient id={`${uid}-prog`} x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" style={{ stopColor: "var(--accent)" }} />
            <stop offset="1" style={{ stopColor: "var(--accent-2)" }} />
          </linearGradient>
          <filter id={`${uid}-glow`} x="-30%" y="-30%" width="160%" height="160%">
            <feGaussianBlur stdDeviation="9" />
          </filter>
        </defs>
      </svg>

      <div className="pointer-events-none absolute" style={{ left: center.x - C, top: center.y - C, width: SIZE, height: SIZE }}>
        <GlassAnnulus variants={ringV} rIn={R_IN} rOut={R_OUT} />

        {operations.map((op, i) => (
          <Segment
            key={op.id}
            uid={uid}
            variants={wedgeV}
            custom={i}
            path={sectorPath(C, R_IN, R_OUT, i * step - step / 2, i * step + step / 2, GAP)}
            angle={i * step}
            rMid={(R_IN + R_OUT) / 2 + 2}
            label={op.label}
            Icon={iconFor(op.icon)}
            highlighted={hi === i && phase === "choosing"}
            hasChildren={!!op.children?.length}
            reduced={reduced}
          />
        ))}

        {/* Outer ring: sub-options of the expanded wedge. */}
        <AnimatePresence>
          {phase === "choosing" && expanded !== null && (
            <motion.div
              key={operations[expanded].id}
              className="absolute inset-0"
              variants={subGroupV}
              initial="hidden"
              animate="open"
              exit="hidden"
            >
              <GlassAnnulus rIn={S_IN} rOut={S_OUT} arc={{ start: subStart, span: step }} />
              {subOps.map((child, j) => {
                const span = step / subOps.length;
                const a0 = subStart + j * span;
                return (
                  <Segment
                    key={child.id}
                    uid={uid}
                    variants={subSegV}
                    custom={j}
                    path={sectorPath(C, S_IN, S_OUT, a0, a0 + span, GAP)}
                    angle={a0 + span / 2}
                    rMid={(S_IN + S_OUT) / 2}
                    label={child.label}
                    Icon={iconFor(child.icon)}
                    highlighted={sub === j}
                    reduced={reduced}
                    small
                  />
                );
              })}
            </motion.div>
          )}
        </AnimatePresence>

        {/* Center disc */}
        <motion.div
          className="absolute flex items-center justify-center rounded-full"
          style={{
            left: C - R_DISC, top: C - R_DISC, width: R_DISC * 2, height: R_DISC * 2,
            background: "var(--disc-bg)", backdropFilter: "blur(22px) saturate(1.5)",
            WebkitBackdropFilter: "blur(22px) saturate(1.5)", willChange: "transform",
          }}
          variants={discV}
        >
          <div className="grad-border" />
          <AnimatePresence mode="wait" initial={false}>
            {phase === "choosing" && (
              <motion.div key="idle" className="flex flex-col items-center px-2 text-center"
                initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={{ duration: 0.12 }}>
                <AnimatePresence mode="wait" initial={false}>
                  {activeOp ? (
                    <motion.div key={activeOp.id} className="flex flex-col items-center"
                      initial={{ opacity: 0, y: reduced ? 0 : 4 }} animate={{ opacity: 1, y: 0 }}
                      exit={{ opacity: 0 }} transition={{ duration: 0.1 }}>
                      <span className="text-[15px] font-semibold leading-tight">{activeOp.label}</span>
                      {activeOp.children?.length && expanded !== hi ? (
                        <span className="mt-0.5 text-[9px] uppercase tracking-wider" style={{ color: "var(--accent)" }}>click for more</span>
                      ) : (
                        <span className="mt-0.5 max-w-[78px] truncate text-[9px]" style={{ color: "var(--text-dim)" }}>{file.name}</span>
                      )}
                    </motion.div>
                  ) : (
                    <motion.div key="file" className="flex flex-col items-center"
                      initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={{ duration: 0.1 }}>
                      <FileIcon size={26} strokeWidth={1.6} />
                      <span className="mt-1 max-w-[78px] truncate text-[10px]" style={{ color: "var(--text-dim)" }}>{file.name}</span>
                    </motion.div>
                  )}
                </AnimatePresence>
              </motion.div>
            )}

            {phase === "running" && (
              <motion.div key="run" className="absolute inset-0 flex items-center justify-center"
                initial={{ opacity: 0, scale: reduced ? 1 : 0.8 }} animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0 }} transition={{ duration: 0.18 }}>
                <svg viewBox="0 0 104 104" className="absolute inset-0 h-full w-full">
                  <g transform="rotate(-90 52 52)" fill="none" strokeWidth="5" strokeLinecap="round">
                    <circle cx="52" cy="52" r="44" style={{ stroke: "var(--track)" }} />
                    <motion.circle cx="52" cy="52" r="44" stroke={`url(#${uid}-prog)`} style={{ pathLength: progress }} />
                  </g>
                </svg>
                <span className="max-w-[60px] truncate text-[10px] font-medium">{chosen}</span>
              </motion.div>
            )}

            {phase === "done" && (
              <motion.div key="done" className="absolute inset-0 flex items-center justify-center"
                initial={{ opacity: 0, scale: reduced ? 1 : 0.6 }} animate={{ opacity: 1, scale: 1 }}
                transition={reduced ? fade : spring}>
                <Burst reduced={reduced} />
                <svg viewBox="0 0 24 24" className="relative h-9 w-9" fill="none" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round" style={{ stroke: "var(--accent-2)" }} aria-label="Done">
                  <motion.path d="M5 12.5l4.5 4.5L19 7.5" initial={{ pathLength: 0 }} animate={{ pathLength: 1 }} transition={{ duration: reduced ? 0.1 : 0.32, delay: 0.05 }} />
                </svg>
              </motion.div>
            )}

            {phase === "error" && (
              <motion.div key="err" className="absolute inset-0 flex items-center justify-center"
                initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={fade}>
                <X size={30} strokeWidth={2.4} style={{ color: "#f87171" }} aria-label="Failed" />
              </motion.div>
            )}
          </AnimatePresence>
        </motion.div>

        {/* Aim dot: eases toward the cursor's direction so flicks feel anchored. */}
        {!reduced && (
          <motion.div
            className="absolute rounded-full"
            style={{
              left: C - 3, top: C - 3, width: 6, height: 6, x: aimX, y: aimY,
              background: "var(--accent)", boxShadow: "0 0 10px var(--accent)",
              opacity: phase === "choosing" && hi !== null ? 1 : 0,
            }}
          />
        )}
      </div>
    </motion.div>
  );
}

/** Frosted-glass annulus (optionally limited to an arc). One static backdrop-blur layer; only its transform/opacity animate. */
function GlassAnnulus({ variants, rIn, rOut, arc }: {
  variants?: Variants; rIn: number; rOut: number; arc?: { start: number; span: number };
}) {
  const radial = `radial-gradient(circle at ${C}px ${C}px, transparent ${rIn}px, #000 ${rIn + 1}px, #000 ${rOut}px, transparent ${rOut + 1}px)`;
  let mask = radial;
  let composite: Record<string, string> = {};
  if (arc) {
    const from = (((arc.start * 360) / TAU) % 360 + 360) % 360;
    const span = (arc.span * 360) / TAU;
    mask = `${radial}, conic-gradient(from ${from}deg at ${C}px ${C}px, #000 0deg ${span}deg, transparent ${span}deg)`;
    composite = { maskComposite: "intersect", WebkitMaskComposite: "source-in" };
  }
  return (
    <motion.div
      className="absolute inset-0"
      variants={variants}
      style={{
        background: "var(--ring-bg)", backdropFilter: "blur(18px) saturate(1.5)",
        WebkitBackdropFilter: "blur(18px) saturate(1.5)", maskImage: mask, WebkitMaskImage: mask,
        willChange: "transform", ...composite,
      }}
    />
  );
}

interface SegmentProps {
  uid: string;
  variants: Variants;
  custom: number;
  path: string;
  angle: number;
  rMid: number;
  label: string;
  Icon: LucideIcon;
  highlighted: boolean;
  hasChildren?: boolean;
  reduced: boolean;
  small?: boolean;
}

/** One wedge. The outer layer owns the bloom/collapse variants; the inner layer owns the highlight scale. */
function Segment({ uid, variants, custom, path, angle, rMid, label, Icon, highlighted, hasChildren, reduced, small }: SegmentProps) {
  const p = polar(rMid, angle);
  return (
    <motion.div className="absolute inset-0" style={{ willChange: "transform, opacity" }} variants={variants} custom={custom}>
      <motion.div
        className="absolute inset-0"
        animate={{ scale: highlighted && !reduced ? 1.05 : 1 }}
        transition={{ type: "spring", stiffness: 520, damping: 30 }}
        style={{ willChange: "transform" }}
      >
        <svg viewBox={`0 0 ${SIZE} ${SIZE}`} className="absolute inset-0 h-full w-full overflow-visible" aria-hidden>
          <motion.path d={path} style={{ fill: "var(--accent)", filter: `url(#${uid}-glow)` }}
            animate={{ opacity: highlighted ? 0.7 : 0 }} transition={{ duration: 0.14 }} />
          <path d={path} style={{ fill: "var(--wedge-fill)" }} />
          <motion.path d={path} fill={`url(#${uid}-hot)`} animate={{ opacity: highlighted ? 1 : 0 }} transition={{ duration: 0.12 }} />
          <path d={path} fill="none" stroke={`url(#${uid}-edge)`} strokeWidth="1.5" strokeLinejoin="round" />
        </svg>
        <motion.div
          role="menuitem"
          aria-label={label}
          className="absolute flex flex-col items-center"
          style={{ left: C + p.x, top: C + p.y, x: "-50%", y: "-50%" }}
          animate={{ scale: highlighted && !reduced ? 1.12 : 1, opacity: highlighted ? 1 : 0.78 }}
          transition={{ type: "spring", stiffness: 520, damping: 30 }}
        >
          <Icon size={small ? 18 : 24} strokeWidth={1.7} />
          <span className={`mt-1 font-medium leading-none ${small ? "text-[10px]" : "text-[11px]"}`}>{label}</span>
          {hasChildren && <span className="mt-1 h-[3px] w-[3px] rounded-full" style={{ background: "var(--accent)" }} />}
        </motion.div>
      </motion.div>
    </motion.div>
  );
}

/** Soft burst of dots + an expanding ring behind the checkmark. */
function Burst({ reduced }: { reduced: boolean }) {
  if (reduced) return null;
  return (
    <>
      <motion.span
        className="absolute rounded-full"
        style={{ width: 60, height: 60, border: "2px solid var(--accent-2)" }}
        initial={{ scale: 0.6, opacity: 0.7 }}
        animate={{ scale: 2.4, opacity: 0 }}
        transition={{ duration: 0.7, ease: "easeOut" }}
      />
      {Array.from({ length: 12 }, (_, i) => {
        const a = (i / 12) * TAU;
        const { x, y } = polar(i % 2 ? 62 : 82, a);
        return (
          <motion.span
            key={i}
            className="absolute rounded-full"
            style={{ width: 5, height: 5, background: i % 2 ? "var(--accent)" : "var(--accent-2)" }}
            initial={{ x: 0, y: 0, scale: 1, opacity: 1 }}
            animate={{ x, y, scale: 0.3, opacity: 0 }}
            transition={{ duration: 0.75, ease: [0.12, 0.8, 0.3, 1] }}
          />
        );
      })}
    </>
  );
}
