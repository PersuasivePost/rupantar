# Rupantar

*Transformation of form.* A tray-resident file converter. Press a hotkey (default
`Ctrl/Cmd+Shift+Space`) or drop files on it, and a radial menu opens at your cursor; flick toward an
operation and the converted file is written next to the original.

Everything runs on your machine. There is no telemetry and conversions never touch the network.
The one exception is the optional packs below, which download only after you agree.

## Install

Download the installer for your system from the
[Releases](../../releases) page:

| System | File |
| --- | --- |
| Windows (x64) | `.msi` or `-setup.exe` |
| macOS (Apple silicon and Intel) | `.dmg` (universal) |
| Linux (x64) | `.deb`, `.rpm` or `.AppImage` |

Rupantar lives in the system tray (on macOS, the menu bar). Quit it from the tray menu.

### Optional packs

Some operations need a larger component. **They are not bundled with the app**; Rupantar offers to
download one the first time you use an operation that needs it (or under Settings), and shows the
size first. Nothing is downloaded without your agreement.

- **Office pack**: LibreOffice, for DOC/XLS/PPT and Office-quality PDF export.
- **AI pack**: ONNX Runtime, BiRefNet lite and Real-ESRGAN, for Remove background and Upscale.

### Opening an unsigned app on macOS

The macOS builds are **not signed or notarized**, so Gatekeeper blocks the first launch
("Rupantar can't be opened because Apple cannot check it for malicious software"). Either:

- **Right-click (or Control-click) Rupantar in Applications → Open → Open.** You only need to do
  this once. On recent macOS versions, if there is no Open button, try to open it once, then go to
  System Settings → Privacy & Security and click *Open Anyway*.
- or remove the quarantine flag in a terminal:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Rupantar.app
  ```

Signing and notarization need a paid Apple Developer account, which this project doesn't have yet.
Until it does, the steps above are required. Windows installers are unsigned too, so SmartScreen may
warn ("More info" → "Run anyway").

## Build from source

You need Rust (1.90 or newer), Node.js 22, and, on Linux, the Tauri system libraries:

```sh
sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev libsoup-3.0-dev libxdo-dev patchelf
```

```sh
npm ci
scripts/fetch-sidecars.sh      # pandoc + typst, as Tauri sidecars (src-tauri/binaries/)
scripts/fetch-pdfium.sh        # pdfium (src-tauri/resources/pdfium/)
npm run tauri dev              # run the desktop app
npm run tauri build            # build the installers into src-tauri/target/release/bundle/
```

Both scripts take a target triple (`scripts/fetch-sidecars.sh aarch64-apple-darwin`) and verify
pinned SHA-256 digests. For a universal macOS build, run `scripts/fetch-universal-macos.sh` and
then `npm run tauri build -- --target universal-apple-darwin`.

Without the desktop shell (no webview libraries needed):

```sh
npm run dev                    # the UI in a browser, with mock operations (port 5000)
npm run build                  # typecheck + bundle the frontend
cargo test -p engine -p placement -p packs
cargo clippy --workspace --all-targets
```

The pandoc/typst/pdfium tests use the files fetched above, and skip when they are missing. See
[CLAUDE.md](CLAUDE.md) for the architecture and the decisions log.

## Releases

CI (`.github/workflows/ci.yml`) runs the tests, clippy and the frontend build on every push.
Pushing a tag such as `v0.1.0` runs `.github/workflows/release.yml`, which builds Windows, macOS
(universal) and Linux installers and attaches them to a **draft** release for you to review and
publish. Bump the version in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and
`package.json` before tagging.

## License

[MIT](LICENSE). Bundled and downloadable third-party components keep their own licenses; see
Settings → Open-source licenses.
