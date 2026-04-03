import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type SetupPhase =
  | "checking"
  | "need-ollama"
  | "install-error"
  | "installing-ollama"
  | "awaiting-restart-ollama"
  | "need-models"
  | "pulling-models"
  | "done";

interface ModelProgressPayload {
  phase: "pulling" | "creating" | "done";
  model?: string;
  step: number;
  total: number;
  status?: string;
  completed?: number;
  bytesTotal?: number;
}

interface DownloadProgressPayload {
  downloaded: number;
  total: number;
}

interface SetupFlags {
  ollamaVerified: boolean;
  modelsVerified: boolean;
}

interface Props {
  onComplete: (result: "ready" | "missing-ollama" | "missing-models") => void;
}

function fmt(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function OllamaSetupModal({ onComplete }: Props) {
  const [phase, setPhase] = useState<SetupPhase>("checking");
  const [modelProgress, setModelProgress] = useState<ModelProgressPayload | null>(null);
  const [dlProgress, setDlProgress] = useState<DownloadProgressPayload | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);
  const [modelError, setModelError] = useState<string | null>(null);
  const unlistenRefs = useRef<(() => void)[]>([]);

  useEffect(() => {
    listen<ModelProgressPayload>("ollama-setup-progress", (event) => {
      setModelProgress(event.payload);
      if (event.payload.phase === "done") setPhase("done");
    }).then((fn) => unlistenRefs.current.push(fn));

    listen<DownloadProgressPayload>("ollama-download-progress", (event) => {
      setDlProgress(event.payload);
    }).then((fn) => unlistenRefs.current.push(fn));

    checkSetup();

    return () => { unlistenRefs.current.forEach(fn => fn()); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (phase === "done") {
      const t = setTimeout(() => invoke("restart_app"), 2000);
      return () => clearTimeout(t);
    }
  }, [phase]);

  async function checkSetup() {
    try {
      const flags = await invoke<SetupFlags>("get_setup_flags");
      if (flags.ollamaVerified && flags.modelsVerified) {
        onComplete("ready");
        return;
      }

      const installed = await invoke<boolean>("check_ollama_installed");
      if (!installed) { setPhase("need-ollama"); return; }

      await invoke("set_setup_flags", { args: { ollamaVerified: true } });
      await checkModels();
    } catch (e) {
      console.error("Ollama setup check failed:", e);
      onComplete("ready");
    }
  }

  async function checkModels() {
    try {
      const missing = await invoke<string[]>("check_models_ready");
      if (missing.length > 0) setPhase("need-models");
      else {
        await invoke("set_setup_flags", { args: { modelsVerified: true } });
        onComplete("ready");
      }
    } catch (e) {
      console.error("Model check failed:", e);
      onComplete("ready");
    }
  }

  async function handleInstallOllama() {
    setPhase("installing-ollama");
    setInstallError(null);
    setDlProgress(null);
    try {
      await invoke("download_and_install_ollama");
      await invoke("set_setup_flags", { args: { ollamaVerified: true } });
      setPhase("awaiting-restart-ollama");
    } catch (e) {
      setInstallError(String(e));
      setPhase("install-error");
    }
  }

  async function handlePullModels() {
    setPhase("pulling-models");
    setModelError(null);
    try {
      await invoke("pull_and_create_models");
      await invoke("set_setup_flags", { args: { modelsVerified: true } });
    } catch (e) {
      setModelError(String(e));
      setPhase("need-models");
    }
  }

  function handleOpenModelfiles() {
    invoke("open_modelfiles_folder").catch(console.error);
  }

  // ── Checking ─────────────────────────────────────────────────────────────
  if (phase === "checking") {
    return (
      <div className="setup-overlay">
        <div className="setup-modal">
          <p>Checking Ollama setup…</p>
        </div>
      </div>
    );
  }

  // ── Ollama not installed ──────────────────────────────────────────────────
  if (phase === "need-ollama") {
    return (
      <div className="setup-overlay">
        <div className="setup-modal">
          <h2>Ollama Not Installed</h2>
          <p>Ollama is not installed on this device. Do you wish to download and install it?</p>
          <div className="setup-buttons">
            <button className="setup-btn primary" onClick={handleInstallOllama}>
              Yes, Download &amp; Install
            </button>
            <button className="setup-btn" onClick={() => onComplete("missing-ollama")}>
              No
            </button>
            <button className="setup-btn" onClick={handleOpenModelfiles}>
              Check Modelfiles
            </button>
          </div>
        </div>
      </div>
    );
  }

  // ── Install failed ────────────────────────────────────────────────────────
  if (phase === "install-error") {
    return (
      <div className="setup-overlay">
        <div className="setup-modal">
          <h2>Installation Failed</h2>
          <p>The Ollama installer could not be launched.</p>
          {installError && <p className="setup-error">{installError}</p>}
          <div className="setup-buttons">
            <button className="setup-btn primary" onClick={handleInstallOllama}>
              Try Again
            </button>
            <button className="setup-btn" onClick={() => onComplete("missing-ollama")}>
              Close
            </button>
            <button className="setup-btn" onClick={handleOpenModelfiles}>
              Check Modelfiles
            </button>
          </div>
        </div>
      </div>
    );
  }

  // ── Downloading installer ─────────────────────────────────────────────────
  if (phase === "installing-ollama") {
    const hasTotals = dlProgress && dlProgress.total > 0;
    const pct = hasTotals
      ? Math.round((dlProgress!.downloaded / dlProgress!.total) * 100)
      : null;
    return (
      <div className="setup-overlay">
        <div className="setup-modal">
          <h2>Downloading Ollama…</h2>
          <div className="setup-progress-bar">
            {pct !== null ? (
              <div className="setup-progress-fill setup-progress-fill--download" style={{ width: `${pct}%` }} />
            ) : (
              <div className="setup-progress-fill setup-progress-fill--download setup-progress-indeterminate" />
            )}
          </div>
          <p className="setup-step">
            {dlProgress
              ? hasTotals
                ? `${fmt(dlProgress.downloaded)} / ${fmt(dlProgress.total)} (${pct}%)`
                : `${fmt(dlProgress.downloaded)} downloaded…`
              : "Starting download…"}
          </p>
        </div>
      </div>
    );
  }

  // ── Installer launched ────────────────────────────────────────────────────
  if (phase === "awaiting-restart-ollama") {
    return (
      <div className="setup-overlay">
        <div className="setup-modal">
          <h2>Installer Launched</h2>
          <p>Complete the Ollama installer that just opened, then restart this app.</p>
          <div className="setup-buttons">
            <button className="setup-btn primary" onClick={() => invoke("restart_app")}>
              Restart App
            </button>
            <button className="setup-btn" onClick={() => onComplete("missing-ollama")}>
              Skip for Now
            </button>
          </div>
        </div>
      </div>
    );
  }

  // ── Models not found ──────────────────────────────────────────────────────
  if (phase === "need-models") {
    return (
      <div className="setup-overlay">
        <div className="setup-modal">
          <h2>Models Not Found</h2>
          <p>
            Currently registered Ollama models have not been pulled. Pull currently
            registered Ollama models?
          </p>
          <p className="setup-note">
            This will download approximately 10 GB of model data and may take a while.
          </p>
          {modelError && <p className="setup-error">{modelError}</p>}
          <div className="setup-buttons">
            <button className="setup-btn primary" onClick={handlePullModels}>
              Yes, Pull Models
            </button>
            <button className="setup-btn" onClick={() => onComplete("missing-models")}>
              No
            </button>
            <button className="setup-btn" onClick={handleOpenModelfiles}>
              Check Modelfiles
            </button>
          </div>
        </div>
      </div>
    );
  }

  // ── Pulling / creating models ─────────────────────────────────────────────
  if (phase === "pulling-models") {
    const step = modelProgress?.step ?? 0;
    const total = modelProgress?.total ?? 1;
    const bytesDone = modelProgress?.completed ?? 0;
    const bytesTotal = modelProgress?.bytesTotal ?? 0;
    const sub = bytesTotal > 0 ? Math.min(1, bytesDone / bytesTotal) : 0;
    const pct = modelProgress
      ? Math.round((((Math.max(step, 1) - 1) + sub) / Math.max(total, 1)) * 100)
      : 0;

    return (
      <div className="setup-overlay">
        <div className="setup-modal">
          <h2>Setting Up Models…</h2>
          {modelProgress ? (
            <>
              <p>
                {modelProgress.phase === "pulling" ? "Pulling base model:" : "Creating:"}{" "}
                <strong>{modelProgress.model}</strong>
              </p>
              <div className="setup-progress-bar">
                <div className="setup-progress-fill" style={{ width: `${pct}%` }} />
              </div>
              <p className="setup-step">Step {modelProgress.step} of {modelProgress.total} ({pct}%)</p>
              {modelProgress.phase === "pulling" && (
                <p className="setup-step">
                  {modelProgress.status || "pulling"}
                  {bytesTotal > 0
                    ? ` • ${fmt(bytesDone)} / ${fmt(bytesTotal)}`
                    : ""}
                </p>
              )}
            </>
          ) : (
            <p>Starting download… This may take a while.</p>
          )}
        </div>
      </div>
    );
  }

  // ── All done ──────────────────────────────────────────────────────────────
  if (phase === "done") {
    return (
      <div className="setup-overlay">
        <div className="setup-modal">
          <h2>Setup Complete!</h2>
          <p>All models are ready. Restarting app…</p>
        </div>
      </div>
    );
  }

  return null;
}
