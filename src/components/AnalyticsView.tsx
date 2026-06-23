import { useState } from "react";
import type { Analytics, CategoryItem, WindowAnalytics } from "../types";

const BEHAVIORS: Record<
  string,
  { title: string; desc: string; color: string }
> = {
  long_context: {
    title: "de tu uso fue con contexto >150k",
    desc: "Las sesiones largas cuestan más aunque haya caché. Usa /compact a media tarea y /clear al cambiar de tema.",
    color: "#E8A23D",
  },
  subagent_heavy: {
    title: "vino de sesiones intensivas en subagentes",
    desc: "Cada subagente hace sus propias peticiones. Lánzalos con criterio y usa un modelo más barato para tareas simples.",
    color: "#D97757",
  },
  high_parallel: {
    title: "fue con 4+ sesiones en paralelo",
    desc: "Varias sesiones a la vez consumen muy rápido. Asegúrate de que es intencional.",
    color: "#C2533F",
  },
  cron: {
    title: "vino de sesiones activas 8h+",
    desc: "Suelen ser sesiones en bucle o de fondo. El uso continuo suma rápido — que sea intencional.",
    color: "#C2533F",
  },
  cache_miss: {
    title: "fue un fallo de caché de >100k tokens",
    desc: "Arrancar con >100k tokens sin cachear es caro. Reanuda sesiones en vez de empezar de cero.",
    color: "#E8A23D",
  },
};

function Insight({ pct, title, desc, color }: { pct: number; title: string; desc: string; color: string }) {
  return (
    <div className="flex flex-col gap-1">
      <p className="text-[14px] font-medium leading-snug text-[#EDE9E1]">
        <span className="font-semibold" style={{ color }}>
          {pct}%
        </span>{" "}
        {title}
      </p>
      <p className="text-[13px] leading-snug text-[#9B968D]">{desc}</p>
    </div>
  );
}

function Breakdown({
  title,
  items,
  prefix = "",
}: {
  title: string;
  items: CategoryItem[];
  prefix?: string;
}) {
  if (!items.length) return null;
  return (
    <div className="flex flex-col gap-2">
      <span className="text-[12px] font-semibold uppercase tracking-wider text-[#8B877E]">
        {title}
      </span>
      {items.map((it) => (
        <div key={it.name} className="flex items-center gap-2">
          <span className="min-w-0 flex-1 truncate text-[14px] text-[#D6D1C8]">
            {prefix}
            {it.name}
          </span>
          <div className="h-1.5 w-16 shrink-0 overflow-hidden rounded-full bg-white/[0.08]">
            <div
              className="h-full rounded-full bg-[#D97757]"
              style={{ width: `${Math.min(100, it.pct)}%` }}
            />
          </div>
          <span className="w-10 shrink-0 text-right text-[14px] tabular-nums text-[#9B968D]">
            {it.pct}%
          </span>
        </div>
      ))}
    </div>
  );
}

export function AnalyticsView({ analytics }: { analytics: Analytics | null }) {
  const [win, setWin] = useState<"day" | "week">("week");
  const a: WindowAnalytics | undefined = analytics ? analytics[win] : undefined;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex gap-1 rounded-lg bg-white/[0.06] p-1">
        {(["day", "week"] as const).map((w) => (
          <button
            key={w}
            onClick={() => setWin(w)}
            className={`flex-1 rounded-md py-1.5 text-[13px] transition-colors ${
              win === w
                ? "bg-[#D97757] font-medium text-white"
                : "text-[#A3A097] hover:text-[#EDE9E1]"
            }`}
          >
            {w === "day" ? "Hoy" : "Semana"}
          </button>
        ))}
      </div>

      <p className="text-[12px] leading-snug text-[#8B877E]">
        Aproximado · basado en {win === "day" ? "las últimas 24 h" : "los últimos 7 días"} de
        sesiones locales en este equipo. No es un desglose: son rasgos independientes.
      </p>

      {!analytics ? (
        <div className="py-8 text-center text-[13px] text-[#9B968D]">Calculando…</div>
      ) : !a?.has_data ? (
        <div className="py-8 text-center text-[13px] text-[#9B968D]">
          Sin datos en esta ventana.
        </div>
      ) : (
        <>
          {a.behaviors.length > 0 && (
            <div className="flex flex-col gap-3.5">
              {a.behaviors.map((b) => {
                const meta = BEHAVIORS[b.key];
                if (!meta) return null;
                return (
                  <Insight
                    key={b.key}
                    pct={b.pct}
                    title={meta.title}
                    desc={meta.desc}
                    color={meta.color}
                  />
                );
              })}
            </div>
          )}
          <Breakdown title="Subagentes" items={a.agents} />
          <Breakdown title="Skills" items={a.skills} prefix="/" />
          <Breakdown title="Plugins" items={a.plugins} />
          <Breakdown title="MCP servers" items={a.mcp_servers} />
        </>
      )}
    </div>
  );
}
