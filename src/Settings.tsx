import { useEffect, useState } from "react";
import { ArrowLeft, ChevronRight, FileText, Keyboard, Package, Power, Scale } from "lucide-react";
import { getSettings, setAutostart, setShortcut, thirdPartyNotices, type Notice, type Settings as SettingsData } from "./api";
import { THIRD_PARTY, type ThirdParty } from "./licenses";
import { formatBytes, progressFraction, progressLabel, useOfficePack } from "./lib/useOfficePack";

const panel = { background: "var(--panel)", borderColor: "var(--panel-border)" };
const button = "rounded-lg px-3 py-1.5 text-sm font-medium";
const primary = { background: "var(--accent)", color: "#fff" };
const secondary = { color: "var(--text)", border: "1px solid var(--panel-border)" };

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
  const [view, setView] = useState<"general" | "licenses">("general");
  return view === "licenses" ? <Licenses onBack={() => setView("general")} /> : <General onLicenses={() => setView("licenses")} />;
}

function General({ onLicenses }: { onLicenses: () => void }) {
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
    <main className="mx-auto flex h-full max-w-md flex-col gap-4 overflow-y-auto p-6">
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

      <OfficePack />

      <button onClick={onLicenses} className="flex items-center gap-2 rounded-xl border p-4 text-left text-sm font-medium" style={panel}>
        <Scale size={16} /> <span className="flex-1">Open-source licenses</span> <ChevronRight size={16} />
      </button>
    </main>
  );
}

/** Settings → Office pack: what it is, its size, download / cancel / remove, progress. */
function OfficePack() {
  const { status, progress, error, removing, install, cancel, remove } = useOfficePack();
  const offer = status?.offer;
  return (
    <section className="flex flex-col gap-3 rounded-xl border p-4 text-sm" style={panel}>
      <div className="flex items-center gap-2 font-medium"><Package size={16} /> Office pack</div>
      <p style={{ color: "var(--text-dim)" }}>
        LibreOffice, for Word, Excel and PowerPoint conversions (to PDF, between formats, slides to images). Optional, and
        downloaded from documentfoundation.org only when you ask. Conversions stay on this computer.
      </p>
      {!status && <p style={{ color: "var(--text-dim)" }}>Checking…</p>}
      {status?.state === "unsupported" && <p>Not available for this system.</p>}
      {status?.state === "installed" && status.installed && (
        <div className="flex items-center justify-between gap-3">
          <span>LibreOffice {status.installed.version} · {formatBytes(status.installed.size)} on disk</span>
          <button className={button} style={secondary} disabled={removing} onClick={() => void remove()}>
            {removing ? "Removing…" : "Remove"}
          </button>
        </div>
      )}
      {status?.state === "not_installed" && offer && (
        <div className="flex items-center justify-between gap-3">
          <span className="tabular-nums" style={{ color: "var(--text-dim)" }}>
            LibreOffice {offer.version} · {formatBytes(offer.download_bytes)} download, about {formatBytes(offer.installed_bytes)} on disk
            {offer.downloaded_bytes > 0 && ` (${formatBytes(offer.downloaded_bytes)} already downloaded)`}
          </span>
          <button className={button} style={primary} onClick={() => void install()}>
            {offer.downloaded_bytes > 0 ? "Resume" : "Download"}
          </button>
        </div>
      )}
      {status?.state === "installing" && (
        <div className="flex flex-col gap-2">
          <div className="h-1.5 overflow-hidden rounded-full" style={{ background: "var(--track)" }}>
            <div className="h-full rounded-full transition-transform" style={{ background: "var(--accent)", transformOrigin: "left", transform: `scaleX(${progressFraction(progress)})` }} />
          </div>
          <div className="flex items-center justify-between gap-3">
            <span className="tabular-nums" style={{ color: "var(--text-dim)" }}>{progressLabel(progress)}</span>
            <button className={button} style={secondary} onClick={cancel}>Cancel</button>
          </div>
        </div>
      )}
      {offer && !offer.tested && status?.state !== "installed" && (
        <p style={{ color: "#f59e0b" }}>The Office pack hasn’t been tested on this platform yet.</p>
      )}
      {error && <p role="alert" style={{ color: "#f87171" }}>{error}</p>}
    </section>
  );
}

/** Settings → Licenses: every third-party component, its license text and shipped notices. */
function Licenses({ onBack }: { onBack: () => void }) {
  return (
    <main className="mx-auto flex h-full max-w-md flex-col gap-4 overflow-y-auto p-6">
      <header className="flex items-center gap-2">
        <button aria-label="Back" onClick={onBack} className="rounded-md p-1"><ArrowLeft size={18} /></button>
        <h1 className="text-lg font-semibold">Open-source licenses</h1>
      </header>
      <p className="text-sm" style={{ color: "var(--text-dim)" }}>
        Rupantar uses these programs and libraries. Each keeps its own license.
      </p>
      {THIRD_PARTY.map((t) => <LicenseCard key={t.name} item={t} />)}
    </main>
  );
}

function LicenseCard({ item }: { item: ThirdParty }) {
  const [notices, setNotices] = useState<Notice[] | null>(null);
  return (
    <section className="flex flex-col gap-2 rounded-xl border p-4 text-sm" style={panel}>
      <div className="flex items-baseline justify-between gap-2">
        <span className="font-medium">{item.name} <span style={{ color: "var(--text-dim)" }}>{item.version}</span></span>
        <code className="text-xs" style={{ color: "var(--accent)" }}>{item.license}</code>
      </div>
      <p style={{ color: "var(--text-dim)" }}>{item.use}</p>
      <p className="select-text break-all text-xs" style={{ color: "var(--text-dim)" }}>{item.url}</p>
      {item.note && <p className="select-text text-xs" style={{ color: "var(--text-dim)" }}>{item.note}</p>}
      {item.texts.map((t) => <LicenseText key={t.title} title={t.title} text={t.text} />)}
      {item.notices && notices === null && (
        <button className="self-start text-xs underline" style={{ color: "var(--accent)" }}
          onClick={() => void thirdPartyNotices(item.notices!).then(setNotices)}>
          Show shipped notice files
        </button>
      )}
      {notices?.length === 0 && (
        <p className="text-xs" style={{ color: "var(--text-dim)" }}>
          {item.notices === "office" ? "Install the Office pack to see its notice files." : "No notice files found."}
        </p>
      )}
      {notices?.map((n) => <LicenseText key={n.name} title={n.name} text={n.text} />)}
    </section>
  );
}

function LicenseText({ title, text }: { title: string; text: string }) {
  return (
    <details className="text-xs">
      <summary className="flex cursor-pointer items-center gap-1.5"><FileText size={12} /> {title}</summary>
      <pre className="mt-2 max-h-64 select-text overflow-auto whitespace-pre-wrap rounded-lg p-2 text-[11px] leading-snug"
        style={{ background: "var(--track)" }}>{text}</pre>
    </details>
  );
}
