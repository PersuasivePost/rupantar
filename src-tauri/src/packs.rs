//! Optional packs in the shell: the Office pack (LibreOffice) and the AI pack (ONNX Runtime,
//! BiRefNet lite, Real-ESRGAN). Status, install, cancel, remove, notices. The work happens in the
//! `packs` crate; this wires it to events and swaps the engine registry once a pack appears or
//! goes away.
//!
//! Consent: `install_pack` is the only path to the network in the app, and the frontend calls
//! it only from a button the user pressed after seeing the download size (the consent dialog
//! shown on first use of an operation that needs the pack, or Settings).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use packs::{Build, Pack, PackError, Progress};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::Engine;

/// Emitted while installing (`PackProgress`).
const PROGRESS_EVENT: &str = "pack-progress";
/// Emitted when a pack was installed or removed (payload: its new `PackStatus`).
const CHANGED_EVENT: &str = "pack-changed";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Office,
    Ai,
}

impl Kind {
    fn parse(id: &str) -> Result<Self, String> {
        match id {
            "office" => Ok(Kind::Office),
            "ai" => Ok(Kind::Ai),
            other => Err(format!("unknown pack `{other}`")),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Kind::Office => "office",
            Kind::Ai => "ai",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Office => "Office pack",
            Kind::Ai => "AI pack",
        }
    }

    /// The build this machine would download, if there is one.
    fn build(self) -> Option<&'static Build> {
        match self {
            Kind::Office => packs::office::current(),
            Kind::Ai => packs::ai::current(),
        }
    }
}

/// The cancel flag of each install in progress.
#[derive(Default)]
pub struct PackJobs(Mutex<HashMap<Kind, Arc<AtomicBool>>>);

impl PackJobs {
    fn running(&self, kind: Kind) -> bool {
        self.0.lock().unwrap().contains_key(&kind)
    }
}

fn pack(app: &AppHandle, kind: Kind) -> Option<Pack> {
    let dir = app.path().app_data_dir().ok()?.join("packs");
    Some(match kind {
        Kind::Office => Pack::office(&dir),
        Kind::Ai => Pack::ai(&dir),
    })
}

/// Finishes pack removals that could not complete while the app was running (a loaded DLL on
/// Windows). Call at startup, before any pack library is loaded.
pub fn sweep(app: &AppHandle) {
    for kind in [Kind::Office, Kind::Ai] {
        if let Some(p) = pack(app, kind) {
            p.sweep();
        }
    }
}

/// Points `config` at whatever packs are installed (or at nothing, for removed ones).
pub fn apply(app: &AppHandle, config: &mut engine::EngineConfig) {
    config.soffice_path = pack(app, Kind::Office).and_then(|p| p.program());
    let ai = pack(app, Kind::Ai);
    let file = |key: &str| ai.as_ref().and_then(|p| p.file(key));
    config.onnxruntime_path = file(packs::ai::ONNXRUNTIME);
    config.birefnet_path = file(packs::ai::BIREFNET);
    config.realesrgan_path = file(packs::ai::REALESRGAN);
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
    /// Where it comes from: the sites of its download URLs ("github.com, nuget.org").
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct PackStatus {
    pub pack: &'static str,
    /// `installed`, `installing`, `not_installed` or `unsupported`.
    pub state: &'static str,
    pub installed: Option<Installed>,
    /// The build this machine would download (None: unsupported).
    pub offer: Option<Offer>,
}

#[derive(Debug, Serialize)]
pub struct Installed {
    pub version: String,
    /// Bytes on disk.
    pub size: u64,
}

/// `download.documentfoundation.org` → `documentfoundation.org`.
fn site(url: &str) -> String {
    let host = url.split_once("://").map_or(url, |(_, r)| r).split('/').next().unwrap_or_default();
    let labels: Vec<&str> = host.rsplit('.').take(2).collect();
    labels.into_iter().rev().collect::<Vec<_>>().join(".")
}

fn status(app: &AppHandle, jobs: &PackJobs, kind: Kind) -> PackStatus {
    let pack = pack(app, kind);
    let installed = pack.as_ref().and_then(Pack::installed).map(|i| Installed { version: i.version, size: i.size });
    let offer = kind.build().map(|b| {
        let mut sites: Vec<String> = b.components.iter().filter_map(|c| c.urls.first()).map(|u| site(u)).collect();
        sites.dedup();
        Offer {
            version: b.version,
            download_bytes: b.size(),
            installed_bytes: b.installed_size,
            downloaded_bytes: pack.as_ref().map_or(0, |p| p.downloaded(b)),
            tested: b.tested,
            source: sites.join(", "),
        }
    });
    let state = if jobs.running(kind) {
        "installing"
    } else if installed.is_some() {
        "installed"
    } else if offer.is_none() {
        "unsupported"
    } else {
        "not_installed"
    };
    PackStatus { pack: kind.id(), state, installed, offer }
}

#[tauri::command]
pub fn pack_status(app: AppHandle, jobs: State<PackJobs>, pack: String) -> Result<PackStatus, String> {
    Ok(status(&app, &jobs, Kind::parse(&pack)?))
}

#[derive(Debug, Clone, Serialize)]
struct PackProgress {
    pack: &'static str,
    #[serde(flatten)]
    progress: Progress,
}

/// Downloads and installs a pack, reporting `pack-progress`. Resolves to the new status;
/// rejects with `"cancelled"` when cancelled, or a readable error.
#[tauri::command]
pub async fn install_pack(app: AppHandle, jobs: State<'_, PackJobs>, engine: State<'_, Engine>, pack: String) -> Result<PackStatus, String> {
    let kind = Kind::parse(&pack)?;
    let build = kind
        .build()
        .ok_or_else(|| format!("there is no {} for {} {}", kind.name(), std::env::consts::OS, std::env::consts::ARCH))?;
    let target = self::pack(&app, kind).ok_or("no app data directory available")?;
    let cancel = {
        let mut running = jobs.0.lock().unwrap();
        if running.contains_key(&kind) {
            return Err(format!("the {} is already being downloaded", kind.name()));
        }
        let flag = Arc::new(AtomicBool::new(false));
        running.insert(kind, Arc::clone(&flag));
        flag
    };

    let emitter = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut last: Option<(Progress, Instant)> = None;
        let mut report = |p: Progress| {
            // A few events per second is plenty for a progress bar.
            let due = last.is_none_or(|(prev, at)| prev.phase != p.phase || p.done == p.total || at.elapsed() >= Duration::from_millis(120));
            if due {
                let _ = emitter.emit(PROGRESS_EVENT, PackProgress { pack: kind.id(), progress: p });
                last = Some((p, Instant::now()));
            }
        };
        target.install(build, &mut report, &cancel)
    })
    .await
    .map_err(|e| e.to_string());
    jobs.0.lock().unwrap().remove(&kind);

    match result? {
        Ok(files) => {
            let files: BTreeMap<_, _> = files.iter().map(|(k, p)| (k.as_str(), p.display().to_string())).collect();
            eprintln!("rupantar: {} installed: {files:?}", kind.name());
            engine.update(|config| apply(&app, config));
            let now = status(&app, &jobs, kind);
            let _ = app.emit(CHANGED_EVENT, &now);
            Ok(now)
        }
        Err(PackError::Cancelled) => Err("cancelled".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn cancel_pack(jobs: State<PackJobs>, pack: String) -> Result<(), String> {
    if let Some(flag) = jobs.0.lock().unwrap().get(&Kind::parse(&pack)?) {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Removes a pack. On Windows a library the app already loaded (ONNX Runtime, after a
/// background removal) can't be deleted until Rupantar restarts: the pack then counts as
/// removed and the rest is deleted at the next start (`sweep`).
#[tauri::command]
pub async fn remove_pack(app: AppHandle, jobs: State<'_, PackJobs>, engine: State<'_, Engine>, pack: String) -> Result<PackStatus, String> {
    let kind = Kind::parse(&pack)?;
    if jobs.running(kind) {
        return Err("cancel the download first".into());
    }
    let target = self::pack(&app, kind).ok_or("no app data directory available")?;
    let removed = tauri::async_runtime::spawn_blocking(move || target.remove()).await.map_err(|e| e.to_string())?;
    engine.update(|config| apply(&app, config)); // also after a partial removal
    removed.map_err(|e| e.to_string())?;
    let now = status(&app, &jobs, kind);
    let _ = app.emit(CHANGED_EVENT, &now);
    Ok(now)
}

#[derive(Debug, Serialize)]
pub struct Notice {
    pub name: String,
    pub text: String,
}

/// License and notice files that ship next to a component: `pdfium` (bundled resource),
/// `office` (the installed pack's NOTICE and LICENSE) or `ai` (ONNX Runtime's and, on Windows,
/// DirectML's, from the installed pack). Empty when the pack isn't installed.
#[tauri::command]
pub fn third_party_notices(app: AppHandle, component: String) -> Vec<Notice> {
    let read = |path: PathBuf| {
        let text = std::fs::read_to_string(&path).ok()?;
        Some(Notice { name: path.file_name()?.to_string_lossy().into_owned(), text })
    };
    let sorted_files = |dir: PathBuf| {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        files.sort();
        files
    };
    match component.as_str() {
        "pdfium" => {
            let Ok(dir) = app.path().resource_dir().map(|d| d.join("pdfium")) else { return Vec::new() };
            std::iter::once(dir.join("LICENSE")).chain(sorted_files(dir.join("licenses"))).filter_map(read).collect()
        }
        "office" => {
            let program = pack(&app, Kind::Office).and_then(|p| p.program());
            let Some(root) = program.and_then(|p| p.ancestors().nth(2).map(PathBuf::from)) else { return Vec::new() };
            ["NOTICE", "LICENSE"].into_iter().filter_map(|n| read(root.join(n))).collect()
        }
        "ai" => {
            // <tree>/onnxruntime/<library> → <tree>/notices/<component>/*
            let library = pack(&app, Kind::Ai).and_then(|p| p.file(packs::ai::ONNXRUNTIME));
            let Some(tree) = library.and_then(|p| p.ancestors().nth(2).map(PathBuf::from)) else { return Vec::new() };
            ["onnxruntime", "directml"]
                .into_iter()
                .flat_map(|c| sorted_files(tree.join("notices").join(c)))
                .filter_map(|path| {
                    let component = path.parent()?.file_name()?.to_string_lossy().into_owned();
                    read(path).map(|n| Notice { name: format!("{component}/{}", n.name), ..n })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_are_registrable_domains() {
        assert_eq!(site("https://download.documentfoundation.org/libreoffice/x.tar.gz"), "documentfoundation.org");
        assert_eq!(site("https://api.nuget.org/v3-flatcontainer/x.nupkg"), "nuget.org");
        assert_eq!(site("https://github.com/microsoft/onnxruntime/releases/download/v1/x.tgz"), "github.com");
    }
}
