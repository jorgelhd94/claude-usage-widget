import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  Analytics,
  ClaudeSource,
  ClaudeStatus,
  Profile,
  Settings,
  StateSnapshot,
  UsageData,
} from "./types";
import { timeSince } from "./lib/format";
import { ClaudeLogo } from "./components/ClaudeLogo";
import { LoginCard } from "./components/LoginCard";
import { UsageBar } from "./components/UsageBar";
import { SettingsPanel } from "./components/SettingsPanel";
import { AnalyticsView } from "./components/AnalyticsView";

const CLAY = "#D97757";

const DEFAULT_SETTINGS: Settings = {
  poll_interval_secs: 150,
  notify_high: 80,
  notify_critical: 90,
  enable_notifications: true,
  enable_animation: true,
  start_with_windows: false,
  theme: "dark",
  keep_session_alive: true,
};

export default function App() {
  const [usage, setUsage] = useState<UsageData | null>(null);
  const [profile, setProfile] = useState<Profile | null>(null);
  const [settings, setSettings] = useState<Settings>(DEFAULT_SETTINGS);
  const [claudeSource, setClaudeSource] = useState<ClaudeSource>("");
  const [working, setWorking] = useState(false);
  const [hookInstalled, setHookInstalled] = useState(false);
  const [view, setView] = useState<"main" | "settings" | "analytics">("main");
  const [analytics, setAnalytics] = useState<Analytics | null>(null);
  const [now, setNow] = useState(Date.now());
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    invoke<StateSnapshot>("get_state")
      .then((s) => {
        setUsage(s.usage);
        setProfile(s.profile);
        setSettings(s.settings);
        setClaudeSource(s.claude_source);
        setWorking(s.working);
      })
      .catch(() => {});
    invoke<boolean>("session_hook_installed").then(setHookInstalled).catch(() => {});

    const unUsage = listen<UsageData>("usage-updated", (e) => setUsage(e.payload));
    const unProfile = listen<Profile | null>("profile-updated", (e) => setProfile(e.payload));
    const unStatus = listen<ClaudeStatus>("claude-status", (e) =>
      setClaudeSource(e.payload.source),
    );
    const unWorking = listen<boolean>("working-status", (e) => setWorking(e.payload));

    return () => {
      unUsage.then((f) => f());
      unProfile.then((f) => f());
      unStatus.then((f) => f());
      unWorking.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  // Auto-fit the window to its content: measure the root and tell the backend
  // to resize + re-anchor above the tray, so there's no empty space below.
  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    let raf = 0;
    let last = 0;
    const apply = () => {
      const h = el.offsetHeight;
      if (h > 0 && Math.abs(h - last) >= 1) {
        last = h;
        invoke("resize_popup", { height: h }).catch(() => {});
      }
    };
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(apply);
    });
    ro.observe(el);
    apply();
    return () => {
      ro.disconnect();
      cancelAnimationFrame(raf);
    };
  }, []);

  useEffect(() => {
    if (view === "analytics") {
      invoke<Analytics>("get_analytics", { force: false }).then(setAnalytics).catch(() => {});
    }
  }, [view]);

  const refresh = () => {
    invoke("refresh_now").catch(() => {});
    if (view === "analytics") {
      invoke<Analytics>("get_analytics", { force: true }).then(setAnalytics).catch(() => {});
    }
  };

  const planName = profile?.plan || usage?.plan || "Claude";
  const claudeRunning = claudeSource !== "";
  const status = working
    ? "trabajando…"
    : claudeSource === "code"
      ? "activo"
      : claudeSource === "desktop"
        ? "Claude Desktop abierto"
        : "inactivo";
  const needsLogin = !!usage?.needs_login;
  const errMsg = usage?.error ?? null;
  // 429 / rate-limit is transient and expected — show it subtly, not as an alarm.
  const isTransient = !!errMsg && /429|límite de peticiones/i.test(errMsg);

  const updateSettings = (s: Settings) => {
    setSettings(s);
    invoke("set_settings", { settings: s }).catch(() => {});
  };

  const toggleHook = (install: boolean) => {
    invoke(install ? "install_session_hook" : "remove_session_hook")
      .then(() => setHookInstalled(install))
      .catch(() => {});
  };

  const subView = view !== "main";
  const subTitle = view === "analytics" ? "Análisis de uso" : "Ajustes";

  return (
    <div
      ref={rootRef}
      className="flex max-h-[640px] w-screen flex-col overflow-hidden rounded-lg border border-white/[0.08] bg-[#1E1C1A]/55 text-[#EDE9E1]"
    >
      {/* Header */}
      {subView ? (
        <header className="flex items-center gap-2 px-3 pb-4 pt-4">
          <button
            onClick={() => setView("main")}
            className="shrink-0 rounded-lg p-2 text-[#A3A097] transition-colors hover:bg-white/[0.08] hover:text-[#EDE9E1]"
            title="Volver al inicio"
          >
            <BackIcon />
          </button>
          <span className="text-[16px] font-semibold">{subTitle}</span>
          {view === "analytics" && (
            <button
              onClick={refresh}
              className="ml-auto rounded-lg p-2 text-[#A3A097] transition-colors hover:bg-white/[0.08] hover:text-[#EDE9E1]"
              title="Actualizar ahora"
            >
              <RefreshIcon />
            </button>
          )}
        </header>
      ) : (
        <header className="flex items-start gap-3 px-5 pb-5 pt-4">
          <ClaudeLogo className="mt-0.5 h-6 w-6 shrink-0" color={CLAY} />
          <div className="flex min-w-0 flex-col gap-1">
            <div className="flex items-center gap-2">
              <span className="text-[16px] font-semibold leading-none">{planName}</span>
              {profile?.tier && (
                <span className="rounded-md bg-[#D97757]/20 px-1.5 py-0.5 text-[11px] font-semibold text-[#ECA888]">
                  {profile.tier}
                </span>
              )}
            </div>
            <span className="flex min-w-0 items-center gap-1.5 text-[12px] text-[#9B968D]">
              <span
                className={`h-2 w-2 shrink-0 rounded-full ${
                  working
                    ? "animate-pulse bg-[#D97757]"
                    : claudeRunning
                      ? "bg-emerald-400"
                      : "bg-[#5A564F]"
                }`}
              />
              <span className="truncate">
                {status}
                {profile?.email ? ` · ${profile.email}` : ""}
              </span>
            </span>
          </div>
          <div className="ml-auto flex shrink-0 items-center gap-1">
            <button
              onClick={refresh}
              className="rounded-lg p-2 text-[#A3A097] transition-colors hover:bg-white/[0.08] hover:text-[#EDE9E1]"
              title="Actualizar ahora"
            >
              <RefreshIcon />
            </button>
            <button
              onClick={() => setView("analytics")}
              className="rounded-lg p-2 text-[#A3A097] transition-colors hover:bg-white/[0.08] hover:text-[#EDE9E1]"
              title="Análisis de uso"
            >
              <ChartIcon />
            </button>
            <button
              onClick={() => setView("settings")}
              className="rounded-lg p-2 text-[#A3A097] transition-colors hover:bg-white/[0.08] hover:text-[#EDE9E1]"
              title="Ajustes"
            >
              <GearIcon />
            </button>
          </div>
        </header>
      )}

      <div className="mx-5 border-t border-white/[0.07]" />

      {/* Body */}
      <main className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {view === "settings" ? (
          <SettingsPanel
            settings={settings}
            hookInstalled={hookInstalled}
            onChange={updateSettings}
            onToggleHook={toggleHook}
          />
        ) : view === "analytics" ? (
          <AnalyticsView analytics={analytics} />
        ) : (
          <div className="flex flex-col gap-4">
            {needsLogin ? (
              <LoginCard reason={errMsg} />
            ) : (
              errMsg &&
              !isTransient && (
                <div className="rounded-lg border border-[#D9A05B]/30 bg-[#D9A05B]/10 px-3 py-2 text-[13px] text-[#E0A458]">
                  {errMsg}
                </div>
              )
            )}

            {usage && usage.limits.length > 0
              ? usage.limits.map((l) => (
                  <UsageBar key={l.kind + l.label} limit={l} now={now} />
                ))
              : // An error shown above already explains the empty space; a
                // rate-limit isn't shown there, it just means "not yet".
                (!errMsg || isTransient) && (
                  <div className="py-10 text-center text-[14px] text-[#9B968D]">
                    Cargando uso…
                  </div>
                )}

            {usage?.extra_usage_enabled && (
              <div className="flex items-center justify-between rounded-lg bg-white/[0.05] px-3 py-2 text-[13px]">
                <span className="text-[#A3A097]">Créditos extra</span>
                <span className="tabular-nums text-[#EDE9E1]">
                  {usage.extra_used_credits ?? 0}
                  {usage.extra_monthly_limit ? ` / ${usage.extra_monthly_limit}` : ""}
                </span>
              </div>
            )}
          </div>
        )}
      </main>

      {/* Footer */}
      <footer className="flex items-center justify-between px-5 py-3 text-[12px] text-[#807C73]">
        <span className="flex items-center gap-1.5">
          {isTransient && (
            <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[#E0A458]" />
          )}
          {isTransient
            ? "actualizando…"
            : usage?.fetched_at_ms
              ? `Actualizado ${timeSince(usage.fetched_at_ms, now)}`
              : "—"}
        </span>
        <span className="truncate pl-2">{profile?.organization || "Claude Usage"}</span>
      </footer>
    </div>
  );
}

function BackIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M15 18l-6-6 6-6" />
    </svg>
  );
}

function RefreshIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M21 12a9 9 0 1 1-2.64-6.36" />
      <path d="M21 3v6h-6" />
    </svg>
  );
}

function GearIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

function ChartIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 3v18h18" />
      <rect x="7" y="11" width="3" height="6" rx="0.5" />
      <rect x="12" y="6" width="3" height="11" rx="0.5" />
      <rect x="17" y="13" width="3" height="4" rx="0.5" />
    </svg>
  );
}
