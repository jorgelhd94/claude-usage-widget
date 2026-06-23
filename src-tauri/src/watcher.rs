use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sysinfo::{ProcessesToUpdate, System};

/// Shared "last filesystem activity in ~/.claude/projects" timestamp.
pub type Activity = Arc<Mutex<Option<Instant>>>;

/// True if at least one Claude Code CLI process (`claude.exe`) is running.
/// Excludes the Claude Desktop MSIX app (we only care about Claude Code).
pub fn claude_running(sys: &mut System) -> bool {
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes().values().any(|p| {
        let name = p.name().to_string_lossy().to_lowercase();
        if name != "claude.exe" && name != "claude" {
            return false;
        }
        let path = p
            .exe()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        // Desktop app lives under WindowsApps / package family "Claude_pzs8...".
        !path.contains("windowsapps") && !path.contains("claude_pzs8")
    })
}

fn projects_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("projects"));
    }
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Start a recursive filesystem watcher over `~/.claude/projects`. Every
/// create/modify event (a transcript being appended to) bumps `activity`.
/// The returned watcher must be kept alive for the watch to stay active.
pub fn start_activity_watcher(activity: Activity) -> Option<RecommendedWatcher> {
    let dir = projects_dir()?;
    if !dir.exists() {
        return None;
    }
    let act = activity.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                if let Ok(mut g) = act.lock() {
                    *g = Some(Instant::now());
                }
            }
        }
    })
    .ok()?;
    watcher.watch(&dir, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}
