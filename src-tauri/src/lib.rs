mod analytics;
mod commands;
mod credentials;
mod icons;
mod notifications;
mod settings;
mod usage;
mod watcher;

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::path::PathBuf;
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
use usage::{FetchError, Profile, UsageData, CLAUDE_VERSION_FALLBACK};
use watcher::{Activity, Presence};

/// Poll cadence while no Claude app is open. Usage can still move (claude.ai
/// in a browser, the phone app), just not fast enough to justify more.
const IDLE_POLL_SECS: u64 = 900;

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
    /// Which Claude apps are open, as `Presence` bits.
    pub presence: AtomicU8,
    /// Claude Code login state carried between polls (see `credentials`).
    pub auth: Arc<Mutex<credentials::AuthCache>>,
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
    /// Filesystem watchers, held only to keep them alive.
    pub watchers: Mutex<Vec<RecommendedWatcher>>,
    pub notify_state: Mutex<NotifyState>,
    pub poke: UnboundedSender<()>,
}

impl AppState {
    pub fn presence(&self) -> Presence {
        Presence::from_bits(self.presence.load(Ordering::Relaxed))
    }
}

/// Payload of the `claude-status` event.
#[derive(Clone, serde::Serialize)]
struct ClaudeStatus {
    running: bool,
    source: &'static str,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (poke_tx, poke_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    // Only the SessionStart hook launches with `--hooked`; those instances are
    // ephemeral (exit when Claude Code closes). Manual / autostart stay resident.
    let persistent = !std::env::args().any(|a| a == "--hooked");
    let by_hand = launched_by_hand(std::env::args());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A second launch (e.g. the SessionStart hook firing again) just
            // nudges the resident instance to refresh. If it was the user
            // opening the app, show them the popup: otherwise launching an
            // already-running tray app looks like nothing happened.
            let _ = app.state::<AppState>().poke.send(());
            if launched_by_hand(argv) {
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || show_popup(&handle));
            }
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
            commands::start_login,
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
                presence: AtomicU8::new(0),
                auth: Arc::new(Mutex::new(credentials::AuthCache::default())),
                working: AtomicBool::new(false),
                last_hidden_ms: AtomicI64::new(0),
                activity: activity.clone(),
                tray: Mutex::new(None),
                icon_color: Mutex::new(icons::NEUTRAL.to_string()),
                icon_percent: Mutex::new(0.0),
                frames: Mutex::new(icons::working_frames(icons::NEUTRAL, 0.0)),
                profile: Mutex::new(None),
                analytics: Mutex::new(None),
                watchers: Mutex::new(Vec::new()),
                notify_state: Mutex::new(NotifyState::default()),
                poke: poke_tx.clone(),
            });

            {
                let state = handle.state::<AppState>();
                let mut watchers = state.watchers.lock().unwrap();
                watchers.extend(watcher::start_activity_watcher(activity));
                watchers.extend(watcher::start_credentials_watcher(poke_tx.clone()));
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
            // stay hidden until a Claude app is detected.
            let _ = tray.set_visible(persistent);
            let _ = handle.state::<AppState>().tray.lock().unwrap().replace(tray);

            round_window_corners(&handle);

            spawn_process_watcher(handle.clone());
            spawn_poll_task(handle.clone(), poke_rx);
            spawn_anim_task(handle.clone());

            if by_hand {
                // Give the webview a moment to paint before revealing it.
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(700)).await;
                    let h = handle.clone();
                    let _ = handle.run_on_main_thread(move || show_popup(&h));
                });
            }

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
            show_popup(app);
        }
    }
}

fn show_popup(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        // Show first: a hidden window has no associated monitor, so
        // current_monitor() would be None and positioning a no-op.
        let _ = win.show();
        position_bottom_right(&win);
        let _ = win.set_focus();
    }
}

/// True when the user started the app themselves, as opposed to the
/// SessionStart hook (`--hooked`) or Windows autostart (`--autostarted`).
fn launched_by_hand<I: IntoIterator<Item = String>>(args: I) -> bool {
    !args
        .into_iter()
        .any(|a| a == "--hooked" || a == "--autostarted")
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
        let mut last: Option<Presence> = None;
        let mut misses: u32 = 0;
        loop {
            let now = watcher::claude_presence(&mut sys);
            let was_running = last.is_some_and(|p| p.any());
            {
                let state = app.state::<AppState>();
                state.presence.store(now.bits(), Ordering::Relaxed);
                if last != Some(now) {
                    let _ = app.emit(
                        "claude-status",
                        ClaudeStatus {
                            running: now.any(),
                            source: now.source(),
                        },
                    );
                }
                if now.any() {
                    misses = 0;
                    if !was_running {
                        set_tray_visible(&app, true);
                        let _ = state.poke.send(());
                    }
                } else if !state.persistent {
                    // Hook-launched instances only live while Claude is open.
                    if was_running {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.hide();
                        }
                        set_tray_visible(&app, false);
                    }
                    misses += 1;
                    if misses >= 5 {
                        app.exit(0);
                    }
                }
            }
            last = Some(now);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

fn spawn_poll_task(app: AppHandle, mut poke_rx: UnboundedReceiver<()>) {
    tauri::async_runtime::spawn(async move {
        let mut backoff_until: Option<Instant> = None;
        loop {
            let (running, persistent, interval, keep_alive, http, version, auth_cache) = {
                let state = app.state::<AppState>();
                let settings = state.settings.lock().unwrap();
                (
                    state.presence().any(),
                    state.persistent,
                    settings.poll_interval_secs.max(30),
                    settings.keep_session_alive,
                    state.http.clone(),
                    state.version.clone(),
                    state.auth.clone(),
                )
            };

            // The limits are shared by every Claude surface, so a resident
            // instance keeps polling with nothing open. The request needs only
            // the login on disk, not a running Claude Code.
            let active = running || persistent;
            let in_backoff = backoff_until.map(|t| Instant::now() < t).unwrap_or(false);

            if active && !in_backoff {
                let auth = usage::Auth {
                    cache: &auth_cache,
                    allow_refresh: keep_alive,
                };
                match usage::fetch_usage(&http, &version, &auth).await {
                    Ok(data) => {
                        backoff_until = None;
                        on_usage_update(&app, data);
                        let need_profile =
                            app.state::<AppState>().profile.lock().unwrap().is_none();
                        if need_profile {
                            if let Ok(p) = usage::fetch_profile(&http, &version, &auth).await {
                                *app.state::<AppState>().profile.lock().unwrap() = Some(p.clone());
                                let _ = app.emit("profile-updated", Some(p));
                            }
                        }
                    }
                    Err(FetchError::RateLimited(retry)) => {
                        let secs = retry.unwrap_or(60).clamp(30, 600);
                        backoff_until = Some(Instant::now() + Duration::from_secs(secs));
                        on_fetch_error(&app, &FetchError::RateLimited(retry));
                    }
                    Err(e) => on_fetch_error(&app, &e),
                }
            }

            let mut wait = if running {
                interval
            } else {
                interval.max(IDLE_POLL_SECS)
            };
            // Wake just after the next window resets so its bar drops promptly.
            if let Some(secs) = secs_until_next_reset(&app) {
                wait = wait.min((secs + 5).max(30));
            }

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                _ = poke_rx.recv() => {
                    // Pokes come in bursts (one file save is several fs events);
                    // let the burst, and the write behind it, finish.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    while poke_rx.try_recv().is_ok() {}
                }
            }
        }
    });
}

/// Seconds until the soonest upcoming window reset, if any is known.
fn secs_until_next_reset(app: &AppHandle) -> Option<u64> {
    let now = credentials::now_ms();
    let state = app.state::<AppState>();
    let usage = state.usage.lock().unwrap();
    usage
        .limits
        .iter()
        .filter_map(|l| l.resets_at.as_deref())
        .filter_map(|iso| chrono::DateTime::parse_from_rfc3339(iso).ok())
        .map(|t| t.timestamp_millis() - now)
        .filter(|ms| *ms > 0)
        .min()
        .map(|ms| (ms / 1000) as u64)
}

fn spawn_anim_task(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut frame = 0usize;
        loop {
            let working = {
                let state = app.state::<AppState>();
                let anim = state.settings.lock().unwrap().enable_animation;
                let running = state.presence().any();
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

fn on_fetch_error(app: &AppHandle, err: &FetchError) {
    let needs_login = err.needs_login();
    let state = app.state::<AppState>();
    let (data, newly_signed_out) = {
        let mut u = state.usage.lock().unwrap();
        let newly = needs_login && !u.needs_login;
        u.error = Some(err.user_message());
        u.needs_login = needs_login;
        if needs_login {
            // What we were showing belongs to a login that is gone.
            u.limits.clear();
        }
        (u.clone(), newly)
    };

    if newly_signed_out {
        *state.icon_color.lock().unwrap() = icons::SIGNED_OUT.to_string();
        *state.icon_percent.lock().unwrap() = 0.0;
        *state.frames.lock().unwrap() = icons::working_frames(icons::SIGNED_OUT, 0.0);
        if !state.working.load(Ordering::Relaxed) {
            set_tray_icon(app, icons::idle_icon(icons::SIGNED_OUT, 0.0));
        }
        set_tray_tooltip(app, "Claude Usage — inicia sesión en Claude Code".into());

        // The account may differ after signing back in; refetch it then.
        *state.profile.lock().unwrap() = None;
        let _ = app.emit("profile-updated", None::<Profile>);

        // Only worth a toast if it stopped working mid-run; at startup the
        // popup already says so.
        let notify = state.settings.lock().unwrap().enable_notifications;
        if notify && data.fetched_at_ms > 0 {
            use tauri_plugin_notification::NotificationExt;
            let _ = app
                .notification()
                .builder()
                .title("Claude Usage — sesión caducada")
                .body("Abre el widget para volver a iniciar sesión en Claude Code.")
                .show();
        }
    }

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

/// Parse a leading `major.minor.patch` out of a directory name.
fn parse_version(name: &str) -> Option<(u32, u32, u32)> {
    let mut parts = name.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch: String = parts.next()?.chars().take_while(|c| c.is_ascii_digit()).collect();
    Some((major, minor, patch.parse().ok()?))
}

fn subdirs(dir: PathBuf) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

/// Every Claude Code version installed on this machine, newest first, with
/// the directory it lives in: the native CLI install, the engines Claude
/// Desktop downloads for its Code sessions, and the VS Code extension.
pub(crate) fn installed_claude_code() -> Vec<((u32, u32, u32), PathBuf)> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local").join("share").join("claude").join("versions"));
    }
    if let Some(local) = dirs::data_local_dir() {
        // Store/MSIX Desktop: %LOCALAPPDATA%/Claude-Data/<package>/claude-code/<version>
        for pkg in subdirs(local.join("Claude-Data")) {
            roots.push(pkg.join("claude-code"));
        }
    }
    if let Some(roaming) = dirs::data_dir() {
        roots.push(roaming.join("Claude").join("claude-code"));
    }

    let mut found = Vec::new();
    for root in roots {
        if let Ok(rd) = std::fs::read_dir(&root) {
            for e in rd.flatten() {
                if let Some(v) = parse_version(&e.file_name().to_string_lossy()) {
                    found.push((v, e.path()));
                }
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        for ext in subdirs(home.join(".vscode").join("extensions")) {
            let name = ext.file_name().unwrap_or_default().to_string_lossy().to_string();
            if let Some(v) = name.strip_prefix("anthropic.claude-code-").and_then(parse_version) {
                found.push((v, ext));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found
}

/// Newest installed Claude Code version, for the User-Agent header.
fn detect_version() -> String {
    match installed_claude_code().first() {
        Some(((a, b, c), _)) => format!("{}.{}.{}", a, b, c),
        None => CLAUDE_VERSION_FALLBACK.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_directory_names() {
        assert_eq!(parse_version("2.1.317"), Some((2, 1, 317)));
        assert_eq!(parse_version("2.1.185-win32-x64"), Some((2, 1, 185)));
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version("2.1"), None);
        assert!(parse_version("2.1.317") > parse_version("2.1.99"));
    }

    #[test]
    fn hook_and_autostart_launches_are_not_by_hand() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(launched_by_hand(args(&["widget.exe"])));
        assert!(!launched_by_hand(args(&["widget.exe", "--hooked"])));
        assert!(!launched_by_hand(args(&["widget.exe", "--autostarted"])));
    }

    /// Not a pass/fail test: prints what the widget sees on this machine, for
    /// when "it shows nothing" needs explaining. Makes the real usage request
    /// but never renews the token, and prints no secrets.
    /// `cargo test live_diagnostics -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_diagnostics() {
        let presence = watcher::claude_presence(&mut System::new());
        println!("presence: {:?} -> source {:?}", presence, presence.source());
        let version = detect_version();
        println!("version for the User-Agent: {}", version);
        match credentials::read_credentials() {
            Ok(c) => println!(
                "login on disk: expires_at_ms={} refreshable={}",
                c.expires_at_ms,
                c.can_refresh(credentials::now_ms())
            ),
            Err(e) => println!("login on disk: {}", e),
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        let http = reqwest::Client::new();
        let cache = Mutex::new(credentials::AuthCache::default());
        let auth = usage::Auth {
            cache: &cache,
            allow_refresh: false,
        };
        match rt.block_on(usage::fetch_usage(&http, &version, &auth)) {
            Ok(u) => println!("usage: {} limits, highest {:.0}%", u.limits.len(), u.max_percent()),
            Err(e) => println!(
                "usage: {:?} -> needs_login={} message={:?}",
                e,
                e.needs_login(),
                e.user_message()
            ),
        }
    }
}
