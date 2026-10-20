import { useCallback, useMemo, useRef, useState } from "react";
import { AnimatePresence } from "framer-motion";
import { Monitor, Moon, MousePointerClick, Sun, type LucideIcon } from "lucide-react";
import { listOperations, pickFiles, runOperation, setMockTools, setShowPlanned, type MockTools, type Operation, type Options } from "./api";
import { fileIcon } from "./icons";
import { LAYOUT_TEST_MIME, SAMPLE_FILES, commonMime, mimeFromFile } from "./mockFiles";
import { usePrompt } from "./components/CropDialog";
import { RadialMenu } from "./components/RadialMenu";

type Theme = "system" | "light" | "dark";

interface MockFile {
  name: string;
  mime: string;
}

interface OpenMenu {
  id: number;
  origin: { x: number; y: number };
  /** What the center disc shows: the file name, or "N files". */
  file: MockFile;
  names: string[];
  operations: Operation[];
}

function applyTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

/** `Report.pdf` -> `Report 2.pdf`, for the extra copies in a batch. */
const numbered = (name: string, i: number) => (i === 0 ? name : name.replace(/(\.[^.]+)?$/, ` ${i + 1}$1`));

const TOOLS: { key: keyof MockTools; label: string }[] = [
  { key: "pandoc", label: "pandoc" },
  { key: "typst", label: "typst" },
  { key: "pdfium", label: "pdfium" },
];

/**
 * Dev playground: pick one or more mock file types (and how many of each), click (or drop
 * files) anywhere to summon the menu there. Tool switches hide what a missing tool would;
 * "Planned" shows operations that aren't implemented yet, to preview the full menus.
 */
export default function Playground() {
  const [types, setTypes] = useState<string[]>([SAMPLE_FILES[0].label]);
  const [copies, setCopies] = useState("1");
  const [tools, setTools] = useState<MockTools>({ pandoc: true, typst: true, pdfium: true });
  const [planned, setPlanned] = useState(false);
  const [menu, setMenu] = useState<OpenMenu | null>(null);
  const [log, setLog] = useState<string[]>([]);
  const [theme, setTheme] = useState<Theme>("system");
  const nextId = useRef(1);

  const selection = useMemo<MockFile[]>(
    () =>
      SAMPLE_FILES.filter((f) => types.includes(f.label)).flatMap((f) =>
        Array.from({ length: Number(copies) }, (_, i) => ({ name: numbered(f.name, i), mime: f.mime })),
      ),
    [types, copies],
  );

  // The layout test isn't a real type, so it can't be combined with the others.
  const toggleType = (label: string) => {
    const isTest = (l: string) => SAMPLE_FILES.find((f) => f.label === l)?.mime === LAYOUT_TEST_MIME;
    setTypes((cur) => {
      if (cur.includes(label)) return cur.length > 1 ? cur.filter((l) => l !== label) : cur;
      return isTest(label) ? [label] : [...cur.filter((l) => !isTest(l)), label];
    });
  };

  const toggleTool = (key: keyof MockTools) => {
    const next = { ...tools, [key]: !tools[key] };
    setTools(next);
    setMockTools(next);
  };

  const togglePlanned = () => {
    setPlanned(!planned);
    setShowPlanned(!planned);
  };

  const summon = useCallback(async (x: number, y: number, files: MockFile[]) => {
    const operations = await listOperations(files.map((f) => f.mime));
    const label = files.length === 1 ? files[0].name : `${files.length} files`;
    setMenu({
      id: nextId.current++,
      origin: { x, y },
      file: { name: label, mime: commonMime(files.map((f) => f.mime)) },
      names: files.map((f) => f.name),
      operations,
    });
  }, []);

  const onStageClick = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest("[data-no-menu]")) return;
    void summon(e.clientX, e.clientY, selection);
  };

  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    const dropped = Array.from(e.dataTransfer.files, (f) => ({ name: f.name, mime: mimeFromFile(f) }));
    void summon(e.clientX, e.clientY, dropped.length ? dropped : selection);
  };

  const onConfirm = useCallback(
    async (op: Operation, parent?: Operation, options?: Options) => {
      if (!menu) return;
      const out = await runOperation(op, menu.names, { ...(parent && { parent: parent.id }), ...options });
      const via = parent ? `${parent.label} › ${op.label}` : op.label;
      const result = out.length === 1 ? out[0] : `${out.length} files`;
      const picked = options ? ` (${Object.entries(options).map(([k, v]) => `${k} ${v}`).join(", ")})` : "";
      setLog((l) => [`${menu.file.name} → ${via}${picked} → ${result}`, ...l].slice(0, 5));
    },
    [menu],
  );

  const { prompt, element: promptElement } = usePrompt();
  const onPrompt = useCallback((op: Operation) => prompt(op, menu?.names[0] ?? "", menu?.file.name ?? ""), [menu, prompt]);

  const close = useCallback(() => setMenu(null), []);

  // Double-click on the disc / Ctrl+O: add chosen files to the selection (same name once).
  const menuRef = useRef(menu);
  menuRef.current = menu;
  const browsing = useRef(false);
  const onBrowse = useCallback(async () => {
    if (browsing.current) return;
    browsing.current = true;
    try {
      const picked = await pickFiles();
      const cur = menuRef.current;
      if (!cur) return;
      const names = [...new Set([...cur.names, ...picked])];
      if (names.length === cur.names.length) return;
      const mimes = names.map((name) => mimeFromFile({ name }));
      setMenu({
        ...cur,
        id: nextId.current++,
        names,
        file: { name: names.length === 1 ? names[0] : `${names.length} files`, mime: commonMime(mimes) },
        operations: await listOperations(mimes),
      });
    } finally {
      browsing.current = false;
    }
  }, []);

  return (
    <div
      className="relative h-full w-full cursor-crosshair overflow-hidden"
      style={{ background: "radial-gradient(60% 50% at 50% 40%, var(--bg-glow), transparent), var(--bg)" }}
      onClick={onStageClick}
      onDragOver={(e) => e.preventDefault()}
      onDrop={onDrop}
    >
      <header data-no-menu className="absolute left-0 right-0 top-0 flex flex-wrap items-start justify-between gap-3 p-4 cursor-default">
        <div>
          <h1 className="text-sm font-semibold tracking-wide">Rupantar · Radial playground</h1>
          <p className="text-xs" style={{ color: "var(--text-dim)" }}>Mock data · respects OS reduced-motion</p>
        </div>
        <div className="flex flex-wrap items-center justify-end gap-2">
          <Toggles
            label="File types"
            options={SAMPLE_FILES.map((f) => ({ value: f.label, label: f.label, Icon: fileIcon(f.mime), on: types.includes(f.label) }))}
            onToggle={toggleType}
          />
          <Segmented
            label="Files of each type"
            value={copies}
            options={["1", "2", "3"].map((v) => ({ value: v, label: `×${v}` }))}
            onChange={setCopies}
          />
          <Toggles
            label="Installed tools"
            options={TOOLS.map((t) => ({ value: t.key, label: t.label, on: tools[t.key] }))}
            onToggle={(k) => toggleTool(k as keyof MockTools)}
          />
          <Toggles
            label="Preview"
            options={[{ value: "planned", label: "Planned ops", on: planned }]}
            onToggle={togglePlanned}
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

      <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center gap-2 px-4 text-center transition-opacity duration-200"
        style={{ opacity: menu ? 0 : 1 }}>
        <MousePointerClick size={28} style={{ color: "var(--text-dim)" }} />
        <p className="text-sm">Click anywhere, or drop files</p>
        <p className="text-xs" style={{ color: "var(--text-dim)" }}>
          Flick toward a wedge, click to confirm · Esc or move away to cancel · arrows + Enter work too
        </p>
        <p className="text-xs" style={{ color: "var(--text-dim)" }}>
          Selected: {selection.length === 1 ? selection[0].name : `${selection.length} files`} · mixed types show only what all of them support
        </p>
        <p className="text-xs" style={{ color: "var(--text-dim)" }}>
          Wedges with a dot have sub-options (DPI, page range, delimiter, …) · past six operations, “More” opens the rest (scroll to page)
        </p>
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
            onPrompt={onPrompt}
            onBrowse={onBrowse}
            onDismiss={close}
            onDone={close}
          />
        )}
        {promptElement}
      </AnimatePresence>
    </div>
  );
}

const groupStyle = { background: "var(--panel)", borderColor: "var(--panel-border)", backdropFilter: "blur(12px)" };
const buttonClass = "flex items-center gap-1.5 rounded-[10px] px-2.5 py-1.5 text-xs font-medium transition-colors";
const onStyle = { background: "var(--accent)", color: "#fff" };
const offStyle = { color: "var(--text-dim)" };

function Segmented<T extends string>({ label, value, options, onChange }: {
  label: string;
  value: T;
  options: { value: T; label: string; Icon?: LucideIcon }[];
  onChange: (v: T) => void;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="flex rounded-xl border p-0.5" style={groupStyle}>
      {options.map(({ value: v, label: l, Icon }) => (
        <button key={v} role="radio" aria-checked={v === value} onClick={() => onChange(v)}
          className={buttonClass} style={v === value ? onStyle : offStyle}>
          {Icon && <Icon size={14} />}{l}
        </button>
      ))}
    </div>
  );
}

/** Like `Segmented`, but each option toggles independently. */
function Toggles({ label, options, onToggle }: {
  label: string;
  options: { value: string; label: string; Icon?: LucideIcon; on: boolean }[];
  onToggle: (v: string) => void;
}) {
  return (
    <div role="group" aria-label={label} className="flex flex-wrap rounded-xl border p-0.5" style={groupStyle}>
      {options.map(({ value, label: l, Icon, on }) => (
        <button key={value} aria-pressed={on} onClick={() => onToggle(value)}
          className={buttonClass} style={on ? onStyle : offStyle}>
          {Icon && <Icon size={14} />}{l}
        </button>
      ))}
    </div>
  );
}
