# Rupantar

Cross-platform desktop app (Tauri v2 + Rust backend; React + TypeScript + Vite + Tailwind + Framer Motion frontend). Tray-resident file-conversion utility.

**UX flow:** the user summons a circular radial menu at the cursor (hotkey or file drop), drops/selects files, flicks toward an operation wedge, and the converted file is written next to the original.

**Everything runs locally. No network access, ever** — no telemetry, no remote APIs, no CDN assets at runtime.

## Repository layout

```
Cargo.toml          # workspace root
crates/engine/      # pure Rust: all conversion logic + tests. NO Tauri deps.
src-tauri/          # thin shell: commands, tray, hotkeys, windows
src/                # React frontend
  api.ts            # the only place the frontend talks to Rust
```

Plus `index.html`, `vite.config.ts` (port 5000, `strictPort`), and `src-tauri/tauri.conf.json` (`devUrl` = `http://localhost:5000`).

## Architecture rules

1. **Cargo workspace** with two members:
   - `crates/engine` — all conversion logic and its tests. Must never depend on `tauri` or any UI crate, so it builds and tests on Replit.
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
