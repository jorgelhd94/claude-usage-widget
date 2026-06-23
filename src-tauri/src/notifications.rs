use std::collections::HashMap;

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

use crate::settings::Settings;
use crate::usage::UsageData;

/// Tracks the last notification level fired per window so we only alert on the
/// way *up* across a threshold, and re-arm automatically once usage drops.
#[derive(Default)]
pub struct NotifyState {
    fired: HashMap<String, u8>,
}

impl NotifyState {
    pub fn evaluate(&mut self, app: &AppHandle, usage: &UsageData, settings: &Settings) {
        if !settings.enable_notifications {
            // Keep levels in sync so toggling back on doesn't double-fire.
            for l in &usage.limits {
                self.fired.insert(l.kind.clone(), level_of(l.percent, settings));
            }
            return;
        }

        for l in &usage.limits {
            let level = level_of(l.percent, settings);
            let prev = self.fired.get(&l.kind).copied().unwrap_or(0);
            if level > prev {
                let (title, body) = match level {
                    2 => (
                        "Claude — límite casi alcanzado",
                        format!("{} está al {:.0}%", l.label, l.percent),
                    ),
                    _ => (
                        "Claude — uso elevado",
                        format!("{} está al {:.0}%", l.label, l.percent),
                    ),
                };
                let _ = app.notification().builder().title(title).body(body).show();
            }
            self.fired.insert(l.kind.clone(), level);
        }
    }
}

fn level_of(percent: f64, s: &Settings) -> u8 {
    if percent >= s.notify_critical {
        2
    } else if percent >= s.notify_high {
        1
    } else {
        0
    }
}
