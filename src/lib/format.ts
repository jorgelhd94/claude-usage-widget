/** Tray/UI color for a utilization percentage (matches the Rust ring logic):
 *  clay (normal) → amber (high) → red (critical). The clay is Claude's brand. */
export function colorFor(percent: number): string {
  if (percent >= 90) return "#C2533F"; // red
  if (percent >= 70) return "#E0A458"; // amber
  return "#D97757"; // clay
}

/** Human countdown until an ISO-8601 reset timestamp. */
export function timeUntil(iso: string | null, now: number): string {
  if (!iso) return "";
  const target = new Date(iso).getTime();
  if (isNaN(target)) return "";
  let secs = Math.floor((target - now) / 1000);
  if (secs <= 0) return "reiniciando…";

  const d = Math.floor(secs / 86400);
  secs -= d * 86400;
  const h = Math.floor(secs / 3600);
  secs -= h * 3600;
  const m = Math.floor(secs / 60);

  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m`;
  return "<1m";
}

/** Absolute reset time, e.g. "vie 22:59" (this week) or "27 jun" (further out). */
export function resetAbsolute(iso: string | null, now: number): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (isNaN(date.getTime())) return "";
  const within7d = date.getTime() - now < 7 * 86400_000;
  try {
    if (within7d) {
      return new Intl.DateTimeFormat("es", {
        weekday: "short",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
      }).format(date);
    }
    return new Intl.DateTimeFormat("es", { day: "numeric", month: "short" }).format(date);
  } catch {
    return "";
  }
}

/** "hace Xs / Xm" since a ms timestamp, for the last-updated footer. */
export function timeSince(ms: number, now: number): string {
  if (!ms) return "—";
  const secs = Math.max(0, Math.floor((now - ms) / 1000));
  if (secs < 60) return `hace ${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `hace ${m}m`;
  const h = Math.floor(m / 60);
  return `hace ${h}h`;
}
