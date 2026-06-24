mod analytics;
mod commands;
mod credentials;
mod icons;
mod notifications;
mod settings;
mod usage;
mod watcher;

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::RecommendedWatcher;
use sysinfo::System;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use notifications::NotifyState;
use settings::Settings;
use usage::{Profile, UsageData, CLAUDE_VERSION_FALLBACK};
use watcher::Activity;

/// Shared application state, managed by Tauri and reachable from commands/tasks.
pub struct AppState {
    pub http: reqwest::Client,
    pub version: String,
    /// False only for hook-launched instances (`--hooked`), which exit shortly
    /// after Claude Code closes so they clean up after themselves. Manual and
    /// autostart launches are persistent: they stay resident with the tray icon
    /// kept visible (and quittable) even while Claude Code isn't running.
    pub persistent: bool,
    pub usage: Mutex<UsageData>,
    pub settings: Mutex<Settings>,
    pub claude_running: AtomicBool,
    pub working: AtomicBool,
    /// Epoch-ms when the popup was last hidden — used to make a tray click that
    /// just blurred-and-hid the popup count as "close", not immediately reopen.
    pub last_hidden_ms: AtomicI64,
    pub activity: Activity,
    pub tray: Mutex<Option<TrayIcon>>,
    pub icon_color: Mutex<String>,
    pub icon_percent: Mutex<f64>,
    pub frames: Mutex<Vec<Image<'static>>>,
    pub profile: Mutex<Option<Profile>>,
    pub analytics: Mutex<Option<analytics::Analytics>>,
    pub watcher: Mutex<Option<RecommendedWatcher>>,
    pub notify_state: Mutex<NotifyState>,
    pub poke: UnboundedSender<()>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (poke_tx, poke_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    // Only the SessionStart hook launches with `--hooked`; those instances are
    // ephemeral (exit when Claude Code closes). Manual / autostart stay resident.
    let persistent = !std::env::args().any(|a| a == "--hooked");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // A second launch (e.g. the SessionStart hook firing again) just
            // nudges the resident instance to refresh.
            let _ = app.state::<AppState>().poke.send(());
        }))
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostarted"]),
        ))
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::refresh_now,
            commands::get_settings,
            commands::set_settings,
            commands::hide_popup,
            commands::session_hook_installed,
            commands::install_session_hook,
            commands::remove_session_hook,
            commands::get_analytics,
            commands::resize_popup,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            let loaded = settings::load(&handle);
            let activity: Activity = Arc::new(Mutex::new(None));
            let http = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());
            let version = detect_version();

            app.manage(AppState {
                http,
                version,
                persistent,
                usage: Mutex::new(UsageData::default()),
                settings: Mutex::new(loaded),
                claude_running: AtomicBool::new(false),
                working: AtomicBool::new(false),
                last_hidden_ms: AtomicI64::new(0),
                activity: activity.clone(),
                tray: Mutex::new(None),
                icon_color: Mutex::new(icons::NEUTRAL.to_string()),
                icon_percent: Mutex::new(0.0),
                frames: Mutex::new(icons::working_frames(icons::NEUTRAL, 0.0)),
                profile: Mutex::new(None),
                analytics: Mutex::new(None),
                watcher: Mutex::new(None),
                notify_state: Mutex::new(NotifyState::default()),
                poke: poke_tx.clone(),
            });

            if let Some(w) = watcher::start_activity_watcher(activity) {
                let _ = handle.state::<AppState>().watcher.lock().unwrap().replace(w);
            }

            // Right-click tray menu — lets a resident instance be closed
            // (left-click still toggles the popup).
            let quit_item = MenuItem::with_id(&handle, "quit", "Salir", true, None::<&str>)?;
            let tray_menu = Menu::with_items(&handle, &[&quit_item])?;

            let tray = TrayIconBuilder::with_id("claude-usage")
                .icon(icons::idle_icon(icons::NEUTRAL, 0.0))
                .tooltip("Claude Usage")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    let app = tray.app_handle();
                    tauri_plugin_positioner::on_tray_event(app, &event);
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_popup(app);
                    }
                })
                .build(&handle)?;
            // Resident instances show the tray immediately; hook-launched ones
            // stay hidden until Claude Code is detected.
            let _ = tray.set_visible(persistent);
            let _ = handle.state::<AppState>().tray.lock().unwrap().replace(tray);

            round_window_corners(&handle);

            spawn_process_watcher(handle.clone());
            spawn_poll_task(handle.clone(), poke_rx);
            spawn_anim_task(handle.clone());

            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Focused(false) => {
                // Click outside / lose focus closes the popup.
                if window.label() == "main" {
                    window
                        .state::<AppState>()
                        .last_hidden_ms
                        .store(credentials::now_ms(), Ordering::Relaxed);
                    let _ = window.hide();
                }
            }
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---- Tray helpers -----------------------------------------------------------

fn with_tray<F: FnOnce(&TrayIcon)>(app: &AppHandle, f: F) {
    if let Some(tray) = app.state::<AppState>().tray.lock().unwrap().as_ref() {
        f(tray);
    }
}

fn set_tray_icon(app: &AppHandle, img: Image<'static>) {
    with_tray(app, |t| {
        let _ = t.set_icon(Some(img));
    });
}

fn set_tray_visible(app: &AppHandle, visible: bool) {
    with_tray(app, |t| {
        let _ = t.set_visible(visible);
    });
}

fn set_tray_tooltip(app: &AppHandle, tooltip: String) {
    with_tray(app, |t| {
        let _ = t.set_tooltip(Some(tooltip));
    });
}

fn toggle_popup(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            app.state::<AppState>()
                .last_hidden_ms
                .store(credentials::now_ms(), Ordering::Relaxed);
            let _ = win.hide();
        } else {
            // If the popup was hidden moments ago, this same click is what
            // blurred-and-hid it — treat the click as "close", don't reopen.
            let since = credentials::now_ms()
                - app.state::<AppState>().last_hidden_ms.load(Ordering::Relaxed);
            if since < 300 {
                return;
            }
            // Show first: a hidden window has no associated monitor, so
            // current_monitor() would be None and positioning a no-op.
            let _ = win.show();
            position_bottom_right(&win);
            let _ = win.set_focus();
        }
    }
}

/// Anchor the popup to the bottom-right of the monitor, just above the taskbar.
/// Positions via the plugin, then clamps fully on-screen using the window's own
/// reported geometry (all physical, unit-consistent). Call on the main thread
/// after show().
pub(crate) fn position_bottom_right(win: &tauri::WebviewWindow) {
    use tauri_plugin_positioner::{Position, WindowExt};
    let _ = win.move_window(Position::BottomRight);
    if let (Ok(Some(m)), Ok(pos), Ok(size)) =
        (win.current_monitor(), win.outer_position(), win.outer_size())
    {
        let scale = m.scale_factor();
        let mp = m.position();
        let ms = m.size();
        let taskbar = (48.0 * scale) as i32;
        let margin = (12.0 * scale) as i32;
        let max_x = mp.x + ms.width as i32 - size.width as i32 - margin;
        let max_y = mp.y + ms.height as i32 - size.height as i32 - taskbar;
        let x = pos.x.min(max_x).max(mp.x + margin);
        let y = pos.y.min(max_y).max(mp.y + margin);
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

// ---- Background tasks -------------------------------------------------------

fn spawn_process_watcher(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut sys = System::new();
        let mut was_running = false;
        let mut misses: u32 = 0;
        loop {
            let running = watcher::claude_running(&mut sys);
            {
                let state = app.state::<AppState>();
                state.claude_running.store(running, Ordering::Relaxed);
                if running {
                    misses = 0;
                    if !was_running {
                        set_tray_visible(&app, true);
                        let _ = state.poke.send(());
                        let _ = app.emit("claude-status", true);
                    }
                } else {
                    if was_running {
                        let _ = app.emit("claude-status", false);
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.hide();
                        }
                        if state.persistent {
                            // Stay resident & accessible: keep the tray visible
                            // with a neutral icon while Claude Code is closed.
                            set_tray_icon(&app, icons::idle_icon(icons::NEUTRAL, 0.0));
                            set_tray_tooltip(&app, "Claude Code no está abierto".into());
                        } else {
                            set_tray_visible(&app, false);
                        }
                    }
                    if !state.persistent {
                        misses += 1;
                        if misses >= 5 {
                            app.exit(0);
                        }
                    }
                }
            }
            was_running = running;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

fn spawn_poll_task(app: AppHandle, mut poke_rx: UnboundedReceiver<()>) {
    tauri::async_runtime::spawn(async move {
        let mut backoff_until: Option<Instant> = None;
        loop {
            let (running, interval, http, version) = {
                let state = app.state::<AppState>();
                let running = state.claude_running.load(Ordering::Relaxed);
                let interval = state.settings.lock().unwrap().poll_interval_secs.max(30);
                let http = state.http.clone();
                let version = state.version.clone();
                (running, interval, http, version)
            };

            let in_backoff = backoff_until.map(|t| Instant::now() < t).unwrap_or(false);

            if running {
                let need_profile = app.state::<AppState>().profile.lock().unwrap().is_none();
                if need_profile {
                    if let Ok(p) = usage::fetch_profile(&http, &version).await {
                        *app.state::<AppState>().profile.lock().unwrap() = Some(p.clone());
                        let _ = app.emit("profile-updated", p);
                    }
                }
            }

            if running && !in_backoff {
                match usage::fetch_usage(&http, &version).await {
                    Ok(data) => {
                        backoff_until = None;
                        on_usage_update(&app, data);
                    }
                    Err(usage::FetchError::RateLimited(retry)) => {
                        let secs = retry.unwrap_or(60).clamp(30, 600);
                        backoff_until = Some(Instant::now() + Duration::from_secs(secs));
                        emit_error(&app, "Límite de peticiones (429) — reintentando");
                    }
                    Err(e) => emit_error(&app, &e.user_message()),
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
                _ = poke_rx.recv() => {}
            }
        }
    });
}

fn spawn_anim_task(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut frame = 0usize;
        loop {
            let working = {
                let state = app.state::<AppState>();
                let anim = state.settings.lock().unwrap().enable_animation;
                let running = state.claude_running.load(Ordering::Relaxed);
                let active = state
                    .activity
                    .lock()
                    .unwrap()
                    .map(|t| t.elapsed() < Duration::from_millis(2500))
                    .unwrap_or(false);
                anim && running && active
            };

            let prev = app.state::<AppState>().working.swap(working, Ordering::Relaxed);
            if prev != working {
                let _ = app.emit("working-status", working);
            }

            if working {
                let img = {
                    let state = app.state::<AppState>();
                    let frames = state.frames.lock().unwrap();
                    if frames.is_empty() {
                        None
                    } else {
                        frame = (frame + 1) % frames.len();
                        Some(frames[frame].clone())
                    }
                };
                if let Some(img) = img {
                    set_tray_icon(&app, img);
                }
            } else if prev {
                let (color, percent) = {
                    let st = app.state::<AppState>();
                    let c = st.icon_color.lock().unwrap().clone();
                    let p = *st.icon_percent.lock().unwrap();
                    (c, p)
                };
                set_tray_icon(&app, icons::idle_icon(&color, percent));
                let _ = app.state::<AppState>().poke.send(());
            }

            let delay = if working { 80 } else { 400 };
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    });
}

// ---- Usage update plumbing --------------------------------------------------

fn on_usage_update(app: &AppHandle, data: UsageData) {
    let percent = data.max_percent();
    let color = icons::color_for_percent(percent).to_string();
    let tooltip = build_tooltip(&data);

    {
        let state = app.state::<AppState>();
        *state.usage.lock().unwrap() = data.clone();

        {
            let settings = state.settings.lock().unwrap().clone();
            state.notify_state.lock().unwrap().evaluate(app, &data, &settings);
        }

        let changed = {
            let mut cur_c = state.icon_color.lock().unwrap();
            let mut cur_p = state.icon_percent.lock().unwrap();
            if *cur_c != color || cur_p.round() as i64 != percent.round() as i64 {
                *cur_c = color.clone();
                *cur_p = percent;
                true
            } else {
                false
            }
        };
        if changed {
            *state.frames.lock().unwrap() = icons::working_frames(&color, percent);
            if !state.working.load(Ordering::Relaxed) {
                set_tray_icon(app, icons::idle_icon(&color, percent));
            }
        }
    }

    set_tray_tooltip(app, tooltip);
    let _ = app.emit("usage-updated", data);
}

fn emit_error(app: &AppHandle, msg: &str) {
    let data = {
        let state = app.state::<AppState>();
        let mut u = state.usage.lock().unwrap();
        u.error = Some(msg.to_string());
        u.clone()
    };
    let _ = app.emit("usage-updated", data);
}

fn build_tooltip(data: &UsageData) -> String {
    if data.limits.is_empty() {
        return "Claude Usage".to_string();
    }
    let parts: Vec<String> = data
        .limits
        .iter()
        .take(2)
        .map(|l| {
            let short = match l.kind.as_str() {
                "session" => "5h",
                "weekly_all" => "7d",
                _ => "wk",
            };
            format!("{} {:.0}%", short, l.percent)
        })
        .collect();
    format!("Claude · {}", parts.join(" · "))
}

/// Round the borderless window's corners (Windows 11) so the acrylic glass
/// doesn't show square edges / a white backing behind the rounded content.
#[cfg(target_os = "windows")]
fn round_window_corners(app: &AppHandle) {
    use std::ffi::c_void;
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, pv: *const c_void, cb: u32) -> i32;
    }
    if let Some(win) = app.get_webview_window("main") {
        if let Ok(hwnd) = win.hwnd() {
            // DWMWA_WINDOW_CORNER_PREFERENCE = 33, DWMWCP_ROUND = 2
            let pref: i32 = 2;
            unsafe {
                DwmSetWindowAttribute(
                    hwnd.0 as isize,
                    33,
                    &pref as *const i32 as *const c_void,
                    4,
                );
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn round_window_corners(_app: &AppHandle) {}

fn detect_version() -> String {
    if let Some(home) = dirs::home_dir() {
        let ext = home.join(".vscode").join("extensions");
        if let Ok(entries) = std::fs::read_dir(&ext) {
            let mut best: Option<String> = None;
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(rest) = name.strip_prefix("anthropic.claude-code-") {
                    let ver = rest.split('-').next().unwrap_or("").to_string();
                    if !ver.is_empty() {
                        best = Some(ver);
                    }
                }
            }
            if let Some(v) = best {
                return v;
            }
        }
    }
    CLAUDE_VERSION_FALLBACK.to_string()
}
