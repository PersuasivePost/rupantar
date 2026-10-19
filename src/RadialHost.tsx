import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { FileDown } from "lucide-react";
import {
  describeFiles, frontendReady, hideRadial, listOperations, onFilesDropped, onSummon,
  runOperation, setHideOnBlur, type FileBatch, type Operation, type SummonPayload,
} from "./api";
import { usePrompt } from "./components/CropDialog";
import { RadialMenu } from "./components/RadialMenu";

type Origin = { x: number; y: number };
type State =
  | { kind: "idle" }
  | { kind: "await"; origin: Origin }
  | { kind: "menu"; id: number; origin: Origin; batch: FileBatch; operations: Operation[] };

/** How long the exit animation needs before the OS window can be hidden without clipping it. */
const EXIT_MS = 380;

/** The transparent desktop window: waits for files, then shows the radial menu at the cursor. */
export default function RadialHost() {
  const [state, setState] = useState<State>({ kind: "idle" });
  const [dragOver, setDragOver] = useState(false);
  const { prompt, element: promptElement } = usePrompt();
  const origin = useRef<Origin>({ x: 300, y: 300 });
  const nextId = useRef(1);
  const hideTimer = useRef<number>();

  const cancelHide = () => window.clearTimeout(hideTimer.current);

  const open = useCallback(async (at: Origin, paths: string[]) => {
    cancelHide();
    try {
      const batch = await describeFiles(paths);
      const operations = await listOperations(batch.mimes);
      setState({ kind: "menu", id: nextId.current++, origin: at, batch, operations });
    } catch (e) {
      console.error(e);
      setState({ kind: "await", origin: at }); // unreadable drop: keep waiting
    }
  }, []);

  const close = useCallback(() => {
    setState({ kind: "idle" });
    cancelHide();
    hideTimer.current = window.setTimeout(() => void hideRadial(), EXIT_MS);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.shell = "radial";
    let cancelled = false;
    const unlisten: Array<() => void> = [];

    const onSummonPayload = (p: SummonPayload) => {
      cancelHide();
      origin.current = p.cursor;
      setDragOver(false);
      if (p.paths.length) void open(p.cursor, p.paths);
      else setState({ kind: "await", origin: p.cursor });
    };

    (async () => {
      const offSummon = await onSummon(onSummonPayload);
      const offDrop = await onFilesDropped((e) => {
        if (e.type === "over") setDragOver(true);
        else if (e.type === "leave") setDragOver(false);
        else {
          setDragOver(false);
          if (e.paths.length) void open(origin.current, e.paths);
        }
      });
      if (cancelled) return offSummon(), offDrop();
      unlisten.push(offSummon, offDrop);
      const pending = await frontendReady(); // a summon may have fired before we were listening
      if (pending && !cancelled) onSummonPayload(pending);
    })();

    return () => {
      cancelled = true;
      unlisten.forEach((f) => f());
    };
  }, [open]);

  // Clicking away dismisses an open menu, but must not cancel the "drop files here" state: the
  // user is about to drag from another window, which takes focus.
  useEffect(() => {
    void setHideOnBlur(state.kind === "menu");
  }, [state.kind]);

  return (
    <AnimatePresence>
      {state.kind === "await" && (
        <DropTarget key="await" origin={state.origin} active={dragOver} onDismiss={close} />
      )}
      {state.kind === "menu" && (
        <RadialMenu
          key={state.id}
          scrim={false}
          origin={state.origin}
          file={{ name: state.batch.label, mime: state.batch.mime }}
          operations={state.operations}
          onConfirm={(op, parent, options) =>
            runOperation(op, state.batch.paths, { ...(parent && { parent: parent.id }), ...options })
          }
          onPrompt={(op) => prompt(op, state.batch.paths[0], state.batch.label)}
          onDismiss={close}
          onDone={close}
        />
      )}
      {promptElement}
    </AnimatePresence>
  );
}

function DropTarget({ origin, active, onDismiss }: { origin: Origin; active: boolean; onDismiss: () => void }) {
  const reduced = !!useReducedMotion();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onDismiss();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onDismiss]);

  const size = 132;
  return (
    <motion.div
      className="fixed inset-0 z-50 select-none"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.15 }}
      onClick={onDismiss}
      onContextMenu={(e) => { e.preventDefault(); onDismiss(); }}
      role="dialog"
      aria-label="Drop files to convert"
    >
      <motion.div
        className="absolute flex flex-col items-center justify-center gap-1 rounded-full text-center"
        style={{
          left: origin.x - size / 2, top: origin.y - size / 2, width: size, height: size,
          background: "linear-gradient(var(--shell-under, transparent), var(--shell-under, transparent)), var(--disc-bg)",
          backdropFilter: "blur(22px) saturate(1.5)", WebkitBackdropFilter: "blur(22px) saturate(1.5)",
          willChange: "transform",
        }}
        initial={{ scale: reduced ? 1 : 0.6 }}
        animate={{ scale: active && !reduced ? 1.1 : 1 }}
        exit={{ scale: reduced ? 1 : 0.6 }}
        transition={{ type: "spring", stiffness: 420, damping: 28 }}
      >
        <div className="grad-border" />
        <motion.div
          className="pointer-events-none absolute inset-0 rounded-full"
          style={{ background: "var(--accent)", filter: "blur(14px)" }}
          animate={{ opacity: active ? 0.45 : 0 }}
          transition={{ duration: 0.15 }}
        />
        <FileDown size={28} strokeWidth={1.6} className="relative" />
        <span className="relative text-[11px] font-medium">Drop files</span>
        <span className="relative text-[9px]" style={{ color: "var(--text-dim)" }}>Esc to cancel</span>
      </motion.div>
    </motion.div>
  );
}
