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

  try {
    const msPromise = fetchJson("http://127.0.0.1:11434/api/generate", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model, prompt: microSummaryPrompt, stream: false, keep_alive: "90s" }),
    }).then(j => {
      const result = j.response || "";
      console.log("[DEBUG] microSummary response:", result.substring(0, 100));
      return result;
    }).catch(e => { console.log("[DEBUG] microSummary error:", e); return ""; });

    const psPromise = fetchJson("http://127.0.0.1:11434/api/generate", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model, prompt: projectSummaryPrompt, stream: false, keep_alive: "90s" }),
    }).then(j => {
      const result = j.response || "";
      console.log("[DEBUG] projectSummary response:", result.substring(0, 100));
      return result;
    }).catch(e => { console.log("[DEBUG] projectSummary error:", e); return ""; });

    const dbPromise = fetchJson("http://127.0.0.1:11434/api/generate", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model, prompt: dialogueBulletsPrompt, stream: false, keep_alive: "5m" }),
    }).then(j => {
      const result = (j?.response || "").toString().trim();
      console.log("[DEBUG] dialogueBullets response:", result.substring(0, 100));
      return result;
    }).catch(e => { console.log("[DEBUG] dialogueBullets error:", e); return ""; });

    const [microSummary, projectSummary, dialogueBullets] = await Promise.all([msPromise, psPromise, dbPromise]);
    return { microSummary, projectSummary, dialogueBullets };
  } catch (e) {
    console.error("[DEBUG] summarizeExchange catch:", e);
    return { microSummary: "", projectSummary: "", dialogueBullets: "" };
  }
}