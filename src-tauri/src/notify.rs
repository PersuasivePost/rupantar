//! OS notifications for runs that finish while the radial window is hidden (it hides on blur, so
//! a long conversion often outlives it). The notification offers "Show in folder".

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use tauri_plugin_notification::{ActionType, NotificationExt};

use crate::shell::RADIAL;

const ACTION_TYPE: &str = "result";
const ACTION_SHOW: &str = "show";

/// The newest result, which a click on any notification falls back to when it can't say which
/// file it was about.
#[derive(Default)]
pub struct LastResult(Mutex<Option<PathBuf>>);

/// Registers the action and its handler. Call once, after the notification plugin.
pub fn init(app: &AppHandle) {
    let notifications = app.notification();
    let action = tauri_plugin_notification::Action::builder(ACTION_SHOW, "Show in folder").build();
    let _ = notifications.register_action_types(vec![ActionType::builder(ACTION_TYPE).actions(vec![action]).build()]);
    let handle = app.clone();
    let _ = notifications.on_action(move |performed| {
        if !matches!(performed.action_id(), ACTION_SHOW | "tap") {
            return;
        }
        let named = performed
            .notification()
            .and_then(|n| n.extra().get("path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let path = named.or_else(|| handle.state::<LastResult>().0.lock().unwrap().clone());
        if let Some(path) = path {
            if let Err(e) = tauri_plugin_opener::reveal_item_in_dir(&path) {
                eprintln!("rupantar: could not show {} in its folder: {e}", path.display());
            }
        }
    });
}

fn radial_hidden(app: &AppHandle) -> bool {
    !app.get_webview_window(RADIAL).and_then(|w| w.is_visible().ok()).unwrap_or(false)
}

fn name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// A finished run, if nobody is looking at the radial window. `engine` (debug builds) is added
/// to the text, so a run that went through the wrong converter shows.
pub fn succeeded(app: &AppHandle, label: &str, outputs: &[PathBuf], engine: Option<&str>) {
    let Some(first) = outputs.first() else { return };
    *app.state::<LastResult>().0.lock().unwrap() = Some(first.clone());
    if !radial_hidden(app) {
        return;
    }
    let mut body = match outputs {
        [one] => name(one),
        many => format!("{} files, starting with {}", many.len(), name(&many[0])),
    };
    if let Some(engine) = engine {
        body.push_str(&format!("\nvia {engine}"));
    }
    show(app, format!("{label} finished"), body, Some(first));
}

/// A failed run (not a cancelled one), if nobody is looking at the radial window.
pub fn failed(app: &AppHandle, label: &str, error: &str) {
    if radial_hidden(app) {
        let body: String = error.chars().take(200).collect();
        show(app, format!("{label} failed"), body, None);
    }
}

fn show(app: &AppHandle, title: String, body: String, reveal: Option<&PathBuf>) {
    let mut n = app.notification().builder().title(title).body(body);
    if let Some(path) = reveal {
        n = n.action_type_id(ACTION_TYPE).extra("path", path.display().to_string());
    }
    if let Err(e) = n.show() {
        eprintln!("rupantar: could not show a notification: {e}");
    }
}
