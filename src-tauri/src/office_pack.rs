//! The Office pack (LibreOffice) in the shell: status, install, cancel, remove, notices. The
//! work happens in the `packs` crate; this wires it to events and swaps the engine registry
//! once LibreOffice appears or goes away.
//!
//! Consent: `install_office_pack` is the only path to the network in the app, and the frontend
//! calls it only from a button the user pressed after seeing the download size (the consent
//! dialog shown on first use of an Office op, or Settings → Office pack).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use packs::{Pack, PackError, Progress};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::Engine;

/// Emitted while installing (`packs::Progress`).
const PROGRESS_EVENT: &str = "office-pack-progress";
/// Emitted when the pack was installed or removed (payload: the new status).
const CHANGED_EVENT: &str = "office-pack-changed";

/// The cancel flag of the install in progress, if any.
#[derive(Default)]
pub struct OfficeJob(Mutex<Option<Arc<AtomicBool>>>);

impl OfficeJob {
    fn running(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

pub fn pack(app: &AppHandle) -> Option<Pack> {
    app.path().app_data_dir().ok().map(|d| Pack::office(&d.join("packs")))
}

/// The installed LibreOffice program, if any.
pub fn program(app: &AppHandle) -> Option<PathBuf> {
    pack(app)?.program()
}

/// What the consent dialog and Settings show.
#[derive(Debug, Serialize)]
pub struct Offer {
    pub version: &'static str,
    pub download_bytes: u64,
    pub installed_bytes: u64,
    /// Already on disk from an interrupted download (a retry resumes after it).
    pub downloaded_bytes: u64,
    /// False when this platform's build has not been tried on a real machine yet.
    pub tested: bool,
    pub source: &'static str,
}

#[derive(Debug, Serialize)]
pub struct PackStatus {
    /// `installed`, `installing`, `not_installed` or `unsupported`.
    pub state: &'static str,
    pub installed: Option<packs::Installed>,
    /// The build this machine would download (None: unsupported).
    pub offer: Option<Offer>,
}

fn status(app: &AppHandle, job: &OfficeJob) -> PackStatus {
    let pack = pack(app);
    let installed = pack.as_ref().and_then(Pack::installed);
    let offer = packs::office::current().map(|b| Offer {
        version: b.version,
        download_bytes: b.size,
        installed_bytes: b.installed_size,
        downloaded_bytes: pack.as_ref().map_or(0, |p| p.downloaded(b)),
        tested: b.tested,
        source: "documentfoundation.org",
    });
    let state = if job.running() {
        "installing"
    } else if installed.is_some() {
        "installed"
    } else if offer.is_none() {
        "unsupported"
    } else {
        "not_installed"
    };
    PackStatus { state, installed, offer }
}

#[tauri::command]
pub fn office_pack_status(app: AppHandle, job: State<OfficeJob>) -> PackStatus {
    status(&app, &job)
}

/// Downloads and installs LibreOffice, reporting `office-pack-progress`. Resolves to the new
/// status; rejects with `"cancelled"` when cancelled, or a readable error.
#[tauri::command]
pub async fn install_office_pack(app: AppHandle, job: State<'_, OfficeJob>, engine: State<'_, Engine>) -> Result<PackStatus, String> {
    let build = packs::office::current()
        .ok_or_else(|| format!("there is no Office pack for {} {}", std::env::consts::OS, std::env::consts::ARCH))?;
    let pack = pack(&app).ok_or("no app data directory available")?;
    let cancel = {
        let mut slot = job.0.lock().unwrap();
        if slot.is_some() {
            return Err("the Office pack is already being downloaded".into());
        }
        let flag = Arc::new(AtomicBool::new(false));
        *slot = Some(Arc::clone(&flag));
        flag
    };

    let emitter = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut last: Option<(Progress, Instant)> = None;
        let mut report = |p: Progress| {
            // A few events per second is plenty for a progress bar.
            let due = last.is_none_or(|(prev, at)| prev.phase != p.phase || p.done == p.total || at.elapsed() >= Duration::from_millis(120));
            if due {
                let _ = emitter.emit(PROGRESS_EVENT, p);
                last = Some((p, Instant::now()));
            }
        };
        pack.install(build, &mut report, &cancel)
    })
    .await
    .map_err(|e| e.to_string());
    *job.0.lock().unwrap() = None;

    match result? {
        Ok(program) => {
            eprintln!("rupantar: Office pack installed at {}", program.display());
            engine.set_soffice(Some(program));
            let now = status(&app, &job);
            let _ = app.emit(CHANGED_EVENT, &now);
            Ok(now)
        }
        Err(PackError::Cancelled) => Err("cancelled".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn cancel_office_pack(job: State<OfficeJob>) {
    if let Some(flag) = job.0.lock().unwrap().as_ref() {
        flag.store(true, Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn remove_office_pack(app: AppHandle, job: State<'_, OfficeJob>, engine: State<'_, Engine>) -> Result<PackStatus, String> {
    if job.running() {
        return Err("cancel the download first".into());
    }
    let pack = pack(&app).ok_or("no app data directory available")?;
    engine.set_soffice(None);
    tauri::async_runtime::spawn_blocking(move || pack.remove()).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let now = status(&app, &job);
    let _ = app.emit(CHANGED_EVENT, &now);
    Ok(now)
}

#[derive(Debug, Serialize)]
pub struct Notice {
    pub name: String,
    pub text: String,
}

/// License and notice files that ship next to a component: `pdfium` (bundled resource) or
/// `office` (the installed pack's NOTICE and LICENSE; empty when not installed).
#[tauri::command]
pub fn third_party_notices(app: AppHandle, component: String) -> Vec<Notice> {
    let read = |path: PathBuf| {
        let text = std::fs::read_to_string(&path).ok()?;
        Some(Notice { name: path.file_name()?.to_string_lossy().into_owned(), text })
    };
    match component.as_str() {
        "pdfium" => {
            let Ok(dir) = app.path().resource_dir().map(|d| d.join("pdfium")) else { return Vec::new() };
            let mut files: Vec<PathBuf> =
                std::fs::read_dir(dir.join("licenses")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
            files.sort();
            std::iter::once(dir.join("LICENSE")).chain(files).filter_map(read).collect()
        }
        "office" => {
            let Some(root) = program(&app).and_then(|p| p.ancestors().nth(2).map(PathBuf::from)) else { return Vec::new() };
            ["NOTICE", "LICENSE"].into_iter().filter_map(|n| read(root.join(n))).collect()
        }
        _ => Vec::new(),
    }
}
