use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Parsed OAuth credentials from `~/.claude/.credentials.json`.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_token: String,
    pub subscription_type: Option<String>,
}

/// Resolve the path to Claude Code's credentials file.
/// Honors `CLAUDE_CONFIG_DIR`, otherwise falls back to `~/.claude`.
pub fn credentials_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }
    let home = dirs::home_dir()?;
    Some(home.join(".claude").join(".credentials.json"))
}

/// Read and parse the credentials file. Re-read on every poll: Claude Code
/// keeps the access token fresh in the background while it is running.
pub fn read_credentials() -> Result<Credentials, String> {
    let path = credentials_path().ok_or_else(|| "No se encontró el directorio home".to_string())?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("No se pudo leer {}: {}", path.display(), e))?;
    let v: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Credenciales con JSON inválido: {}", e))?;

    let oauth = v
        .get("claudeAiOauth")
        .ok_or_else(|| "Falta claudeAiOauth en las credenciales".to_string())?;

    let access_token = oauth
        .get("accessToken")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "Falta accessToken".to_string())?
        .to_string();

    let subscription_type = oauth
        .get("subscriptionType")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    Ok(Credentials {
        access_token,
        subscription_type,
    })
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
