//! Thin Tauri shell: commands delegate to `engine`; no conversion logic lives here.

use std::path::PathBuf;

use engine::{Operation, Options, Registry};

#[tauri::command]
fn list_operations(mime: String) -> Vec<Operation> {
    Registry::with_defaults().operations(&mime)
}

#[tauri::command]
fn run_operation(
    mime: String,
    op: Operation,
    inputs: Vec<PathBuf>,
    options: Option<Options>,
) -> Result<Vec<PathBuf>, String> {
    Registry::with_defaults()
        .run(&mime, &op, &inputs, &options.unwrap_or_default())
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![list_operations, run_operation])
        .run(tauri::generate_context!())
        .expect("error while running Rupantar");
}
