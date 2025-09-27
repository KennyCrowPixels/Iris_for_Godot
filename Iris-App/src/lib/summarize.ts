// filepath: d:\Iris_for_Godot\Iris-App\src\lib\summarize.ts
import { fetchJson } from "./ollama";

export async function summarizeExchange(params: {
  model: string;
  microSummaryPrompt: string;
  projectSummaryPrompt: string;
  dialogueBulletsPrompt: string;
}): Promise<{
  microSummary: string;
  projectSummary: string;
  dialogueBullets: string;
}> {
  const { model, microSummaryPrompt, projectSummaryPrompt, dialogueBulletsPrompt } = params;
  let microSummary = microSummaryPrompt;
  let projectSummary = projectSummaryPrompt;
  let dialogueBullets = dialogueBulletsPrompt;

  try {
    const [ms, ps, db] = await Promise.all([
      fetchJson("http://127.0.0.1:11434/api/generate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model, prompt: microSummaryPrompt, stream: false, keep_alive: "90s" }),
      }).then(j => j.response || microSummaryPrompt).catch(() => microSummaryPrompt),

      fetchJson("http://127.0.0.1:11434/api/generate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model, prompt: projectSummaryPrompt, stream: false, keep_alive: "90s" }),
      }).then(j => j.response || projectSummaryPrompt).catch(() => projectSummaryPrompt),

      fetchJson("http://127.0.0.1:11434/api/generate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model, prompt: dialogueBulletsPrompt, stream: false, keep_alive: "5m" }),
      }).then(j => (j?.response || "").toString().trim()).catch(() => ""),
    ]);
    return { microSummary: ms, projectSummary: ps, dialogueBullets: db };
  } catch {
    return { microSummary, projectSummary, dialogueBullets };
  }
}