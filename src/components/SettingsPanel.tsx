import type { ReactNode } from "react";
import type { Settings } from "../types";

function Toggle({ on, onClick }: { on: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={`relative h-5 w-9 shrink-0 rounded-full transition-colors ${
        on ? "bg-[#D97757]" : "bg-white/15"
      }`}
    >
      <span
        className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all ${
          on ? "left-[18px]" : "left-0.5"
        }`}
      />
    </button>
  );
}

function Row({
  label,
  desc,
  children,
}: {
  label: string;
  desc?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-3 py-2.5">
      <div className="flex flex-col gap-0.5">
        <span className="text-[14px] text-[#EDE9E1]">{label}</span>
        {desc && <span className="text-[12px] text-[#9B968D]">{desc}</span>}
      </div>
      {children}
    </div>
  );
}

function ThresholdRow({
  color,
  label,
  hint,
  value,
  onChange,
}: {
  color: string;
  label: string;
  hint: string;
  value: number;
  onChange: (v: number) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ backgroundColor: color }} />
      <span className="text-[13px] text-[#EDE9E1]">{label}</span>
      <span className="truncate text-[12px] text-[#9B968D]">· {hint}</span>
      <div className="ml-auto flex items-center gap-1">
        <input
          type="number"
          min={1}
          max={100}
          value={value}
          onChange={(e) => onChange(Number(e.target.value))}
          className="w-12 rounded-md bg-white/[0.08] px-2 py-1 text-right text-[13px] text-[#EDE9E1] outline-none"
        />
        <span className="text-[13px] text-[#9B968D]">%</span>
      </div>
    </div>
  );
}

const INTERVALS = [
  { v: 60, label: "1m" },
  { v: 150, label: "2.5m" },
  { v: 300, label: "5m" },
  { v: 900, label: "15m" },
];

interface Props {
  settings: Settings;
  hookInstalled: boolean;
  onChange: (s: Settings) => void;
  onToggleHook: (install: boolean) => void;
}

export function SettingsPanel({
  settings,
  hookInstalled,
  onChange,
  onToggleHook,
}: Props) {
  const set = (patch: Partial<Settings>) => onChange({ ...settings, ...patch });

  return (
    <div className="flex flex-col divide-y divide-white/[0.06]">
      <Row label="Mostrar al abrir Claude Code" desc="Instala un hook SessionStart">
        <Toggle on={hookInstalled} onClick={() => onToggleHook(!hookInstalled)} />
      </Row>

      <Row
        label="Mantener la sesión activa"
        desc="Renueva el login si solo usas Claude Desktop"
      >
        <Toggle
          on={settings.keep_session_alive}
          onClick={() => set({ keep_session_alive: !settings.keep_session_alive })}
        />
      </Row>

      <Row label="Iniciar con Windows" desc="Vigila en segundo plano">
        <Toggle
          on={settings.start_with_windows}
          onClick={() => set({ start_with_windows: !settings.start_with_windows })}
        />
      </Row>

      <Row label="Notificaciones" desc="Avisar al cruzar umbrales">
        <Toggle
          on={settings.enable_notifications}
          onClick={() =>
            set({ enable_notifications: !settings.enable_notifications })
          }
        />
      </Row>

      <Row label="Animar icono al trabajar">
        <Toggle
          on={settings.enable_animation}
          onClick={() => set({ enable_animation: !settings.enable_animation })}
        />
      </Row>

      <div className="flex flex-col gap-2.5 py-3">
        <span className="text-[14px] text-[#EDE9E1]">Intervalo de actualización</span>
        <div className="flex gap-1 rounded-lg bg-white/[0.06] p-1">
          {INTERVALS.map((o) => (
            <button
              key={o.v}
              onClick={() => set({ poll_interval_secs: o.v })}
              className={`flex-1 rounded-md py-1.5 text-[13px] transition-colors ${
                settings.poll_interval_secs === o.v
                  ? "bg-[#D97757] font-medium text-white"
                  : "text-[#A3A097] hover:text-[#EDE9E1]"
              }`}
            >
              {o.label}
            </button>
          ))}
        </div>
      </div>

      <div className="flex flex-col gap-2.5 py-3">
        <div className="flex flex-col gap-0.5">
          <span className="text-[14px] text-[#EDE9E1]">Avisarme al llegar a</span>
          <span className="text-[12px] text-[#9B968D]">
            Cambia el color del icono y manda notificación
          </span>
        </div>
        <ThresholdRow
          color="#E8A23D"
          label="Aviso"
          hint="icono ámbar"
          value={settings.notify_high}
          onChange={(v) => set({ notify_high: v })}
        />
        <ThresholdRow
          color="#EF6248"
          label="Crítico"
          hint="icono rojo + alerta"
          value={settings.notify_critical}
          onChange={(v) => set({ notify_critical: v })}
        />
      </div>
    </div>
  );
}
