use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

/// OAuth token endpoint Claude Code itself uses to renew its login.
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Claude Code's public OAuth client id. The token in `.credentials.json` was
/// issued to this client, so it is the only one that can renew it.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Treat a token as expired slightly early so a request never races the clock.
const EXPIRY_SKEW_MS: i64 = 30_000;

/// Epoch-ms of our last write to the credentials file, so the file watcher can
/// tell our own write-back apart from Claude Code (or a fresh login) changing it.
static LAST_SELF_WRITE_MS: AtomicI64 = AtomicI64::new(0);

pub fn wrote_recently() -> bool {
    now_ms() - LAST_SELF_WRITE_MS.load(Ordering::Relaxed) < 3_000
}

/// Parsed OAuth credentials from `~/.claude/.credentials.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Epoch-ms; 0 when the file doesn't say (try the token and see).
    pub expires_at_ms: i64,
    /// Epoch-ms; 0 when unknown.
    pub refresh_expires_at_ms: i64,
    pub scopes: Vec<String>,
    pub subscription_type: Option<String>,
}

impl Credentials {
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at_ms > 0 && self.expires_at_ms <= now + EXPIRY_SKEW_MS
    }

    pub fn can_refresh(&self, now: i64) -> bool {
        self.refresh_token.as_deref().is_some_and(|t| !t.is_empty())
            && (self.refresh_expires_at_ms == 0 || self.refresh_expires_at_ms > now)
    }
}

#[derive(Debug)]
pub enum AuthError {
    /// No credentials file / no Claude login in it.
    NoCredentials(String),
    /// The login is dead (expired or rejected) and can't be renewed from here.
    NeedsLogin,
    /// Expired, and the user turned off renewing it from the widget.
    Expired,
    /// Renewal failed for a reason that should clear up by itself (network…).
    Transient(String),
}

/// What this process remembers about the login between polls.
#[derive(Default)]
pub struct AuthCache {
    /// Newest credentials this process obtained by renewing the token. Kept so a
    /// stale write to the file by another process can't strand us on a
    /// refresh token that has already been rotated out.
    refreshed: Option<Credentials>,
    /// Access token the API rejected and we couldn't renew. Not retried until
    /// the file holds a different one, so a dead login costs no requests.
    dead_token: Option<String>,
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

fn as_ms(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
        .max(0)
}

pub fn parse_credentials(content: &str) -> Result<Credentials, String> {
    let v: Value = serde_json::from_str(content)
        .map_err(|e| format!("Credenciales con JSON inválido: {}", e))?;

    let oauth = v
        .get("claudeAiOauth")
        .filter(|o| o.is_object())
        .ok_or_else(|| "No hay sesión de Claude Code iniciada".to_string())?;

    let access_token = oauth
        .get("accessToken")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "No hay sesión de Claude Code iniciada".to_string())?
        .to_string();

    let str_field = |k: &str| {
        oauth
            .get(k)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };

    let scopes = oauth
        .get("scopes")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(Credentials {
        access_token,
        refresh_token: str_field("refreshToken"),
        expires_at_ms: as_ms(oauth.get("expiresAt")),
        refresh_expires_at_ms: as_ms(oauth.get("refreshTokenExpiresAt")),
        scopes,
        subscription_type: str_field("subscriptionType"),
    })
}

/// Read and parse the credentials file. Re-read on every poll: Claude Code
/// rewrites it whenever it renews the token or the user logs in again.
pub fn read_credentials() -> Result<Credentials, String> {
    let path = credentials_path().ok_or_else(|| "No se encontró el directorio home".to_string())?;
    let content = std::fs::read_to_string(&path)
        .map_err(|_| "No hay sesión de Claude Code iniciada".to_string())?;
    parse_credentials(&content)
}

/// Fold renewed tokens into the existing file content, touching only the
/// token fields: everything else Claude Code keeps there (`mcpOAuth`,
/// `subscriptionType`, `rateLimitTier`…) is preserved.
pub fn merge_oauth(existing: &str, new: &Credentials) -> String {
    let mut root = serde_json::from_str::<Value>(existing)
        .ok()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    let obj = root.as_object_mut().unwrap();
    let oauth = obj.entry("claudeAiOauth").or_insert_with(|| json!({}));
    if !oauth.is_object() {
        *oauth = json!({});
    }
    let o = oauth.as_object_mut().unwrap();
    o.insert("accessToken".into(), json!(new.access_token));
    o.insert("refreshToken".into(), json!(new.refresh_token));
    o.insert("expiresAt".into(), json!(new.expires_at_ms));
    if new.refresh_expires_at_ms > 0 {
        o.insert("refreshTokenExpiresAt".into(), json!(new.refresh_expires_at_ms));
    } else {
        // Unknown for the new token: a leftover value would describe the old one.
        o.remove("refreshTokenExpiresAt");
    }
    if !new.scopes.is_empty() {
        o.insert("scopes".into(), json!(new.scopes));
    }
    serde_json::to_string(&root).unwrap_or_else(|_| existing.to_string())
}

/// Persist renewed tokens where Claude Code reads them. The refresh token is
/// single-use, so not writing the new one back would log the CLI out.
fn write_back(new: &Credentials) -> Result<(), String> {
    let path = credentials_path().ok_or_else(|| "sin directorio home".to_string())?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = merge_oauth(&existing, new);
    // Write-then-rename so a reader never sees a half-written file.
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, merged).map_err(|e| e.to_string())?;
    LAST_SELF_WRITE_MS.store(now_ms(), Ordering::Relaxed);
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token_expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

enum RefreshError {
    /// The server refused this refresh token: it's spent, revoked or expired.
    Rejected,
    Transient(String),
}

fn apply_refresh(old: &Credentials, resp: TokenResponse, now: i64) -> Credentials {
    Credentials {
        access_token: resp.access_token,
        refresh_token: resp
            .refresh_token
            .filter(|t| !t.is_empty())
            .or_else(|| old.refresh_token.clone()),
        // Claude Code's tokens last hours; assume one if the server doesn't say.
        expires_at_ms: now + resp.expires_in.unwrap_or(3600).max(60) * 1000,
        refresh_expires_at_ms: resp
            .refresh_token_expires_in
            .filter(|s| *s > 0)
            .map(|s| now + s * 1000)
            .unwrap_or(0),
        scopes: resp
            .scope
            .map(|s| s.split_whitespace().map(|x| x.to_string()).collect::<Vec<_>>())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| old.scopes.clone()),
        subscription_type: old.subscription_type.clone(),
    }
}

async fn refresh(http: &reqwest::Client, cur: &Credentials) -> Result<Credentials, RefreshError> {
    let refresh_token = cur.refresh_token.clone().unwrap_or_default();
    let mut with_scope = !cur.scopes.is_empty();
    loop {
        let mut body = json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        });
        if with_scope {
            body["scope"] = json!(cur.scopes.join(" "));
        }
        let resp = http
            .post(TOKEN_URL)
            .json(&body)
            .send()
            .await
            .map_err(|e| RefreshError::Transient(e.to_string()))?;
        let status = resp.status().as_u16();
        if status == 200 {
            let parsed: TokenResponse = resp
                .json()
                .await
                .map_err(|e| RefreshError::Transient(e.to_string()))?;
            return Ok(apply_refresh(cur, parsed, now_ms()));
        }
        if matches!(status, 400 | 401 | 403) {
            let text = resp.text().await.unwrap_or_default();
            // The granted scopes can shrink server-side; ask again without
            // naming any and take whatever the token still carries.
            if with_scope && text.contains("invalid_scope") {
                with_scope = false;
                continue;
            }
            return Err(RefreshError::Rejected);
        }
        return Err(RefreshError::Transient(format!("HTTP {}", status)));
    }
}

/// Produce credentials to make a request with, renewing the token if needed.
///
/// `rejected` is an access token the API just answered 401 to: it forces a
/// renewal even if the file claims the token is still good.
///
/// The token is only renewed once it has actually expired. Claude Code renews
/// its own a few minutes ahead of that, so while it is in use it always gets
/// there first and the two never race for the single-use refresh token.
pub async fn resolve(
    http: &reqwest::Client,
    cache: &Mutex<AuthCache>,
    allow_refresh: bool,
    rejected: Option<&str>,
) -> Result<Credentials, AuthError> {
    let now = now_ms();
    let file = match read_credentials() {
        Ok(c) => c,
        Err(e) => {
            // Logged out: forget what we held rather than resurrect it.
            *cache.lock().unwrap() = AuthCache::default();
            return Err(AuthError::NoCredentials(e));
        }
    };

    let mut cur = file.clone();
    let mut heal = false;
    {
        let mut c = cache.lock().unwrap();
        if let Some(mem) = c.refreshed.clone() {
            if mem.access_token != file.access_token {
                if mem.expires_at_ms > file.expires_at_ms {
                    // The file went back to an older token than the one we
                    // renewed (another process wrote a stale copy over it).
                    cur = mem;
                    heal = true;
                } else {
                    c.refreshed = None;
                }
            }
        }
        if c.dead_token.as_deref() == Some(cur.access_token.as_str()) {
            return Err(AuthError::NeedsLogin);
        }
        c.dead_token = None;
    }
    if heal {
        let _ = write_back(&cur);
    }

    let was_rejected = rejected == Some(cur.access_token.as_str());
    if !was_rejected && !cur.is_expired(now) {
        return Ok(cur);
    }

    let give_up = |err: AuthError| {
        if was_rejected {
            cache.lock().unwrap().dead_token = Some(cur.access_token.clone());
        }
        Err(err)
    };
    if !cur.can_refresh(now) {
        return give_up(AuthError::NeedsLogin);
    }
    if !allow_refresh {
        return give_up(AuthError::Expired);
    }

    match refresh(http, &cur).await {
        Ok(new) => {
            let _ = write_back(&new);
            cache.lock().unwrap().refreshed = Some(new.clone());
            Ok(new)
        }
        Err(RefreshError::Transient(e)) => Err(AuthError::Transient(e)),
        Err(RefreshError::Rejected) => {
            // Claude Code may have renewed at the same instant and spent the
            // refresh token first; if so its result is about to land on disk.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if let Ok(f) = read_credentials() {
                if f.access_token != cur.access_token && !f.is_expired(now_ms()) {
                    return Ok(f);
                }
            }
            let mut c = cache.lock().unwrap();
            c.refreshed = None;
            c.dead_token = Some(cur.access_token.clone());
            Err(AuthError::NeedsLogin)
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(token: &str, expires: i64) -> Credentials {
        Credentials {
            access_token: token.into(),
            refresh_token: Some("r1".into()),
            expires_at_ms: expires,
            refresh_expires_at_ms: 0,
            scopes: vec!["user:profile".into()],
            subscription_type: Some("max".into()),
        }
    }

    #[test]
    fn parses_a_normal_login() {
        let c = parse_credentials(
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":5000.0,
                "refreshTokenExpiresAt":9000,"scopes":["x","y"],"subscriptionType":"max"}}"#,
        )
        .unwrap();
        assert_eq!(c.access_token, "a");
        assert_eq!(c.refresh_token.as_deref(), Some("r"));
        assert_eq!(c.expires_at_ms, 5000);
        assert_eq!(c.refresh_expires_at_ms, 9000);
        assert_eq!(c.scopes, vec!["x", "y"]);
        assert!(c.is_expired(10_000));
        assert!(!c.can_refresh(10_000));
        assert!(c.can_refresh(8_000));
    }

    /// The state Claude Desktop-only users end up in: the CLI login lapsed and
    /// was left with no refresh token and `expiresAt: 0`.
    #[test]
    fn lapsed_login_is_not_refreshable() {
        let c = parse_credentials(
            r#"{"mcpOAuth":{},"claudeAiOauth":{"accessToken":"a","refreshToken":"",
                "expiresAt":0,"refreshTokenExpiresAt":1793367264769}}"#,
        )
        .unwrap();
        assert_eq!(c.refresh_token, None);
        assert!(!c.is_expired(1_800_000_000_000), "0 means unknown, not expired");
        assert!(!c.can_refresh(1_800_000_000_000));
    }

    #[test]
    fn missing_login_is_an_error() {
        assert!(parse_credentials(r#"{"mcpOAuth":{}}"#).is_err());
        assert!(parse_credentials(r#"{"claudeAiOauth":{"accessToken":""}}"#).is_err());
        assert!(parse_credentials("not json").is_err());
    }

    #[test]
    fn merge_keeps_everything_but_the_tokens() {
        let existing = r#"{"mcpOAuth":{"srv":{"t":1}},"claudeAiOauth":{"accessToken":"old",
            "refreshToken":"oldr","expiresAt":1,"refreshTokenExpiresAt":2,
            "scopes":["a"],"subscriptionType":"max","rateLimitTier":"default_claude_max_5x"}}"#;
        let mut new = creds("new", 777);
        new.refresh_token = Some("newr".into());
        let v: Value = serde_json::from_str(&merge_oauth(existing, &new)).unwrap();
        assert_eq!(v["mcpOAuth"]["srv"]["t"], 1);
        let o = &v["claudeAiOauth"];
        assert_eq!(o["accessToken"], "new");
        assert_eq!(o["refreshToken"], "newr");
        assert_eq!(o["expiresAt"], 777);
        assert!(o.get("refreshTokenExpiresAt").is_none(), "stale expiry dropped");
        assert_eq!(o["scopes"][0], "user:profile");
        assert_eq!(o["subscriptionType"], "max");
        assert_eq!(o["rateLimitTier"], "default_claude_max_5x");
        // The merged file must read back as the credentials we wrote.
        let back = parse_credentials(&merge_oauth(existing, &new)).unwrap();
        assert_eq!(back, new);
    }

    #[test]
    fn refresh_response_keeps_the_old_refresh_token_when_not_rotated() {
        let old = creds("a", 1);
        let resp: TokenResponse =
            serde_json::from_str(r#"{"access_token":"b","expires_in":28800}"#).unwrap();
        let new = apply_refresh(&old, resp, 1_000);
        assert_eq!(new.access_token, "b");
        assert_eq!(new.refresh_token.as_deref(), Some("r1"));
        assert_eq!(new.expires_at_ms, 1_000 + 28_800_000);
        assert_eq!(new.scopes, old.scopes);

        let resp: TokenResponse = serde_json::from_str(
            r#"{"access_token":"c","refresh_token":"r2","expires_in":60,
                "refresh_token_expires_in":120,"scope":"s1 s2"}"#,
        )
        .unwrap();
        let new = apply_refresh(&old, resp, 1_000);
        assert_eq!(new.refresh_token.as_deref(), Some("r2"));
        assert_eq!(new.refresh_expires_at_ms, 121_000);
        assert_eq!(new.scopes, vec!["s1", "s2"]);
    }
}
