export interface LimitInfo {
  kind: string;
  label: string;
  percent: number;
  severity: string;
  resets_at: string | null;
  is_active: boolean;
  /** Length of the window (5h / 7d) in seconds, when known. */
  window_secs: number | null;
}

export interface UsageData {
  plan: string;
  limits: LimitInfo[];
  extra_usage_enabled: boolean;
  extra_used_credits: number | null;
  extra_monthly_limit: number | null;
  fetched_at_ms: number;
  error: string | null;
  /** The Claude Code login is missing or dead; only signing in again fixes it. */
  needs_login: boolean;
}

export interface Profile {
  email: string;
  name: string;
  plan: string;
  tier: string;
  organization: string;
  active: boolean;
}

export interface Settings {
  poll_interval_secs: number;
  notify_high: number;
  notify_critical: number;
  enable_notifications: boolean;
  enable_animation: boolean;
  start_with_windows: boolean;
  theme: string;
  keep_session_alive: boolean;
}

/** Which Claude app is open: a Claude Code engine, only the Desktop app, or none. */
export type ClaudeSource = "code" | "desktop" | "";

export interface ClaudeStatus {
  running: boolean;
  source: ClaudeSource;
}

export interface StateSnapshot {
  usage: UsageData;
  settings: Settings;
  profile: Profile | null;
  claude_running: boolean;
  claude_source: ClaudeSource;
  working: boolean;
}

export interface CategoryItem {
  name: string;
  pct: number;
}

export interface Behavior {
  key: string;
  pct: number;
  count: number;
}

export interface WindowAnalytics {
  request_count: number;
  session_count: number;
  behaviors: Behavior[];
  agents: CategoryItem[];
  skills: CategoryItem[];
  plugins: CategoryItem[];
  mcp_servers: CategoryItem[];
  has_data: boolean;
}

export interface Analytics {
  day: WindowAnalytics;
  week: WindowAnalytics;
  computed_at_ms: number;
}
