use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use crate::analytics::Analytics;
use crate::settings::Settings;
use crate::usage::{Profile, UsageData};
use crate::AppState;

#[derive(Serialize)]
pub struct StateSnapshot {
    pub usage: UsageData,
    pub settings: Settings,
    pub profile: Option<Profile>,
    pub claude_running: bool,
    pub working: bool,
}

#[tauri::command]
pub fn get_state(state: State<AppState>) -> StateSnapshot {
    StateSnapshot {
        usage: state.usage.lock().unwrap().clone(),
        settings: state.settings.lock().unwrap().clone(),
        profile: state.profile.lock().unwrap().clone(),
        claude_running: state.claude_running.load(Ordering::Relaxed),
        working: state.working.load(Ordering::Relaxed),
    }
}

#[tauri::command]
pub fn refresh_now(state: State<AppState>) {
    let _ = state.poke.send(());
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> Result<(), String> {
    let mgr = app.autolaunch();
    if settings.start_with_windows {
        let _ = mgr.enable();
    } else {
        let _ = mgr.disable();
    }
    crate::settings::save(&app, &settings)?;
    *state.settings.lock().unwrap() = settings;
    let _ = state.poke.send(());
    Ok(())
}

#[tauri::command]
pub fn hide_popup(window: tauri::WebviewWindow) {
    let _ = window.hide();
}

/// Resize the popup to fit its content height (logical px) and re-anchor it to
/// the bottom-right, so it grows/shrinks from the top while staying above the
/// tray. Width is fixed. Called by the frontend whenever content height changes.
#[tauri::command]
pub fn resize_popup(window: tauri::WebviewWindow, height: f64) {
    let h = height.clamp(120.0, 640.0);
    let _ = window.set_size(tauri::LogicalSize::new(372.0, h));
    crate::position_bottom_right(&window);
}

/// Local usage analytics (approximate). Cached for 2 min; recomputed off-thread.
#[tauri::command]
pub async fn get_analytics(state: State<'_, AppState>, force: bool) -> Result<Analytics, String> {
    {
        let cache = state.analytics.lock().unwrap();
        if !force {
            if let Some(a) = cache.as_ref() {
                if crate::credentials::now_ms() - a.computed_at_ms < 120_000 {
                    return Ok(a.clone());
                }
            }
        }
    }
    let a = tokio::task::spawn_blocking(crate::analytics::compute_analytics)
        .await
        .map_err(|e| e.to_string())?;
    *state.analytics.lock().unwrap() = Some(a.clone());
    Ok(a)
}

// ---- SessionStart hook management ------------------------------------------

fn claude_settings_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("settings.json"));
    }
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

const HOOK_MARKER: &str = "claude-usage-widget";

fn launch_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_str = exe.to_string_lossy().replace('\'', "''");
    Ok(format!(
        "powershell -NoProfile -WindowStyle Hidden -Command \"Start-Process -FilePath '{}' -ArgumentList '--hooked'\"",
        exe_str
    ))
}

fn read_settings_json(path: &PathBuf) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}))
}

#[tauri::command]
pub fn session_hook_installed() -> bool {
    let Some(path) = claude_settings_path() else {
        return false;
    };
    let root = read_settings_json(&path);
    let Some(arr) = root
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(|s| s.as_array())
    else {
        return false;
    };
    arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|hk| {
                    hk.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains(HOOK_MARKER))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

#[tauri::command]
pub fn install_session_hook() -> Result<(), String> {
    let path = claude_settings_path().ok_or("No se encontró ~/.claude/settings.json")?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut root = read_settings_json(&path);
    let cmd = launch_command()?;

    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let session = hooks
        .as_object_mut()
        .ok_or("hooks no es un objeto")?
        .entry("SessionStart")
        .or_insert_with(|| json!([]));
    let arr = session.as_array_mut().ok_or("SessionStart no es un array")?;

    let already = arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hs| {
                hs.iter().any(|hk| {
                    hk.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains(HOOK_MARKER))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already {
        arr.push(json!({
            "hooks": [ { "type": "command", "command": cmd } ]
        }));
    }

    let pretty = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, pretty).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn remove_session_hook() -> Result<(), String> {
    let path = claude_settings_path().ok_or("No se encontró ~/.claude/settings.json")?;
    let mut root = read_settings_json(&path);
    if let Some(arr) = root
        .get_mut("hooks")
        .and_then(|h| h.get_mut("SessionStart"))
        .and_then(|s| s.as_array_mut())
    {
        arr.retain(|entry| {
            let has_marker = entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|hs| {
                    hs.iter().any(|hk| {
                        hk.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| c.contains(HOOK_MARKER))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            !has_marker
        });
    }
    let pretty = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, pretty).map_err(|e| e.to_string())?;
    Ok(())
}
