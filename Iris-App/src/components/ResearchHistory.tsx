import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Copy, RefreshCw, ThumbsDown, ThumbsUp, Trash2 } from "lucide-react";

type ResearchReport = {
  taskId: string;
  goal: string;
  model: string;
  status: string;
  updatedAt: number;
  evidenceSupported: boolean;
  helpful: boolean | null;
  lesson: string | null;
  lastError: string;
  budget: { suggestedSources: number; maxSearches: number; maxReads: number };
  attempts: { tool: string; input: string; provider: string; ok: boolean; hits: number; error: string }[];
  assessments: { subject: string; timeScope: string; strategy: string; nextStep: string; gaps: string[]; conflicts: string[] }[];
  sources: { id: number; title: string; url: string }[];
};

export default function ResearchHistory() {
  const [reports, setReports] = useState<ResearchReport[]>([]);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");

  async function refresh() {
    setBusy(true);
    try { const result = await invoke<{ research: ResearchReport[] }>("get_agent_proficiency"); setReports(result.research || []); }
    catch (error) { setMessage(String(error)); }
    finally { setBusy(false); }
  }
  useEffect(() => { void refresh(); }, []);

  async function feedback(taskId: string, helpful: boolean) {
    try { await invoke("record_research_feedback", { taskId, helpful }); await refresh(); }
    catch (error) { setMessage(String(error)); }
  }

  return <section className="research-history">
    <div className="server-heading"><h4>Research history</h4><div className="controller-actions">
      <button type="button" className="icon-button" title="Refresh research history" aria-label="Refresh research history" disabled={busy} onClick={() => void refresh()}><RefreshCw size={15} /></button>
      <button type="button" className="icon-button" title="Copy research report" aria-label="Copy research report" disabled={!reports.length} onClick={() => {
        void navigator.clipboard.writeText(JSON.stringify(reports, null, 2)).then(() => setMessage("Research report copied.")).catch((error) => setMessage(String(error)));
      }}><Copy size={15} /></button>
      <button type="button" className="icon-button" title="Clear learned research history" aria-label="Clear learned research history" disabled={busy || !reports.length} onClick={() => {
        if (window.confirm("Clear the reusable research history? Saved task evidence and chats will remain.")) void invoke("clear_research_learning").then(refresh).catch((error) => setMessage(String(error)));
      }}><Trash2 size={15} /></button>
    </div></div>
    {message && <div role="status" className="controller-notice">{message}</div>}
    {!reports.length && <div className="service-empty">No research sessions recorded</div>}
    {reports.slice().reverse().map((report) => <details className="research-report" key={report.taskId}>
      <summary><span>{report.goal}</span><small>{report.status} · {report.attempts.length} requests</small></summary>
      <div className="research-report-meta">{report.model} · {new Date(report.updatedAt * 1000).toLocaleString()} · {report.evidenceSupported ? "Evidence-supported assessment" : "Incomplete assessment"}</div>
      <div className="research-report-meta">Suggested scope: {report.budget?.suggestedSources ?? "?"} sources · Request ceilings: {report.budget?.maxSearches ?? "?"} searches, {report.budget?.maxReads ?? "?"} reads</div>
      {report.lesson && <p className="research-lesson">{report.lesson}</p>}
      <div className="controller-actions"><span className="research-report-meta">{report.helpful === null ? "Not user-reviewed" : report.helpful ? "Marked helpful" : "Excluded from reusable methods"}</span>
        <button type="button" className="icon-button" title="This research was helpful" aria-label="This research was helpful" aria-pressed={report.helpful === true} onClick={() => void feedback(report.taskId, true)}><ThumbsUp size={14} /></button>
        <button type="button" className="icon-button" title="This research was not helpful" aria-label="This research was not helpful" aria-pressed={report.helpful === false} onClick={() => void feedback(report.taskId, false)}><ThumbsDown size={14} /></button>
      </div>
      <ol className="research-attempts">{report.attempts.map((attempt, index) => <li key={index}><strong>{attempt.provider || attempt.tool}</strong><span>{attempt.ok ? `${attempt.hits ? `${attempt.hits} results` : "Retrieved"}` : "Failed"}</span><code>{attempt.input}</code>{attempt.error && <p>{attempt.error}</p>}</li>)}</ol>
      {report.assessments.map((assessment, index) => <div className="research-review" key={index}><strong>{assessment.subject}</strong><span>{assessment.timeScope}</span><p>{assessment.strategy}</p>{assessment.gaps.length > 0 && <p>Gaps: {assessment.gaps.join("; ")}</p>}{assessment.conflicts.length > 0 && <p>Conflicts: {assessment.conflicts.join("; ")}</p>}</div>)}
      {report.lastError && <div className="service-error">{report.lastError}</div>}
      <div className="research-report-meta">{report.sources.length} retrieved sources · {report.taskId}</div>
    </details>)}
  </section>;
}