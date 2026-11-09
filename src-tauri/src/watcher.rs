use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::sync::mpsc::UnboundedSender;

/// Shared "last filesystem activity in ~/.claude/projects" timestamp.
pub type Activity = Arc<Mutex<Option<Instant>>>;

/// Which Claude apps are open right now. They all draw on the same usage
/// limits, so any of them counts as "Claude is running".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Presence {
    /// A Claude Code engine: the CLI, an IDE extension's, or one of the
    /// sessions the Claude Desktop app runs.
    pub code: bool,
    /// The Claude Desktop app itself.
    pub desktop: bool,
}

impl Presence {
    const CODE: u8 = 1;
    const DESKTOP: u8 = 2;

    pub fn any(self) -> bool {
        self.code || self.desktop
    }

    pub fn bits(self) -> u8 {
        (if self.code { Self::CODE } else { 0 }) | (if self.desktop { Self::DESKTOP } else { 0 })
    }

    pub fn from_bits(b: u8) -> Self {
        Self {
            code: b & Self::CODE != 0,
            desktop: b & Self::DESKTOP != 0,
        }
    }

    /// What the UI names as the active app: "code", "desktop" or "".
    pub fn source(self) -> &'static str {
        if self.code {
            "code"
        } else if self.desktop {
            "desktop"
        } else {
            ""
        }
    }
}

/// Classify one process by name and executable path (both any case).
///
/// The Desktop app and Claude Code are both `claude.exe`; what tells them
/// apart is where they live. The Desktop app runs from its install directory
/// (`WindowsApps\Claude_…` for the Store/MSIX build, `AnthropicClaude\app-…`
/// for the classic installer). Everything else is a Claude Code engine —
/// including the ones Desktop spawns for its Code sessions, which sit under
/// its *data* directory (`Claude-Data\Claude_pzs8…\claude-code\<version>\`).
pub fn classify(name: &str, exe_path: &str) -> Presence {
    let name = name.to_lowercase();
    if name != "claude.exe" && name != "claude" {
        return Presence::default();
    }
    let path = exe_path.to_lowercase().replace('/', "\\");
    let desktop = path.contains("\\windowsapps\\")
        || path.contains("\\anthropicclaude\\")
        || path.contains("\\claude.app\\");
    Presence {
        code: !desktop,
        desktop,
    }
}

pub fn claude_presence(sys: &mut System) -> Presence {
    // Only names and exe paths are needed; skipping CPU/memory/disk stats keeps
    // this cheap enough to run every couple of seconds.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    let mut found = Presence::default();
    for p in sys.processes().values() {
        let path = p
            .exe()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let one = classify(&p.name().to_string_lossy(), &path);
        found.code |= one.code;
        found.desktop |= one.desktop;
        if found.code && found.desktop {
            break;
        }
    }
    found
}

fn config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// Start a recursive filesystem watcher over `~/.claude/projects`. Every
/// create/modify event (a transcript being appended to) bumps `activity`.
/// The returned watcher must be kept alive for the watch to stay active.
pub fn start_activity_watcher(activity: Activity) -> Option<RecommendedWatcher> {
    let dir = config_dir()?.join("projects");
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

/// Watch the credentials file and nudge the poll loop when someone else
/// changes it (a fresh login, or Claude Code renewing the token), so the
/// widget recovers the moment the user signs in instead of at the next poll.
pub fn start_credentials_watcher(poke: UnboundedSender<()>) -> Option<RecommendedWatcher> {
    let file = crate::credentials::credentials_path()?;
    let dir = file.parent()?.to_path_buf();
    if !dir.exists() {
        return None;
    }
    let name = file.file_name()?.to_os_string();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if !matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) {
            return;
        }
        let ours = ev.paths.iter().any(|p| p.file_name() == Some(name.as_os_str()));
        if ours && !crate::credentials::wrote_recently() {
            let _ = poke.send(());
        }
    })
    .ok()?;
    watcher.watch(&dir, RecursiveMode::NonRecursive).ok()?;
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_app_shell_is_desktop() {
        let p = classify(
            "claude.exe",
            r"C:\Program Files\WindowsApps\Claude_4.23118.0.0_x64__pzs8sxrjxfjjc\app\Claude.exe",
        );
        assert_eq!(p, Presence { code: false, desktop: true });
        let p = classify(
            "Claude.exe",
            r"C:\Users\me\AppData\Local\AnthropicClaude\app-1.2.3\claude.exe",
        );
        assert_eq!(p, Presence { code: false, desktop: true });
    }

    /// The bug this guards: Desktop's Code sessions live under a directory
    /// named after the Desktop package, and used to be mistaken for the app.
    #[test]
    fn code_session_spawned_by_desktop_is_code() {
        let p = classify(
            "claude.exe",
            r"C:\Users\me\AppData\Local\Claude-Data\claude_pzs8sxrjxfjjc\claude-code\2.1.317\a33aa5518497\claude.exe",
        );
        assert_eq!(p, Presence { code: true, desktop: false });
    }

    #[test]
    fn cli_is_code_and_other_processes_are_nothing() {
        assert!(classify("claude.exe", r"C:\Users\me\.local\bin\claude.exe").code);
        assert!(classify("claude", "/home/me/.local/bin/claude").code);
        assert!(classify("claude.exe", "").code, "unreadable path still counts");
        assert!(!classify("claude-usage-widget.exe", r"C:\x\claude-usage-widget.exe").any());
        assert!(!classify("chrome-native-host.exe", r"C:\x\Claude_pzs8\host.exe").any());
    }

    #[test]
    fn presence_round_trips_through_bits() {
        for (code, desktop) in [(false, false), (true, false), (false, true), (true, true)] {
            let p = Presence { code, desktop };
            assert_eq!(Presence::from_bits(p.bits()), p);
        }
        assert_eq!(Presence { code: true, desktop: true }.source(), "code");
        assert_eq!(Presence { code: false, desktop: true }.source(), "desktop");
    }
}
