use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "settings.json";
const KEY: &str = "settings";

// `default`: settings saved by an older version lack the newer fields; fill
// those in rather than discarding everything the user had configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Usage poll cadence in seconds (clamped to >= 30 to respect rate limits).
    pub poll_interval_secs: u64,
    /// Notify when any window crosses this percentage.
    pub notify_high: f64,
    /// Notify (stronger) when any window crosses this percentage.
    pub notify_critical: f64,
    pub enable_notifications: bool,
    pub enable_animation: bool,
    pub start_with_windows: bool,
    /// "dark" | "light" | "auto"
    pub theme: String,
    /// Renew Claude Code's OAuth token from the widget once it expires. Needed
    /// when nothing else does: Claude Desktop signs in on its own and never
    /// touches the CLI's login, which otherwise lapses from disuse.
    pub keep_session_alive: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_interval_secs: 150,
            notify_high: 80.0,
            notify_critical: 90.0,
            enable_notifications: true,
            enable_animation: true,
            start_with_windows: false,
            theme: "dark".to_string(),
            keep_session_alive: true,
        }
    }
}

pub fn load(app: &AppHandle) -> Settings {
    if let Ok(store) = app.store(STORE_FILE) {
        if let Some(v) = store.get(KEY) {
            if let Ok(s) = serde_json::from_value::<Settings>(v) {
                return s;
            }
        }
    }
    Settings::default()
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    let v = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    store.set(KEY, v);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}
