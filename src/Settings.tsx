import { useEffect, useState } from "react";
import { Keyboard, Power } from "lucide-react";
import { getSettings, setAutostart, setShortcut, type Settings as SettingsData } from "./api";

const isMac = /Mac/i.test(navigator.platform);

/** Turns a keydown into a Tauri accelerator like `CommandOrControl+Shift+Space`, or null if unusable. */
function toAccelerator(e: KeyboardEvent): string | null {
  const key = keyName(e.code);
  if (!key) return null;
  const primary = isMac ? e.metaKey : e.ctrlKey;
  const mods = [
    primary && "CommandOrControl",
    (isMac ? e.ctrlKey : e.metaKey) && (isMac ? "Control" : "Super"),
    e.altKey && "Alt",
    e.shiftKey && "Shift",
  ].filter(Boolean) as string[];
  // A bare key (other than F-keys) would hijack normal typing system-wide.
  if (mods.length === 0 && !/^F\d+$/.test(key)) return null;
  return [...mods, key].join("+");
}

function keyName(code: string): string | null {
  let m: RegExpMatchArray | null;
  if ((m = code.match(/^Key([A-Z])$/))) return m[1];
  if ((m = code.match(/^Digit(\d)$/))) return m[1];
  if (/^F\d{1,2}$/.test(code)) return code;
  const named: Record<string, string> = {
    Space: "Space", Enter: "Enter", Tab: "Tab", Backspace: "Backspace",
    ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right",
  };
  return named[code] ?? null;
}

function pretty(accelerator: string): string {
  return accelerator
    .replace("CommandOrControl", isMac ? "⌘" : "Ctrl")
    .replace("Control", isMac ? "⌃" : "Ctrl")
    .replace("Alt", isMac ? "⌥" : "Alt")
    .replace("Shift", isMac ? "⇧" : "Shift")
    .replace("Super", "Win")
    .split("+")
    .join(isMac ? " " : " + ");
}

export default function Settings() {
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [recording, setRecording] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void getSettings().then(setSettings);
  }, []);

  useEffect(() => {
    if (!recording) return;
    const onKey = async (e: KeyboardEvent) => {
      e.preventDefault();
      if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return; // wait for the real key
      if (e.key === "Escape") return setRecording(false);
      const accelerator = toAccelerator(e);
      if (!accelerator) return setError("Use a modifier (Ctrl, Alt, Shift or Cmd) plus a letter, digit, Space or F-key.");
      setRecording(false);
      try {
        setSettings(await setShortcut(accelerator));
        setError(null);
      } catch (err) {
        setError(String(err));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [recording]);

  const toggleAutostart = async (enabled: boolean) => {
    try {
      setSettings(await setAutostart(enabled));
      setError(null);
    } catch (err) {
      setError(String(err));
    }
  };

  return (
    <main className="mx-auto flex h-full max-w-md flex-col gap-4 p-6">
      <h1 className="text-lg font-semibold">Rupantar settings</h1>

      <section className="rounded-xl border p-4" style={{ background: "var(--panel)", borderColor: "var(--panel-border)" }}>
        <div className="mb-3 flex items-center gap-2 text-sm font-medium"><Keyboard size={16} /> Summon shortcut</div>
        <div className="flex items-center justify-between gap-3">
          <kbd className="rounded-lg border px-3 py-1.5 text-sm" style={{ borderColor: "var(--panel-border)" }}>
            {recording ? "Press a shortcut…" : settings ? pretty(settings.shortcut) : "…"}
          </kbd>
          <button
            onClick={() => { setError(null); setRecording((r) => !r); }}
            className="rounded-lg px-3 py-1.5 text-sm font-medium text-white"
            style={{ background: "var(--accent)" }}
          >
            {recording ? "Cancel" : "Change"}
          </button>
        </div>
      </section>

      <section className="rounded-xl border p-4" style={{ background: "var(--panel)", borderColor: "var(--panel-border)" }}>
        <label className="flex cursor-pointer items-center justify-between gap-3">
          <span className="flex items-center gap-2 text-sm font-medium"><Power size={16} /> Start at login</span>
          <input
            type="checkbox"
            className="h-4 w-4"
            checked={settings?.autostart ?? false}
            disabled={!settings}
            onChange={(e) => void toggleAutostart(e.target.checked)}
          />
        </label>
      </section>

      {error && <p role="alert" className="text-sm" style={{ color: "#f87171" }}>{error}</p>}
    </main>
  );
}
