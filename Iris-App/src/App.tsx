import { useState, useRef, useEffect } from "react";
import ReactMarkdown from 'react-markdown';
import "./App.css";

import {
  OLLAMA_URL,
  ensureOllamaServer,
  ensureModel,
  fetchJson,
  // waitForTauri,
} from "./lib/ollama";
import {
  createTabMemory,
  updateTabMemory,
  restoreFullTabMemory,
  listOpenTabs,
} from "./state/memory";
import {
  updateMessagesAppendUser,
  insertLLMBubble,
  patchLastLLMBubble,
  extractArtifacts,
} from "./state/tabs";
import useOllamaStream from "./hooks/useOllamaStream";
import { summarizeExchange } from "./lib/summarize";

declare global {
  interface Window { __TAURI__?: any }
}

const SUMMARY_MODEL = "iris-summarizer:latest";

type ModelStatus = "checking" | "ready" | "loading" | "error";
type Message = { role: "user" | "llm"; text: string };

function normalizeMessages(raw: any[] | undefined): Message[] {
  const out: Message[] = [];
  for (const m of raw || []) {
    const role = (m.role || "").toLowerCase();
    const text = String(m.text ?? "");
    if (role === "user" || role === "llm") {
      out.push({ role, text });
      continue;
    }
    // Legacy transcript that merges both speakers into one block:
    const re = /^User:\s*([\s\S]*?)\nIris:\s*([\s\S]*)$/i;
    const match = text.match(re);
    if (match) {
      const u = match[1].trim();
      const a = match[2].trim();
      if (u) out.push({ role: "user", text: u });
      if (a) out.push({ role: "llm", text: a });
    } else {
      // Fallback: treat as LLM message to keep render safe
      out.push({ role: "llm", text });
    }
  }
  return out;
}

type Tab = {
  id: number;
  title: string;
  type: "chat" | "settings";
  messages?: Message[];
};
type TestModelResult = { ok: boolean; error: string | null; response?: string };
type Artifact = { lang: string; filename?: string; content: string; ts?: number };
type Snapshot = {
  title: string;
  messages: { role: "user" | "llm"; text: string }[];
  microSummary: string;
  dialogueBullets: string;
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

function App() {
  const [modelStatus, setModelStatus] = useState<ModelStatus>("checking");
  const [coderReady, setCoderReady] = useState<boolean | null>(null);
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
    setModelStatus("checking");
    // waitForTauri().then(() => {
      Promise.all([
        ensureModel("iris-coder:latest")
          .then(() => setCoderReady(true))
          .catch(e => { setCoderReady(false); console.error("ensure_model", e); }),
        // universal prompts and open tabs
        listOpenTabs()
          .then(async (snaps) => {
            if (!Array.isArray(snaps) || snaps.length === 0) return;
            const capped = snaps.slice(-8);
            let nextId = 1;
            const newTabs: Tab[] = capped.map((snap, i) => ({
              id: nextId++,
              title: snap.title && snap.title.trim() ? snap.title : `Tab #${nextId - 1}`,
              type: "chat",
              messages: normalizeMessages(snap.messages)
            }));
            setTabs(newTabs);
            setActiveTab(newTabs[newTabs.length - 1].id);
            for (let i = 0; i < newTabs.length; ++i) {
              const tab = newTabs[i];
              const snap = capped[i];
              await restoreFullTabMemory({
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
          })
          .catch(e => { console.error("list_open_tabs", e); })
      ])
        .then(() => setModelStatus("ready"))
        .catch(() => setModelStatus("error"));
    // }).catch(e => {
    //   setModelStatus("error");
    //   console.error("Tauri never became ready", e);
    // });
  }, []);

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
          // You may want to modularize this as well
          if (window.__TAURI__?.invoke) {
            const snap = await window.__TAURI__?.invoke("restore_last_closed_tab") as Snapshot;
            if (!snap || !snap.messages) return;
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
            await createTabMemory(newId);
            await updateTabMemory({
              tabId: newId,
              summary: snap.summary,
              microSummary: snap.microSummary,
              dialogueBullets: snap.dialogueBullets,
              newMessage: snap.messages.map((m: {role: "user" | "llm"; text: string}) => `${m.role === "user" ? "User" : "Iris"}: ${m.text}`).join("\n"),
              artifacts: snap.artifacts
            });
          }
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
  const [irisStatus, setIrisStatus] = useState<"idle" | "thinking" | "summarizing" | "responding" | "coding">("idle");
  const [respondingTab, setRespondingTab] = useState<number | null>(null);

  const { stream } = useOllamaStream();

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

    const coderish = useCoder || isCoderIntent(text);
    await ensureOllamaServer();
    const primaryModel = coderish ? "iris-coder:latest" : "iris-organizer:latest";
    await ensureModel(primaryModel);

    // 3) Flip UI to “thinking” and block Send
    setThinking(true);
    setRespondingTab(activeTab);
    setIrisStatus("thinking");
    setIsGenerating(true);

    updateTabMessages(activeTab, msgs => updateMessagesAppendUser(msgs, text));

    try {
      // get_compiled_context is still a tauri invoke, not modularized yet
      try {
        if (window.__TAURI__?.invoke) {

          const result = await window.__TAURI__?.invoke("get_compiled_context", { tabId: activeTab, tokenBudget: 1200 }) as typeof compiled;
          if (result && typeof result === "object") {
            compiled = {
              microSummary: result.microSummary ?? "",
              dialogueBullets: result.dialogueBullets ?? "",
              recentTranscript: result.recentTranscript ?? "",
              recentArtifacts: result.recentArtifacts ?? []
            };
          }
        }
      } catch (e) {
        console.error("get_compiled_context failed", e);
      }

      const { microSummary, dialogueBullets: compiledDialogueBullets, recentTranscript, recentArtifacts } = compiled;

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
${compiledDialogueBullets}

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
${compiledDialogueBullets}

${recentTranscript ? `Transcript:\n${recentTranscript}\n` : ""}
The user asks: ${text}

${formatHint}
Respond only inside <final>...</final>.
<final>`;
      }

      let fullText = "";
      let streamedText = "";

      await stream({
        model,
        prompt,
        options,
        signal: controller.signal,
        onHeaders: () => setIrisStatus(coderish ? "coding" : "responding"),
        onFirstToken: (first) => {
          streamedText += first;
          updateTabMessages(activeTab, msgs => insertLLMBubble(msgs, first));
        },
        onTokens: (delta) => {
          streamedText += delta;
          updateTabMessages(activeTab, msgs => patchLastLLMBubble(msgs, delta));
        },
        onDone: () => {
          // No message replacement here; streamedText already has the full reply
        },
      });

      // --- Artifact extraction and filename sniff ---
      fullText = streamedText;
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
            const coderRes = await fetchJson(OLLAMA_URL, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                model: "iris-coder:latest",
                prompt: coderPrompt,
                stream: false,
                keep_alive: "90s"
              })
            });
            let codeText = (coderRes?.response || "").replace(/<\/?final>/gi, "").trim();
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

      await ensureModel(SUMMARY_MODEL);

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

      const { microSummary: newMicroSummary, projectSummary: newProjectSummary, dialogueBullets: newDialogueBullets } =
        await summarizeExchange({ model: SUMMARY_MODEL, microSummaryPrompt, projectSummaryPrompt, dialogueBulletsPrompt });

      const now = Math.floor(Date.now() / 1000);
      const artifactsWithTs = artifacts.map((a: Artifact) => ({ ...a, ts: now }));

      await updateTabMemory({
        tabId: activeTab,
        summary: newProjectSummary,
        microSummary: newMicroSummary,
        dialogueBullets: newDialogueBullets,
        newMessage: `User: ${text}\nIris: ${deliveredText}`,
        artifacts: artifactsWithTs,
      });

      setIsSummarizing(false);
      setIrisStatus("idle");
      setRespondingTab(null);

    } catch (err: any) {
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
      await createTabMemory(newId);
    } else {
      const newTabs = [...tabs];
      newTabs.splice(settingsIdx, 0, newTab);
      setTabs(newTabs);
      setActiveTab(newId);
      await createTabMemory(newId);
    }
  }

  const [settingsTab, setSettingsTab] = useState<"General" | "Repos" |"MCPs" | "LLMs" | "Controller">("General");

  function handleSettings() {
    let settingsTab = tabs.find(tab => tab.type === "settings");
    if (settingsTab) {
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
    if (window.__TAURI__?.invoke) {
      await window.__TAURI__?.invoke("close_window");
    }
  }

  async function handleCloseTab(tabId: number) {
    await window.__TAURI__?.invoke("close_tab_and_snapshot", { tabId });
    setTabs(prevTabs => {
      const idx = prevTabs.findIndex(tab => tab.id === tabId);
      let newTabs = prevTabs.filter(tab => tab.id !== tabId);
      const settingsTab = newTabs.find(tab => tab.type === "settings");
      if (settingsTab && newTabs[newTabs.length - 1].type !== "settings") {
        newTabs = [
          ...newTabs.filter(tab => tab.type !== "settings"),
          settingsTab
        ];
      }
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

  const lastSentRef = useRef<string>("");

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
            onPaste={(e) => {
              const raw = e.clipboardData.getData("text");
              if (!raw) return;
              e.preventDefault();
              let norm = raw.replace(/\r\n/g, "\n");
              norm = norm.replace(/[ \t]+$/gm, "");
              norm = norm.replace(/\n{3,}/g, "\n\n");
              norm = norm.replace(/\n+$/, "\n");
              const el = e.currentTarget;
              const start = el.selectionStart ?? 0;
              const end = el.selectionEnd ?? 0;
              setInput(prev => {
                const next = prev.slice(0, start) + norm + prev.slice(end);
                queueMicrotask(() => {
                  const pos = start + norm.length;
                  el.selectionStart = el.selectionEnd = pos;
                  if (inputRef.current) {
                    inputRef.current.style.height = "auto";
                    const single = 24, maxH = single * 8;
                    inputRef.current.style.height = Math.min(inputRef.current.scrollHeight, maxH) + "px";
                  }
                });
                return next;
              });
            }}
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
                if (window.__TAURI__?.invoke) {
                  const j = await window.__TAURI__?.invoke("test_model", {
                    model: "iris-coder:latest",
                    prompt: "Write a TS function add(a:number,b:number):number"
                  }) as TestModelResult;
                  console.log("Test coder result:", j);
                  alert(j.ok ? "Coder ✓ responding" : `Coder ✗ not responding: ${j.error ?? "unknown error"}`);
                } else {
                  alert("Coder ✗ not responding: Tauri not available");
                }
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