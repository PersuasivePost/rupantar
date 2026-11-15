//! The text behind Settings → About → "Copy debug info". Built on demand, handed to the frontend,
//! which puts it on the clipboard. Nothing is sent anywhere.

use std::path::Path;
use std::process::Command;

use packs::Pack;
use tauri::{AppHandle, Manager, State};

use crate::commands::Engine;
use crate::{applog, settings, shell};

/// How many log lines the report ends with.
const LOG_LINES: usize = 100;

/// Runs `program args…` and returns its trimmed stdout (first line only if `first_line`).
fn output(program: impl AsRef<std::ffi::OsStr>, args: &[&str], first_line: bool) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let text = if first_line { text.lines().next().unwrap_or("") } else { text.trim() };
    (!text.is_empty()).then(|| text.trim().to_string())
}

fn os_version() -> String {
    #[cfg(target_os = "linux")]
    {
        let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        let name = release
            .lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_else(|| "Linux".into());
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        format!("{name} (kernel {})", kernel.trim())
    }
    #[cfg(target_os = "macos")]
    {
        format!("macOS {}", output("sw_vers", &["-productVersion"], true).unwrap_or_default())
    }
    #[cfg(windows)]
    {
        output("cmd", &["/C", "ver"], true).unwrap_or_else(|| "Windows".into())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        String::new()
    }
}

/// X11 or Wayland (Linux only).
fn session_type() -> String {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match var("XDG_SESSION_TYPE") {
        Some(t) => t,
        None if var("WAYLAND_DISPLAY").is_some() => "wayland".into(),
        None if var("DISPLAY").is_some() => "x11".into(),
        None => "unknown".into(),
    }
}

fn tool_line(name: &str, path: Option<&Path>, version_args: &[&str]) -> String {
    match path {
        None => format!("{name}: not found"),
        Some(p) => {
            let version = if version_args.is_empty() { None } else { output(p, version_args, true) };
            match version {
                Some(v) => format!("{name}: {} ({v})", p.display()),
                None => format!("{name}: {}", p.display()),
            }
        }
    }
}

fn pack_line(app: &AppHandle, name: &str, pack: impl FnOnce(&Path) -> Pack) -> String {
    let dir = app.path().app_data_dir().map(|d| d.join("packs")).ok();
    match dir.map(|d| pack(&d)).and_then(|p| p.installed()) {
        Some(i) => format!("{name}: installed, version {} ({:.0} MB)", i.version, i.size as f64 / 1e6),
        None => format!("{name}: not installed"),
    }
}

#[tauri::command]
pub async fn debug_info(app: AppHandle, engine: State<'_, Engine>) -> Result<String, String> {
    let config = engine.config();
    // Running the tools' `--version` can take a moment; keep it off the main thread.
    tauri::async_runtime::spawn_blocking(move || {
        let mut lines = vec![
            "Rupantar debug info".to_string(),
            format!("Version: {}", app.package_info().version),
            format!(
                "OS: {} {} ({})",
                std::env::consts::OS,
                os_version(),
                std::env::consts::ARCH
            ),
        ];
        if cfg!(target_os = "linux") {
            lines.push(format!("Session: {}", session_type()));
        }
        if cfg!(windows) {
            lines.push(format!("Elevated: {}", if shell::is_elevated() { "yes" } else { "no" }));
        }
        lines.push(format!("Hotkey: {}", settings::load(&app).shortcut));
        lines.push(String::new());
        lines.push("Tools".into());
        lines.push(tool_line("pandoc", config.pandoc_path.as_deref(), &["--version"]));
        lines.push(tool_line("typst", config.typst_path.as_deref(), &["--version"]));
        // pdfium is a library: its version is the pinned build (scripts/fetch-pdfium.sh).
        lines.push(tool_line("pdfium (pinned chromium/7881)", config.pdfium_path.as_deref(), &[]));
        lines.push(String::new());
        lines.push("Packs".into());
        lines.push(pack_line(&app, "Office pack (LibreOffice)", Pack::office));
        lines.push(pack_line(&app, "AI pack", Pack::ai));
        lines.push(String::new());
        lines.push(format!("Last {LOG_LINES} log lines"));
        lines.extend(applog::tail(LOG_LINES));
        lines.join("\n")
    })
    .await
    .map_err(|e| e.to_string())
}
