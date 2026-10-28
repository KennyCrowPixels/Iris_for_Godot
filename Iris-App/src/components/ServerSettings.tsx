import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Copy, KeyRound, Play, RefreshCw, Server, Square } from "lucide-react";

type ControllerConfig = { port: number; allowRead: boolean; allowControl: boolean };
type ControllerStatus = {
  running: boolean;
  endpoint: string | null;
  apiVersion: string;
  config: ControllerConfig;
  jobs: { id: string; tabId: number; status: string; error: string | null; updatedAt: number }[];
};

export default function ServerSettings() {
  const [status, setStatus] = useState<ControllerStatus | null>(null);
  const [config, setConfig] = useState<ControllerConfig>({ port: 1435, allowRead: true, allowControl: false });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  async function refresh() {
    const next = await invoke<ControllerStatus>("get_controller_status");
    setStatus(next);
    if (next.running) setConfig(next.config);
  }

  async function action(command: string) {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await invoke(command, command === "start_controller_server" ? { config } : {});
      await refresh();
      if (command === "rotate_controller_token") setNotice("Previous token revoked. Running remote turns were cancelled.");
    }
    catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }

  useEffect(() => { void refresh().catch((failure) => setError(String(failure))); }, []);
  useEffect(() => {
    if (!status?.running) return;
    const timer = setInterval(() => { void refresh().catch((failure) => setError(String(failure))); }, 3000);
    return () => clearInterval(timer);
  }, [status?.running]);

  async function copyConnection(tokenOnly: boolean) {
    try {
      const text = tokenOnly ? await invoke<string>("get_controller_token") : JSON.stringify({ baseUrl: status?.endpoint, apiVersion: "v1", authorization: "Bearer <session-token>" }, null, 2);
      await navigator.clipboard.writeText(text);
      setNotice(tokenOnly ? "Session token copied." : "Connection configuration copied.");
    } catch (failure) { setError(String(failure)); }
  }

  return <div className="llm-settings-card server-settings">
    <div className="server-heading"><h3><Server size={18} /> External controllers</h3>
      <button type="button" className="icon-button" title="Refresh controller status" aria-label="Refresh controller status" disabled={busy} onClick={() => void refresh().catch((failure) => setError(String(failure)))}><RefreshCw size={16} /></button>
    </div>
    <section className="server-section">
      <div className="server-service-row"><h4>Iris Controller API</h4><span className={`service-state ${status?.running ? "online" : "offline"}`}>{status?.running ? "Listening" : "Stopped"}</span></div>
      <dl className="service-facts">
        <div><dt>Endpoint</dt><dd><code>{status?.endpoint || `http://127.0.0.1:${config.port}`}</code></dd></div>
        <div><dt>API version</dt><dd>v1</dd></div>
        <div><dt>Binding</dt><dd>Localhost only</dd></div>
        <div><dt>Authentication</dt><dd>Session bearer token</dd></div>
      </dl>
      <fieldset className="controller-config" disabled={busy || status?.running}>
        <label>Port<input type="number" min={1024} max={65535} value={config.port} onChange={(event) => setConfig({ ...config, port: Number(event.target.value) })} /></label>
        <label><input type="checkbox" checked={config.allowRead} onChange={(event) => setConfig({ ...config, allowRead: event.target.checked })} /> Read chats and task status</label>
        <label><input type="checkbox" checked={config.allowControl} onChange={(event) => setConfig({ ...config, allowControl: event.target.checked })} /> Create chats, submit turns and cancel work</label>
      </fieldset>
      <div className="controller-actions">
        <button type="button" className="setup-btn primary service-action" disabled={busy || !config.allowRead || !Number.isInteger(config.port) || config.port < 1024 || config.port > 65535} onClick={() => void action(status?.running ? "stop_controller_server" : "start_controller_server")}>
          {status?.running ? <Square size={14} /> : <Play size={14} />} {status?.running ? "Stop server" : "Start server"}
        </button>
        <button type="button" className="setup-btn service-action" disabled={busy || !status?.running} onClick={() => void copyConnection(true)}><KeyRound size={14} /> Copy token</button>
        <button type="button" className="icon-button" title="Copy connection configuration" aria-label="Copy connection configuration" disabled={!status?.running} onClick={() => void copyConnection(false)}><Copy size={15} /></button>
        <button type="button" className="setup-btn service-action" disabled={busy || !status?.running} onClick={() => void action("rotate_controller_token")}><RefreshCw size={14} /> Revoke and rotate token</button>
      </div>
      {error && <div className="service-error" role="alert">{error}</div>}
      {notice && <div className="controller-notice" role="status">{notice}</div>}
    </section>
    <section className="server-section"><h4>Connection policy</h4><dl className="service-facts">
      <div><dt>Other devices</dt><dd>Encrypted tunnel or HTTPS proxy required</dd></div><div><dt>Tool approvals</dt><dd>Confirmed in Iris</dd></div>
      <div><dt>Token lifetime</dt><dd>Until revoked, stopped or Iris closes</dd></div><div><dt>Remote network access</dt><dd>Off unless requested; global Network setting still applies</dd></div>
    </dl></section>
    <section className="server-section"><h4>Controller jobs</h4>
      {status?.jobs.length ? <div className="controller-jobs">{status.jobs.slice().reverse().map((job) => <div className="controller-job" key={job.id}><strong>Chat {job.tabId}</strong><span>{job.status}</span><code>{job.id}</code>{job.error && <p>{job.error}</p>}</div>)}</div> : <div className="service-empty">No controller jobs</div>}
    </section>
  </div>;
}