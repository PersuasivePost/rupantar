# Rupantar

Cross-platform desktop app (Tauri v2 + Rust backend; React + TypeScript + Vite + Tailwind + Framer Motion frontend). Tray-resident file-conversion utility.

**UX flow:** the user summons a circular radial menu at the cursor (hotkey or file drop), drops/selects files, flicks toward an operation wedge, and the converted file is written next to the original.

**Everything runs locally. No network access, ever** — no telemetry, no remote APIs, no CDN assets at runtime.

## Repository layout

```
Cargo.toml          # workspace root
crates/engine/      # pure Rust: all conversion logic + tests. NO Tauri deps.
crates/placement/   # pure window-placement math (multi-monitor / DPI), no deps
src-tauri/          # thin shell: commands, tray, hotkeys, windows
src/                # React frontend
  api.ts            # the only place the frontend talks to Rust
```

Plus `index.html`, `vite.config.ts` (port 5000, `strictPort`), and `src-tauri/tauri.conf.json` (`devUrl` = `http://localhost:5000`).

## Architecture rules

1. **Cargo workspace** with three members:
   - `crates/engine` — all conversion logic and its tests. Must never depend on `tauri` or any UI crate, so it builds and tests on Replit.
   - `crates/placement` — pure arithmetic for positioning the radial window (multi-monitor, mixed DPI). Split out of the shell only so it is testable on Replit.
   - `src-tauri` — thin shell only: Tauri commands, tray, global hotkeys, window management. No conversion logic here; commands just delegate to the engine.
2. **Engine trait** (each converter implements it):
   ```rust
   fn can_handle(&self, mime: &str) -> bool;
   fn operations(&self, mime: &str) -> Vec<Operation>;
   fn run(&self, op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Vec<PathBuf>>;
   ```
   `run` returns the paths of the files it wrote. Output goes next to the original input; never overwrite an existing file (pick a unique name).
3. **Frontend ↔ Rust only through the typed `api.ts` wrapper.** Components never call `invoke` directly. When not running inside Tauri (`window.__TAURI_INTERNALS__` absent), `api.ts` uses mocks so the UI works in a plain browser.
4. Operation/option types are shared in shape between Rust (serde) and TypeScript; change both sides together.

## Development environment

- **Replit:** browser preview of the Vite frontend (mock mode) and `cargo test -p engine`. Do not try to build or run the Tauri shell there (no webview/GUI libs).
- **Local machine:** anything desktop-specific — tray, global hotkey, cursor-positioned window, drag-drop from the OS, `cargo tauri dev` — must be tested locally. State clearly when a change is only verifiable locally.

## Commands

- Frontend dev (browser preview, mock mode): `npm run dev` (port 5000)
- Frontend typecheck + bundle: `npm run build`
- Engine tests: `cargo test -p engine`
- Desktop dev (local only, needs webkit2gtk on Linux): `npm run tauri dev`

## Conventions

- Engine: every converter has unit tests; use small fixtures under `crates/engine/tests/`. Errors via a typed error enum, no panics on bad input.
- Frontend: TypeScript strict; Tailwind for styling; Framer Motion for radial menu animation.
- Keep the shell thin; when tempted to add logic to `src-tauri`, put it in `engine`.

## Decisions log

Keep this file updated as decisions are made. Append entries here (date — decision — why).

- 2026-10-19 — Project conventions established (workspace split, engine trait, api.ts mocks, Replit-for-UI/engine, local-for-desktop).
- 2026-10-19 — Dev server port is 5000 (not Tauri's 1420) with `strictPort`; `devUrl` matches. Why: Replit preview port.
- 2026-10-19 — Workspace `default-members = ["crates/engine"]`, so bare `cargo` commands never build `src-tauri`. Why: webkit2gtk is unavailable on Replit.
- 2026-10-19 — `.replit` uses the `rust-stable` module. `src-tauri` has not been compiled yet (needs a local machine); only `cargo metadata` was verified.
- 2026-10-19 — `api.ts` uses `isTauri()` from `@tauri-apps/api/core` to choose between `invoke` and mocks.
- 2026-10-19 — Tauri v2 only (`@tauri-apps/api` ^2, `tauri` 2.x). `src-tauri/icons/icon.png` is a placeholder; replace via `npm run tauri icon <source.png>`.
- 2026-10-19 — `Operation` gained optional `children` (sub-options → outer ring), mirrored in Rust (`#[serde(default)]`) and `api.ts`. Mock operations per mime live in `api.ts`; sample files in `src/mockFiles.ts`.
- 2026-10-19 — RadialMenu (`src/components/RadialMenu.tsx`, geometry in `src/lib/radial.ts`) is presentational: parent supplies `operations` and an `onConfirm` promise. Selection is by angle from the center (dead zone 34px, dismiss beyond the ring), confirm on click, keyboard arrows/Enter/Esc also work. The menu root stops click propagation so the confirming click can't reopen it. Dev playground is `src/Playground.tsx` (currently the whole `App`).
- 2026-10-19 — Animation is transform/opacity on fixed-size layers (one backdrop-blur annulus per ring, masked). Exception: the progress ring animates SVG `pathLength` on a tiny element. Theme via CSS variables in `index.css` (`data-theme` or OS preference); reduced motion swaps springs/scale for short fades.
- 2026-10-19 — Engine output naming: `name (tag).ext` next to the source, then `name (tag 2).ext`, etc. Files are created with `create_new` (`create_unique_sibling`), so nothing is ever overwritten, even under a race. Tag per op: converted / resized / compressed / rotated / cropped / copy.
- 2026-10-19 — `Converter::run` must return `UnknownOperation` for ids it doesn't own; `Registry::run` tries each converter that `can_handle` the mime until one owns the op.
- 2026-10-19 — Image ops live in `crates/engine/src/images.rs` (`image` crate, png/jpeg/webp only). Op ids are `image.<action>.<param>.<value>` (e.g. `image.resize.pct.50`); the same values can come from `options`, which win over the id. `image.crop` (x/y/width/height options) is supported but has no menu entry until there is UI for picking a rect.
- 2026-10-19 — Limitation: the `image` crate writes only lossless WebP, so lossy WebP (quality < 100) returns an error instead of being silently ignored. Real lossy WebP needs the `webp` crate (libwebp, C toolchain). Compress defaults to JPEG output. Resize by max dimension never upscales. JPEG output flattens transparency onto white. EXIF orientation is not applied or preserved.
- 2026-10-19 — The mock operations in `src/api.ts` still use their own ids (`img-*`) and do not match the engine's `image.*` ids; reconcile when wiring the menu to the real backend.
- 2026-10-19 — Desktop shell (`src-tauri/src/`): `shell.rs` (summon/placement/shortcut/autostart/window events), `tray.rs`, `commands.rs`, `settings.rs` (JSON in the app config dir). Two pre-created hidden windows from `tauri.conf.json`: `radial` (600x600 logical, frameless, transparent, always-on-top, skip-taskbar) and `settings` (same `index.html`, `#/settings`). Closing either only hides it; the app exits only via tray Quit (`RunEvent::ExitRequested` is prevented when `code` is None).
- 2026-10-19 — Summon flow: hotkey/tray/second-instance -> `shell::summon` reads the cursor (physical desktop px), picks the monitor under it, `placement::place` slides a window of 600 *logical* px onto that monitor and returns the cursor's offset inside it, the window is positioned/sized in *physical* px (position, size, position again, to survive DPI changes), then a `summon` event carries that offset so the menu centers under the real cursor even when the window had to slide away from a screen edge. With no files it shows a "Drop files" target; files from a drop, `argv`, or a second instance open the menu directly. Summons before the frontend registers listeners are queued and handed over by the `frontend_ready` command.
- 2026-10-19 — Hotkey default `CommandOrControl+Shift+Space`, rebindable in Settings (`set_shortcut` restores the old binding if the new one fails). It toggles: pressing it while the window is visible hides it. Hide-on-blur is enabled only while a menu is open (the frontend calls `set_hide_on_blur`), because dragging a file in from another window steals focus while the drop target is up.
- 2026-10-19 — Single instance: a second launch forwards its existing file arguments (relative paths resolved against its cwd) to the running instance. First-launch file args are handled the same way. Autostart uses `tauri-plugin-autostart` (login item / LaunchAgent / registry) and is re-synced to the saved setting at startup.
- 2026-10-19 — `run_operation` is async + `spawn_blocking` (sync commands run on the main thread and would freeze the tray). File types come from `engine::mime::detect` (content sniff first, extension second); a mixed-type batch falls back to `application/octet-stream` (only Duplicate is offered).
- 2026-10-19 — Known limits: a webview can't blur the desktop behind a transparent window, so glass in the shell is a stronger tint (`--shell-under`) rather than true frosted blur. The 600px transparent square still intercepts clicks over other apps while shown. Wayland doesn't expose the cursor position or allow window positioning (falls back to screen center) and has limited global-shortcut support; X11, Windows and macOS are the targets. Tray left-click is not reported on Linux (menu only). `macos-private-api` (required for transparency on macOS) rules out Mac App Store distribution.
- 2026-10-19 — Toolchain: current Tauri (2.12) needs rustc >= 1.90, but Replit's `rust-stable` module is 1.88. `cargo test` (engine + placement) works on 1.88; compiling `src-tauri` needs 1.90+ and the webkit2gtk/gtk3/libsoup3/appindicator system libs. To type-check the shell on Replit, use `nix-shell -p pkg-config webkitgtk_4_1 gtk3 libsoup_3 glib cairo pango gdk-pixbuf atk librsvg openssl libayatana-appindicator xdotool` with a 1.90 rustc.
