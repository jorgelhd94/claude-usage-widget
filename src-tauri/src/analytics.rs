//! Local usage analytics — a faithful reimplementation of Claude Code's
//! "what's contributing to your usage" panel. The algorithm (cost weighting,
//! behavior thresholds, attribution-based breakdowns) was derived from Claude
//! Code's own implementation so the numbers line up closely.
//!
//! Per-message cost (matches Claude Code exactly):
//!   cost = (cache_read + input*10 + cache_creation*12.5 + output*50) * tier
//!   tier: fable=10, opus=5, sonnet/other=3, haiku=1

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::credentials::now_ms;

const MIN_BEHAVIOR_PCT: f64 = 10.0; // behaviors shown only at/above this %
const CACHE_MISS_TOKENS: f64 = 100_000.0; // uncached input above this = cache miss
const HIGH_CONTEXT: f64 = 150_000.0; // total input tokens above this = high context
const BUCKET_MS: i64 = 300_000; // 5-minute activity buckets (parallel detection)
const PARALLEL_SESSIONS: usize = 4; // >= this many sessions in a bucket = parallel
const SUBAGENT_MSG_COUNT: u32 = 3; // >= this many subagent msgs = subagent-heavy
const SUBAGENT_COST_RATIO: f64 = 0.5; // OR subagent cost share above this
const LONG_SESSION_HOURS: usize = 8; // session touching >= this many hours = "cron"

#[derive(Serialize, Clone, Default)]
pub struct Behavior {
    pub key: String,
    pub pct: f64,
    pub count: u32,
}

#[derive(Serialize, Clone, Default)]
pub struct CategoryItem {
    pub name: String,
    pub pct: f64,
}

#[derive(Serialize, Clone, Default)]
pub struct WindowAnalytics {
    pub request_count: u32,
    pub session_count: u32,
    pub behaviors: Vec<Behavior>,
    pub agents: Vec<CategoryItem>,
    pub skills: Vec<CategoryItem>,
    pub plugins: Vec<CategoryItem>,
    pub mcp_servers: Vec<CategoryItem>,
    pub has_data: bool,
}

#[derive(Serialize, Clone, Default)]
pub struct Analytics {
    pub day: WindowAnalytics,
    pub week: WindowAnalytics,
    pub computed_at_ms: i64,
}

struct Msg {
    ts: i64,
    session_id: String,
    cached: f64,
    uncached: f64,
    cache_create: f64,
    output: f64,
    is_subagent: bool,
    tier: f64,
    uuid: String,
    attr_agent: Option<String>,
    attr_skill: Option<String>,
    attr_plugin: Option<String>,
    attr_mcp: Option<String>,
}

impl Msg {
    fn cost(&self) -> f64 {
        (self.cached + self.uncached * 10.0 + self.cache_create * 12.5 + self.output * 50.0)
            * self.tier
    }
    fn context(&self) -> f64 {
        self.cached + self.cache_create + self.uncached
    }
}

#[derive(Default)]
struct SessionAgg {
    cost: f64,
    sub_cost: f64,
    sub_count: u32,
    hours: HashSet<i64>,
}

#[derive(Default)]
struct BucketAgg {
    sids: HashSet<String>,
    cost: f64,
    count: u32,
}

#[derive(Default)]
struct Acc {
    total: f64,
    requests: u32,
    cache_miss_cost: f64,
    cache_miss_count: u32,
    long_ctx_cost: f64,
    long_ctx_count: u32,
    by_agent: HashMap<String, f64>,
    by_skill: HashMap<String, f64>,
    by_plugin: HashMap<String, f64>,
    by_mcp: HashMap<String, f64>,
    sessions: HashMap<String, SessionAgg>,
    buckets: HashMap<i64, BucketAgg>,
}

fn model_tier(model: Option<&str>) -> f64 {
    match model {
        None => 3.0,
        Some(m) => {
            let m = m.to_lowercase();
            if m.contains("fable") {
                10.0
            } else if m.contains("opus") {
                5.0
            } else if m.contains("haiku") {
                1.0
            } else {
                3.0
            }
        }
    }
}

fn str_field<'a>(v: &'a serde_json::Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty())
}

fn parse_msg(v: &serde_json::Value, cutoff: i64) -> Option<Msg> {
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return None;
    }
    let msg = v.get("message")?;
    let usage = msg.get("usage")?;
    let ts = chrono::DateTime::parse_from_rfc3339(v.get("timestamp")?.as_str()?)
        .ok()?
        .timestamp_millis();
    if ts < cutoff {
        return None;
    }
    let g = |k: &str| usage.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let (uncached, output, cache_create, cached) = (
        g("input_tokens"),
        g("output_tokens"),
        g("cache_creation_input_tokens"),
        g("cache_read_input_tokens"),
    );
    if uncached + output + cache_create + cached == 0.0 {
        return None;
    }
    let session_id = str_field(v, "sessionId").unwrap_or("").to_string();
    let is_subagent = v.get("isSidechain").and_then(|s| s.as_bool()).unwrap_or(false);
    let tier = model_tier(msg.get("model").and_then(|m| m.as_str()));
    let uuid = str_field(v, "requestId")
        .or_else(|| msg.get("id").and_then(|x| x.as_str()))
        .or_else(|| str_field(v, "uuid"))
        .unwrap_or("")
        .to_string();

    Some(Msg {
        ts,
        session_id,
        cached,
        uncached,
        cache_create,
        output,
        is_subagent,
        tier,
        uuid,
        attr_agent: str_field(v, "attributionAgent").map(String::from),
        attr_skill: str_field(v, "attributionSkill").map(String::from),
        attr_plugin: str_field(v, "attributionPlugin").map(String::from),
        attr_mcp: str_field(v, "attributionMcpServer").map(String::from),
    })
}

fn accumulate(acc: &mut Acc, m: &Msg) {
    let n = m.cost();
    acc.total += n;
    acc.requests += 1;

    // Agent-attributed usage goes to agents (keyed by the skill it ran, else the
    // agent name); otherwise skill-attributed usage goes to skills.
    if let Some(agent) = &m.attr_agent {
        let key = m.attr_skill.clone().unwrap_or_else(|| agent.clone());
        *acc.by_agent.entry(key).or_default() += n;
    } else if let Some(skill) = &m.attr_skill {
        *acc.by_skill.entry(skill.clone()).or_default() += n;
    }
    if let Some(p) = &m.attr_plugin {
        *acc.by_plugin.entry(p.clone()).or_default() += n;
    }
    if let Some(s) = &m.attr_mcp {
        *acc.by_mcp.entry(s.clone()).or_default() += n;
    }

    if m.uncached > CACHE_MISS_TOKENS {
        acc.cache_miss_cost += n;
        acc.cache_miss_count += 1;
    }
    if m.context() > HIGH_CONTEXT {
        acc.long_ctx_cost += n;
        acc.long_ctx_count += 1;
    }

    let sess = acc.sessions.entry(m.session_id.clone()).or_default();
    sess.cost += n;
    if m.is_subagent {
        sess.sub_cost += n;
        sess.sub_count += 1;
    }
    sess.hours.insert(m.ts / 3_600_000);

    let bucket = acc.buckets.entry(m.ts / BUCKET_MS).or_default();
    bucket.sids.insert(m.session_id.clone());
    bucket.cost += n;
    bucket.count += 1;
}

fn breakdown(map: &HashMap<String, f64>, total: f64) -> Vec<CategoryItem> {
    if map.is_empty() || total <= 0.0 {
        return vec![];
    }
    let mut v: Vec<(&String, &f64)> = map.iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    v.into_iter()
        .map(|(name, c)| CategoryItem {
            name: name.clone(),
            pct: (c / total * 100.0).round(),
        })
        .filter(|i| i.pct > 0.0)
        .take(8)
        .collect()
}

fn finalize(acc: Acc) -> WindowAnalytics {
    let total = acc.total;

    let (mut hp_cost, mut hp_count) = (0.0_f64, 0u32);
    for b in acc.buckets.values() {
        if b.sids.len() >= PARALLEL_SESSIONS {
            hp_cost += b.cost;
            hp_count += b.count;
        }
    }
    let (mut sub_cost, mut sub_count) = (0.0_f64, 0u32);
    let (mut cron_cost, mut cron_count) = (0.0_f64, 0u32);
    for s in acc.sessions.values() {
        if s.sub_count >= SUBAGENT_MSG_COUNT
            || (s.cost > 0.0 && s.sub_cost / s.cost > SUBAGENT_COST_RATIO)
        {
            sub_cost += s.cost;
            sub_count += 1;
        }
        if s.hours.len() >= LONG_SESSION_HOURS {
            cron_cost += s.cost;
            cron_count += 1;
        }
    }

    let t = if total > 0.0 { total } else { 1.0 };
    let mut raw = vec![
        ("cache_miss", acc.cache_miss_cost, acc.cache_miss_count),
        ("long_context", acc.long_ctx_cost, acc.long_ctx_count),
        ("subagent_heavy", sub_cost, sub_count),
        ("high_parallel", hp_cost, hp_count),
        ("cron", cron_cost, cron_count),
    ];
    raw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let behaviors = raw
        .into_iter()
        .filter(|(_, c, _)| total > 0.0 && c / t * 100.0 >= MIN_BEHAVIOR_PCT)
        .map(|(k, c, n)| Behavior {
            key: k.to_string(),
            pct: (c / t * 100.0).round(),
            count: n,
        })
        .collect();

    WindowAnalytics {
        request_count: acc.requests,
        session_count: acc.sessions.len() as u32,
        behaviors,
        agents: breakdown(&acc.by_agent, total),
        skills: breakdown(&acc.by_skill, total),
        plugins: breakdown(&acc.by_plugin, total),
        mcp_servers: breakdown(&acc.by_mcp, total),
        has_data: total > 0.0,
    }
}

fn projects_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(d).join("projects"));
    }
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Files to scan, matching Claude Code: each project's top-level `*.jsonl`
/// (main sessions) plus everything under `<session>/subagents/**.jsonl`.
/// Workflow transcripts are intentionally excluded.
fn collect_files(projects: &Path, week_cut: i64) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(proj_entries) = std::fs::read_dir(projects) else { return out };
    for proj in proj_entries.flatten() {
        let pdir = proj.path();
        if !pdir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&pdir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() && p.extension().map(|x| x == "jsonl").unwrap_or(false) {
                push_if_recent(p, week_cut, &mut out);
            } else if p.is_dir() {
                collect_subagents(&p.join("subagents"), week_cut, &mut out);
            }
        }
    }
    out
}

fn collect_subagents(dir: &Path, week_cut: i64, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_subagents(&p, week_cut, out);
        } else if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
            push_if_recent(p, week_cut, out);
        }
    }
}

fn push_if_recent(p: PathBuf, week_cut: i64, out: &mut Vec<PathBuf>) {
    if let Ok(modt) = std::fs::metadata(&p).and_then(|m| m.modified()) {
        if let Ok(d) = modt.duration_since(std::time::UNIX_EPOCH) {
            if d.as_millis() as i64 >= week_cut {
                out.push(p);
            }
        }
    }
}

pub fn compute_analytics() -> Analytics {
    let now = now_ms();
    let day_cut = now - 24 * 3600 * 1000;
    let week_cut = now - 7 * 24 * 3600 * 1000;

    let mut day = Acc::default();
    let mut week = Acc::default();
    let mut seen: HashSet<String> = HashSet::new();

    let Some(projects) = projects_dir() else {
        return Analytics {
            computed_at_ms: now,
            ..Default::default()
        };
    };

    for path in collect_files(&projects, week_cut) {
        let Ok(f) = File::open(&path) else { continue };
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if line.is_empty() || !line.contains("\"type\":\"assistant\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            let Some(m) = parse_msg(&v, week_cut) else { continue };
            if !m.uuid.is_empty() && !seen.insert(m.uuid.clone()) {
                continue;
            }
            let in_day = m.ts >= day_cut;
            accumulate(&mut week, &m);
            if in_day {
                accumulate(&mut day, &m);
            }
        }
    }

    Analytics {
        day: finalize(day),
        week: finalize(week),
        computed_at_ms: now,
    }
}
