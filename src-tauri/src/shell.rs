//! Window, shortcut and autostart plumbing. Conversion logic stays in `engine`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use placement::MonitorRect;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Window, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub const RADIAL: &str = "radial";
pub const SETTINGS: &str = "settings";

/// Logical size of the (square) radial window; must exceed the menu's outer diameter (540px).
const WINDOW_LOGICAL_SIZE: f64 = 600.0;
/// Focus often flickers while a window is being shown; ignore blur events this soon after.
const BLUR_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Serialize)]
pub struct Cursor {
    pub x: f64,
    pub y: f64,
}

/// Sent to the radial window on every summon. `cursor` is in logical px relative to the window.
#[derive(Debug, Clone, Serialize)]
pub struct Summon {
    pub cursor: Cursor,
    pub paths: Vec<String>,
}

pub struct Shell {
    /// Hide the radial window when it loses focus (set by the frontend while a menu is open).
    pub hide_on_blur: AtomicBool,
    shown_at: Mutex<Instant>,
    /// False until the frontend registered its listeners; summons before that are queued.
    ready: AtomicBool,
    pending: Mutex<Option<Summon>>,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            hide_on_blur: AtomicBool::new(false),
            shown_at: Mutex::new(Instant::now()),
            ready: AtomicBool::new(false),
            pending: Mutex::new(None),
        }
    }

    /// Marks the frontend as listening and hands over any summon that arrived before it was ready.
    pub fn frontend_ready(&self) -> Option<Summon> {
        self.ready.store(true, Ordering::SeqCst);
        self.pending.lock().unwrap().take()
    }
}

/// Keeps existing files only, resolving relative paths against `cwd` (a second instance's
/// working directory). Flags like `--hidden` and non-existent paths are dropped.
pub fn files_from_args(args: &[String], cwd: Option<&Path>) -> Vec<PathBuf> {
    args.iter()
        .map(PathBuf::from)
        .map(|p| match cwd {
            Some(dir) if p.is_relative() => dir.join(p),
            _ => p,
        })
        .filter(|p| p.is_file())
        .collect()
}

/// Where the cursor is, in physical desktop pixels. Some platforms (Wayland) refuse to report
/// it; then the menu opens at the center of the primary monitor instead.
fn cursor_or_screen_center(app: &AppHandle) -> (f64, f64) {
    if let Ok(p) = app.cursor_position() {
        return (p.x, p.y);
    }
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let (pos, size) = (m.position(), m.size());
            (f64::from(pos.x) + f64::from(size.width) / 2.0, f64::from(pos.y) + f64::from(size.height) / 2.0)
        })
        .unwrap_or((0.0, 0.0))
}

fn try_summon(app: &AppHandle, files: Vec<PathBuf>) -> tauri::Result<()> {
    let win = app.get_webview_window(RADIAL).ok_or(tauri::Error::WindowNotFound)?;
    let (cx, cy) = cursor_or_screen_center(app);
    let monitors: Vec<MonitorRect> = app
        .available_monitors()?
        .iter()
        .map(|m| MonitorRect {
            x: m.position().x,
            y: m.position().y,
            width: m.size().width,
            height: m.size().height,
            scale: m.scale_factor(),
        })
        .collect();
    let p = placement::place(cx, cy, &monitors, WINDOW_LOGICAL_SIZE);

    // Position, size, position again: crossing monitors with different DPI can make the OS
    // rescale the window mid-move, so the first move lands on the right monitor and the last
    // one corrects any drift. Sizes are physical so the result doesn't depend on which scale
    // factor the window believes it has at that instant.
    let pos = PhysicalPosition::new(p.x, p.y);
    win.set_position(pos)?;
    win.set_size(PhysicalSize::new(p.size, p.size))?;
    win.set_position(pos)?;

    let payload = Summon {
        cursor: Cursor { x: p.cursor_x, y: p.cursor_y },
        paths: files.iter().map(|f| f.to_string_lossy().into_owned()).collect(),
    };
    let state = app.state::<Shell>();
    *state.shown_at.lock().unwrap() = Instant::now();
    if state.ready.load(Ordering::SeqCst) {
        app.emit_to(RADIAL, "summon", payload)?;
    } else {
        *state.pending.lock().unwrap() = Some(payload);
    }
    win.show()?;
    win.set_focus()?;
    Ok(())
}

pub fn summon(app: &AppHandle, files: Vec<PathBuf>) {
    if let Err(e) = try_summon(app, files) {
        eprintln!("rupantar: could not show the radial window: {e}");
    }
}

pub fn hide_radial(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(RADIAL) {
        let _ = w.hide();
    }
}

/// Hotkey behaviour: show the menu, or hide it if it is already up.
pub fn toggle(app: &AppHandle) {
    let visible = app.get_webview_window(RADIAL).and_then(|w| w.is_visible().ok()).unwrap_or(false);
    if visible {
        hide_radial(app);
    } else {
        summon(app, Vec::new());
    }
}

pub fn open_settings(app: &AppHandle) {
    hide_radial(app);
    if let Some(w) = app.get_webview_window(SETTINGS) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn register_shortcut(app: &AppHandle, accelerator: &str) -> Result<(), String> {
    let shortcut: Shortcut =
        accelerator.parse().map_err(|e| format!("invalid shortcut `{accelerator}`: {e}"))?;
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                toggle(app);
            }
        })
        .map_err(|e| format!("could not register `{accelerator}` (is another app using it?): {e}"))
}

pub fn unregister_shortcut(app: &AppHandle, accelerator: &str) {
    if let Ok(shortcut) = accelerator.parse::<Shortcut>() {
        let _ = app.global_shortcut().unregister(shortcut);
    }
}

/// Makes the OS login-item state match the setting.
pub fn apply_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    let current = launcher.is_enabled().map_err(|e| e.to_string())?;
    match (enabled, current) {
        (true, false) => launcher.enable().map_err(|e| e.to_string()),
        (false, true) => launcher.disable().map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

/// Tool paths for the engine. Tauri installs `bundle.externalBin` sidecars next to the main
/// executable (`Contents/MacOS` on macOS) without the target-triple suffix, and `tauri dev`
/// copies them there too; PATH is the fallback (the engine does that part).
pub fn engine_config() -> engine::EngineConfig {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let config = engine::EngineConfig::locate(exe_dir.as_slice());
    for (tool, path) in [("pandoc", &config.pandoc_path), ("typst", &config.typst_path)] {
        match path {
            Some(p) => eprintln!("rupantar: using {tool} at {}", p.display()),
            None => eprintln!("rupantar: {tool} not found; operations that need it are hidden"),
        }
    }
    config
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    match event {
        // Windows are created once and reused, so closing only hides them.
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = window.hide();
        }
        WindowEvent::Focused(false) if window.label() == RADIAL => {
            let state = window.app_handle().state::<Shell>();
            let settled = state.shown_at.lock().unwrap().elapsed() > BLUR_GRACE;
            if settled && state.hide_on_blur.load(Ordering::SeqCst) {
                let _ = window.hide();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_keep_only_existing_files() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.png");
        std::fs::write(&a, b"x").unwrap();
        std::fs::create_dir(d.path().join("folder")).unwrap();
        let args = vec![
            a.to_string_lossy().into_owned(),
            "--hidden".into(),
            d.path().join("folder").to_string_lossy().into_owned(),
            d.path().join("missing.png").to_string_lossy().into_owned(),
        ];
        assert_eq!(files_from_args(&args, None), vec![a]);
    }

    #[test]
    fn relative_args_resolve_against_the_other_instances_cwd() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("rel.pdf"), b"x").unwrap();
        let got = files_from_args(&["rel.pdf".to_string()], Some(d.path()));
        assert_eq!(got, vec![d.path().join("rel.pdf")]);
        assert!(files_from_args(&["rel.pdf".to_string()], None).is_empty());
    }

    #[test]
    fn early_summon_is_queued_until_the_frontend_is_ready() {
        let shell = Shell::new();
        *shell.pending.lock().unwrap() = Some(Summon { cursor: Cursor { x: 1.0, y: 2.0 }, paths: vec![] });
        assert!(!shell.ready.load(Ordering::SeqCst));
        assert!(shell.frontend_ready().is_some());
        assert!(shell.ready.load(Ordering::SeqCst));
        assert!(shell.frontend_ready().is_none()); // only handed over once
    }
}
