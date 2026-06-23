export interface LimitInfo {
  kind: string;
  label: string;
  percent: number;
  severity: string;
  resets_at: string | null;
  is_active: boolean;
}

export interface UsageData {
  plan: string;
  limits: LimitInfo[];
  extra_usage_enabled: boolean;
  extra_used_credits: number | null;
  extra_monthly_limit: number | null;
  fetched_at_ms: number;
  error: string | null;
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
}

export interface StateSnapshot {
  usage: UsageData;
  settings: Settings;
  profile: Profile | null;
  claude_running: boolean;
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
