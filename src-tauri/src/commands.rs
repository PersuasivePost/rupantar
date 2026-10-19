//! Tauri commands: thin wrappers that delegate to `engine` or to `shell`.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use engine::{Operation, Options, Registry};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::settings::{self, Settings};
use crate::shell::{self, Shell, Summon};

#[tauri::command]
pub fn list_operations(mime: String) -> Vec<Operation> {
    Registry::with_defaults().operations(&mime)
}

/// Async + `spawn_blocking`: conversions can take seconds and sync commands run on the main
/// thread, which would freeze the tray and window.
#[tauri::command]
pub async fn run_operation(
    mime: String,
    op: Operation,
    inputs: Vec<PathBuf>,
    options: Option<Options>,
) -> Result<Vec<PathBuf>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Registry::with_defaults().run(&mime, &op, &inputs, &options.unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
pub struct FileBatch {
    pub paths: Vec<String>,
    /// File name for one file, "N files" for several.
    pub label: String,
    pub mime: String,
}

#[tauri::command]
pub fn describe_files(paths: Vec<PathBuf>) -> Result<FileBatch, String> {
    let files: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_file()).collect();
    if files.is_empty() {
        return Err("no readable files were dropped".into());
    }
    let mimes: Vec<String> = files.iter().map(|f| engine::mime::detect(f)).collect();
    let label = match files.as_slice() {
        [one] => one.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned()),
        many => format!("{} files", many.len()),
    };
    Ok(FileBatch {
        paths: files.iter().map(|f| f.to_string_lossy().into_owned()).collect(),
        label,
        mime: engine::mime::common(&mimes),
    })
}

/// Called once the frontend has registered its listeners; returns a summon that arrived earlier.
#[tauri::command]
pub fn frontend_ready(state: State<Shell>) -> Option<Summon> {
    state.frontend_ready()
}

#[tauri::command]
pub fn hide_radial(app: AppHandle) {
    shell::hide_radial(&app);
}

#[tauri::command]
pub fn set_hide_on_blur(state: State<Shell>, enabled: bool) {
    state.hide_on_blur.store(enabled, Ordering::SeqCst);
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    settings::load(&app)
}

/// Rebinds the global shortcut. On failure the previous binding is restored and the error returned.
#[tauri::command]
pub fn set_shortcut(app: AppHandle, accelerator: String) -> Result<Settings, String> {
    let mut current = settings::load(&app);
    shell::unregister_shortcut(&app, &current.shortcut);
    if let Err(e) = shell::register_shortcut(&app, &accelerator) {
        let _ = shell::register_shortcut(&app, &current.shortcut);
        return Err(e);
    }
    current.shortcut = accelerator;
    settings::save(&app, &current)?;
    Ok(current)
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<Settings, String> {
    shell::apply_autostart(&app, enabled)?;
    let mut current = settings::load(&app);
    current.autostart = enabled;
    settings::save(&app, &current)?;
    Ok(current)
}
