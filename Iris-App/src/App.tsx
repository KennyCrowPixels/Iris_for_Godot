import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";
import ReactMarkdown from 'react-markdown';

const OLLAMA_URL = "http://127.0.0.1:11434/api/generate";
const ENSURE_CMD = "ensure_model";
const KEEP_ALIVE = "600s";
const SUMMARY_MODEL = "iris-summarizer:latest";

type ModelStatus = "checking" | "ready" | "loading" | "error";
type Message = { role: "user" | "llm"; text: string };
type Tab = {
  id: number;
  title: string;
  type: "chat" | "settings";
  messages?: Message[];
};
type TestModelResult = { ok: boolean; error: string | null; response?: string };
type Artifact = { lang: string; filename?: string; content: string };
type Snapshot = {
  title: string;
  messages: { role: "user" | "llm"; text: string }[];
  microSummary: string;      // camelCase for frontend
  dialogueBullets: string;   // camelCase for frontend
  summary: string;
  artifacts: Artifact[];
  last_updated?: number;
};

function isCoderIntent(text: string): boolean {
  const fences = /```[\s\S]*?```/m.test(text);
  const codeyWords = /\b(function|class|def|lambda|const|let|var|async|await|import|export|stack\s*trace|TypeError|ReferenceError|NullReference|traceback|compiler error|build failed|npm ERR!|cargo build|gdscript|godot|unity|typescript|python|c#|regex|SQL)\b/i.test(text);
  const fileNames = /\b\w+\.(ts|tsx|js|jsx|py|gd|cs|rs|json|toml|yaml|yml|sql|html|css)\b/i.test(text);
  return fences || codeyWords || fileNames;
}

function extractArtifacts(s: string): Artifact[] {
  const out: Artifact[] = [];
  const re = /```(\w+)?\s*\n([\s\S]*?)```/g;
  let m;
  while ((m = re.exec(s))) {
    const lang = (m[1] || "").toLowerCase();
    const content = m[2].trim();
    out.push({ lang, content });
  }
  return out;
}

function App() {
  const [modelStatus, setModelStatus] = useState<ModelStatus>("checking");
  const [coderReady, setCoderReady] = useState<boolean | null>(null);
  useEffect(() => {
    setModelStatus("checking");
    Promise.all([
      invoke<string>("ensure_model", { name: "iris-coder:latest" })
        .then(() => setCoderReady(true))
        .catch(e => { setCoderReady(false); console.error("ensure_model", e); }),
      invoke<string[]>("get_universal_prompts")
        .then(setUniversalPrompts)
        .catch(e => { console.error("get_universal_prompts", e); }),
      invoke<Snapshot[]>("list_open_tabs")
        .then(async (snaps) => {
          if (snaps && snaps.length > 0) {
            // Cap to 8
            const capped = snaps.slice(-8);
            let nextId = 1;
            const newTabs: Tab[] = capped.map((snap, i) => ({
              id: nextId++,
              title: snap.title && snap.title.trim() ? snap.title : `Tab #${nextId - 1}`,
              type: "chat",
              messages: snap.messages
            }));
            setTabs(newTabs);
            setActiveTab(newTabs[newTabs.length - 1].id);
            // Recreate tab memory for each
            for (let i = 0; i < newTabs.length; ++i) {
              const tab = newTabs[i];
              const snap = capped[i];
              await invoke("restore_full_tab_memory", {
                tabId: tab.id,
                title: snap.title,
                messages: snap.messages,
                artifacts: snap.artifacts,
                microSummary: snap.microSummary,
                dialogueBullets: snap.dialogueBullets,
                summary: snap.summary,
                lastUpdated: snap.last_updated ?? ((Date.now()/1000)|0),
              });
            }
          }
        })
        .catch(e => { console.error("list_open_tabs", e); })
    ])
      .then(() => setModelStatus("ready"))
      .catch(() => setModelStatus("error"));
  }, []);

  const [input, setInput] = useState("");
  const [thinking, setThinking] = useState(false);
  const [ellipsis, setEllipsis] = useState(".");
  const [isGenerating, setIsGenerating] = useState(false);
  const [isSummarizing, setIsSummarizing] = useState(false);
  const [tabs, setTabs] = useState<Tab[]>([
    { id: 1, title: "Tab #1", type: "chat", messages: [] }
  ]);
  const [activeTab, setActiveTab] = useState(1);
  const [openMenu, setOpenMenu] = useState<null | "file" | "options">(null);
  const abortController = useRef<AbortController | null>(null);
  const historyRef = useRef<HTMLDivElement>(null);
  const taskbarRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const isMounted = useRef(true);

  const [useCoder, setUseCoder] = useState(false);

  const currentTab = tabs.find(t => t.id === activeTab);

  useEffect(() => {
    if (historyRef.current) {
      historyRef.current.scrollTop = historyRef.current.scrollHeight;
    }
  }, [currentTab?.messages]);

  useEffect(() => {
    if (!thinking) return;
    const interval = setInterval(() => {
      setEllipsis((prev) => (prev.length < 3 ? prev + "." : "."));
    }, 500);
    return () => clearInterval(interval);
  }, [thinking]);

  // Ctrl+Shift+T restore last closed tab
  useEffect(() => {
    const handler = async (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "t") {
        const chatTabs = tabs.filter(tab => tab.type === "chat");
        if (chatTabs.length >= 8) return;
        try {
          const snap = await invoke<Snapshot>("restore_last_closed_tab");
          if (!snap || !snap.messages) return;
          // When restoring a tab
          const usedIds = tabs.map(t => t.id);
          let newId = 1;
          while (usedIds.includes(newId)) newId++;
          const newTab: Tab = {
            id: newId,
            title: snap.title || `Tab #${newId}`,
            type: "chat",
            messages: snap.messages
          };
          setTabs(prev => [...prev, newTab]);
          setActiveTab(newId);
          await invoke("create_tab_memory", { tabId: newId });
          await invoke("update_tab_memory", {
            tabId: newId,
            summary: snap.summary,
            microSummary: snap.microSummary,
            dialogueBullets: snap.dialogueBullets,
            newMessage: snap.messages.map(m => `${m.role === "user" ? "User" : "Iris"}: ${m.text}`).join("\n"),
            artifacts: snap.artifacts
          });
        } catch {}
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
    // eslint-disable-next-line
  }, [tabs]);

  function updateTabMessages(tabId: number, updater: (msgs: Message[]) => Message[]) {
    setTabs(tabs =>
      tabs.map(tab =>
        tab.id === tabId
          ? { ...tab, messages: updater(tab.messages || []) }
          : tab
      )
    );
  }

  const [thinkingModel, setThinkingModel] = useState<string | null>(null);
  const [universalPrompts, setUniversalPrompts] = useState<string[]>([]);
  const [irisStatus, setIrisStatus] = useState<"idle" | "thinking" | "summarizing" | "responding" | "coding">("idle");
  const [respondingTab, setRespondingTab] = useState<number | null>(null);

  async function sendMessage(e: React.FormEvent) {
    e.preventDefault();

    // 1) Validate input and tab BEFORE setting any flags
    const text = input.trim();
    if (!text || currentTab?.type !== "chat") return;

    // Create a local controller and assign it once
    const controller = new AbortController();
    abortController.current = controller;

    // 2) Compute coderish FIRST, then ensure the right model
    let compiled: {
      microSummary: string,
      dialogueBullets: string,
      recentTranscript: string,
      recentArtifacts: Artifact[]
    } = {
      microSummary: "",
      dialogueBullets: "",
      recentTranscript: "",
      recentArtifacts: []
    };

    // --- Model selection logic and per-model options ---
    const coderish = useCoder || isCoderIntent(text);
    await invoke("ensure_ollama_server").catch(()=>{});
    const primaryModel = coderish ? "iris-coder:latest" : "iris-organizer:latest";
    await invoke<string>("ensure_model", { name: primaryModel });

    // 3) Flip UI to “thinking” and block Send
    setThinking(true);
    setRespondingTab(activeTab);
    setIrisStatus("thinking");
    setIsGenerating(true);

    updateTabMessages(activeTab, msgs => [...msgs, { role: "user", text }]);

    try {
      try {
        const result = await invoke<typeof compiled>("get_compiled_context", { tabId: activeTab, tokenBudget: 1200 });
        if (result && typeof result === "object") {
          compiled = {
            microSummary: result.microSummary ?? "",
            dialogueBullets: result.dialogueBullets ?? "",
            recentTranscript: result.recentTranscript ?? "",
            recentArtifacts: result.recentArtifacts ?? []
          };
        }
      } catch (e) {
        console.error("get_compiled_context failed", e);
      }

      const { microSummary, dialogueBullets, recentTranscript, recentArtifacts } = compiled;

      // --- Model selection logic and per-model options ---
      const organizerOpts = {
        num_ctx: 768, num_keep: 24, num_predict: 896,
        temperature: 0.25, top_p: 0.9, top_k: 40,
        repeat_penalty: 1.06,
        num_thread: 8, num_batch: 80, num_gpu: 999
      };
      const coderOpts = {
        num_ctx: 1024, num_keep: 32, num_predict: 384,
        temperature: 0.20, top_p: 0.90, top_k: 40,
        repeat_penalty: 1.06, repeat_last_n: 128,
        num_thread: 8, num_batch: 8, num_gpu: 100
      };
      const model = primaryModel;
      const options = coderish ? coderOpts : organizerOpts;
      setThinkingModel(coderish ? "iris-coder" : "iris-organizer");

      // --- Artifact pack for coder ---
      const artPack = (recentArtifacts || []).slice(-2).map(a =>
        `Artifact${a.filename ? ` (${a.filename})` : ""}:\n\`\`\`${a.lang || ""}\n${a.content}\n\`\`\``
      ).join("\n\n");

      // --- Prompt construction ---
      let prompt = "";
      let langHint = "";
      if (coderish) {
        langHint =
          /\bgdscript|godot|\.gd\b/i.test(text) ? "gdscript" :
          /\btypescript|\.ts\b/i.test(text) ? "ts" :
          /\bpython|\.py/i.test(text) ? "python" :
          /\bc#|csharp|\.cs\b/i.test(text) ? "csharp" : "";
        prompt = `
${artPack ? artPack + "\n\n" : ""}
Context:
${microSummary}

Recent chat (compressed):
${dialogueBullets}

Task:
${text}

Rules:
- Return ONLY a single fenced code block${langHint ? " with language: " + langHint : ""}.
- No plans, no explanations, no <final> tags, no prose.
- If making edits, return the full revised file in the fence.
`;
      } else {
        const planRequest = /\b(plan|planning|roadmap|steps|strategy|how should I plan|help me plan|make a plan|give me a plan)\b/i.test(text);
        const formatHint = planRequest
          ? "MODE: PLAN. Return a brief plan, next 1–3 actions, and blockers."
          : "MODE: DIRECT. Output only the requested content. ≤120 words unless asked otherwise.";
        prompt = `Project snapshot for quick context:
${microSummary}

Recent chat (compressed):
${dialogueBullets}

${recentTranscript ? `Transcript:\n${recentTranscript}\n` : ""}
The user asks: ${text}

${formatHint}
Respond only inside <final>...</final>.
<final>`;
      }

      abortController.current = new AbortController();
      // const llmMsg: Message = { role: "llm", text: "" };
      // updateTabMessages(activeTab, msgs => [...msgs, llmMsg]);

      // --- Buffered streaming for smooth UI ---
      let fullText = "";
      let buffer = "";
      let lastFlush = 0;
      let done = false;
      let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
      let streamTimeout: NodeJS.Timeout | null = null;
      const fetchOptions = {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model,
          prompt,
          stream: true,
          keep_alive: "90s",
          ...(model === "iris-coder:latest" ? {} : { options })
        }),
        signal: controller.signal
      };

      function flush() {
        if (!buffer) return;
        fullText += buffer;
        buffer = "";
        updateTabMessages(activeTab, msgs => {
          const copy = [...msgs];
          for (let i = copy.length - 1; i >= 0; --i) {
            if (copy[i].role === "llm") {
              copy[i] = { ...copy[i], text: fullText };
              break;
            }
          }
          return copy;
        });
      }

      const doStream = async (batchOverride?: number) => {
        let llmInserted = false;
        let acc="";
        if (batchOverride && model !== "iris-coder:latest") {
          fetchOptions.body = JSON.stringify({
            model,
            prompt,
            stream: true,
            keep_alive: "90s",
            options: { ...options, num_batch: batchOverride }
          });
        }
        // Abort if headers don't arrive in time
        // const prefetchAbort = setTimeout(() => {
        //   if (abortController.current) abortController.current.abort();
        // }, 15000);
        
        // Debug: fetch start
        console.info("[chat] fetch → /api/generate", { model, coderish });


        // Use 127.0.0.1
        const response = await fetch(OLLAMA_URL, fetchOptions);
        if (!response.ok || !response.body) throw new Error("No response body from Ollama");
        // clearTimeout(prefetchAbort);
        setIrisStatus(coderish ? "coding" : "responding");

        reader = response.body.getReader();
        fullText = "";
        buffer = "";
        done = false;

        let gotFirstChunk = false;
        let timedOut = false;
        let lastFlush = performance.now();

        // 600s first-byte timeout
        // streamTimeout = setTimeout(() => {
        //   if (!gotFirstChunk && abortController.current) {
        //     timedOut = true;
        //     abortController.current.abort();
        //   }
        // }, 600000);

        // 60s overall watchdog
        // const overallWatchdog = setTimeout(() => {
        //   if (abortController.current) abortController.current.abort();
        // }, 60000);

        const flush = () => {
          if (!llmInserted || !buffer) return;
          fullText += buffer;
          updateTabMessages(activeTab, msgs => {
            const copy = [...msgs];
            for (let i = copy.length - 1; i >= 0; --i) {
              if (copy[i].role === "llm") { copy[i] = { ...copy[i], text: fullText }; break; }
            }
            return copy;
          });
          buffer = "";
        };

        while (!done) {
          const { value, done: doneReading } = await reader.read();
          done = doneReading;
          if (value) {
            const chunk = new TextDecoder().decode(value);
            acc += chunk;

            // parse all complete JSON objects in acc by balancing braces
            for (;;) {
              let depth = 0, end = -1;
              for (let i = 0; i < acc.length; i++) {
                const c = acc[i];
                if (c === '{') depth++;
                else if (c === '}') { depth--; if (depth === 0) { end = i; break; } }
              }
              if (end === -1) break;

              const objStr = acc.slice(0, end + 1);
              acc = acc.slice(end + 1);

              try {
                const json = JSON.parse(objStr);

                if (!gotFirstChunk) {
                  gotFirstChunk = true;
                  // Debug: first JSON chunk
                  console.info("[chat] first JSON chunk");

                  // if (streamTimeout) clearTimeout(streamTimeout);
                  setIrisStatus(coderish ? "coding" : "responding");
                }

                if (json.response) {
                  buffer += json.response;

                  // Insert the LLM bubble on the first actual token
                  if (!llmInserted && buffer.length > 0) {
                    llmInserted = true;
                    fullText = buffer;
                    buffer = "";
                                  // Debug: inserting llm bubble
                    console.info("[chat] inserting llm bubble");

                    updateTabMessages(activeTab, msgs => [...msgs, { role: "llm", text: fullText }]);
                  }

                  const now = performance.now();
                  if (llmInserted && now - lastFlush > 50) { 
                    // Debug: streaming flush
                    console.info("[chat] streaming flush");

                    flush(); 
                    lastFlush = now; }
                }
              } catch {
                // ignore and continue accumulating
              }
            }
          }
        }

        flush();
        // if (streamTimeout) clearTimeout(streamTimeout);
        // clearTimeout(overallWatchdog);

        // Retry with smaller batch once if we got no text (non-coder path)
        if (!fullText && model !== "iris-coder:latest" && !timedOut) {
          await doStream(4);
        }
      };

      try {
        await doStream();
      } catch (err) {
        // 5) Guard all early exits so status resets
        setThinking(false);
        setIsGenerating(false);
        setIrisStatus("idle");
        setRespondingTab(null);
        setIsSummarizing(false);

        const msg =
          (err as any)?.name === "AbortError"
            ? "[Timeout waiting for model output. If this was the first run, Ollama may still be pulling the model. Try again in 1–2 minutes.]"
            : `[Error: ${err instanceof Error ? err.message : String(err)}]`;

        updateTabMessages(activeTab, msgs => [...msgs, { role: "llm", text: msg }]);
        return;
      }

      // --- Artifact extraction and filename sniff ---
      let deliveredText = fullText.replace(/<\/?final>/gi, "").trim();
      let artifacts = extractArtifacts(deliveredText);
      const filenameMatch = deliveredText.match(/(?:here's|file|save as|edit)\s+`([^`]+)`/i);
      if (filenameMatch && artifacts[0]) artifacts[0].filename = filenameMatch[1];

      // Robust code handoff: If we used organizer, and the output is NOT a code block but looks like a code request, call coder
      if (!coderish) {
        const hasCodeBlock = /```[\s\S]*?```/m.test(deliveredText);
        const wantsCode = /\b(implement|write|generate|produce|create).{0,40}(script|function|class|code|gdscript|python|c#|typescript|shader|sql)\b/i.test(deliveredText);
        if (!hasCodeBlock && wantsCode) {
          setIrisStatus("coding");
          try {
            langHint =
              /\bgdscript|godot|\.gd\b/i.test(text) ? "gdscript" :
              /\btypescript|\.ts\b/i.test(text) ? "ts" :
              /\bpython|\.py/i.test(text) ? "python" :
              /\bc#|csharp|\.cs\b/i.test(text) ? "csharp" : "";
            const coderPrompt = `
[Persona]
You are Iris-Coder, an expert code generator.

[Micro Summary]
${microSummary}

[User Request]
${text}
`;
            const coderRes = await fetch("http://127.0.0.1:11434/api/generate", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                model: "iris-coder:latest",
                prompt: coderPrompt,
                stream: false,
                keep_alive: "90s"
              })
            });
            const coderJson = await coderRes.json();
            let codeText = (coderJson?.response || "").replace(/<\/?final>/gi, "").trim();
            deliveredText = codeText ? codeText : "[Coder did not return any code.]";
            artifacts = extractArtifacts(deliveredText);
            const fallbackFilenameMatch = deliveredText.match(/(?:here's|file|save as|edit)\s+`([^`]+)`/i);
            if (fallbackFilenameMatch && artifacts[0]) artifacts[0].filename = fallbackFilenameMatch[1];
          } catch (err) {
            deliveredText = "[Error: Coder model failed to respond.]";
          }
        }
      }

      updateTabMessages(activeTab, msgs => {
        const copy = [...msgs];
        for (let i = copy.length - 1; i >= 0; --i) {
          if (copy[i].role === "llm") {
            copy[i] = { ...copy[i], text: deliveredText };
            break;
          }
        }
        return copy;
      });

      setIrisStatus("summarizing");

      // 6) Ensure summarizer exists before Promise.all
      await invoke<string>("ensure_model", { name: SUMMARY_MODEL }).catch(()=>{});

      // --- Summarization and memory update ---
      const microSummaryPrompt = `
Summarize the following last exchange and the current project state in 1-2 sentences for fast recall.
Last exchange:
User: ${text}
Iris: ${deliveredText}
Project state: [Describe the current state here, e.g., "middle of planning", "polishing", etc.]
`;

      const projectSummaryPrompt = `
Here is the current summary of the conversation:
${microSummary}

Here is the latest user message:
${text}

Here is your latest response:
${deliveredText}

If the summary needs to be updated, return the new summary. If not, return the current summary unchanged.
`;

      const dialogueBulletsPrompt = `Summarize the following chat into <=6 bullets preserving names, files, decisions:\nUser: ${text}\nIris: ${deliveredText}`;

      Promise.all([
        fetch("http://127.0.0.1:11434/api/generate", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            model: SUMMARY_MODEL,
            prompt: microSummaryPrompt,
            stream: false,
            keep_alive: "90s",
          }),
        }).then(res => res.json()).then(data => data.response || microSummary),

        fetch("http://127.0.0.1:11434/api/generate", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            model: SUMMARY_MODEL,
            prompt: projectSummaryPrompt,
            stream: false,
            keep_alive: "90s",
          }),
        }).then(res => res.json()).then(data => data.response || microSummary),

        fetch("http://127.0.0.1:11434/api/generate", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            model: SUMMARY_MODEL,
            prompt: dialogueBulletsPrompt,
            stream: false,
            keep_alive: "5m"
          }),
        }).then(res => res.json()).then(data => (data?.response || "").toString().trim())
      ])
      .then(([newMicroSummary, newProjectSummary, dialogueBullets]) => {
        const tabStillExists = tabs.some(tab => tab.id === activeTab);
        if (tabStillExists) {
          const now = Math.floor(Date.now() / 1000);
          const artifactsWithTs = artifacts.map((a: Artifact) => ({ ...a, ts: now }));

          invoke("update_tab_memory", {
            tabId: activeTab,
            summary: newProjectSummary,
            microSummary: newMicroSummary,
            dialogueBullets,
            newMessage: `User: ${text}\nIris: ${deliveredText}`,
            artifacts: artifactsWithTs,
          });
        }
      })
      .finally(() => {
        if (isMounted.current) {
          setIsSummarizing(false);
          setIrisStatus("idle");
          setRespondingTab(null);
        }
      });
    } catch (err: any) {
      // 5) Guard all early exits so status resets
      setThinking(false);
      setIsGenerating(false);
      setIrisStatus("idle");
      setRespondingTab(null);
      setIsSummarizing(false);

      updateTabMessages(activeTab, msgs => [
        ...msgs,
        { role: "llm", text: `[Error: ${err?.message || err}]` }
      ]);
    } finally {
      // 5) Keep these resets in finally
      setThinking(false);
      setThinkingModel(null);
      setIsGenerating(false);
      abortController.current = null;
    }
  }

  function handleStop(e: React.FormEvent) {
    e.preventDefault();
    if (abortController.current) {
      abortController.current.abort();
      setIsGenerating(false);
      setIrisStatus("idle");
      setRespondingTab(null);
      setIsSummarizing(false);
    }
  }

  async function handleNewTab() {
    const chatTabs = tabs.filter(tab => tab.type === "chat");
    if (chatTabs.length >= 8) return;

    const settingsIdx = tabs.findIndex(tab => tab.type === "settings");
    const newId = tabs.length ? Math.max(...tabs.map(t => t.id)) + 1 : 1;
    const newTabNumber = chatTabs.length + 1;
    const newTab: Tab = { id: newId, title: `Tab #${newTabNumber}`, type: "chat", messages: [] };

    if (settingsIdx === -1) {
      setTabs([...tabs, newTab]);
      setActiveTab(newId);
      await invoke("create_tab_memory", { tabId: newId });
    } else {
      const newTabs = [...tabs];
      newTabs.splice(settingsIdx, 0, newTab);
      setTabs(newTabs);
      setActiveTab(newId);
      await invoke("create_tab_memory", { tabId: newId });
    }
  }

  const [settingsTab, setSettingsTab] = useState<"General" | "Repos" |"MCPs" | "LLMs" | "Controller">("General");

  function handleSettings() {
    let settingsTab = tabs.find(tab => tab.type === "settings");
    if (settingsTab) {
      // Move settings tab to end if not already
      if (tabs[tabs.length - 1].type !== "settings") {
        setTabs([
          ...tabs.filter(tab => tab.type !== "settings"),
          settingsTab
        ]);
      }
      setActiveTab(settingsTab.id);
    } else {
      if (tabs.length >= 10) return;
      const newId = tabs.length ? Math.max(...tabs.map(t => t.id)) + 1 : 1;
      const newSettingsTab: Tab = { id: newId, title: "Settings", type: "settings" };
      setTabs([...tabs, newSettingsTab]);
      setActiveTab(newId);
    }
  }

  async function handleClose() {
    await invoke("close_window");
  }

  async function handleCloseTab(tabId: number) {
    await invoke("close_tab_and_snapshot", { tabId });
    setTabs(prevTabs => {
      const idx = prevTabs.findIndex(tab => tab.id === tabId);
      let newTabs = prevTabs.filter(tab => tab.id !== tabId);
      // Move settings tab to end if it exists
      const settingsTab = newTabs.find(tab => tab.type === "settings");
      if (settingsTab && newTabs[newTabs.length - 1].type !== "settings") {
        newTabs = [
          ...newTabs.filter(tab => tab.type !== "settings"),
          settingsTab
        ];
      }
      // If the closed tab was active, activate the previous tab or the first one
      if (tabId === activeTab && newTabs.length > 0) {
        const newIdx = idx > 0 ? idx - 1 : 0;
        setActiveTab(newTabs[newIdx].id);
      }
      return newTabs;
    });
  }

  useEffect(() => {
    function handleClick(e: MouseEvent) {
      if (
        taskbarRef.current &&
        !taskbarRef.current.contains(e.target as Node)
      ) {
        setOpenMenu(null);
      }
    }
    if (openMenu) {
      window.addEventListener("mousedown", handleClick);
      return () => window.removeEventListener("mousedown", handleClick);
    }
  }, [openMenu]);

  function autoResize() {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    const singleLineHeight = 24;
    const maxHeight = singleLineHeight * 8;
    el.style.height = Math.min(el.scrollHeight, maxHeight) + "px";
  }

  const MODEL_STATUS_MESSAGES: Record<string, string> = {
    "iris-coder": "iris_coder is working",
    "iris-organizer": "iris_organizer is thinking",
    "iris-summarizer": "iris_summarizer is summarizing",
    "iris-vision": "iris_vision is looking"
  };

  const STATUS_MESSAGES = {
    thinking: "Iris is thinking...",
    summarizing: "Iris is summarizing...",
    responding: "Iris is responding...",
    coding: "Iris is coding...",
    responding_elsewhere: "Iris is responding in a different chat..."
  };

  return (
    <div className="chat-root">
      <div className="taskbar" ref={taskbarRef}>
        <div className="menu">
          <button
            className="menu-btn"
            onClick={() => setOpenMenu(openMenu === "file" ? null : "file")}
          >
            File ▾
          </button>
          {openMenu === "file" && (
            <div className="dropdown file-dropdown" onClick={e => e.stopPropagation()}>
              <div className="dropdown-item" onClick={handleNewTab}>New tab</div>
              <div className="dropdown-item" onClick={handleClose}>Close</div>
            </div>
          )}
        </div>
        <div className="menu">
          <button
            className="menu-btn"
            onClick={() => setOpenMenu(openMenu === "options" ? null : "options")}
          >
            Options ▾
          </button>
          {openMenu === "options" && (
            <div className="dropdown options-dropdown" onClick={e => e.stopPropagation()}>
              <div className="dropdown-item" onClick={handleSettings}>Settings</div>
            </div>
          )}
        </div>
        <span className="text-xs" style={{ marginLeft: 16 }}>
          Coder: {coderReady === null ? "…" : coderReady ? "ready" : "missing"}
        </span>
      </div>

      <div className="tab-bar">
        {tabs.map(tab => (
          <div key={tab.id} className={`tab-wrapper${tab.id === activeTab ? " active" : ""}`}>
            <button
              className={tab.id === activeTab ? "tab active" : "tab"}
              onClick={() => setActiveTab(tab.id)}
            >
              {tab.title}
            </button>
            {tabs.length > 1 && (
              <button
                className="tab-close"
                onClick={e => {
                  e.stopPropagation();
                  handleCloseTab(tab.id);
                }}
                title="Close tab"
              >
                ×
              </button>
            )}
          </div>
        ))}
      </div>

      <header className="chat-header">
        <h2>
          {currentTab?.type === "settings" ? "Iris • Settings" : "Iris • Chat"}
        </h2>
        {currentTab?.type === "settings" && (
          <div className="settings-subtab-bar">
            {["General", "Repos", "MCPs", "LLMs", "Controller"].map(subtab => (
              <button
                key={subtab}
                className={settingsTab === subtab ? "settings-subtab active" : "settings-subtab"}
                onClick={() => setSettingsTab(subtab as any)}
              >
                {subtab}
              </button>
            ))}
          </div>
        )}
        {modelStatus === "checking" && (
          <span style={{ color: '#aaa', fontSize: 14 }}>Checking model...</span>
        )}
        {modelStatus === "loading" && (
          <span style={{ color: '#aaa', fontSize: 14 }}>Loading model...</span>
        )}
        {modelStatus === "error" && (
          <span style={{ color: 'red', fontSize: 14 }}>Model error!</span>
        )}
      </header>
      {currentTab?.type === "chat" && irisStatus !== "idle" && (
        <div className="thinking-indicator" style={{ marginTop: 12 }}>
          <strong>
            {respondingTab !== activeTab
              ? STATUS_MESSAGES.responding_elsewhere
              : irisStatus === "coding"
                ? STATUS_MESSAGES.coding
                : irisStatus === "responding"
                  ? STATUS_MESSAGES.responding
                  : irisStatus === "thinking"
                    ? STATUS_MESSAGES.thinking
                    : irisStatus === "summarizing"
                      ? STATUS_MESSAGES.summarizing
                      : ""}
            {ellipsis}
          </strong>
        </div>
      )}

      {currentTab?.type === "chat" && (
        <div className="chat-history" ref={historyRef}>
          {(currentTab.messages || []).map((msg, idx) => (
            <div className={`bubble ${msg.role}`} key={idx}>
              <strong>
                {msg.role === "user" ? "You:" : "Iris:"}
              </strong>{" "}
              <ReactMarkdown>{msg.text}</ReactMarkdown>
            </div>
          ))}
        </div>
      )}
      {currentTab?.type === "settings" && (
        <div className="settings-container">
        </div>
      )}

      {currentTab?.type === "chat" && (
        <form
          className="chat-input-row"
          onSubmit={isGenerating ? handleStop : sendMessage}
          autoComplete="off"
        >
          <textarea
            className="chat-input"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder="Type your message..."
            autoFocus
            disabled={isGenerating}
            rows={1}
            style={{ resize: "none" }}
            ref={inputRef}
            onInput={autoResize}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                if (input.trim()) sendMessage(e);
              }
            }}
          />
          <label className="inline-flex items-center gap-2 text-sm opacity-80" style={{ marginLeft: 8, marginRight: 8 }}>
            <input type="checkbox" checked={useCoder} onChange={(e)=>setUseCoder(e.target.checked)} />
            Use Coder
          </label>
          <button
            type="button"
            className="px-2 py-1 text-sm border rounded"
            style={{ marginRight: 8 }}
            onClick={async ()=>{
              try {
                const j = await invoke<TestModelResult>("test_model", {
                  model: "iris-coder:latest",
                  prompt: "Write a TS function add(a:number,b:number):number"
                });
                console.log("Test coder result:", j);
                alert(j.ok ? "Coder ✓ responding" : `Coder ✗ not responding: ${j.error ?? "unknown error"}`);
              } catch (e:any) {
                alert("Coder ✗ not responding: " + e?.message);
              }
            }}
          >
            Test Coder
          </button>
          <button
            className="send-btn"
            type="submit"
            disabled={
              modelStatus === "checking" ||
              isGenerating ||
              (respondingTab !== null && respondingTab !== activeTab && !isGenerating)
            }
          >
            {isGenerating ? "Stop" : "Send"}
          </button>
        </form>
      )}
    </div>
  );
}

export default App;
