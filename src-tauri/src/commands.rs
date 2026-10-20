//! Tauri commands: thin wrappers that delegate to `engine` or to `shell`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use engine::{EngineConfig, EngineError, Job, Operation, Options, Registry};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::settings::{self, Settings};
use crate::shell::{self, Shell, Summon};

/// The converter registry, built at startup with the tools found then, and rebuilt when a pack
/// is installed or removed. A conversion keeps the registry it started with.
pub struct Engine {
    config: Mutex<EngineConfig>,
    registry: RwLock<Arc<Registry>>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        let registry = RwLock::new(Arc::new(Registry::with_config(&config)));
        Self { config: Mutex::new(config), registry }
    }

    pub fn current(&self) -> Arc<Registry> {
        Arc::clone(&self.registry.read().unwrap())
    }

    /// Changes the configuration (e.g. a pack was installed or removed) and rebuilds the registry.
    pub fn update(&self, change: impl FnOnce(&mut EngineConfig)) {
        let mut config = self.config.lock().unwrap();
        change(&mut config);
        *self.registry.write().unwrap() = Arc::new(Registry::with_config(&config));
    }
}

/// Runs in progress that the frontend gave an id, so it can cancel them.
#[derive(Default)]
pub struct Runs(Mutex<HashMap<String, Arc<Job>>>);

/// Emitted while a run reports progress (only long ones do, e.g. the AI pack's).
const RUN_PROGRESS_EVENT: &str = "operation-progress";

#[derive(Debug, Clone, Serialize)]
struct RunProgress {
    run_id: String,
    fraction: f32,
    label: String,
}

/// Operations for a selection: `mimes` has one entry per file (see `FileBatch::mimes`).
/// Unavailable ones (missing tools, planned) are included, flagged; the menu hides them, except
/// ones that need a pack (`Operation::pack`), which it offers to download.
#[tauri::command]
pub fn list_operations(engine: State<Engine>, mimes: Vec<String>) -> Vec<Operation> {
    engine.current().operations_for(&mimes)
}

/// Async + `spawn_blocking`: conversions can take seconds and sync commands run on the main
/// thread, which would freeze the tray and window. File types are detected again here rather
/// than trusted from the frontend. Each run is logged with the engine that did it (e.g. DOCX ->
/// PDF: "LibreOffice" once the Office pack is installed, else "pandoc"; Remove background: "AI
/// (ONNX Runtime on CPU)").
///
/// With a `run_id`, progress is emitted as `operation-progress` (throttled) and
/// `cancel_operation(run_id)` stops the run; it then rejects with `"cancelled"`.
#[tauri::command]
pub async fn run_operation(
    app: AppHandle,
    engine: State<'_, Engine>,
    runs: State<'_, Runs>,
    op: Operation,
    inputs: Vec<PathBuf>,
    options: Option<Options>,
    run_id: Option<String>,
) -> Result<Vec<PathBuf>, String> {
    let registry = engine.current();
    let job = Arc::new(match run_id.clone() {
        Some(id) => {
            let last = Mutex::new(None::<Instant>);
            Job::new(move |p| {
                let mut last = last.lock().unwrap();
                if p.fraction < 1.0 && last.is_some_and(|at| at.elapsed() < Duration::from_millis(100)) {
                    return;
                }
                *last = Some(Instant::now());
                let _ = app.emit(RUN_PROGRESS_EVENT, RunProgress { run_id: id.clone(), fraction: p.fraction, label: p.label.clone() });
            })
        }
        None => Job::default(),
    });
    if let Some(id) = &run_id {
        runs.0.lock().unwrap().insert(id.clone(), Arc::clone(&job));
    }
    let running = Arc::clone(&job);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        let mimes: Vec<String> = inputs.iter().map(|f| engine::mime::detect(f)).collect();
        let result = registry.run_job(&mimes, &op, &inputs, &options.unwrap_or_default(), &running);
        let secs = started.elapsed().as_secs_f32();
        match &result {
            Ok(run) => eprintln!("rupantar: {} on {} file(s) via {} in {secs:.1} s", op.id, inputs.len(), run.engine),
            Err(e) => eprintln!("rupantar: {} on {} file(s) failed after {secs:.1} s: {e}", op.id, inputs.len()),
        }
        result.map(|run| run.outputs)
    })
    .await;
    if let Some(id) = &run_id {
        runs.0.lock().unwrap().remove(id);
    }
    match result.map_err(|e| e.to_string())? {
        Ok(outputs) => Ok(outputs),
        Err(EngineError::Cancelled) => Err("cancelled".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// Asks the run started with `run_id` to stop (a no-op once it has finished).
#[tauri::command]
pub fn cancel_operation(runs: State<Runs>, run_id: String) {
    if let Some(job) = runs.0.lock().unwrap().get(&run_id) {
        job.cancel();
    }
}

/// A downscaled image for the crop dialog; `width`/`height` are the original's (crop options
/// are in original pixels).
#[derive(Debug, Serialize)]
pub struct ImagePreview {
    /// `data:` URL, so the webview needs no file access.
    pub src: String,
    pub width: u32,
    pub height: u32,
}

#[tauri::command]
pub async fn preview_image(path: PathBuf) -> Result<ImagePreview, String> {
    use base64::Engine as _;
    let preview = tauri::async_runtime::spawn_blocking(move || engine::images::preview(&path, 1024))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let data = base64::engine::general_purpose::STANDARD.encode(&preview.bytes);
    Ok(ImagePreview { src: format!("data:{};base64,{data}", preview.mime), width: preview.width, height: preview.height })
}

#[derive(Debug, Serialize)]
pub struct FileBatch {
    pub paths: Vec<String>,
    /// File name for one file, "N files" for several.
    pub label: String,
    /// The type shared by every file, or `application/octet-stream` for a mixed batch.
    pub mime: String,
    /// One per file, in `paths` order.
    pub mimes: Vec<String>,
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
        mimes,
    })
}

/// Native multi-select file picker, a child of the radial window so it opens above it. Returns
/// the chosen paths (empty on cancel). Hide-on-blur is off while the dialog is up and goes back
/// to what it was afterwards, whichever way the dialog closed; the folder is remembered.
#[tauri::command]
pub async fn pick_files(app: AppHandle, state: State<'_, Shell>) -> Result<Vec<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    if state.picking.swap(true, Ordering::SeqCst) {
        return Ok(Vec::new());
    }
    let hide_before = state.hide_on_blur.swap(false, Ordering::SeqCst);

    let mut dialog = app.dialog().file().set_title("Choose files");
    let extensions: Vec<&str> = engine::mime::known_extensions().collect();
    dialog = dialog.add_filter("Supported files", &extensions).add_filter("All files", &["*"]);
    let dir = settings::load(&app).last_dir.map(PathBuf::from).filter(|d| d.is_dir());
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    if let Some(win) = app.get_webview_window(shell::RADIAL) {
        dialog = dialog.set_parent(&win);
    }

    let (tx, rx) = tauri::async_runtime::channel(1);
    dialog.pick_files(move |files| {
        let _ = tx.try_send(files);
    });
    let mut rx = rx;
    let picked = rx.recv().await.flatten();

    state.hide_on_blur.store(hide_before, Ordering::SeqCst);
    state.touch();
    state.picking.store(false, Ordering::SeqCst);
    // The dialog hands focus back unevenly (sometimes to another window); take it back.
    if let Some(win) = app.get_webview_window(shell::RADIAL) {
        let _ = win.set_focus();
    }

    let paths: Vec<PathBuf> = picked.unwrap_or_default().into_iter().filter_map(|p| p.into_path().ok()).collect();
    if let Some(dir) = paths.first().and_then(|p| p.parent()) {
        let mut current = settings::load(&app);
        current.last_dir = Some(dir.to_string_lossy().into_owned());
        let _ = settings::save(&app, &current);
    }
    Ok(paths.iter().map(|p| p.to_string_lossy().into_owned()).collect())
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
    // Arming it restarts the grace period, so focus still settling (e.g. right after the file
    // dialog closed) can't hide the window the moment the menu appears.
    if enabled && !state.hide_on_blur.load(Ordering::SeqCst) {
        state.touch();
    }
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
