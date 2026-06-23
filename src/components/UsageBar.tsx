import type { LimitInfo } from "../types";
import { colorFor, resetAbsolute, timeUntil } from "../lib/format";

export function UsageBar({ limit, now }: { limit: LimitInfo; now: number }) {
  const color = colorFor(limit.percent);
  const pct = Math.min(100, Math.max(0, limit.percent));
  const reset = timeUntil(limit.resets_at, now);
  const abs = resetAbsolute(limit.resets_at, now);

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

      <div className="h-2.5 w-full overflow-hidden rounded-full bg-white/[0.08]">
        <div
          className="h-full rounded-full transition-all duration-500 ease-out"
          style={{ width: `${pct}%`, backgroundColor: color }}
        />
      </div>

      {reset && (
        <span className="text-[12px] text-[#9B968D]">
          se reinicia en {reset}
          {abs ? ` · ${abs}` : ""}
        </span>
      )}
    </div>
  );
}
