import { useCallback, useRef, useState } from "react";
import { AnimatePresence } from "framer-motion";
import { Monitor, Moon, MousePointerClick, Sun, type LucideIcon } from "lucide-react";
import { listOperations, runOperation, type Operation } from "./api";
import { fileIcon } from "./icons";
import { SAMPLE_FILES, mimeFromFile, type SampleFile } from "./mockFiles";
import { RadialMenu } from "./components/RadialMenu";

type Theme = "system" | "light" | "dark";

interface OpenMenu {
  id: number;
  origin: { x: number; y: number };
  file: { name: string; mime: string };
  operations: Operation[];
}

function applyTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

/** Dev playground: pick a mock file type, click (or drop a file) anywhere to summon the menu there. */
export default function Playground() {
  const [selected, setSelected] = useState<SampleFile>(SAMPLE_FILES[0]);
  const [menu, setMenu] = useState<OpenMenu | null>(null);
  const [log, setLog] = useState<string[]>([]);
  const [theme, setTheme] = useState<Theme>("system");
  const nextId = useRef(1);

  const summon = useCallback(async (x: number, y: number, file: { name: string; mime: string }) => {
    const operations = await listOperations(file.mime);
    setMenu({ id: nextId.current++, origin: { x, y }, file, operations });
  }, []);

  const onStageClick = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest("[data-no-menu]")) return;
    void summon(e.clientX, e.clientY, selected);
  };

  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    const f = e.dataTransfer.files[0];
    void summon(e.clientX, e.clientY, f ? { name: f.name, mime: mimeFromFile(f) } : selected);
  };

  const onConfirm = useCallback(
    async (op: Operation, parent?: Operation) => {
      if (!menu) return;
      const out = await runOperation(menu.file.mime, op, [menu.file.name], parent ? { parent: parent.id } : {});
      const via = parent ? `${parent.label} › ${op.label}` : op.label;
      setLog((l) => [`${menu.file.name} → ${via} → ${out[0]}`, ...l].slice(0, 5));
    },
    [menu],
  );

  const close = useCallback(() => setMenu(null), []);

  return (
    <div
      className="relative h-full w-full cursor-crosshair overflow-hidden"
      style={{ background: "radial-gradient(60% 50% at 50% 40%, var(--bg-glow), transparent), var(--bg)" }}
      onClick={onStageClick}
      onDragOver={(e) => e.preventDefault()}
      onDrop={onDrop}
    >
      <header data-no-menu className="absolute left-0 right-0 top-0 flex flex-wrap items-center justify-between gap-3 p-4 cursor-default">
        <div>
          <h1 className="text-sm font-semibold tracking-wide">Rupantar · Radial playground</h1>
          <p className="text-xs" style={{ color: "var(--text-dim)" }}>Mock data · respects OS reduced-motion</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Segmented
            label="File type"
            value={selected.label}
            options={SAMPLE_FILES.map((f) => ({ value: f.label, label: f.label, Icon: fileIcon(f.mime) }))}
            onChange={(v) => setSelected(SAMPLE_FILES.find((f) => f.label === v)!)}
          />
          <Segmented
            label="Theme"
            value={theme}
            options={[
              { value: "system", label: "Auto", Icon: Monitor },
              { value: "light", label: "Light", Icon: Sun },
              { value: "dark", label: "Dark", Icon: Moon },
            ]}
            onChange={(v) => { setTheme(v as Theme); applyTheme(v as Theme); }}
          />
        </div>
      </header>

      <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center gap-2 text-center transition-opacity duration-200"
        style={{ opacity: menu ? 0 : 1 }}>
        <MousePointerClick size={28} style={{ color: "var(--text-dim)" }} />
        <p className="text-sm">Click anywhere, or drop a file</p>
        <p className="text-xs" style={{ color: "var(--text-dim)" }}>
          Flick toward a wedge, click to confirm · Esc or move away to cancel · arrows + Enter work too
        </p>
        <p className="text-xs" style={{ color: "var(--text-dim)" }}>Wedges with a dot have sub-options (Compress, Resize)</p>
      </div>

      {log.length > 0 && (
        <ul data-no-menu className="absolute bottom-4 left-4 max-w-[calc(100%-2rem)] space-y-1 rounded-xl border p-3 text-xs cursor-default"
          style={{ background: "var(--panel)", borderColor: "var(--panel-border)", backdropFilter: "blur(12px)" }}>
          {log.map((l, i) => <li key={i} className="truncate">{l}</li>)}
        </ul>
      )}

      <AnimatePresence>
        {menu && (
          <RadialMenu
            key={menu.id}
            origin={menu.origin}
            file={menu.file}
            operations={menu.operations}
            onConfirm={onConfirm}
            onDismiss={close}
            onDone={close}
          />
        )}
      </AnimatePresence>
    </div>
  );
}

function Segmented<T extends string>({ label, value, options, onChange }: {
  label: string;
  value: T;
  options: { value: T; label: string; Icon: LucideIcon }[];
  onChange: (v: T) => void;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="flex rounded-xl border p-0.5"
      style={{ background: "var(--panel)", borderColor: "var(--panel-border)", backdropFilter: "blur(12px)" }}>
      {options.map(({ value: v, label: l, Icon }) => (
        <button key={v} role="radio" aria-checked={v === value} onClick={() => onChange(v)}
          className="flex items-center gap-1.5 rounded-[10px] px-2.5 py-1.5 text-xs font-medium transition-colors"
          style={v === value ? { background: "var(--accent)", color: "#fff" } : { color: "var(--text-dim)" }}>
          <Icon size={14} />{l}
        </button>
      ))}
    </div>
  );
}
