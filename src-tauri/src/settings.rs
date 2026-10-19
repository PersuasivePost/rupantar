//! User settings, persisted as JSON in the app config dir.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

pub const DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+Space";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub shortcut: String,
    pub autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { shortcut: DEFAULT_SHORTCUT.to_string(), autostart: false }
    }
}

fn file(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

/// Missing or unreadable settings fall back to defaults rather than blocking startup.
pub fn load(app: &AppHandle) -> Settings {
    file(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = file(app).ok_or("no config directory available")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = serde_json::from_str(r#"{"autostart": true}"#).unwrap();
        assert_eq!(s, Settings { shortcut: DEFAULT_SHORTCUT.into(), autostart: true });
    }

    #[test]
    fn round_trips() {
        let s = Settings { shortcut: "Alt+Space".into(), autostart: true };
        assert_eq!(serde_json::from_str::<Settings>(&serde_json::to_string(&s).unwrap()).unwrap(), s);
    }
}
