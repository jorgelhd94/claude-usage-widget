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
    /// Which Claude app is open: "code", "desktop" or "".
    pub claude_source: &'static str,
    pub working: bool,
}

#[tauri::command]
pub fn get_state(state: State<AppState>) -> StateSnapshot {
    let presence = state.presence();
    StateSnapshot {
        usage: state.usage.lock().unwrap().clone(),
        settings: state.settings.lock().unwrap().clone(),
        profile: state.profile.lock().unwrap().clone(),
        claude_running: presence.any(),
        claude_source: presence.source(),
        working: state.working.load(Ordering::Relaxed),
    }
}

// ---- Signing in to Claude Code ---------------------------------------------

/// Find a Claude Code executable to sign in with: the CLI on PATH or in its
/// default install location, else the newest engine Claude Desktop downloaded
/// (the same binary — which is what lets Desktop-only users sign in at all).
fn find_claude_cli() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd"]
    } else {
        &["claude"]
    };
    let mut dirs_to_try: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs_to_try.push(home.join(".local").join("bin"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs_to_try.extend(std::env::split_paths(&path));
    }
    for dir in dirs_to_try {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // Desktop keeps each engine at claude-code/<version>/<build>/claude.exe.
    for (_, dir) in crate::installed_claude_code() {
        let mut level = vec![dir];
        for _ in 0..2 {
            for d in &level {
                let candidate = d.join(names[0]);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            level = level
                .iter()
                .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
        }
    }
    None
}

/// Open a terminal running `claude auth login`. Claude Code does the whole
/// OAuth flow itself (browser and all) and writes the result to its
/// credentials file, which the widget is watching — so the popup comes back to
/// life as soon as the user finishes, and the widget never handles a sign-in.
#[tauri::command]
pub fn start_login() -> Result<(), String> {
    let cli = find_claude_cli().ok_or_else(|| {
        "No se encontró Claude Code. Instálalo y ejecuta «claude auth login».".to_string()
    })?;

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        // `cmd /k` keeps the window open afterwards, so a failed login can be read.
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/k").arg(&cli).args(["auth", "login"]);
        // If a Claude session launched us (the SessionStart hook), we carry its
        // environment; a login run with it would act as that embedded session
        // rather than as the user's own CLI.
        for (key, _) in std::env::vars_os() {
            let k = key.to_string_lossy().to_uppercase();
            let inherited = k == "CLAUDECODE"
                || k == "CLAUDE_PID"
                || k.starts_with("CLAUDE_CODE_")
                || k.starts_with("CLAUDE_AGENT_SDK_")
                || k.starts_with("ANTHROPIC_");
            if inherited {
                cmd.env_remove(&key);
            }
        }
        cmd.creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("No se pudo abrir la terminal: {}", e))
    }
    #[cfg(not(windows))]
    {
        let _ = cli;
        Err("Ejecuta «claude auth login» en una terminal.".to_string())
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
