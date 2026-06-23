use serde::{Deserialize, Serialize};

use crate::credentials::{now_ms, read_credentials};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Fallback Claude Code version for the User-Agent header (critical to avoid
/// aggressive 429s). Overridden at runtime when the real version is detected.
pub const CLAUDE_VERSION_FALLBACK: &str = "2.1.185";

/// A single usage window, normalized for the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct LimitInfo {
    pub kind: String,
    pub label: String,
    pub percent: f64,
    pub severity: String,
    pub resets_at: Option<String>,
    pub is_active: bool,
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
    Auth(String),
    Unauthorized,
    RateLimited(Option<u64>),
    Http(u16),
    Network(String),
    Parse(String),
}

impl FetchError {
    pub fn user_message(&self) -> String {
        match self {
            FetchError::Auth(e) => format!("Credenciales: {}", e),
            FetchError::Unauthorized => "Sesión expirada — reautentica en Claude Code".to_string(),
            FetchError::RateLimited(_) => "Límite de peticiones (429) — reintentando".to_string(),
            FetchError::Http(c) => format!("Error HTTP {}", c),
            FetchError::Network(e) => format!("Red: {}", e),
            FetchError::Parse(e) => format!("Respuesta inesperada: {}", e),
        }
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

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    limits: Vec<RawLimit>,
    #[serde(default)]
    extra_usage: Option<RawExtra>,
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

/// Fetch and normalize the subscription usage from the OAuth endpoint.
pub async fn fetch_usage(client: &reqwest::Client, version: &str) -> Result<UsageData, FetchError> {
    let creds = read_credentials().map_err(FetchError::Auth)?;

    let resp = client
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", format!("claude-code/{}", version))
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let status = resp.status();
    if status.as_u16() == 429 {
        let retry = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        return Err(FetchError::RateLimited(retry));
    }
    if status.as_u16() == 401 {
        return Err(FetchError::Unauthorized);
    }
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }

    let raw: RawUsage = resp.json().await.map_err(|e| FetchError::Parse(e.to_string()))?;

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
            kind,
            percent: l.percent.unwrap_or(0.0),
            severity: l.severity.unwrap_or_else(|| "normal".to_string()),
            resets_at: l.resets_at,
            is_active: l.is_active.unwrap_or(false),
        });
    }

    let (extra_enabled, used, monthly) = match raw.extra_usage {
        Some(e) => (e.is_enabled.unwrap_or(false), e.used_credits, e.monthly_limit),
        None => (false, None, None),
    };

    Ok(UsageData {
        plan: plan_label(creds.subscription_type.as_deref()),
        limits,
        extra_usage_enabled: extra_enabled,
        extra_used_credits: used,
        extra_monthly_limit: monthly,
        fetched_at_ms: now_ms(),
        error: None,
    })
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

#[derive(Deserialize)]
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

pub async fn fetch_profile(client: &reqwest::Client, version: &str) -> Result<Profile, FetchError> {
    let creds = read_credentials().map_err(FetchError::Auth)?;
    let resp = client
        .get(PROFILE_URL)
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", format!("claude-code/{}", version))
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(FetchError::Http(resp.status().as_u16()));
    }
    let raw: RawProfile = resp.json().await.map_err(|e| FetchError::Parse(e.to_string()))?;

    let acc = raw.account.unwrap_or(RawAccount {
        full_name: None,
        display_name: None,
        email: None,
        has_claude_max: None,
        has_claude_pro: None,
    });
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
