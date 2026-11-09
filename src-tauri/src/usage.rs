use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::credentials::{self, now_ms, AuthCache, AuthError, Credentials};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Fallback Claude Code version for the User-Agent header (critical to avoid
/// aggressive 429s). Overridden at runtime when the real version is detected.
pub const CLAUDE_VERSION_FALLBACK: &str = "2.1.317";

/// A single usage window, normalized for the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct LimitInfo {
    pub kind: String,
    pub label: String,
    pub percent: f64,
    pub severity: String,
    pub resets_at: Option<String>,
    pub is_active: bool,
    /// Length of the window in seconds (5h / 7d) when the kind tells us, so
    /// the UI can show how far through it we are.
    pub window_secs: Option<u64>,
}

/// Everything the UI and tray need from one usage fetch.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UsageData {
    pub plan: String,
    pub limits: Vec<LimitInfo>,
    pub extra_usage_enabled: bool,
    pub extra_used_credits: Option<f64>,
    pub extra_monthly_limit: Option<f64>,
    pub fetched_at_ms: i64,
    pub error: Option<String>,
    /// The Claude Code login is missing or dead: only signing in again fixes it.
    pub needs_login: bool,
}

impl UsageData {
    /// Highest utilization across all windows — drives the tray icon color.
    pub fn max_percent(&self) -> f64 {
        self.limits
            .iter()
            .map(|l| l.percent)
            .fold(0.0_f64, f64::max)
    }
}

#[derive(Debug)]
pub enum FetchError {
    Auth(AuthError),
    RateLimited(Option<u64>),
    Http(u16),
    Network(String),
    Parse(String),
}

impl FetchError {
    pub fn user_message(&self) -> String {
        match self {
            FetchError::Auth(AuthError::NoCredentials(e)) => e.clone(),
            FetchError::Auth(AuthError::NeedsLogin) => {
                "La sesión de Claude Code ha caducado".to_string()
            }
            FetchError::Auth(AuthError::Expired) => {
                "Token caducado — abre Claude Code o activa «Mantener la sesión activa»".to_string()
            }
            FetchError::Auth(AuthError::Transient(e)) => {
                format!("No se pudo renovar la sesión: {}", e)
            }
            FetchError::RateLimited(_) => "Límite de peticiones (429) — reintentando".to_string(),
            FetchError::Http(c) => format!("Error HTTP {}", c),
            FetchError::Network(e) => format!("Red: {}", e),
            FetchError::Parse(e) => format!("Respuesta inesperada: {}", e),
        }
    }

    /// True when nothing but the user signing in again will fix it.
    pub fn needs_login(&self) -> bool {
        matches!(
            self,
            FetchError::Auth(AuthError::NoCredentials(_) | AuthError::NeedsLogin)
        )
    }
}

// ---- Raw API deserialization ------------------------------------------------

#[derive(Deserialize)]
struct RawModel {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct RawScope {
    #[serde(default)]
    model: Option<RawModel>,
}

#[derive(Deserialize)]
struct RawLimit {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    percent: Option<f64>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    scope: Option<RawScope>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct RawExtra {
    #[serde(default)]
    is_enabled: Option<bool>,
    #[serde(default)]
    used_credits: Option<f64>,
    #[serde(default)]
    monthly_limit: Option<f64>,
}

/// One window in the older response shape (`five_hour`, `seven_day`, …).
#[derive(Deserialize)]
struct RawWindow {
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    limits: Vec<RawLimit>,
    #[serde(default)]
    extra_usage: Option<RawExtra>,
    #[serde(default)]
    five_hour: Option<RawWindow>,
    #[serde(default)]
    seven_day: Option<RawWindow>,
    #[serde(default)]
    seven_day_opus: Option<RawWindow>,
    #[serde(default)]
    seven_day_sonnet: Option<RawWindow>,
}

fn label_for(kind: &str, model: Option<&str>) -> String {
    match kind {
        "session" => "Sesión (5h)".to_string(),
        "weekly_all" => "Semanal (7d)".to_string(),
        "weekly_scoped" => match model {
            Some(m) => format!("Semanal · {}", m),
            None => "Semanal (modelo)".to_string(),
        },
        other => other.to_string(),
    }
}

fn window_secs_for(kind: &str) -> Option<u64> {
    match kind {
        "session" => Some(5 * 3600),
        k if k.starts_with("weekly") => Some(7 * 86400),
        _ => None,
    }
}

fn plan_label(sub: Option<&str>) -> String {
    match sub {
        Some("max") => "Claude Max".to_string(),
        Some("pro") => "Claude Pro".to_string(),
        Some("team") => "Claude Team".to_string(),
        Some("enterprise") => "Claude Enterprise".to_string(),
        Some(other) => format!("Claude {}", other),
        None => "Claude".to_string(),
    }
}

fn severity_for(percent: f64) -> &'static str {
    if percent >= 90.0 {
        "critical"
    } else if percent >= 70.0 {
        "warning"
    } else {
        "normal"
    }
}

/// Normalize the usage response. The endpoint is undocumented and has shipped
/// two shapes: a `limits` array (current) and one object per window (older);
/// the older one is read only when the array is absent.
fn parse_usage(body: &str, plan: String) -> Result<UsageData, String> {
    let raw: RawUsage = serde_json::from_str(body).map_err(|e| e.to_string())?;

    let mut limits = Vec::new();
    for l in raw.limits {
        let kind = l.kind.unwrap_or_default();
        let model = l
            .scope
            .as_ref()
            .and_then(|s| s.model.as_ref())
            .and_then(|m| m.display_name.as_deref());
        limits.push(LimitInfo {
            label: label_for(&kind, model),
            window_secs: window_secs_for(&kind),
            kind,
            percent: l.percent.unwrap_or(0.0),
            severity: l.severity.unwrap_or_else(|| "normal".to_string()),
            resets_at: l.resets_at,
            is_active: l.is_active.unwrap_or(false),
        });
    }

    if limits.is_empty() {
        let legacy = [
            ("session", None, raw.five_hour),
            ("weekly_all", None, raw.seven_day),
            ("weekly_scoped", Some("Opus"), raw.seven_day_opus),
            ("weekly_scoped", Some("Sonnet"), raw.seven_day_sonnet),
        ];
        for (kind, model, window) in legacy {
            let Some(w) = window else { continue };
            // Per-model windows the plan doesn't have come back without a value.
            let Some(percent) = w.utilization else { continue };
            limits.push(LimitInfo {
                label: label_for(kind, model),
                window_secs: window_secs_for(kind),
                kind: kind.to_string(),
                percent,
                severity: severity_for(percent).to_string(),
                resets_at: w.resets_at,
                is_active: false,
            });
        }
    }

    let (extra_enabled, used, monthly) = match raw.extra_usage {
        Some(e) => (e.is_enabled.unwrap_or(false), e.used_credits, e.monthly_limit),
        None => (false, None, None),
    };

    Ok(UsageData {
        plan,
        limits,
        extra_usage_enabled: extra_enabled,
        extra_used_credits: used,
        extra_monthly_limit: monthly,
        fetched_at_ms: now_ms(),
        error: None,
        needs_login: false,
    })
}

/// How a request authenticates: the shared login cache plus whether the user
/// lets the widget renew the token itself.
pub struct Auth<'a> {
    pub cache: &'a Mutex<AuthCache>,
    pub allow_refresh: bool,
}

/// GET an OAuth endpoint with Claude Code's login. A 401 gets one more try
/// with a renewed token (or the one Claude Code just wrote to disk).
async fn authorized_get(
    client: &reqwest::Client,
    url: &str,
    version: &str,
    auth: &Auth<'_>,
) -> Result<(reqwest::Response, Credentials), FetchError> {
    let mut creds = credentials::resolve(client, auth.cache, auth.allow_refresh, None)
        .await
        .map_err(FetchError::Auth)?;
    let mut retried = false;
    loop {
        let resp = client
            .get(url)
            .header("Authorization", format!("Bearer {}", creds.access_token))
            .header("Content-Type", "application/json")
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", format!("claude-code/{}", version))
            .send()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;

        match resp.status().as_u16() {
            401 if !retried => {
                retried = true;
                creds = credentials::resolve(
                    client,
                    auth.cache,
                    auth.allow_refresh,
                    Some(&creds.access_token),
                )
                .await
                .map_err(FetchError::Auth)?;
            }
            401 => return Err(FetchError::Auth(AuthError::NeedsLogin)),
            429 => {
                let retry = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok());
                return Err(FetchError::RateLimited(retry));
            }
            code if !resp.status().is_success() => return Err(FetchError::Http(code)),
            _ => return Ok((resp, creds)),
        }
    }
}

/// Fetch and normalize the subscription usage from the OAuth endpoint.
pub async fn fetch_usage(
    client: &reqwest::Client,
    version: &str,
    auth: &Auth<'_>,
) -> Result<UsageData, FetchError> {
    let (resp, creds) = authorized_get(client, USAGE_URL, version, auth).await?;
    let body = resp.text().await.map_err(|e| FetchError::Parse(e.to_string()))?;
    parse_usage(&body, plan_label(creds.subscription_type.as_deref())).map_err(FetchError::Parse)
}

// ---- Account profile --------------------------------------------------------

/// Account details shown in the header / account section (mirrors Claude Code's
/// `/usage` ACCOUNT block).
#[derive(Debug, Clone, Serialize, Default)]
pub struct Profile {
    pub email: String,
    pub name: String,
    pub plan: String,
    pub tier: String,
    pub organization: String,
    pub active: bool,
}

#[derive(Deserialize, Default)]
struct RawAccount {
    #[serde(default)]
    full_name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    has_claude_max: Option<bool>,
    #[serde(default)]
    has_claude_pro: Option<bool>,
}

#[derive(Deserialize)]
struct RawOrg {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    rate_limit_tier: Option<String>,
    #[serde(default)]
    subscription_status: Option<String>,
}

#[derive(Deserialize)]
struct RawProfile {
    #[serde(default)]
    account: Option<RawAccount>,
    #[serde(default)]
    organization: Option<RawOrg>,
}

/// Turn `default_claude_max_5x` into `5x` (the multiplier suffix), else "".
fn tier_label(tier: Option<&str>) -> String {
    match tier {
        Some(t) => {
            let last = t.rsplit('_').next().unwrap_or("");
            if last.ends_with('x') && last.len() <= 4 {
                last.to_string()
            } else {
                String::new()
            }
        }
        None => String::new(),
    }
}

pub async fn fetch_profile(
    client: &reqwest::Client,
    version: &str,
    auth: &Auth<'_>,
) -> Result<Profile, FetchError> {
    let (resp, _) = authorized_get(client, PROFILE_URL, version, auth).await?;
    let raw: RawProfile = resp.json().await.map_err(|e| FetchError::Parse(e.to_string()))?;

    let acc = raw.account.unwrap_or_default();
    let org = raw.organization;

    let plan = if acc.has_claude_max.unwrap_or(false) {
        "Claude Max".to_string()
    } else if acc.has_claude_pro.unwrap_or(false) {
        "Claude Pro".to_string()
    } else {
        "Claude".to_string()
    };

    Ok(Profile {
        email: acc.email.unwrap_or_default(),
        name: acc.display_name.or(acc.full_name).unwrap_or_default(),
        plan,
        tier: tier_label(org.as_ref().and_then(|o| o.rate_limit_tier.as_deref())),
        organization: org.as_ref().and_then(|o| o.name.clone()).unwrap_or_default(),
        active: org
            .as_ref()
            .and_then(|o| o.subscription_status.as_deref())
            .map(|s| s == "active")
            .unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_limits_array() {
        let body = r#"{"limits":[
            {"kind":"session","group":"session","percent":42.5,"severity":"normal",
             "resets_at":"2026-11-09T23:00:00Z","is_active":true},
            {"kind":"weekly_all","percent":12},
            {"kind":"weekly_scoped","percent":91,"severity":"critical",
             "scope":{"model":{"display_name":"Opus"}}}],
            "extra_usage":{"is_enabled":true,"used_credits":3.5,"monthly_limit":50}}"#;
        let u = parse_usage(body, "Claude Max".into()).unwrap();
        assert_eq!(u.limits.len(), 3);
        assert_eq!(u.limits[0].label, "Sesión (5h)");
        assert_eq!(u.limits[0].window_secs, Some(18_000));
        assert!(u.limits[0].is_active);
        assert_eq!(u.limits[1].window_secs, Some(604_800));
        assert_eq!(u.limits[2].label, "Semanal · Opus");
        assert_eq!(u.max_percent(), 91.0);
        assert!(u.extra_usage_enabled);
        assert_eq!(u.extra_monthly_limit, Some(50.0));
        assert!(!u.needs_login);
    }

    #[test]
    fn falls_back_to_the_per_window_shape() {
        let body = r#"{"five_hour":{"utilization":30,"resets_at":"2026-11-09T23:00:00Z"},
            "seven_day":{"utilization":75.0,"resets_at":null},
            "seven_day_opus":null,
            "seven_day_sonnet":{"utilization":null,"resets_at":null}}"#;
        let u = parse_usage(body, "Claude".into()).unwrap();
        let kinds: Vec<_> = u.limits.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(kinds, vec!["session", "weekly_all"]);
        assert_eq!(u.limits[1].severity, "warning");
    }

    #[test]
    fn unknown_fields_and_bad_bodies() {
        let u = parse_usage(r#"{"something_new":1}"#, "Claude".into()).unwrap();
        assert!(u.limits.is_empty());
        assert!(parse_usage("<html>", "Claude".into()).is_err());
    }

    #[test]
    fn only_a_dead_login_asks_to_sign_in() {
        assert!(FetchError::Auth(AuthError::NeedsLogin).needs_login());
        assert!(FetchError::Auth(AuthError::NoCredentials("x".into())).needs_login());
        assert!(!FetchError::Auth(AuthError::Expired).needs_login());
        assert!(!FetchError::Auth(AuthError::Transient("x".into())).needs_login());
        assert!(!FetchError::RateLimited(None).needs_login());
        assert!(!FetchError::Http(500).needs_login());
    }
}
