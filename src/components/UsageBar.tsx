import type { LimitInfo } from "../types";
import { colorFor, formatSpan, paceOf, resetAbsolute, timeUntil } from "../lib/format";

export function UsageBar({ limit, now }: { limit: LimitInfo; now: number }) {
  const color = colorFor(limit.percent);
  const pct = Math.min(100, Math.max(0, limit.percent));
  const reset = timeUntil(limit.resets_at, now);
  const abs = resetAbsolute(limit.resets_at, now);
  const pace = paceOf(limit, now);

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-baseline justify-between">
        <span className="flex items-center gap-2 text-[14px] font-medium text-[#EDE9E1]">
          {limit.label}
          {limit.is_active && (
            <span
              className="h-2 w-2 rounded-full bg-[#D97757]"
              title="Ventana activa ahora"
            />
          )}
        </span>
        <span className="text-[14px] font-semibold tabular-nums" style={{ color }}>
          {Math.round(limit.percent)}%
        </span>
      </div>

      <div className="relative h-2.5 w-full">
        <div className="h-full w-full overflow-hidden rounded-full bg-white/[0.08]">
          <div
            className="h-full rounded-full transition-all duration-500 ease-out"
            style={{ width: `${pct}%`, backgroundColor: color }}
          />
        </div>
        {/* Where the fill would be if usage were spread evenly over the window:
            fill past the tick means spending faster than the clock. */}
        {pace && (
          <span
            className="absolute -top-0.5 h-3.5 w-0.5 -translate-x-1/2 rounded-full bg-[#EDE9E1]/70"
            style={{ left: `${pace.elapsed * 100}%` }}
            title="Tiempo transcurrido de la ventana"
          />
        )}
      </div>

      {(reset || pace?.exhaustsInMs != null) && (
        <span className="text-[12px] text-[#9B968D]">
          {reset && (
            <>
              se reinicia en {reset}
              {abs ? ` · ${abs}` : ""}
            </>
          )}
          {pace?.exhaustsInMs != null && (
            <span className="text-[#E0A458]">
              {reset ? " · " : ""}a este ritmo se agota en{" "}
              {formatSpan(pace.exhaustsInMs / 1000)}
            </span>
          )}
        </span>
      )}
    </div>
  );
}
