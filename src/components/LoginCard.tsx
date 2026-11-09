import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** Shown when Claude Code's login is missing or dead. The button hands the
 *  sign-in to Claude Code itself (`claude auth login` in a terminal); the
 *  backend watches the credentials file and recovers as soon as it's done. */
export function LoginCard({ reason }: { reason: string | null }) {
  const [started, setStarted] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const login = () => {
    setFailure(null);
    invoke("start_login")
      .then(() => setStarted(true))
      .catch((e) => setFailure(String(e)));
  };

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-[#D97757]/30 bg-[#D97757]/10 px-4 py-3.5">
      <div className="flex flex-col gap-1">
        <span className="text-[14px] font-semibold text-[#EDE9E1]">
          {reason || "Inicia sesión en Claude Code"}
        </span>
        <span className="text-[13px] leading-snug text-[#B5B0A6]">
          El widget lee el uso con el inicio de sesión de Claude Code. Claude Desktop
          usa el suyo propio y no lo renueva, así que caduca si no abres el CLI.
          Inicia sesión una vez y el widget lo mantendrá activo.
        </span>
      </div>

      {started && (
        <span className="text-[13px] text-[#ECA888]">
          Completa el inicio de sesión en la terminal y el navegador. El widget se
          actualizará solo al terminar.
        </span>
      )}
      <button
        onClick={login}
        className={`self-start rounded-md px-3 py-1.5 text-[13px] font-medium transition-colors ${
          started
            ? "bg-white/[0.08] text-[#EDE9E1] hover:bg-white/[0.14]"
            : "bg-[#D97757] text-white hover:bg-[#C96747]"
        }`}
      >
        {started ? "Abrir de nuevo" : "Iniciar sesión"}
      </button>

      {failure && <span className="text-[13px] text-[#E0A458]">{failure}</span>}
    </div>
  );
}
