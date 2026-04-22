import { useState, useRef, useEffect } from "react";
import ReactMarkdown from 'react-markdown';
import { invoke } from '@tauri-apps/api/core';
import { openUrl } from '@tauri-apps/plugin-opener';
import "./App.css";
import type { ChatMessage, Artifact } from "./types/models";


import {
  ensureOllamaServer,
  ensureModel,
  // waitForTauri,
} from "./lib/ollama";
import {
  listOpenTabs,
  readTabSnapshot, 
  persistSnapshot,
  migrateOpenTabsToSnapshotFormat,
  sanitizeTabTitles
} from "./state/memory";
import {
  updateMessagesAppendUser,
  insertLLMBubble,
  patchLastLLMBubble,
  extractArtifacts,
} from "./state/tabs";
import useOllamaStream from "./hooks/useOllamaStream";
import OllamaSetupModal from "./components/OllamaSetupModal";
import { summarizeExchange } from "./lib/summarize";

declare global {
  interface Window { __TAURI__?: any }
}


const SUMMARY_MODEL = "iris-summarizer:latest";
const DEBUG_MEMORY = false;

type ModelStatus = "checking" | "ready" | "loading" | "error";
type Message = { role: "user" | "llm"; text: string; images?: string[] };
type StartupStatus = {
  active: boolean;
  step: string;
  progress: number;
};

type CenterDialogState = {
  open: boolean;
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
};

function normalizeMessages(raw: any[] | undefined): Message[] {
  const out: Message[] = [];
  for (const m of raw || []) {
    const role = (m.role || "").toLowerCase();
    const text = String(m.text ?? "");
    if (role === "user" || role === "llm") {
      const images = Array.isArray((m as any).images)
        ? (m as any).images.filter((v: any) => typeof v === "string")
        : undefined;
      out.push({ role, text, ...(images && images.length ? { images } : {}) });
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
  promptHistory?: string[];
};
type TestModelResult = { ok: boolean; error: string | null; response?: string };
type Snapshot = {
  tab_id?: number;
  title: string;
  messages: { role: "user" | "llm"; text: string }[];
  associatedProjectId?: string | null;
  microSummary: string;
  dialogueBullets: string;
  summary: string;
  artifacts: any[];
  promptHistory?: string[];
  last_updated?: number;
};

type RepoEntry = {
  id: string;
  name: string;
  path: string;
  isDir: boolean;
  sizeBytes: number;
};

type RepoFolder = {
  id: string;
  name: string;
  path: string;
  enabled: boolean;
  entries: RepoEntry[];
  selectedEntryIds: string[];
};

type McpConnection = {
  id: string;
  name: string;
  target: string;        // URL (http://...) or stdio command string or JSON
  enabled: boolean;
  notes: string;
  // optional parsed metadata (populated on save)
  connectionType?: "url" | "stdio";
  launchCommand?: string;
  launchArgs?: string[];
};

type SshConnection = {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  privateKeyPath: string;
  knownHostsPath: string;
  remoteRoot: string;
  strictHostKeyChecking: boolean;
  extraArgs: string[];
  enabled: boolean;
  notes: string;
};

type WindowInfo = { id: number; title: string; processName: string };

type RoutineStepType = "screenshot" | "vision" | "coder" | "llm_reply";
type RoutineStepStatus = "pending" | "running" | "done" | "error" | "skipped";
type RoutineStep = {
  id: string;
  type: RoutineStepType;
  label: string;
  status: RoutineStepStatus;
  result?: string;
  imageData?: string;
  error?: string;
};
type ActiveRoutine = {
  id: string;
  goal: string;
  steps: RoutineStep[];
  isLongRunning: boolean;
  status: "pending_confirm" | "running" | "done" | "cancelled" | "error";
};

type WorkLogEntry = {
  ts: number;
  text: string;
};

type McpToolDescriptor = {
  mcpId: string;
  mcpName: string;
  connectionType: "url" | "stdio";
  target: string;
  command?: string;
  args?: string[];
  toolName: string;
  description: string;
  inputSchema?: any;
};

type ParsedExplicitMcpCall = {
  toolName: string;
  args: Record<string, any>;
};

type ParsedExplicitFsCall = {
  action: "list" | "read" | "write" | "delete" | "move" | "mkdir";
  args: Record<string, any>;
};

type ParsedExplicitSshCall = {
  action: "list" | "read" | "write" | "delete" | "move" | "mkdir" | "exec";
  args: Record<string, any>;
};

type ParsedExplicitDesktopCall = {
  action: "list" | "read" | "write" | "delete" | "move" | "mkdir";
  args: Record<string, any>;
};

type ParsedExplicitProjectCall = {
  action: "checkpoint" | "checkpoints" | "restore";
  args: Record<string, any>;
};

type InferredActions = {
  fs: ParsedExplicitFsCall | null;
  ssh: ParsedExplicitSshCall | null;
  desktop: ParsedExplicitDesktopCall | null;
  mcp: ParsedExplicitMcpCall | null;
  project: ParsedExplicitProjectCall | null;
};

type IrisActionTrace = {
  id: string;
  ts: number;
  action: string;
  reason: string;
  argsSummary: string;
  outcome: string;
};

const IRIS_ACTION_REPOSITORY = [
  "filesystem.list/read/write/delete/move/mkdir",
  "ssh.list/read/write/delete/move/mkdir/exec",
  "desktop.list/read/write/delete/move/mkdir",
  "mcp.connect/list_tools/call_tool",
  "project.checkpoint/checkpoints/restore",
  "network.search/weather",
] as const;

function parseExplicitMcpCall(text: string): ParsedExplicitMcpCall | null {
  const t = text.trim();
  const slash = t.match(/^\/mcp\s+([A-Za-z0-9_.:-]+)(?:\s+(\{[\s\S]*\}))?$/i);
  const verbose = t.match(/(?:^|\n)\s*(?:use\s+)?mcp\s+tool\s+([A-Za-z0-9_.:-]+)(?:\s+(\{[\s\S]*\}))?\s*$/i);
  const m = slash || verbose;
  if (!m) return null;
  const toolName = String(m[1] || "").trim();
  if (!toolName) return null;
  const rawArgs = String(m[2] || "").trim();
  if (!rawArgs) return { toolName, args: {} };
  try {
    const parsed = JSON.parse(rawArgs);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { toolName, args: parsed as Record<string, any> };
    }
  } catch {}
  return { toolName, args: {} };
}

function parseToolCallsFromText(text: string): Array<{ toolName: string; args: Record<string, unknown> }> {
  const calls: Array<{ toolName: string; args: Record<string, unknown> }> = [];
  const lines = String(text || "").split(/\r?\n/);
  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (!/^TOOL_CALL\s*:/i.test(line)) continue;
    const payload = line.replace(/^TOOL_CALL\s*:\s*/i, "").trim();
    if (!payload.startsWith("{")) continue;
    try {
      const parsed = JSON.parse(payload);
      const toolName = String(parsed?.tool || parsed?.toolName || "").trim();
      const args = parsed?.args || parsed?.arguments || {};
      if (!toolName) continue;
      if (!args || typeof args !== "object" || Array.isArray(args)) {
        calls.push({ toolName, args: {} });
      } else {
        calls.push({ toolName, args: args as Record<string, unknown> });
      }
    } catch {
      // Ignore malformed lines and keep scanning.
    }
  }
  return calls;
}

function resolveMcpToolMatch(catalog: McpToolDescriptor[], requestedToolName: string): McpToolDescriptor | null {
  const raw = String(requestedToolName || "").trim().toLowerCase();
  if (!raw) return null;
  const exact = catalog.find((t) => t.toolName.toLowerCase() === raw);
  if (exact) return exact;
  const withMcpName = catalog.find((t) => `${t.mcpName}.${t.toolName}`.toLowerCase() === raw);
  if (withMcpName) return withMcpName;
  const withMcpId = catalog.find((t) => `${t.mcpId}.${t.toolName}`.toLowerCase() === raw);
  if (withMcpId) return withMcpId;
  const suffix = catalog.find((t) => raw.endsWith(`.${t.toolName.toLowerCase()}`));
  return suffix || null;
}

function parseExplicitFsCall(text: string): ParsedExplicitFsCall | null {
  const t = text.trim();
  const slash = t.match(/^\/fs\s+(list|read|write|delete|move|mkdir)(?:\s+(\{[\s\S]*\}))?$/i);
  const verbose = t.match(/(?:^|\n)\s*fs\s+(list|read|write|delete|move|mkdir)(?:\s+(\{[\s\S]*\}))?\s*$/i);
  const m = slash || verbose;
  if (!m) return null;
  const action = String(m[1] || "").toLowerCase() as ParsedExplicitFsCall["action"];
  const rawArgs = String(m[2] || "").trim();
  if (!rawArgs) return { action, args: {} };
  try {
    const parsed = JSON.parse(rawArgs);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { action, args: parsed as Record<string, any> };
    }
  } catch {}
  return { action, args: {} };
}

function parseExplicitSshCall(text: string): ParsedExplicitSshCall | null {
  const t = text.trim();
  const slash = t.match(/^\/ssh\s+(list|read|write|delete|move|mkdir|exec)(?:\s+(\{[\s\S]*\}))?$/i);
  const verbose = t.match(/(?:^|\n)\s*ssh\s+(list|read|write|delete|move|mkdir|exec)(?:\s+(\{[\s\S]*\}))?\s*$/i);
  const m = slash || verbose;
  if (!m) return null;
  const action = String(m[1] || "").toLowerCase() as ParsedExplicitSshCall["action"];
  const rawArgs = String(m[2] || "").trim();
  if (!rawArgs) return { action, args: {} };
  try {
    const parsed = JSON.parse(rawArgs);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { action, args: parsed as Record<string, any> };
    }
  } catch {}
  return { action, args: {} };
}

function parseExplicitDesktopCall(text: string): ParsedExplicitDesktopCall | null {
  const t = text.trim();
  const slash = t.match(/^\/desktop\s+(list|read|write|delete|move|mkdir)(?:\s+(\{[\s\S]*\}))?$/i);
  const verbose = t.match(/(?:^|\n)\s*desktop\s+(list|read|write|delete|move|mkdir)(?:\s+(\{[\s\S]*\}))?\s*$/i);
  const m = slash || verbose;
  if (!m) return null;
  const action = String(m[1] || "").toLowerCase() as ParsedExplicitDesktopCall["action"];
  const rawArgs = String(m[2] || "").trim();
  if (!rawArgs) return { action, args: {} };
  try {
    const parsed = JSON.parse(rawArgs);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { action, args: parsed as Record<string, any> };
    }
  } catch {}
  return { action, args: {} };
}

function parseExplicitProjectCall(text: string): ParsedExplicitProjectCall | null {
  const t = text.trim();
  const slash = t.match(/^\/project\s+(checkpoint|checkpoints|restore)(?:\s+(\{[\s\S]*\}))?$/i);
  const verbose = t.match(/(?:^|\n)\s*project\s+(checkpoint|checkpoints|restore)(?:\s+(\{[\s\S]*\}))?\s*$/i);
  const m = slash || verbose;
  if (m) {
    const action = String(m[1] || "").toLowerCase() as ParsedExplicitProjectCall["action"];
    const rawArgs = String(m[2] || "").trim();
    if (!rawArgs) return { action, args: {} };
    try {
      const parsed = JSON.parse(rawArgs);
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        return { action, args: parsed as Record<string, any> };
      }
    } catch {}
    return { action, args: {} };
  }

  const lower = t.toLowerCase();
  if (/\brestore\b/.test(lower) && /\b(previous|prior|last|checkpoint|state)\b/.test(lower)) {
    return { action: "restore", args: {} };
  }
  if (/\b(checkpoint)\b/.test(lower) && /\b(save|create|capture|take|make)\b/.test(lower)) {
    return { action: "checkpoint", args: {} };
  }
  if (/\b(checkpoints?)\b/.test(lower) && /\b(list|show|view|what|which)\b/.test(lower)) {
    return { action: "checkpoints", args: {} };
  }
  return null;
}

function inferConversationalActions(text: string): InferredActions {
  const lower = text.toLowerCase();
  const jsonPayload = (() => {
    const m = text.match(/(\{[\s\S]*\})/);
    if (!m) return {} as Record<string, any>;
    try {
      const parsed = JSON.parse(m[1]);
      return parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? parsed as Record<string, any>
        : {};
    } catch {
      return {} as Record<string, any>;
    }
  })();
  const guessPath = (() => {
    const m = text.match(/(?:file|path|folder|directory)\s+([A-Za-z0-9_./\\:-]+)/i);
    return m ? m[1] : "";
  })();

  const project = parseExplicitProjectCall(text);

  let fs: ParsedExplicitFsCall | null = null;
  if (!fs && /(list|show).*(files|folders|directory)|what.*files/i.test(lower)) {
    fs = { action: "list", args: { path: guessPath || "." } };
  } else if (!fs && /(read|open|show).*(file|contents?)/i.test(lower)) {
    fs = { action: "read", args: { path: guessPath } };
  } else if (!fs && /(create|make).*(folder|directory)/i.test(lower)) {
    fs = { action: "mkdir", args: { path: guessPath } };
  } else if (!fs && /(delete|remove).*(file|folder|directory)/i.test(lower)) {
    fs = { action: "delete", args: { path: guessPath, recursive: true } };
  } else if (!fs && /(write|update|edit).*(file|contents?)/i.test(lower) && guessPath) {
    fs = { action: "write", args: { path: guessPath, content: String((jsonPayload as any).content || "") } };
  }

  let ssh: ParsedExplicitSshCall | null = null;
  if (/(ssh|remote server|raspberry pi|remote machine)/i.test(lower)) {
    if (/(run|execute|exec|command)/i.test(lower)) {
      ssh = { action: "exec", args: jsonPayload };
    } else if (/(list|show).*(files|folders|directory)/i.test(lower)) {
      ssh = { action: "list", args: { path: guessPath || ".", ...jsonPayload } };
    } else if (/(read|open|show).*(file|contents?)/i.test(lower)) {
      ssh = { action: "read", args: { path: guessPath, ...jsonPayload } };
    }
  }

  let desktop: ParsedExplicitDesktopCall | null = null;
  if (/(desktop|this computer|local machine|on my pc)/i.test(lower)) {
    if (/(list|show).*(files|folders|directory)/i.test(lower)) {
      desktop = { action: "list", args: { path: guessPath || ".", ...jsonPayload } };
    } else if (/(read|open|show).*(file|contents?)/i.test(lower)) {
      desktop = { action: "read", args: { path: guessPath, ...jsonPayload } };
    } else if (/(create|make).*(folder|directory)/i.test(lower)) {
      desktop = { action: "mkdir", args: { path: guessPath, ...jsonPayload } };
    }
  }

  let mcp: ParsedExplicitMcpCall | null = null;
  const toolMatch = text.match(/(?:mcp\s+tool|tool)\s+([A-Za-z0-9_.:-]+)/i);
  if (toolMatch && /(mcp|godot|bridge|server tool)/i.test(lower)) {
    mcp = {
      toolName: String(toolMatch[1] || "").trim(),
      args: jsonPayload,
    };
  }

  return { fs, ssh, desktop, mcp, project };
}

function detectFlowAdjustmentNote(text: string): string | null {
  const lower = text.toLowerCase();
  if (/(too long|be concise|shorter|brief)/i.test(lower)) return "Prefer concise responses unless deeper detail is requested.";
  if (/(more detail|go deeper|more thorough|expand)/i.test(lower)) return "Provide deeper, stepwise detail by default.";
  if (/(ask me before|don't do that automatically|confirm first)/i.test(lower)) return "Ask for explicit confirmation before performing impactful actions.";
  if (/(stop using slash commands|no slash commands|conversational only)/i.test(lower)) return "Infer actions from conversational language; do not require slash commands.";
  return null;
}

function isActionWhyQuestion(text: string): boolean {
  const lower = text.toLowerCase();
  return /(why did you|why were you|what did you do|what steps did you take|why that step|explain your steps)/i.test(lower);
}

function isSimpleCasualRequest(text: string): boolean {
  const t = text.trim().toLowerCase();
  if (!t) return false;
  const words = t.split(/\s+/).filter(Boolean);
  if (words.length > 18) return false;
  const casual = [
    "joke", "random number", "flip a coin", "coin flip", "hello", "hi", "hey",
    "how are you", "thanks", "thank you", "good morning", "good night"
  ];
  const codey = ["function", "class", "script", "bug", "error", "compile", "mcp", "bridge", "repo", "project", "godot", "file", "path"];
  if (codey.some((k) => t.includes(k))) return false;
  return casual.some((k) => t.includes(k));
}

function isReferentialFollowup(text: string): boolean {
  const t = text.trim().toLowerCase();
  if (!t) return false;
  const patterns = [
    /\bthat\b/, /\bit\b/, /\bthis\b/, /\bthose\b/, /\bthese\b/, /\bprevious\b/, /\babove\b/,
    /\byou said\b/, /\bearlier\b/, /\bcontinue\b/, /\bgo on\b/, /\bkeep going\b/, /\bmore\b/,
    /\belaborate\b/, /\bdeeper\b/, /\bexpand\b/, /\bwhat did you mean\b/, /\bwhy\b/,
    /\bwhat joke\b/, /\byou just told me\b/, /\bjust said\b/
  ];
  if (patterns.some((rx) => rx.test(t))) return true;
  if (/^(and|also|plus|then)\b/.test(t)) return true;
  return false;
}

function normalizeConversationalQuery(text: string): string {
  return text
    .replace(/\?+$/g, "")
    .replace(/^\s*(?:hey|hi|hello)\s+[a-z]+,?\s*/i, "")
    .replace(/^\s*(?:can you|could you|would you|please|pls|tell me|show me|give me)\b\s*/i, "")
    .replace(/^\s*(?:what(?:'s| is)?|how(?:'s| is)?)\s+/i, "")
    .replace(/^\s*(?:for me|about)\s+/i, "")
    .replace(/\b(?:thanks|thank you)\b\s*$/i, "")
    .replace(/\s{2,}/g, " ")
    .trim();
}

function isProjectIdeationRequest(text: string): boolean {
  const t = text.trim().toLowerCase();
  if (!t) return false;
  const ideationMarkers = [
    "idea", "ideas", "brainstorm", "concept", "concepts", "inspiration",
    "environment", "world", "worldbuilding", "world building", "setting",
    "atmosphere", "biome", "level theme", "areas for the game"
  ];
  const planningMarkers = ["plan", "roadmap", "milestone", "task list", "implementation plan"];
  if (planningMarkers.some((k) => t.includes(k))) return false;
  return ideationMarkers.some((k) => t.includes(k));
}

function parseWeatherRequest(text: string): { location: string; dayOffset: number } | null {
  const raw = normalizeConversationalQuery(text.trim());
  const lower = raw.toLowerCase();
  if (!(lower.includes("weather") || lower.includes("forecast"))) return null;

  let dayOffset = 0;
  if (lower.includes("day after tomorrow")) dayOffset = 2;
  else if (lower.includes("tomorrow")) dayOffset = 1;
  else {
    const weekdays = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
    const weekdayIdx = weekdays.findIndex((d) => new RegExp(`\\b${d}\\b`, "i").test(lower));
    if (weekdayIdx >= 0) {
      const nowIdx = new Date().getDay();
      dayOffset = (weekdayIdx - nowIdx + 7) % 7;
      if (dayOffset === 0) dayOffset = 7;
    }
  }

  const inMatch = raw.match(/\b(?:in|for|at)\s+(.+)$/i);
  if (inMatch?.[1]) {
    const loc = inMatch[1]
      .replace(/\b(?:today|tonight|tomorrow|day after tomorrow|this morning|this afternoon|this evening|monday|tuesday|wednesday|thursday|friday|saturday|sunday)\b/gi, "")
      .replace(/\b(?:weather|forecast|like|please|now|right now|currently)\b/gi, "")
      .replace(/^[\s,.-]+|[\s,.-]+$/g, "")
      .replace(/\s{2,}/g, " ")
      .trim();
    if (loc) return { location: loc, dayOffset };
  }

  let location = raw
    .replace(/\?+$/g, "")
    .replace(/^what(?:'s| is)?\s+the\s+/i, "")
    .replace(/^tell me\s+the\s+/i, "")
    .replace(/\b(?:weather|forecast)\b/gi, "")
    .replace(/\b(?:day after tomorrow|tomorrow|today|tonight|this morning|this afternoon|this evening)\b/gi, "")
    .replace(/^[\s,.-]+|[\s,.-]+$/g, "")
    .replace(/^(?:for|in|at|on)\s+/i, "")
    .replace(/^(?:the\s+)?(?:island|city|town|state|region|area)\s+of\s+/i, "")
    .replace(/\b(?:like|please|now|right now|currently)\b/gi, "")
    .replace(/^[\s,.-]+|[\s,.-]+$/g, "")
    .replace(/\s{2,}/g, " ")
    .trim();
  if (!location) return null;

  return { location, dayOffset };
}

function buildWeatherReply(payload: any): string {
  const location = String(payload?.location || "the requested location").trim();
  const dayLabel = String(payload?.dayLabel || "the requested day").trim();
  const summary = String(payload?.summary || "").trim();
  const sourceUrl = String(payload?.sourceUrl || "").trim();
  if (!summary) {
    return `I couldn't retrieve a reliable forecast for ${location} right now.`;
  }
  return [
    `Here is the forecast I found for ${location} ${dayLabel}:`,
    `- ${summary}`,
    sourceUrl ? `Source: ${sourceUrl}` : ""
  ].filter(Boolean).join("\n");
}

function sanitizeAssistantOutput(raw: string): string {
  let out = String(raw || "").replace(/<\/?final>/gi, "").trim();
  const improvedIdx = out.toLowerCase().indexOf("here's an improved final response");
  if (improvedIdx >= 0) {
    out = out.slice(improvedIdx + "here's an improved final response".length).replace(/^\s*[:\-]\s*/, "").trim();
  }
  out = out
    .replace(/^\s*(user intent|current draft analysis|output)\s*:\s*.*$/gim, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return out;
}

async function persistDirectReply(params: {
  requestTabId: number;
  tabs: Tab[];
  userDisplayText: string;
  replyText: string;
  associatedProjectId: string | null;
  promptHistory?: string[];
}) {
  const { requestTabId, tabs, userDisplayText, replyText, associatedProjectId, promptHistory } = params;
  const tabObj = tabs.find(t => t.id === requestTabId);
  const nowTs = Math.floor(Date.now() / 1000);
  const uiMsgs = (tabObj?.messages || []).map((m: any) => ({
    role: m.role,
    text: m.text,
    time: (m as any).time ?? nowTs,
  }));
  const messagesForSnapshot: { role: 'user' | 'llm'; text: string; time: number }[] = [...uiMsgs];
  if (!messagesForSnapshot.length || messagesForSnapshot[messagesForSnapshot.length - 1].role !== 'user' ||
      messagesForSnapshot[messagesForSnapshot.length - 1].text !== userDisplayText) {
    messagesForSnapshot.push({ role: 'user', text: userDisplayText, time: nowTs });
  }
  if (!messagesForSnapshot.length || messagesForSnapshot[messagesForSnapshot.length - 1].role !== 'llm' ||
      messagesForSnapshot[messagesForSnapshot.length - 1].text !== replyText) {
    messagesForSnapshot.push({ role: 'llm', text: replyText, time: nowTs });
  }
  await withTimeout(persistSnapshot(requestTabId, {
    title: tabObj?.title ?? `Tab #${requestTabId}`,
    messages: capSnapshotMessages(messagesForSnapshot),
    associatedProjectId,
    microSummary: "",
    dialogueBullets: "",
    summary: "",
    artifacts: [],
    promptHistory: promptHistory ?? tabObj?.promptHistory ?? [],
    last_updated: nowTs,
  }), 3200, "persistSnapshot(direct-reply)");
}

function shallowEqualTabProjectMap(
  a: Record<number, string | null>,
  b: Record<number, string | null>
): boolean {
  const aKeys = Object.keys(a);
  const bKeys = Object.keys(b);
  if (aKeys.length !== bKeys.length) return false;
  for (const k of aKeys) {
    if (a[Number(k)] !== b[Number(k)]) return false;
  }
  return true;
}

/** Detects the connection type of a raw MCP target string. */
function parseMcpTarget(raw: unknown): { type: "url" | "stdio"; command?: string; args?: string[]; url?: string } {
  const asString = typeof raw === "string"
    ? raw
    : (raw == null ? "" : (typeof raw === "object" ? JSON.stringify(raw) : String(raw)));
  const s = asString.trim();
  if (!s) return { type: "url" };
  // JSON object with "command" key (e.g. pasted from MCP server docs)
  if (s.startsWith("{")) {
    try {
      const parsed = JSON.parse(s);
      // Support both {"command":"uv","args":[...]} and mcpServers wrapper
      const server = parsed.command ? parsed
        : parsed.mcpServers ? Object.values(parsed.mcpServers as Record<string, any>)[0]
        : null;
      if (server?.command) {
        return { type: "stdio", command: String(server.command), args: Array.isArray(server.args) ? server.args.map(String) : [] };
      }
      if (server?.url) {
        return { type: "url", url: String(server.url) };
      }
      // Unknown JSON schema: treat as URL-style config (non-launchable) instead of shell command.
      return { type: "url" };
    } catch {}
  }
  // URL patterns
  if (/^(https?|wss?|local):\/\//i.test(s)) return { type: "url" };
  // Anything else is treated as a stdio command string ("uv run /path/server.py")
  // Support quoted tokens
  const parts = s.match(/(?:[^\s"']+|"[^"]*"|'[^']*')+/g) ?? [];
  if (parts.length >= 1) {
    const command = (parts[0] ?? "").replace(/^["']|["']$/g, "");
    let args = parts.slice(1).map(a => a.replace(/^["']|["']$/g, ""));
    // Common user typo: full uv.exe path followed by an extra leading "uv" token.
    if (/uv(?:\.exe)?$/i.test(command) && args[0]?.toLowerCase() === "uv") {
      args = args.slice(1);
    }
    return { type: "stdio", command, args };
  }
  return { type: "url" };
}

type ProjectDef = {
  id: string;
  name: string;
  enabled: boolean;
  description: string;
  manipulationRootPath: string;
  datawebEnabled: boolean;
  repoIds: string[];
  entryIds: string[];
  mcpIds: string[];
  sshIds: string[];
};

type RepoProjectStore = {
  repos: RepoFolder[];
  mcps: McpConnection[];
  sshs: SshConnection[];
  projects: ProjectDef[];
};

type RepoAssistantMessage = {
  role: "user" | "assistant";
  text: string;
};

type RepoTreeNode = {
  id: string;
  entryId: string | null;
  name: string;
  path: string;
  isDir: boolean;
  sizeBytes: number;
  children: RepoTreeNode[];
};

type NetworkHit = {
  title: string;
  url: string;
  snippet: string;
  score: number;
};

type MemoryLanes = {
  project: string;
  coding: string;
  recall: string;
};

type ResolverKind = "none" | "math" | "recall";
type LlmProfile = "Ultra" | "High" | "MediumHigh" | "Medium" | "Low" | "Minimal";
type ThemePreset = "Black" | "Blue" | "Pink" | "Purple" | "Silver" | "Grey" | "Custom";

type HardwareProfile = {
  vramGb: number;
  ramGb: number;
  cpuCores: number;
  gpuName: string;
  detectedProfile: LlmProfile;
  detectionNote: string;
};

type ModelfileParamFE = { key: string; value: string };
type ModelfileDataFE = {
  filename: string;
  displayName: string;
  nickname: string;
  fromModel: string;
  systemPrompt: string;
  params: ModelfileParamFE[];
};

type CustomModelDef = { id: string; filename: string; nickname: string; enabled: boolean; note: string };
type ModelConfigFE = {
  coderEnabled: boolean;
  visionEnabled: boolean;
  customModels: CustomModelDef[];
  modelNotes: Record<string, string>;
  statusVerbs: Record<string, string>;
};

type PendingImage = {
  id: string;
  name: string;
  mimeType: string;
  dataUrl: string;
};

/** Returns capability flags and token budget for the given profile. */
function profileCapabilities(profile: LlmProfile) {
  const tier =
    profile === "Ultra" ? 4 :
    profile === "High" ? 3 :
    profile === "MediumHigh" ? 2 :
    profile === "Medium" ? 1 : 0; // Low and Minimal = 0

  return {
    // Repos/MCP are available on all tiers; users can toggle them for speed/cognitive bandwidth.
    reposAvailable: true,
    mcpAvailable: true,
    fullRagEnabled: tier >= 2,     // MediumHigh and above
    multiMcpEnabled: tier >= 2,    // MediumHigh and above
    deepReasoningEnabled: tier >= 2,
    tokenBudget: tier >= 3 ? 3600 : tier === 2 ? 2400 : 1200,
  };
}

function iterationBudgetForProfile(profile: LlmProfile): number {
  switch (profile) {
    case "Ultra":
      return 4;
    case "High":
      return 3;
    case "MediumHigh":
      return 2;
    case "Medium":
      return 2;
    case "Low":
    case "Minimal":
    default:
      return 1;
  }
}

type FootprintLevel = "Very Light" | "Light" | "Balanced" | "Heavy" | "Very Heavy";

function computeEffectiveContextFootprint(params: {
  profile: LlmProfile;
  tokenBudget: number;
  reposOn: boolean;
  mcpOn: boolean;
  multiMcp: boolean;
  fullRag: boolean;
  deepReasoning: boolean;
  networkOn: boolean;
  interpretV2On: boolean;
}): { score: number; level: FootprintLevel; rationale: string } {
  const {
    profile,
    tokenBudget,
    reposOn,
    mcpOn,
    multiMcp,
    fullRag,
    deepReasoning,
    networkOn,
    interpretV2On,
  } = params;

  const profileBase =
    profile === "Ultra" ? 2.8 :
    profile === "High" ? 2.2 :
    profile === "MediumHigh" ? 1.7 :
    profile === "Medium" ? 1.2 :
    profile === "Low" ? 0.8 : 0.5;

  const tokenLoad = Math.max(0, (tokenBudget - 1200) / 1200) * 0.8;
  const repoLoad = reposOn ? (fullRag ? 1.2 : 0.7) : 0.0;
  const mcpLoad = mcpOn ? (multiMcp ? 1.1 : 0.6) : 0.0;
  const reasoningLoad = deepReasoning ? 0.8 : 0.25;
  const netLoad = networkOn ? 0.5 : 0.0;
  const plannerLoad = interpretV2On ? 0.4 : 0.0;

  const score = Number((profileBase + tokenLoad + repoLoad + mcpLoad + reasoningLoad + netLoad + plannerLoad).toFixed(2));

  const level: FootprintLevel =
    score < 2.0 ? "Very Light" :
    score < 3.0 ? "Light" :
    score < 4.2 ? "Balanced" :
    score < 5.4 ? "Heavy" : "Very Heavy";

  const rationale = [
    `Profile=${profile}`,
    `TokenBudget=${tokenBudget}`,
    `Repos=${reposOn ? "ON" : "OFF"}`,
    `MCP=${mcpOn ? (multiMcp ? "ON (multi)" : "ON (single)") : "OFF"}`,
    `Planner=${interpretV2On ? "ON" : "OFF"}`,
    `Network=${networkOn ? "ON" : "OFF"}`,
  ].join(" | ");

  return { score, level, rationale };
}

const THEME_PRESET_COLORS: Record<Exclude<ThemePreset, "Custom">, string> = {
  Black: "#232323",
  Blue: "#1d3557",
  Pink: "#5a2a4d",
  Purple: "#3c2a5a",
  Silver: "#4a4f57",
  Grey: "#3a3a3a",
};

function hexToRgb(hex: string): { r: number; g: number; b: number } {
  const clean = hex.replace("#", "").trim();
  const full = clean.length === 3 ? clean.split("").map((c) => `${c}${c}`).join("") : clean;
  const int = parseInt(full, 16);
  return {
    r: (int >> 16) & 255,
    g: (int >> 8) & 255,
    b: int & 255,
  };
}

function mixHex(a: string, b: string, ratio: number): string {
  const ra = hexToRgb(a);
  const rb = hexToRgb(b);
  const t = Math.max(0, Math.min(1, ratio));
  const r = Math.round(ra.r * (1 - t) + rb.r * t);
  const g = Math.round(ra.g * (1 - t) + rb.g * t);
  const b2 = Math.round(ra.b * (1 - t) + rb.b * t);
  return `#${r.toString(16).padStart(2, "0")}${g.toString(16).padStart(2, "0")}${b2.toString(16).padStart(2, "0")}`;
}

function makeId(prefix: string): string {
  return `${prefix}_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}

function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ""));
    reader.onerror = () => reject(reader.error || new Error("Failed to read file"));
    reader.readAsDataURL(file);
  });
}

function normalizeSlashes(value: string): string {
  return (value || "").replace(/\\+/g, "/");
}

function toRelativeRepoPath(repoPath: string, absolutePath: string): string {
  const root = normalizeSlashes(repoPath).replace(/\/+$/, "");
  const full = normalizeSlashes(absolutePath);
  if (full.toLowerCase().startsWith(root.toLowerCase())) {
    return full.slice(root.length).replace(/^\/+/, "");
  }
  return full;
}

function buildRepoTree(repo: RepoFolder): RepoTreeNode[] {
  const root: RepoTreeNode[] = [];
  const entryByNormPath = new Map<string, RepoEntry>();
  for (const entry of repo.entries) {
    entryByNormPath.set(normalizeSlashes(entry.path).toLowerCase(), entry);
  }

  function sortNodes(nodes: RepoTreeNode[]): RepoTreeNode[] {
    return nodes
      .map((n) => ({ ...n, children: sortNodes(n.children) }))
      .sort((a, b) => {
        if (a.isDir !== b.isDir) {
          return a.isDir ? -1 : 1;
        }
        return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
      });
  }

  for (const entry of repo.entries) {
    const rel = toRelativeRepoPath(repo.path, entry.path);
    const parts = rel.split("/").filter(Boolean);
    if (!parts.length) continue;

    let cursor = root;
    let accPath = normalizeSlashes(repo.path).replace(/\/+$/, "");

    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      accPath = `${accPath}/${part}`;
      const nodeId = normalizeSlashes(accPath).toLowerCase();
      const matchedEntry = entryByNormPath.get(nodeId);

      let node = cursor.find((n) => n.id === nodeId);
      if (!node) {
        node = {
          id: nodeId,
          entryId: matchedEntry?.id ?? null,
          name: part,
          path: matchedEntry?.path ?? accPath,
          isDir: matchedEntry ? matchedEntry.isDir : true,
          sizeBytes: matchedEntry?.sizeBytes ?? 0,
          children: [],
        };
        cursor.push(node);
      } else if (matchedEntry) {
        node.entryId = matchedEntry.id;
        node.path = matchedEntry.path;
        node.isDir = matchedEntry.isDir;
        node.sizeBytes = matchedEntry.sizeBytes;
      }

      cursor = node.children;
    }
  }

  return sortNodes(root);
}

function nextChatLabelNumber(allTabs: Tab[]): number {
  const used = new Set<number>();
  for (const t of allTabs) {
    if (t.type !== "chat") continue;
    const m = t.title.match(/^Tab\s+#(\d+)$/i);
    if (m) {
      const n = Number(m[1]);
      if (Number.isFinite(n) && n > 0) used.add(n);
    }
  }
  let n = 1;
  while (used.has(n)) n++;
  return n;
}

type MemoryDebugState = {
  lastResolver: ResolverKind;
  plannerPath: "v2" | "legacy";
  plannerStrategy: string;
  plannerIntent: string;
  suggestedGodotVersion: string;
  lastNumericAnchor: number | null;
  activeArtifactLabel: string;
  laneProject: string;
  laneCoding: string;
  laneRecall: string;
  transcriptChars: number;
  updatedAt: number;
};

function pickActiveArtifact(
  recentArtifacts?: Artifact[],
  snapshotArtifacts?: Artifact[]
): Artifact | null {
  const all = [...(recentArtifacts || []), ...(snapshotArtifacts || [])];
  if (!all.length) return null;
  const sorted = all.slice().sort((a: any, b: any) => (b?.ts ?? 0) - (a?.ts ?? 0));
  return sorted[0] || null;
}

function parseLaneBlock(summary: string | undefined, laneName: "PROJECT" | "CODING" | "RECALL"): string {
  if (!summary) return "";
  const re = new RegExp(`\\[${laneName}\\]\\n([\\s\\S]*?)(?=\\n\\[(?:PROJECT|CODING|RECALL)\\]|$)`);
  const m = summary.match(re);
  return (m?.[1] || "").trim();
}

function compactText(value: string, maxLen = 260): string {
  const t = (value || "").replace(/\s+/g, " ").trim();
  if (!t) return "";
  return t.length > maxLen ? `${t.slice(0, maxLen - 1)}…` : t;
}

function buildMemoryLanes(params: {
  transcript: string;
  microSummary: string;
  dialogueBullets: string;
  summary: string;
  activeArtifact: Artifact | null;
}): MemoryLanes {
  const { transcript, microSummary, dialogueBullets, summary, activeArtifact } = params;

  const priorProject = parseLaneBlock(summary, "PROJECT");
  const priorCoding = parseLaneBlock(summary, "CODING");
  const priorRecall = parseLaneBlock(summary, "RECALL");

  const lines = (transcript || "").split("\n").map((l) => l.trim()).filter(Boolean);
  const userLines = lines.filter((l) => l.startsWith("User:")).slice(-4).map((l) => l.replace(/^User:\s*/, ""));
  const recallLines = lines.slice(-6).map((l) => l.replace(/^(User|Iris):\s*/, ""));

  const artifactLabel = activeArtifact
    ? `${activeArtifact.filename || "(unsaved snippet)"}${activeArtifact.lang ? ` [${activeArtifact.lang}]` : ""}`
    : "";

  const project = compactText(priorProject || microSummary || userLines.join(" | ") || "No project facts captured yet.");
  const coding = compactText(
    priorCoding ||
      (artifactLabel
        ? `Active artifact: ${artifactLabel}. Keep follow-up code edits aligned to this artifact unless user asks for a rewrite.`
        : dialogueBullets || "No active code artifact."
      )
  );
  const recall = compactText(priorRecall || recallLines.join(" | ") || "No recent recall context.");

  return { project, coding, recall };
}

function serializeMemoryLanes(lanes: MemoryLanes): string {
  return `[PROJECT]\n${lanes.project}\n\n[CODING]\n${lanes.coding}\n\n[RECALL]\n${lanes.recall}`;
}

function extractLastAssistantNumber(transcript: string): number | null {
  if (!transcript) return null;
  const lines = transcript.split("\n");
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = lines[i].trim();
    if (!line.startsWith("Iris:")) continue;
    const matches = line.match(/-?\d+(?:\.\d+)?/g);
    if (!matches || matches.length === 0) continue;
    const last = Number(matches[matches.length - 1]);
    if (Number.isFinite(last)) return last;
  }
  return null;
}

// Local, minimal wait to let Tauri inject window.__TAURI__
async function waitForTauri(timeoutMs = 3000): Promise<void> {
  const start = Date.now();
  // quick exit if it's already there
  if ((window as any).__TAURI__?.invoke) return;

  while (Date.now() - start < timeoutMs) {
    if ((window as any).__TAURI__?.invoke) return;
    await new Promise(r => setTimeout(r, 50));
  }
  // no throw; we just return and let callers decide what to do
}

async function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function capSnapshotMessages(
  messages: Array<{ role: 'user' | 'llm'; text: string; time: number }>,
  maxMessages = 140,
  maxCharsPerMessage = 16000
): Array<{ role: 'user' | 'llm'; text: string; time: number }> {
  const trimmedWindow = messages.slice(-maxMessages);
  return trimmedWindow.map((m) => ({
    role: m.role,
    text: typeof m.text === "string" ? m.text.slice(0, maxCharsPerMessage) : "",
    time: Number.isFinite(m.time) ? m.time : Math.floor(Date.now() / 1000),
  }));
}


function App() {
  const [modelStatus, setModelStatus] = useState<ModelStatus>("checking");
  const [coderReady, setCoderReady] = useState<boolean | null>(null);
  const [input, setInput] = useState("");
  const [pendingImages, setPendingImages] = useState<PendingImage[]>([]);
  const [chatDragActive, setChatDragActive] = useState(false);
  const [thinking, setThinking] = useState(false);
  const [ellipsis, setEllipsis] = useState(".");
  const [isGenerating, setIsGenerating] = useState(false);
  const [isSummarizing, setIsSummarizing] = useState(false);
  const [tabs, setTabs] = useState<Tab[]>([
    { id: 1, title: "Tab #1", type: "chat", messages: [], promptHistory: [] }
  ]);
  const [activeTab, setActiveTab] = useState(1);
  const [editingTabId, setEditingTabId] = useState<number | null>(null);
  const [editingTabTitle, setEditingTabTitle] = useState("");
  const [draggingTabId, setDraggingTabId] = useState<number | null>(null);
  const [openMenu, setOpenMenu] = useState<null | "file" | "options">(null);
  const [openSubmenu, setOpenSubmenu] = useState<string | null>(null);
  const abortController = useRef<AbortController | null>(null);
  const mcpProbeAbortedRef = useRef<Set<string>>(new Set());
  const historyRef = useRef<HTMLDivElement>(null);
  const taskbarRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const promptHistoryIndexRef = useRef<number>(-1);
  const promptHistoryDraftRef = useRef<string>("");
  const projectDraftSaveTimerRef = useRef<Record<string, ReturnType<typeof setTimeout>>>({});
  const mcpToolCacheRef = useRef<Record<string, { tools: McpToolDescriptor[]; ts: number }>>({});
  const isMounted = useRef(true);

  const [useCoder, setUseCoder] = useState(false);
  const [setupResult, setSetupResult] = useState<"ready" | "missing-ollama" | "missing-models" | null>(null);

  // Model config (loaded from model_config.json on startup)
  const [modelConfig, setModelConfig] = useState<ModelConfigFE>({ coderEnabled: true, visionEnabled: true, customModels: [], modelNotes: {}, statusVerbs: {} });
  const [modelConfigDirty, setModelConfigDirty] = useState(false);

  const [interpretV2Enabled, setInterpretV2Enabled] = useState(true);
  const [llmProfile, setLlmProfile] = useState<LlmProfile>("Medium");
  const [reposEnabled, setReposEnabled] = useState(true);
  const [mcpEnabled, setMcpEnabled] = useState(true);
  const [desktopToolsEnabled, setDesktopToolsEnabled] = useState(false);
  const [desktopDashboardEnabled, setDesktopDashboardEnabled] = useState(false);
  const [hwToast, setHwToast] = useState<string | null>(null);
  const hwToastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [assistantName, setAssistantName] = useState("Iris");
  const [themePreset, setThemePreset] = useState<ThemePreset>("Black");
  const [customThemeColor, setCustomThemeColor] = useState("#232323");
  const [colorMode, setColorMode] = useState<"dark" | "light">("dark");
  const [networkEnabled, setNetworkEnabled] = useState(false);
  const [universalDatawebEnabled, setUniversalDatawebEnabled] = useState(true);
  const [networkDraftEnabled, setNetworkDraftEnabled] = useState(false);
  const [manualDownloadUrl, setManualDownloadUrl] = useState("");
  const [releaseNotesUrl, setReleaseNotesUrl] = useState("");
  const [updateFeedUrl, setUpdateFeedUrl] = useState("");
  const [autoUpdatesEnabled, setAutoUpdatesEnabled] = useState(false);
  const [githubReleaseRepo, setGithubReleaseRepo] = useState("KennyCrowPixels/Iris_for_Godot");
  const [preferMsiInstaller, setPreferMsiInstaller] = useState(false);
  const [includePrereleaseInstaller, setIncludePrereleaseInstaller] = useState(true);
  // MCP draft: buffered edits, only saved to repoStore on "Save MCPs" button
  const [mcpDraft, setMcpDraft] = useState<McpConnection[]>([]);
  const [sshDraft, setSshDraft] = useState<SshConnection[]>([]);
  const [mcpLaunchStatus, setMcpLaunchStatus] = useState<Record<string, { pid: number; launched: boolean }>>({});
  const [mcpHealthStatus, setMcpHealthStatus] = useState<Record<string, { state: "stopped" | "launching" | "bridge_up" | "connected" | "error"; message: string; toolCount?: number }>>({});
  const [sshConnectStatus, setSshConnectStatus] = useState<Record<string, { connected: boolean; checking: boolean; message?: string }>>({});
  const [chatBridgePanelOpen, setChatBridgePanelOpen] = useState(false);
  const [actionTraceByTab, setActionTraceByTab] = useState<Record<number, IrisActionTrace[]>>({});
  const [flowAdjustmentsByTab, setFlowAdjustmentsByTab] = useState<Record<number, string[]>>({});
  // Routine execution state
  const [activeRoutine, setActiveRoutine] = useState<ActiveRoutine | null>(null);
  const [activeRoutineTabId, setActiveRoutineTabId] = useState<number | null>(null);
  const [routineExpanded, setRoutineExpanded] = useState(false);
  const [workLogByTab, setWorkLogByTab] = useState<Record<number, WorkLogEntry[]>>({});
  const [windowList, setWindowList] = useState<WindowInfo[]>([]);
  const [systemStats, setSystemStats] = useState<{ cpuPercent: number; memUsedMb: number; memTotalMb: number } | null>(null);
  const [corePersonaPrompt, setCorePersonaPrompt] = useState("");
  const [userPersonaPrompt, setUserPersonaPrompt] = useState("");
  const [userPersonaSavedPrompt, setUserPersonaSavedPrompt] = useState("");
  const [userPersonaDirty, setUserPersonaDirty] = useState(false);
  const [repoStore, setRepoStore] = useState<RepoProjectStore>({ repos: [], mcps: [], sshs: [], projects: [] });
  const [projectDescriptionDrafts, setProjectDescriptionDrafts] = useState<Record<string, string>>({});
  const [projectManipulationRootDrafts, setProjectManipulationRootDrafts] = useState<Record<string, string>>({});
  const [projectDescriptionDirty, setProjectDescriptionDirty] = useState<Record<string, boolean>>({});
  const [repoAssistantMessages, setRepoAssistantMessages] = useState<RepoAssistantMessage[]>([
    {
      role: "assistant",
      text: "Repo assistant is available. Ask what repos are useful, or paste a Git URL and ask me to install it into a selected repo folder.",
    },
  ]);
  const [repoAssistantInput, setRepoAssistantInput] = useState("");
  const [repoAssistantBusy, setRepoAssistantBusy] = useState(false);
  const [repoAssistantTargetRepoId, setRepoAssistantTargetRepoId] = useState<string>("");
  const [repoTreeExpanded, setRepoTreeExpanded] = useState<Record<string, boolean>>({});
  const [repoAutoSelectingId, setRepoAutoSelectingId] = useState<string | null>(null);
  const [repoAutoStatus, setRepoAutoStatus] = useState<Record<string, string>>({});
  const [projectTreeExpanded, setProjectTreeExpanded] = useState<Record<string, boolean>>({});
  const [projectAutoSelectingId, setProjectAutoSelectingId] = useState<string | null>(null);
  const [projectAutoStatus, setProjectAutoStatus] = useState<Record<string, string>>({});
  const [tabProjectMap, setTabProjectMap] = useState<Record<number, string | null>>({});
  const [memoryDebug, setMemoryDebug] = useState<MemoryDebugState>({
    lastResolver: "none",
    plannerPath: "legacy",
    plannerStrategy: "",
    plannerIntent: "",
    suggestedGodotVersion: "unspecified",
    lastNumericAnchor: null,
    activeArtifactLabel: "(none)",
    laneProject: "",
    laneCoding: "",
    laneRecall: "",
    transcriptChars: 0,
    updatedAt: 0,
  });
  const [startupStatus, setStartupStatus] = useState<StartupStatus>({
    active: true,
    step: "Booting Iris desktop app...",
    progress: 3,
  });
  const [centerDialog, setCenterDialog] = useState<CenterDialogState>({
    open: false,
    title: "",
    message: "",
    confirmLabel: "OK",
    cancelLabel: "Cancel",
  });
  const centerDialogResolverRef = useRef<((value: boolean) => void) | null>(null);

  function askCenteredConfirm(args: {
    title: string;
    message: string;
    confirmLabel?: string;
    cancelLabel?: string;
  }): Promise<boolean> {
    return new Promise((resolve) => {
      centerDialogResolverRef.current = resolve;
      setCenterDialog({
        open: true,
        title: args.title,
        message: args.message,
        confirmLabel: args.confirmLabel || "OK",
        cancelLabel: args.cancelLabel || "Cancel",
      });
    });
  }

  function resolveCenteredConfirm(value: boolean) {
    const resolver = centerDialogResolverRef.current;
    centerDialogResolverRef.current = null;
    setCenterDialog((prev) => ({ ...prev, open: false }));
    if (resolver) resolver(value);
  }

  useEffect(() => {
    if (setupResult === "ready") {
      setCoderReady(true);
      setModelStatus("ready");
    } else if (setupResult === "missing-ollama" || setupResult === "missing-models") {
      setCoderReady(false);
    }
  }, [setupResult]);

  // Modelfile editor state
  const [modelfileDatas, setModelfileDatas] = useState<Record<string, ModelfileDataFE>>({});
  const [modelfileEdits, setModelfileEdits] = useState<Record<string, ModelfileParamFE[]>>({});
  const [modelfileFromEdits, setModelfileFromEdits] = useState<Record<string, string>>({});
  const [modelfileSubTab, setModelfileSubTab] = useState<string>("modelfile_organizer.txt");
  const [modelfileSaving, setModelfileSaving] = useState<Record<string, boolean>>({});
  const [modelfileLoadError, setModelfileLoadError] = useState<string | null>(null);
  const [modelfileFilenames, setModelfileFilenames] = useState<string[]>([]);
  const modelfileLoadedRef = useRef(false);

  const currentTab = tabs.find(t => t.id === activeTab);
  const hasChatTabs = tabs.some(t => t.type === "chat");
  const menuLocked = startupStatus.active || modelStatus === "checking" || modelStatus === "loading";
  const activeThemeColor = themePreset === "Custom" ? customThemeColor : THEME_PRESET_COLORS[themePreset as Exclude<ThemePreset, "Custom">];
  const isLightMode = colorMode === "light";
  // App background and border follow ONLY theme color — not light/dark mode
  const appBgColor = activeThemeColor;
  const appBorderColor = mixHex(activeThemeColor, "#b0b0b0", 0.18);
  // Detail/accent elements (tabs, inputs, surfaces, panels) respond to light/dark mode
  const appSurfaceColor = isLightMode ? mixHex(activeThemeColor, "#d6d9de", 0.72) : mixHex(activeThemeColor, "#111111", 0.45);
  const appTabBgColor = isLightMode ? mixHex(activeThemeColor, "#c5cbd3", 0.74) : mixHex(activeThemeColor, "#1e1e1e", 0.58);
  const appTabActiveBgColor = isLightMode ? mixHex(activeThemeColor, "#e7ebf0", 0.82) : mixHex(activeThemeColor, "#0e0e0e", 0.52);
  const appInputBgColor = isLightMode ? "rgba(235, 239, 244, 0.84)" : mixHex(activeThemeColor, "#141414", 0.68);
  const appPanelBgColor = isLightMode ? "rgba(208, 214, 223, 0.72)" : mixHex(activeThemeColor, "#0d0d0d", 0.55);
  const appTextColor = isLightMode ? "#1a1a1a" : "#e8e8e8";
  const assistantLabel = assistantName.trim() || "Iris";
  const activeProjectId = tabProjectMap[activeTab] ?? null;

  function updateTabPromptHistory(tabId: number, prompt: string) {
    const normalized = prompt.trim();
    if (!normalized) return;
    setTabs(prev => prev.map(tab => {
      if (tab.id !== tabId || tab.type !== "chat") return tab;
      const existing = Array.isArray(tab.promptHistory) ? tab.promptHistory : [];
      const deduped = [normalized, ...existing.filter((item) => item !== normalized)].slice(0, 60);
      return { ...tab, promptHistory: deduped };
    }));
  }

  async function persistTabSnapshotState(tabId: number, overrides?: Partial<Snapshot>) {
    const tabObj = tabs.find(t => t.id === tabId);
    if (!tabObj || tabObj.type !== "chat") return;
    const nowTs = Math.floor(Date.now() / 1000);
    const existing = await readTabSnapshot(tabId).catch(() => null);
    await persistSnapshot(tabId, {
      title: overrides?.title ?? tabObj.title,
      messages: overrides?.messages ?? (existing?.messages as any) ?? ((tabObj.messages || []).map((m: any) => ({
        role: m.role,
        text: m.text,
        time: m?.time ?? nowTs,
      })) as any),
      associatedProjectId: overrides?.associatedProjectId ?? existing?.associatedProjectId ?? tabProjectMap[tabId] ?? null,
      microSummary: overrides?.microSummary ?? existing?.microSummary ?? "",
      dialogueBullets: overrides?.dialogueBullets ?? existing?.dialogueBullets ?? "",
      summary: overrides?.summary ?? existing?.summary ?? "",
      artifacts: overrides?.artifacts ?? existing?.artifacts ?? [],
      promptHistory: overrides?.promptHistory ?? tabObj.promptHistory ?? existing?.promptHistory ?? [],
      last_updated: overrides?.last_updated ?? nowTs,
    });
  }

  useEffect(() => {
    promptHistoryIndexRef.current = -1;
    promptHistoryDraftRef.current = "";
  }, [activeTab]);
  const activeProject = repoStore.projects.find((p) => p.id === activeProjectId) || null;
  const activeProjectMcpIds = new Set((activeProject?.mcpIds || []).map((id) => String(id)));
  const activeProjectSshIds = new Set((activeProject?.sshIds || []).map((id) => String(id)));
  const launchableProjectMcps = activeProject
    ? repoStore.mcps.filter((m) => {
        if (!m.enabled || !activeProjectMcpIds.has(String(m.id))) return false;
        const parsed = parseMcpTarget(m.target);
        const isStdio = m.connectionType === "stdio" || parsed.type === "stdio";
        return isStdio && !!(m.launchCommand || parsed.command);
      })
    : [];
  const launchableProjectSsh = activeProject
    ? repoStore.sshs.filter((s) => s.enabled && !!s.host.trim() && activeProjectSshIds.has(String(s.id)))
    : [];
  const showChatBridgeControls = currentTab?.type === "chat" && (launchableProjectMcps.length > 0 || launchableProjectSsh.length > 0);
  const llmCapabilities = profileCapabilities(llmProfile);
  const configuredRouteModels = [
    "iris-organizer",
    ...(modelConfig.coderEnabled ? ["iris-coder"] : []),
    ...(modelConfig.visionEnabled ? ["iris-vision"] : []),
    ...modelConfig.customModels.filter((m) => m.enabled).map((m) => m.filename.replace(/\.txt$/i, "")),
    "iris-summarizer",
  ];
  const configuredRouteSummary = `Configured route: ${configuredRouteModels.join(" -> ")}`;
  const organizerFilename = modelfileFilenames.find((name) => name.toLowerCase().includes("organizer")) || "modelfile_organizer.txt";
  const organizerParamsLive = modelfileEdits[organizerFilename] || modelfileDatas[organizerFilename]?.params || [];
  const organizerNumCtxRaw = organizerParamsLive.find((p) => p.key === "num_ctx")?.value;
  const organizerNumCtxLive = Number.parseInt(String(organizerNumCtxRaw ?? ""), 10);
  const liveTokenBudget = Number.isFinite(organizerNumCtxLive) && organizerNumCtxLive > 0
    ? organizerNumCtxLive
    : llmCapabilities.tokenBudget;
  const reposContextActive = llmCapabilities.reposAvailable && reposEnabled;
  const mcpContextActive = llmCapabilities.mcpAvailable && mcpEnabled;
  const safeStringify = (v: unknown) => {
    try { return JSON.stringify(v); } catch { return "__ERR__"; }
  };
  const mcpDirty = safeStringify(mcpDraft) !== safeStringify(repoStore.mcps);
  const sshDirty = safeStringify(sshDraft) !== safeStringify(repoStore.sshs);
  const effectiveFootprint = computeEffectiveContextFootprint({
    profile: llmProfile,
    tokenBudget: liveTokenBudget,
    reposOn: reposContextActive,
    mcpOn: mcpContextActive,
    multiMcp: llmCapabilities.multiMcpEnabled,
    fullRag: llmCapabilities.fullRagEnabled,
    deepReasoning: llmCapabilities.deepReasoningEnabled,
    networkOn: networkEnabled,
    interpretV2On: interpretV2Enabled,
  });

  useEffect(() => {
    return () => {
      if (hwToastTimerRef.current) clearTimeout(hwToastTimerRef.current);
    };
  }, []);

  // helper: retry listOpenTabs a few times to be resilient to late Tauri injection
  async function tryListOpenTabs(retries = 4) {
    for (let i = 0; i < retries; i++) {
      console.log("[App] Attempt", i + 1, "to load open tabs");
      try {
        const snaps = await listOpenTabs();
        if (Array.isArray(snaps) && snaps.length) return snaps;
      } catch (e) {
        console.warn("listOpenTabs fail", e);
      }
      await new Promise(r => setTimeout(r, 400 * (i + 1))); // 400,800,1200,1600
    }
    return [];
  }

  useEffect(() => {
    (async () => {
      let done = false;
      const startupTimeout = setTimeout(() => {
        if (!done) {
          setStartupStatus({
            active: false,
            step: "Startup taking longer than expected",
            progress: 100,
          });
          setModelStatus("ready"); // failsafe: don't leave user permanently locked
        }
      }, 30000);

      try {
        setStartupStatus({ active: true, step: "Initializing Tauri runtime...", progress: 10 });
        // be more patient for Tauri injection on slow machines
        await waitForTauri(8000);
        setStartupStatus({ active: true, step: "Runtime ready; loading persisted preferences...", progress: 14 });

        try {
          const flags: any = await invoke("get_setup_flags");
          if (typeof flags?.interpretV2Enabled === "boolean") {
            setInterpretV2Enabled(flags.interpretV2Enabled);
          }
          if (typeof flags?.modelProfile === "string") {
            const raw = String(flags.modelProfile);
            if (["Ultra", "High", "MediumHigh", "Medium", "Low", "Minimal"].includes(raw)) {
              setLlmProfile(raw as LlmProfile);
            }
          }
          if (typeof flags?.reposEnabled === "boolean") {
            setReposEnabled(flags.reposEnabled);
          }
          if (typeof flags?.mcpEnabled === "boolean") {
            setMcpEnabled(flags.mcpEnabled);
          }
          if (typeof flags?.desktopToolsEnabled === "boolean") {
            setDesktopToolsEnabled(flags.desktopToolsEnabled);
          }
          if (typeof flags?.desktopDashboardEnabled === "boolean") {
            setDesktopDashboardEnabled(flags.desktopDashboardEnabled);
          }
          if (typeof flags?.assistantName === "string" && String(flags.assistantName).trim()) {
            setAssistantName(String(flags.assistantName));
          }
          if (typeof flags?.themeColor === "string" && String(flags.themeColor).trim()) {
            setCustomThemeColor(String(flags.themeColor));
          }
          if (typeof flags?.themePreset === "string") {
            const rawPreset = String(flags.themePreset);
            if (["Black", "Blue", "Pink", "Purple", "Silver", "Grey", "Custom"].includes(rawPreset)) {
              setThemePreset(rawPreset as ThemePreset);
            }
          }
          if (typeof flags?.networkEnabled === "boolean") {
            setNetworkEnabled(flags.networkEnabled);
            setNetworkDraftEnabled(flags.networkEnabled);
          }
          if (typeof flags?.manualDownloadUrl === "string") {
            setManualDownloadUrl(String(flags.manualDownloadUrl));
          }
          if (typeof flags?.releaseNotesUrl === "string") {
            setReleaseNotesUrl(String(flags.releaseNotesUrl));
          }
          if (typeof flags?.updateFeedUrl === "string") {
            setUpdateFeedUrl(String(flags.updateFeedUrl));
          }
          if (typeof flags?.autoUpdatesEnabled === "boolean") {
            setAutoUpdatesEnabled(!!flags.autoUpdatesEnabled);
          }
          if (typeof flags?.universalDatawebEnabled === "boolean") {
            setUniversalDatawebEnabled(flags.universalDatawebEnabled);
          }
          if (flags?.colorMode === "light") {
            setColorMode("light");
          }
        } catch {}

        try {
          setStartupStatus({ active: true, step: "Loading core persona prompt...", progress: 18 });
          const core = await invoke("get_core_persona_prompt") as string;
          setCorePersonaPrompt(String(core || ""));
        } catch {}

        try {
          const user = await invoke("get_user_persona_prompt") as string;
          setUserPersonaPrompt(String(user || ""));
          setUserPersonaSavedPrompt(String(user || ""));
          setUserPersonaDirty(false);
        } catch {}

        try {
          setStartupStatus({ active: true, step: "Loading repositories and projects...", progress: 22 });
          const store = await invoke("get_repo_project_store") as RepoProjectStore;
          if (store && typeof store === "object") {
            setRepoStore({
              repos: Array.isArray((store as any).repos) ? (store as any).repos : [],
                  mcps: Array.isArray((store as any).mcps)
                    ? (store as any).mcps.map((m: any) => ({
                        id: String(m?.id ?? makeId("mcp")),
                        name: String(m?.name ?? "MCP"),
                        target: typeof m?.target === "string"
                          ? m.target
                          : (m?.target == null ? "" : (typeof m.target === "object" ? JSON.stringify(m.target) : String(m.target))),
                        enabled: !!m?.enabled,
                        notes: String(m?.notes ?? ""),
                        connectionType: m?.connectionType === "stdio" ? "stdio" : "url",
                        launchCommand: typeof m?.launchCommand === "string" ? m.launchCommand : undefined,
                        launchArgs: Array.isArray(m?.launchArgs) ? m.launchArgs.map(String) : undefined,
                      }))
                    : [],
                  sshs: Array.isArray((store as any).sshs)
                    ? (store as any).sshs.map((s: any) => ({
                        id: String(s?.id ?? makeId("ssh")),
                        name: String(s?.name ?? "SSH"),
                        host: String(s?.host ?? ""),
                        port: Number(s?.port ?? 22) || 22,
                        username: String(s?.username ?? ""),
                        privateKeyPath: String(s?.privateKeyPath ?? s?.private_key_path ?? ""),
                        knownHostsPath: String(s?.knownHostsPath ?? s?.known_hosts_path ?? ""),
                        remoteRoot: String(s?.remoteRoot ?? s?.remote_root ?? ""),
                        strictHostKeyChecking: s?.strictHostKeyChecking == null ? true : !!s.strictHostKeyChecking,
                        extraArgs: Array.isArray(s?.extraArgs ?? s?.extra_args) ? (s.extraArgs ?? s.extra_args).map(String) : [],
                        enabled: s?.enabled == null ? true : !!s.enabled,
                        notes: String(s?.notes ?? ""),
                      }))
                    : [],
              projects: Array.isArray((store as any).projects)
                ? (store as any).projects.map((p: any) => ({
                    ...p,
                    id: String(p?.id ?? makeId("proj")),
                    name: String(p?.name ?? "Project"),
                    enabled: p?.enabled == null ? true : !!p.enabled,
                    description: String(p?.description ?? ""),
                    manipulationRootPath: String(p?.manipulationRootPath ?? p?.manipulation_root_path ?? ""),
                    datawebEnabled: p?.datawebEnabled == null ? true : !!p.datawebEnabled,
                    repoIds: Array.isArray(p?.repoIds) ? p.repoIds.map(String) : [],
                    entryIds: Array.isArray(p?.entryIds) ? p.entryIds.map(String) : [],
                    mcpIds: Array.isArray(p?.mcpIds) ? p.mcpIds.map(String) : [],
                    sshIds: Array.isArray(p?.sshIds) ? p.sshIds.map(String) : [],
                  }))
                : [],
            });
          }
        } catch {}

        try {
          const cfg = await invoke<any>("get_model_config");
          setModelConfig({
            coderEnabled: cfg?.coderEnabled ?? cfg?.coder_enabled ?? true,
            visionEnabled: cfg?.visionEnabled ?? cfg?.vision_enabled ?? true,
            customModels: Array.isArray(cfg?.customModels ?? cfg?.custom_models)
              ? (cfg.customModels ?? cfg.custom_models).map((m: any) => ({
                  id: String(m?.id ?? ""),
                  filename: String(m?.filename ?? ""),
                  nickname: String(m?.nickname ?? ""),
                  enabled: !!m?.enabled,
                  note: String(m?.note ?? ""),
                }))
              : [],
            modelNotes: (cfg?.modelNotes ?? cfg?.model_notes ?? {}) as Record<string, string>,
            statusVerbs: (cfg?.statusVerbs ?? cfg?.status_verbs ?? {}) as Record<string, string>,
          });
        } catch {}

        setModelStatus("checking");
        setStartupStatus({ active: true, step: "Checking local setup...", progress: 25 });
        
        // Run migration on startup (converts legacy TabMemory to Snapshot format)
        try {
          setStartupStatus({ active: true, step: "Upgrading saved memory format...", progress: 35 });
          const migrated = await migrateOpenTabsToSnapshotFormat();
          if (migrated > 0) {
            console.log("[App] Migrated", migrated, "legacy tab files to Snapshot format");
          }
        } catch (e) {
          console.warn("[App] Migration skipped/failed (non-fatal):", e);
        }

        await Promise.all([
          // universal prompts and open tabs (with retries)
          (async () => {
            setStartupStatus({ active: true, step: "Loading chat tabs from disk...", progress: 55 });
            const snaps = await tryListOpenTabs();
            if (!snaps.length) {
              console.warn("No snapshots yet (Tauri late or store empty).");
              setStartupStatus({ active: true, step: "No previous chats found", progress: 80 });
              return;
            }

            // select freshest up to 8, then render in stable tab-id order for predictable UI order
            const capped = snaps
              .slice()
              .sort((a, b) => (b.last_updated ?? 0) - (a.last_updated ?? 0))
              .slice(0, 8);

            console.log("[App] Loaded", capped.length, "snapshots on startup");
            const tabEntriesRaw = capped.map((snap) => {
              const id = typeof snap.tab_id === "number" ? snap.tab_id : (snap as any).tab_id ?? 1;
              const tab: Tab = {
                id,
                title: String(snap.title || `Tab #${id}`),
                type: "chat",
                messages: normalizeMessages(snap.messages),
                promptHistory: Array.isArray((snap as any).promptHistory) ? (snap as any).promptHistory.map(String) : [],
              };
              return { tab, snap };
            });

            // Deduplicate by tab id (keep most recently updated snapshot), then sort by id.
            const byId = new Map<number, { tab: Tab; snap: Snapshot }>();
            for (const entry of tabEntriesRaw) {
              const existing = byId.get(entry.tab.id);
              const curTs = (entry.snap.last_updated ?? 0);
              const prevTs = (existing?.snap.last_updated ?? 0);
              if (!existing || curTs >= prevTs) {
                byId.set(entry.tab.id, entry);
              }
            }
            const tabEntries = Array.from(byId.values()).sort((a, b) => a.tab.id - b.tab.id);

            const newTabs: Tab[] = tabEntries.map((e) => e.tab);
            const snapById = new Map<number, Snapshot>(tabEntries.map((e) => [e.tab.id, e.snap]));
            const initialProjectMap: Record<number, string | null> = {};
            for (const entry of tabEntries) {
              initialProjectMap[entry.tab.id] = (entry.snap as any)?.associatedProjectId ?? null;
            }

            setTabs(newTabs);
            setTabProjectMap(initialProjectMap);
            setActiveTab(newTabs[0].id);
            setStartupStatus({ active: true, step: "Hydrating recent chat messages...", progress: 75 });

            // Hydration fallback: if frontend messages are missing, try a per-tab read snapshot
            for (const tab of newTabs) {
              const snap = snapById.get(tab.id);
              if (!tab.messages || tab.messages.length === 0) {
                try {
                  if (typeof readTabSnapshot === "function") {
                    const full = await readTabSnapshot((snap as any)?.tab_id ?? (snap as any)?.id ?? tab.id);
                    if (full?.messages?.length) {
                      setTabs(prev =>
                        prev.map(t =>
                          t.id === tab.id ? { ...t, messages: normalizeMessages(full.messages) } : t
                        )
                      );
                    }
                  }
                } catch (e) {
                  console.warn("readTabSnapshot failed:", e);
                }
              }
            }

            // No backend TabMemory restoration; snapshots are the single source of truth
            setStartupStatus({ active: true, step: `Loaded ${newTabs.length} tab(s)`, progress: 90 });
            setStartupStatus({ active: true, step: "Preparing UI state and shortcuts...", progress: 96 });
          })()
        ]);

        setModelStatus("ready");
        setStartupStatus({ active: true, step: "Startup complete", progress: 100 });
        setTimeout(() => {
          setStartupStatus((s) => ({ ...s, active: false }));
        }, 900);
      } catch (e) {
        setModelStatus("error");
        setStartupStatus({ active: true, step: "Startup encountered an error", progress: 100 });
        setTimeout(() => {
          setStartupStatus((s) => ({ ...s, active: false }));
        }, 1800);
        console.error("Tauri never became ready or init failed", e);
      } finally {
        done = true;
        clearTimeout(startupTimeout);
      }
    })();
  }, []);

  // Auto-resolve "Checking model..." if it lingers after startup completes (e.g. Apply+Restart race)
  useEffect(() => {
    if (startupStatus.active || modelStatus !== "checking") return;
    const t = setTimeout(() => setModelStatus((s) => s === "checking" ? "ready" : s), 10000);
    return () => clearTimeout(t);
  }, [startupStatus.active, modelStatus]);


  useEffect(() => {
    if (historyRef.current) {
      historyRef.current.scrollTop = historyRef.current.scrollHeight;
    }
  }, [currentTab?.messages]);

  useEffect(() => {
    if (!modelConfig.coderEnabled && useCoder) {
      setUseCoder(false);
    }
  }, [modelConfig.coderEnabled, useCoder]);

  useEffect(() => {
    if (tabs.length === 0) return;
    if (!tabs.some(t => t.id === activeTab)) {
      const firstChat = tabs.find(t => t.type === "chat");
      setActiveTab((firstChat ?? tabs[0]).id);
    }
  }, [tabs, activeTab]);

  useEffect(() => {
    const chatIds = new Set(tabs.filter(t => t.type === "chat").map(t => t.id));
    setTabProjectMap((prev) => {
      const next: Record<number, string | null> = {};
      for (const [k, v] of Object.entries(prev)) {
        const id = Number(k);
        if (chatIds.has(id)) {
          next[id] = v;
        }
      }
      for (const id of chatIds) {
        if (!(id in next)) {
          next[id] = null;
        }
      }
      return shallowEqualTabProjectMap(prev, next) ? prev : next;
    });
  }, [tabs]);

  useEffect(() => {
    setProjectDescriptionDrafts((prev) => {
      const next: Record<string, string> = {};
      for (const p of repoStore.projects) {
        next[p.id] = prev[p.id] ?? p.description ?? "";
      }
      return next;
    });
    setProjectManipulationRootDrafts((prev) => {
      const next: Record<string, string> = {};
      for (const p of repoStore.projects) {
        next[p.id] = prev[p.id] ?? p.manipulationRootPath ?? "";
      }
      return next;
    });
    setProjectDescriptionDirty((prev) => {
      const next: Record<string, boolean> = {};
      for (const p of repoStore.projects) {
        const draft = projectDescriptionDrafts[p.id] ?? p.description ?? "";
        const rootDraft = projectManipulationRootDrafts[p.id] ?? p.manipulationRootPath ?? "";
        next[p.id] = !!prev[p.id] && (
          draft.trim() !== (p.description ?? "").trim() ||
          rootDraft.trim() !== (p.manipulationRootPath ?? "").trim()
        );
      }
      return next;
    });
    if (!repoAssistantTargetRepoId && repoStore.repos.length > 0) {
      setRepoAssistantTargetRepoId(repoStore.repos[0].id);
    }
  }, [repoStore.projects, repoStore.repos]);

  // Sync mcpDraft from repoStore.mcps whenever the store is updated externally (load / Add MCP / remove).
  // We compare by IDs so that mid-edit changes to the draft fields are not clobbered.
  useEffect(() => {
    setMcpDraft(prev => {
      const prevIds = new Set(prev.map(m => m.id));
      const storeIds = new Set(repoStore.mcps.map(m => m.id));
      // If the set of IDs changed (add/remove), replace whole draft; otherwise don't clobber user edits.
      const idsMatch = prevIds.size === storeIds.size && [...storeIds].every(id => prevIds.has(id));
      return idsMatch ? prev : [...repoStore.mcps];
    });
  }, [repoStore.mcps]);

  useEffect(() => {
    setSshDraft(prev => {
      const prevIds = new Set(prev.map(s => s.id));
      const storeIds = new Set(repoStore.sshs.map(s => s.id));
      const idsMatch = prevIds.size === storeIds.size && [...storeIds].every(id => prevIds.has(id));
      return idsMatch ? prev : [...repoStore.sshs];
    });
  }, [repoStore.sshs]);

  useEffect(() => {
    if (!thinking) return;
    const interval = setInterval(() => {
      setEllipsis((prev) => (prev.length < 3 ? prev + "." : "."));
    }, 500);
    return () => clearInterval(interval);
  }, [thinking]);

  useEffect(() => {
    if (!showChatBridgeControls) {
      setChatBridgePanelOpen(false);
    }
  }, [showChatBridgeControls]);

  // Poll open windows and system stats while Desktop Dashboard is active
  useEffect(() => {
    if (!desktopDashboardEnabled) return;
    const pollWindows = async () => {
      try {
        const ws = await invoke("list_windows") as WindowInfo[];
        setWindowList(Array.isArray(ws) ? ws : []);
      } catch {
        setWindowList([]);
      }
    };
    const pollStats = async () => {
      try {
        const s = await invoke("get_system_stats") as { cpuPercent: number; memUsedMb: number; memTotalMb: number };
        setSystemStats(s);
      } catch { /* ignore */ }
    };
    pollWindows();
    pollStats();
    const wId = setInterval(pollWindows, 5000);
    const sId = setInterval(pollStats, 4000);
    return () => { clearInterval(wId); clearInterval(sId); };
  }, [desktopDashboardEnabled]);

  // Ctrl+T new tab, Ctrl+Shift+T restore last closed tab
  useEffect(() => {
    const handler = async (e: KeyboardEvent) => {
      if (menuLocked) return;
      if (e.ctrlKey && !e.shiftKey && e.key.toLowerCase() === "t") {
        e.preventDefault();
        const chatTabs = tabs.filter(tab => tab.type === "chat");
        if (chatTabs.length >= 8) return;
        await handleNewTab();
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "t") {
        e.preventDefault();
        const chatTabs = tabs.filter(tab => tab.type === "chat");
        if (chatTabs.length >= 8) return;
        try {
          const snap = await invoke("restore_last_closed_tab") as Snapshot;
          if (snap && Array.isArray(snap.messages) && snap.messages.length) {
            const usedIds = tabs.map(t => t.id);
            let newId = 1;
            while (usedIds.includes(newId)) newId++;
            const labelNumber = nextChatLabelNumber(tabs);
            const newTab: Tab = {
              id: newId,
              title: String(snap.title || `Tab #${labelNumber}`),
              type: "chat",
              messages: normalizeMessages(snap.messages),
              promptHistory: Array.isArray((snap as any).promptHistory) ? (snap as any).promptHistory.map(String) : [],
            };
            setTabs(prev => [...prev, newTab]);
            setTabProjectMap((prev) => ({ ...prev, [newId]: snap.associatedProjectId ?? null }));
            setActiveTab(newId);
            // Persist the restored snapshot under the new tab id
            await persistSnapshot(newId, {
              title: newTab.title,
              messages: snap.messages,
              associatedProjectId: snap.associatedProjectId ?? null,
              microSummary: snap.microSummary,
              dialogueBullets: snap.dialogueBullets,
              summary: snap.summary,
              artifacts: snap.artifacts,
              promptHistory: Array.isArray((snap as any).promptHistory) ? (snap as any).promptHistory.map(String) : [],
              last_updated: Math.floor(Date.now() / 1000)
            });
          }
        } catch {}
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
    // eslint-disable-next-line
  }, [tabs, menuLocked]);

  function updateTabMessages(tabId: number, updater: (msgs: Message[]) => Message[]) {
    setTabs(tabs =>
      tabs.map(tab =>
        tab.id === tabId
          ? { ...tab, messages: updater(tab.messages || []) }
          : tab
      )
    );
  }

  function startTabRename(tabId: number) {
    const tab = tabs.find(t => t.id === tabId && t.type === "chat");
    if (!tab) return;
    setEditingTabId(tabId);
    setEditingTabTitle(tab.title);
  }

  async function commitTabRename(tabId: number) {
    const nextTitle = editingTabTitle.trim() || `Tab #${tabId}`;
    setTabs(prev => prev.map(tab => tab.id === tabId ? { ...tab, title: nextTitle } : tab));
    setEditingTabId(null);
    setEditingTabTitle("");
    try {
      await persistTabSnapshotState(tabId, { title: nextTitle });
    } catch (e) {
      console.warn("Failed to persist tab rename", e);
    }
  }

  function cancelTabRename() {
    setEditingTabId(null);
    setEditingTabTitle("");
  }

  function moveChatTab(dragTabId: number, targetTabId: number) {
    if (dragTabId === targetTabId) return;
    setTabs(prev => {
      const settingsTab = prev.find(tab => tab.type === "settings") || null;
      const chatTabs = prev.filter(tab => tab.type === "chat");
      const fromIndex = chatTabs.findIndex(tab => tab.id === dragTabId);
      const toIndex = chatTabs.findIndex(tab => tab.id === targetTabId);
      if (fromIndex === -1 || toIndex === -1) return prev;
      const reordered = [...chatTabs];
      const [moved] = reordered.splice(fromIndex, 1);
      reordered.splice(toIndex, 0, moved);
      return settingsTab ? [...reordered, settingsTab] : reordered;
    });
  }

  function removeImageFromMessage(tabId: number, msgIndex: number, imageIndex: number) {
    updateTabMessages(tabId, (prev) =>
      prev.map((m, i) => {
        if (i !== msgIndex || !Array.isArray((m as any).images)) return m;
        const nextImgs = ((m as any).images as string[]).filter((_, idx) => idx !== imageIndex);
        return { ...m, ...(nextImgs.length ? { images: nextImgs } : {}) };
      })
    );
  }

  const [thinkingModel, setThinkingModel] = useState<string | null>(null);
  const [irisStatus, setIrisStatus] = useState<"idle" | "thinking" | "summarizing" | "responding" | "coding">("idle");
  const [plannerRoutedModels, setPlannerRoutedModels] = useState<string[]>([]);
  const [plannerRouteSummary, setPlannerRouteSummary] = useState<string>("");
  const [respondingTab, setRespondingTab] = useState<number | null>(null);
  const [thinkingStep, setThinkingStep] = useState<string>("");
  const [thinkingSeconds, setThinkingSeconds] = useState<number>(0);

  const { stream } = useOllamaStream();
  function appendWorkLog(tabId: number, text: string) {
    const normalized = String(text || "").trim();
    if (!normalized) return;
    const ts = Date.now();
    setWorkLogByTab((prev) => {
      const existing = Array.isArray(prev[tabId]) ? prev[tabId] : [];
      if (existing.length > 0 && existing[existing.length - 1].text === normalized) {
        return prev;
      }
      return { ...prev, [tabId]: [...existing, { ts, text: normalized }].slice(-24) };
    });
  }

  function setThinkingProgress(tabId: number, step: string) {
    setThinkingStep(step);
    appendWorkLog(tabId, step);
  }

  function clearWorkLog(tabId: number) {
    setWorkLogByTab((prev) => ({ ...prev, [tabId]: [] }));
  }

  function setRoutineStepStatus(stepId: string, patch: Partial<RoutineStep>) {
    setActiveRoutine(prev => {
      if (!prev) return prev;
      return {
        ...prev,
        steps: prev.steps.map(s => s.id === stepId ? { ...s, ...patch } : s),
      };
    });
  }

  async function executeRoutinePlan(planRaw: any, userText: string, tabId: number): Promise<{ images: string[]; windowHint: string }> {
    if (!desktopToolsEnabled) {
      const proceed = await askCenteredConfirm({
        title: "Enable Desktop Investigation?",
        message: "This routine needs desktop window visibility and screenshot capture. Enable Desktop Tools now?",
        confirmLabel: "Enable",
        cancelLabel: "Cancel",
      });
      if (!proceed) {
        return { images: [], windowHint: "Desktop tools disabled by user; no screenshot was captured." };
      }
      setDesktopToolsEnabled(true);
      try {
        await invoke("set_setup_flags", { args: { desktopToolsEnabled: true } });
      } catch {}
    }

    const rawSteps = Array.isArray(planRaw?.steps) ? planRaw.steps : [];
    const steps: RoutineStep[] = rawSteps.map((s: any, idx: number) => ({
      id: String(s?.id ?? `routine_step_${idx + 1}`),
      type: String(s?.stepType ?? s?.step_type ?? "llm_reply") as RoutineStepType,
      label: String(s?.label ?? s?.stepType ?? s?.step_type ?? "step"),
      status: "pending",
    }));
    const routine: ActiveRoutine = {
      id: String(planRaw?.id ?? makeId("routine")),
      goal: String(planRaw?.goal ?? userText),
      steps,
      isLongRunning: !!(planRaw?.isLongRunning ?? planRaw?.is_long_running),
      status: "pending_confirm",
    };
    setActiveRoutine(routine);
    setActiveRoutineTabId(tabId);
    appendWorkLog(tabId, `Routine started: ${routine.goal}`);

    if (routine.isLongRunning) {
      const proceed = await askCenteredConfirm({
        title: "Run Long Routine?",
        message: "Iris wants to run a longer routine (screen analysis + code/action steps). Continue?",
        confirmLabel: "Run",
        cancelLabel: "Cancel",
      });
      if (!proceed) {
        setActiveRoutine(prev => prev ? { ...prev, status: "cancelled" } : prev);
        return { images: [], windowHint: "Routine cancelled by user." };
      }
    }

    setActiveRoutine(prev => prev ? { ...prev, status: "running" } : prev);

    let windows: WindowInfo[] = [];
    let windowHint = "";
    try {
      const ws = await invoke("list_windows") as WindowInfo[];
      windows = Array.isArray(ws) ? ws : [];
      setWindowList(windows);
    } catch {
      windows = [];
      setWindowList([]);
    }

    const lower = userText.toLowerCase();
    const tokens = lower.split(/\W+/).filter(t => t.length >= 4);
    const scored = windows.map((w) => {
      const title = (w.title || "").toLowerCase();
      const proc = (w.processName || "").toLowerCase();
      let score = 0;
      if (lower.includes("godot") && (title.includes("godot") || proc.includes("godot"))) score += 8;
      if (proc.includes("godot")) score += 5;
      if (title.includes("visual studio") || title.includes("vscode")) score += lower.includes("code") ? 4 : 1;
      for (const t of tokens) {
        if (title.includes(t)) score += 1;
      }
      return { w, score };
    }).sort((a, b) => b.score - a.score);
    const pick = scored[0]?.w || windows[0];
    const topScore = scored[0]?.score ?? 0;
    const secondScore = scored[1]?.score ?? Number.NEGATIVE_INFINITY;
    const ambiguousWindowTarget = scored.length > 1 && Math.abs(topScore - secondScore) <= 1;
    if (pick) {
      windowHint = ambiguousWindowTarget
        ? `Window targeting is ambiguous between multiple windows. Trying top candidate first: ${pick.title} (${pick.processName}), score ${topScore}.`
        : `Window target selected: ${pick.title} (${pick.processName}), score ${topScore}.`;
    }

    const capturedImages: string[] = [];
    let sawRoutineError = false;

    for (const step of steps) {
      setRoutineStepStatus(step.id, { status: "running" });
      appendWorkLog(tabId, `Routine step running: ${step.label}`);
      try {
        if (step.type === "screenshot") {
          let base64 = "";
          if (pick?.id) {
            try {
              base64 = await invoke("take_window_screenshot", { windowId: pick.id }) as string;
            } catch {
              base64 = await invoke("take_screenshot") as string;
            }
          } else {
            base64 = await invoke("take_screenshot") as string;
          }
          const dataUrl = `data:image/png;base64,${base64}`;
          capturedImages.push(dataUrl);
          setRoutineStepStatus(step.id, { status: "done", result: "Screenshot captured", imageData: dataUrl });
          appendWorkLog(tabId, `Routine step done: ${step.label}`);
          updateTabMessages(tabId, msgs => ([...msgs, { role: "llm", text: "Captured screenshot for analysis.", images: [dataUrl] }]));
        } else {
          setRoutineStepStatus(step.id, { status: "done" });
          appendWorkLog(tabId, `Routine step done: ${step.label}`);
        }
      } catch (e: any) {
        sawRoutineError = true;
        setRoutineStepStatus(step.id, { status: "error", error: e?.message || String(e) });
        appendWorkLog(tabId, `Routine step error: ${step.label}`);
      }
    }

    setActiveRoutine(prev => prev ? { ...prev, status: sawRoutineError ? "error" : "done" } : prev);
    appendWorkLog(tabId, sawRoutineError ? "Routine completed with errors" : "Routine completed");
    return { images: capturedImages, windowHint };
  }
  async function sendMessage(e: React.FormEvent) {
    e.preventDefault();
    if (menuLocked) {
      return;
    }

    // 1) Validate input and tab BEFORE setting any flags
    const text = input.trim();
    let hasImages = pendingImages.length > 0;
    const inferredEarlyActions = inferConversationalActions(text);
    const hasConversationalActionIntent = !!(
      inferredEarlyActions.fs ||
      inferredEarlyActions.ssh ||
      inferredEarlyActions.desktop ||
      inferredEarlyActions.mcp ||
      inferredEarlyActions.project
    );
    const bridgeOrFsExplicit = /^\/(mcp|fs|ssh|desktop|project)\b/i.test(text);
    const referentialFollowup = isReferentialFollowup(text);
    const quickMode = !hasImages && !bridgeOrFsExplicit && !hasConversationalActionIntent && isSimpleCasualRequest(text) && !referentialFollowup;
    if ((!text && !hasImages) || currentTab?.type !== "chat") return;
    if (hasImages && !modelConfig.visionEnabled) {
      alert("Vision model is disabled. Enable Vision in Settings > LLMs to send images.");
      return;
    }

    // Capture the originating tab id so async work always attributes to the original tab
    const requestTabId = activeTab;

    // Clear input and show user bubble immediately when send is pressed.
    setInput("");
    queueMicrotask(() => { if (inputRef.current) { inputRef.current.style.height = "auto"; }});
    const userDisplayText = text || `Attached ${pendingImages.length} image${pendingImages.length === 1 ? "" : "s"}.`;
    const userImageDataUrls = pendingImages.map((img) => img.dataUrl);
    let allImageDataUrls = [...userImageDataUrls];
    const promptHistoryForTurn = text.trim()
      ? [text.trim(), ...((currentTab?.promptHistory || []).filter((item) => item !== text.trim()))].slice(0, 60)
      : (currentTab?.promptHistory || []);
    updateTabMessages(requestTabId, msgs => updateMessagesAppendUser(msgs, userDisplayText, userImageDataUrls));
    if (text.trim()) {
      updateTabPromptHistory(requestTabId, text);
    }
    promptHistoryIndexRef.current = -1;
    promptHistoryDraftRef.current = "";

    const flowAdjustmentNote = applyFlowAdjustmentFeedback(requestTabId, text);
    const recentActionTrace = actionTraceByTab[requestTabId] || [];
    if (isActionWhyQuestion(text) && recentActionTrace.length) {
      const lines = recentActionTrace.slice(-6).map((a) => {
        const when = new Date(a.ts * 1000).toLocaleTimeString();
        return `- ${when}: ${a.action} | reason: ${a.reason} | args: ${a.argsSummary} | outcome: ${a.outcome}`;
      }).join("\n");
      const replyText = [
        "Here is why I took those steps:",
        lines,
        "I choose actions based on your request context, project links, and available bridge/tool state. If you want a different flow, tell me the preference and I will adapt immediately.",
      ].join("\n");
      updateTabMessages(requestTabId, msgs => insertLLMBubble(msgs, replyText));
      try {
        await persistDirectReply({
          requestTabId,
          tabs,
          userDisplayText,
          replyText,
          associatedProjectId: tabProjectMap[requestTabId] ?? null,
          promptHistory: promptHistoryForTurn,
        });
      } catch {}
      return;
    }

    // Interpret/routing is owned by backend planner.
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

    let preloadedSnapshot: Snapshot | null = null;
    clearWorkLog(requestTabId);
    setThinkingProgress(requestTabId, "Loading tab memory...");
    try {
      preloadedSnapshot = await readTabSnapshot(requestTabId);
    } catch {}

    const hasTabProjectForPlanner = Object.prototype.hasOwnProperty.call(tabProjectMap, requestTabId);
    const selectedProjectIdForPlanner = hasTabProjectForPlanner
      ? (tabProjectMap[requestTabId] ?? null)
      : ((preloadedSnapshot as any)?.associatedProjectId ?? null);
    const selectedProjectForPlanner = repoStore.projects.find((p) => p.id === selectedProjectIdForPlanner && p.enabled) || null;
    const linkedReposForPlanner = selectedProjectForPlanner
      ? repoStore.repos.filter((r) => selectedProjectForPlanner.repoIds.includes(r.id) && r.enabled)
      : [];
    const selectedEntriesForPlanner = selectedProjectForPlanner
      ? linkedReposForPlanner.flatMap((r) =>
          r.entries.filter((entry) => (selectedProjectForPlanner.entryIds.length
            ? selectedProjectForPlanner.entryIds.includes(entry.id)
            : r.selectedEntryIds.includes(entry.id)))
        )
      : [];
    const linkedSshForPlanner = selectedProjectForPlanner
      ? repoStore.sshs.filter((s) => selectedProjectForPlanner.sshIds?.includes(s.id) && s.enabled)
      : [];
    const projectContextForPlanner = selectedProjectForPlanner
      ? [
          `Active project: ${selectedProjectForPlanner.name}`,
          `Description: ${selectedProjectForPlanner.description || "(none)"}`,
          `Manipulation root: ${selectedProjectForPlanner.manipulationRootPath || "(none)"}`,
          `Linked repos: ${linkedReposForPlanner.map((r) => r.name).join(", ") || "(none)"}`,
          `Linked SSH: ${linkedSshForPlanner.map((s) => `${s.name} (${s.username ? `${s.username}@` : ""}${s.host}:${s.port || 22})`).join(", ") || "(none)"}`,
          `Selected references (${selectedEntriesForPlanner.length}): ${selectedEntriesForPlanner.slice(0, 20).map((e) => e.path).join(", ") || "(none)"}`,
        ].join("\n")
      : "";
    let projectDatawebForPlanner = "";
    let universalDatawebForPlanner = "";
    if (!quickMode && selectedProjectForPlanner?.datawebEnabled) {
      try {
        projectDatawebForPlanner = await withTimeout(
          invoke("read_project_dataweb", { projectId: selectedProjectForPlanner.id }) as Promise<string>,
          1200,
          "read_project_dataweb(planner)"
        );
      } catch {}
    }
    if (!quickMode && universalDatawebEnabled) {
      try {
        universalDatawebForPlanner = await withTimeout(
          invoke("read_universal_dataweb") as Promise<string>,
          1200,
          "read_universal_dataweb(planner)"
        );
      } catch {}
    }

    let plannerV2: any = null;
    let windowHint = "";
    let plannerPrompt = "";
    let plannerResolverUsed: ResolverKind = "none";
    let plannerPath: "v2" | "legacy" = "legacy";
    let plannerSuggestedGodotVersion = "unspecified";
    const runtimeCaps = profileCapabilities(llmProfile);

    let coderish = useCoder;
    let primaryModel = hasImages ? "iris-vision:latest" : (coderish ? "iris-coder:latest" : "iris-organizer:latest");

    if (interpretV2Enabled && !quickMode) {
      setThinkingProgress(requestTabId, "Interpreting request...");
      try {
        const caps = profileCapabilities(llmProfile);
        const tb = caps.tokenBudget;
        const enabledCustom = modelConfig.customModels.filter((m) => m.enabled);
        const enabledCustomModels = enabledCustom.map((m) => m.filename.replace(/\.txt$/i, ""));
        const dispatchNote = [
          `Coder: ${modelConfig.coderEnabled ? "ENABLED" : "DISABLED"}`,
          `Vision: ${modelConfig.visionEnabled ? "ENABLED" : "DISABLED"}`,
          `Custom models enabled: ${enabledCustom.length}`,
          ...enabledCustom.map((m) => `- ${m.nickname || m.filename}: ${m.note || "(no note)"}`),
          ...Object.entries(modelConfig.modelNotes || {}).map(([filename, note]) => `- ${filename}: ${note || "(no note)"}`),
        ].join("\n");
        const attempts: any[] = [
          { tabId: requestTabId, userText: text, tokenBudget: tb, useCoder, coderEnabled: modelConfig.coderEnabled, visionEnabled: modelConfig.visionEnabled, customEnabledModels: enabledCustomModels, organizerDispatchNote: dispatchNote },
          {
            tabId: requestTabId,
            userText: text,
            tokenBudget: tb,
            useCoder,
            coderEnabled: modelConfig.coderEnabled,
            visionEnabled: modelConfig.visionEnabled,
            customEnabledModels: enabledCustomModels,
            organizerDispatchNote: dispatchNote,
            assistantName: assistantLabel,
            modelProfile: llmProfile,
            networkEnabled,
            reposEnabled: reposContextActive,
            mcpEnabled: mcpContextActive,
            desktopToolsEnabled: desktopToolsEnabled,
            selectedProjectName: selectedProjectForPlanner?.name || "",
            projectContext: projectContextForPlanner,
            projectDataweb: projectDatawebForPlanner.slice(-3000),
            universalDataweb: universalDatawebForPlanner.slice(-3000),
          },
          {
            tab_id: requestTabId,
            user_text: text,
            token_budget: tb,
            use_coder: useCoder,
            coder_enabled: modelConfig.coderEnabled,
            vision_enabled: modelConfig.visionEnabled,
            custom_enabled_models: enabledCustomModels,
            organizer_dispatch_note: dispatchNote,
            assistant_name: assistantLabel,
            model_profile: llmProfile,
            network_enabled: networkEnabled,
            repos_enabled: reposContextActive,
            mcp_enabled: mcpContextActive,
            desktop_tools_enabled: desktopToolsEnabled,
            selected_project_name: selectedProjectForPlanner?.name || "",
            project_context: projectContextForPlanner,
            project_dataweb: projectDatawebForPlanner.slice(-3000),
            universal_dataweb: universalDatawebForPlanner.slice(-3000),
          },
          {
            args: {
              tabId: requestTabId,
              userText: text,
              tokenBudget: tb,
              useCoder,
              coderEnabled: modelConfig.coderEnabled,
              visionEnabled: modelConfig.visionEnabled,
              customEnabledModels: enabledCustomModels,
              organizerDispatchNote: dispatchNote,
              assistantName: assistantLabel,
              modelProfile: llmProfile,
              networkEnabled,
              reposEnabled: reposContextActive,
              mcpEnabled: mcpContextActive,
              desktopToolsEnabled: desktopToolsEnabled,
              selectedProjectName: selectedProjectForPlanner?.name || "",
              projectContext: projectContextForPlanner,
              projectDataweb: projectDatawebForPlanner.slice(-3000),
              universalDataweb: universalDatawebForPlanner.slice(-3000),
            }
          },
          {
            args: {
              tab_id: requestTabId,
              user_text: text,
              token_budget: tb,
              use_coder: useCoder,
              coder_enabled: modelConfig.coderEnabled,
              vision_enabled: modelConfig.visionEnabled,
              custom_enabled_models: enabledCustomModels,
              organizer_dispatch_note: dispatchNote,
              assistant_name: assistantLabel,
              model_profile: llmProfile,
              network_enabled: networkEnabled,
              repos_enabled: reposContextActive,
              mcp_enabled: mcpContextActive,
              desktop_tools_enabled: desktopToolsEnabled,
              selected_project_name: selectedProjectForPlanner?.name || "",
              project_context: projectContextForPlanner,
              project_dataweb: projectDatawebForPlanner.slice(-3000),
              universal_dataweb: universalDatawebForPlanner.slice(-3000),
            }
          },
        ];
        for (const payload of attempts) {
          try {
            const plan = await invoke("interpret_turn_v2", payload) as any;
            if (plan && typeof plan === "object") {
              plannerV2 = plan;
              break;
            }
          } catch {}
        }
        if (plannerV2) {
          plannerPath = "v2";
          coderish = !!(plannerV2.shouldUseCoder ?? coderish);
          primaryModel = hasImages
            ? "iris-vision:latest"
            : String(plannerV2.model || (coderish ? "iris-coder:latest" : "iris-organizer:latest"));
          plannerPrompt = String(plannerV2.prompt || "");
          plannerResolverUsed = (plannerV2.resolverUsed as ResolverKind) || "none";
          plannerSuggestedGodotVersion = String(plannerV2.suggestedGodotVersion || plannerV2.suggested_godot_version || "unspecified");
          setPlannerRoutedModels(Array.isArray(plannerV2.routedModels ?? plannerV2.routed_models) ? (plannerV2.routedModels ?? plannerV2.routed_models) : []);
          setPlannerRouteSummary(String(plannerV2.routeSummary ?? plannerV2.route_summary ?? ""));
          const cc = plannerV2.compiledContext || {};
          compiled = {
            microSummary: cc.microSummary ?? cc.micro_summary ?? compiled.microSummary,
            dialogueBullets: cc.dialogueBullets ?? cc.dialogue_bullets ?? compiled.dialogueBullets,
            recentTranscript: cc.recentTranscript ?? cc.recent_transcript ?? compiled.recentTranscript,
            recentArtifacts: cc.recentArtifacts ?? cc.recent_artifacts ?? compiled.recentArtifacts,
          };

          // Optional routine execution path: screen capture + vision/coder staging.
          const routinePlan = plannerV2.routinePlan ?? plannerV2.routine_plan;
          if (routinePlan && typeof routinePlan === "object") {
            const routineRes = await executeRoutinePlan(routinePlan, text, requestTabId);
            if (Array.isArray(routineRes.images) && routineRes.images.length) {
              allImageDataUrls = [...allImageDataUrls, ...routineRes.images];
              hasImages = allImageDataUrls.length > 0;
              primaryModel = "iris-vision:latest";
            }
            windowHint = routineRes.windowHint || "";
          }
        }
      } catch (e) {
        console.warn("interpret_turn_v2 failed; using minimal fallback prompt", e);
      }
    }

    // Show immediate feedback while we run model/server preflight.
    setModelStatus("ready");
    setThinking(true);
    setThinkingSeconds(0);
    setRespondingTab(activeTab);
    setIrisStatus("thinking");
    setIsGenerating(true);
    setThinkingProgress(requestTabId, "Waking local runtime...");

    const weatherRequest = !hasImages && !bridgeOrFsExplicit
      ? parseWeatherRequest(text)
      : null;
    if (weatherRequest) {
      const finishWeatherPath = async (replyText: string) => {
        updateTabMessages(requestTabId, msgs => insertLLMBubble(msgs, replyText));
        try {
          setThinkingProgress(requestTabId, "Saving memory...");
          await persistDirectReply({
            requestTabId,
            tabs,
            userDisplayText,
            replyText,
            associatedProjectId: null,
            promptHistory: promptHistoryForTurn,
          });
        } catch (e) {
          console.warn("[weather-fast-path] snapshot save failed (non-fatal):", e);
        }
        setThinking(false);
        setThinkingModel(null);
        setIsGenerating(false);
        setIrisStatus("idle");
        setThinkingStep("");
        setRespondingTab(null);
        abortController.current = null;
      };

      if (!networkEnabled) {
        await finishWeatherPath("Network access is currently disabled, so I can't fetch a live forecast. Enable Network in Settings and ask again.");
        return;
      }

      try {
        setThinkingProgress(requestTabId, "Fetching forecast...");
        const weather = await withTimeout(
          invoke("weather_lookup", {
            location: weatherRequest.location,
            dayOffset: weatherRequest.dayOffset,
          }) as Promise<any>,
          7000,
          "weather_lookup"
        );
        const weatherReply = buildWeatherReply(weather);
        await finishWeatherPath(weatherReply);
        return;
      } catch (e) {
        console.warn("weather lookup failed", e);
        await finishWeatherPath(`I couldn't retrieve the live forecast right now: ${String((e as any)?.message || e || "unknown error")}`);
        return;
      }
    }

    try {
      await ensureOllamaServer();
      setThinkingProgress(requestTabId, "Checking selected model...");
      await ensureModel(primaryModel);
    } catch (err: any) {
      try {
        await invoke("set_setup_flags", { args: { modelsVerified: false } });
      } catch {}
      setThinking(false);
      setIsGenerating(false);
      setIrisStatus("idle");
      setThinkingStep("");
      setRespondingTab(null);
      updateTabMessages(requestTabId, msgs => [
        ...msgs,
        { role: "llm", text: `[Setup error: ${err?.message || err}]` }
      ]);
      return;
    }

    // Create a local controller and assign it once
    const controller = new AbortController();
    abortController.current = controller;
    // reset stopped flag when starting a new send
    stoppedRef.current = false;

    // already marked as thinking above during preflight

    // message already appended above

    try {
      setThinkingProgress(requestTabId, "Gathering context...");
      // get_compiled_context: use planner-prepared context when available; otherwise legacy fetch.
      if (!plannerV2?.compiledContext) {
        try {
          const _caps = profileCapabilities(llmProfile);
          const _tb = _caps.tokenBudget;
          const attempts: any[] = [
            { tabId: requestTabId, tokenBudget: _tb },
            { tab_id: requestTabId, token_budget: _tb },
            { args: { tabId: requestTabId, tokenBudget: _tb } },
            { args: { tab_id: requestTabId, token_budget: _tb } },
          ];

          let result: any = null;
          let lastErr: unknown = null;
          for (const payload of attempts) {
            try {
              result = await invoke("get_compiled_context", payload);
              if (result && typeof result === "object") break;
            } catch (err) {
              lastErr = err;
            }
          }

          if (result && typeof result === "object") {
            compiled = {
              microSummary: result.microSummary ?? result.micro_summary ?? "",
              dialogueBullets: result.dialogueBullets ?? result.dialogue_bullets ?? "",
              recentTranscript: result.recentTranscript ?? result.recent_transcript ?? "",
              recentArtifacts: result.recentArtifacts ?? result.recent_artifacts ?? []
            };
          } else if (lastErr) {
            console.error("get_compiled_context failed", lastErr);
          }
        } catch (e) {
          console.error("get_compiled_context failed", e);
        }
      }

      let { microSummary, dialogueBullets: compiledDialogueBullets, recentTranscript, recentArtifacts } = compiled;
      let memorySnapshot: Snapshot | null = preloadedSnapshot;

      // Fallback: if compiled context is empty, rebuild transcript from disk snapshot.
      if (!recentTranscript?.trim()) {
        try {
          const snap = memorySnapshot || await readTabSnapshot(requestTabId);
          memorySnapshot = snap;
          if (snap?.messages?.length) {
            const transcriptTail = runtimeCaps.deepReasoningEnabled ? 36 : 24;
            const rebuilt = snap.messages
              .slice(-transcriptTail)
              .map((m: any) => `${m.role === "user" ? "User" : "Iris"}: ${String(m.text ?? "")}`)
              .join("\n");
            if (rebuilt.trim()) {
              recentTranscript = rebuilt;
              if (!microSummary?.trim()) microSummary = snap.microSummary || "";
              if (!compiledDialogueBullets?.trim()) compiledDialogueBullets = snap.dialogueBullets || "";
            }
          }
        } catch {}
      }

      if (!memorySnapshot) {
        try {
          memorySnapshot = await readTabSnapshot(requestTabId);
        } catch {}
      }

      const activeArtifact = pickActiveArtifact(
        recentArtifacts,
        memorySnapshot?.artifacts as Artifact[] | undefined
      );
      const plannerLanes = plannerV2?.memoryLanes || plannerV2?.memory_lanes;
      const memoryLanes = plannerLanes
        ? {
            project: plannerLanes.project ?? "",
            coding: plannerLanes.coding ?? "",
            recall: plannerLanes.recall ?? "",
          }
        : buildMemoryLanes({
        transcript: recentTranscript,
        microSummary: microSummary || "",
        dialogueBullets: compiledDialogueBullets || "",
        summary: memorySnapshot?.summary || "",
        activeArtifact,
      });

      const hasTabProject = Object.prototype.hasOwnProperty.call(tabProjectMap, requestTabId);
      const selectedProjectId = hasTabProject
        ? (tabProjectMap[requestTabId] ?? null)
        : ((preloadedSnapshot as any)?.associatedProjectId ?? (memorySnapshot as any)?.associatedProjectId ?? null);
      const selectedProject = repoStore.projects.find((p) => p.id === selectedProjectId && p.enabled) || null;
      const projectIdeationFastPath = !!selectedProject && !hasImages && isProjectIdeationRequest(text);
      const engineeringTaskRequest = !!selectedProject && !hasImages && !quickMode && /(create|build|implement|add|make|refactor|fix)/i.test(text) && /(project|scene|node|godot|feature|system|cloud|effect)/i.test(text);
      let activeManipulationRoot: { name: string; path: string } | null = null;
      let activeMcps: McpConnection[] = [];
      let projectContext = "";
      let deterministicProjectReply = "";
      const enabledProjects = quickMode ? [] : repoStore.projects.filter((p) => p.enabled);
      const projectRegistryContext = enabledProjects
        .map((p) => {
          const repoNames = reposContextActive
            ? repoStore.repos.filter((r) => p.repoIds.includes(r.id)).map((r) => r.name)
            : [];
          const mcpNames = mcpContextActive
            ? repoStore.mcps.filter((m) => p.mcpIds?.includes(m.id)).map((m) => m.name)
            : [];
          const sshNames = mcpContextActive
            ? repoStore.sshs.filter((s) => p.sshIds?.includes(s.id)).map((s) => s.name)
            : [];
          return `- ${p.name}: ${p.description || "(no description)"} | repos: ${repoNames.join(", ") || "(none)"} | mcps: ${mcpNames.join(", ") || "(none)"} | ssh: ${sshNames.join(", ") || "(none)"}`;
        })
        .join("\n");
      if (selectedProject) {
        const linkedRepos = reposContextActive
          ? repoStore.repos.filter((r) => selectedProject.repoIds.includes(r.id) && r.enabled)
          : [];
        if ((selectedProject.manipulationRootPath || "").trim()) {
          activeManipulationRoot = {
            name: selectedProject.name,
            path: selectedProject.manipulationRootPath.trim(),
          };
        }
        const linkedMcpsRaw = mcpContextActive
          ? repoStore.mcps.filter((m) => selectedProject.mcpIds?.includes(m.id) && m.enabled)
          : [];
        const linkedMcps = runtimeCaps.multiMcpEnabled ? linkedMcpsRaw : linkedMcpsRaw.slice(0, 1);
        const linkedSshs = mcpContextActive
          ? repoStore.sshs.filter((s) => selectedProject.sshIds?.includes(s.id) && s.enabled)
          : [];
        activeMcps = linkedMcps;
        const selectedEntries = linkedRepos.flatMap((r) =>
          r.entries.filter((e) => (selectedProject.entryIds.length ? selectedProject.entryIds.includes(e.id) : r.selectedEntryIds.includes(e.id)))
        );
        const previewLimit = runtimeCaps.fullRagEnabled ? 24 : 12;
        const entriesPreview = selectedEntries.slice(0, previewLimit).map((e) => `- ${e.isDir ? "[DIR]" : "[FILE]"} ${e.path} (${formatBytes(e.sizeBytes)})`).join("\n");

        const lowerText = text.toLowerCase();
        const projectNameLower = selectedProject.name.toLowerCase();
        const asksProjectOverview =
          /what\s+do\s+you\s+know|information\s+do\s+you\s+have|tell\s+me\s+about|project\s+overview|summari[sz]e\s+our\s+project|again\s+what\s+you\s+know/i.test(text) ||
          (lowerText.includes("project") && lowerText.includes(projectNameLower));

        let excerptBlock = "";
        const textFiles = runtimeCaps.fullRagEnabled
          ? selectedEntries.filter((e) => !e.isDir).slice(0, 4)
          : [];
        // For high-level project questions, do not inject repo tutorial excerpts into context.
        if (!asksProjectOverview && textFiles.length) {
          const snippets: string[] = [];
          for (const f of textFiles) {
            try {
              const excerpt = await invoke("read_repo_entry_excerpt", { path: f.path, maxChars: 1200 }) as string;
              snippets.push(`[${f.name}]\n${excerpt}`);
            } catch {}
          }
          if (snippets.length) {
            excerptBlock = `\n\nReference excerpts:\n${snippets.join("\n\n---\n\n")}`;
          }
        }

  projectContext = `Active project: ${selectedProject.name}\nDescription: ${selectedProject.description || "(none)"}\nManipulation root: ${selectedProject.manipulationRootPath || "(none)"}\nLinked repos: ${linkedRepos.map((r) => r.name).join(", ") || "(none)"}\nLinked MCPs: ${linkedMcps.map((m) => `${m.name} (${m.target})`).join(", ") || "(none)"}\nLinked SSH: ${linkedSshs.map((s) => `${s.name} (${s.username ? `${s.username}@` : ""}${s.host}:${s.port || 22}${s.remoteRoot ? ` root=${s.remoteRoot}` : ""})`).join(", ") || "(none)"}\nSelected references:\n${entriesPreview || "(none)"}${excerptBlock}`;

        const asksProjectMemory = asksProjectOverview;
        if (asksProjectMemory) {
          const topRefLimit = runtimeCaps.fullRagEnabled ? 5 : 3;
          const topRefList = selectedEntries.slice(0, topRefLimit).map((e) => e.name || e.path);
          const extraRefCount = Math.max(0, selectedEntries.length - topRefList.length);
          const topRefs = topRefList.join(", ");
          deterministicProjectReply = [
            `Here is what I currently know about ${selectedProject.name}:`,
            `- Description: ${selectedProject.description || "(no description saved yet)"}`,
            `- Manipulation root: ${selectedProject.manipulationRootPath || "(none configured yet)"}`,
            `- Linked repositories: ${linkedRepos.map((r) => r.name).join(", ") || "(none)"}`,
            `- Linked MCP connections: ${linkedMcps.map((m) => m.name).join(", ") || "(none)"}`,
            `- Linked SSH connections: ${linkedSshs.map((s) => s.name).join(", ") || "(none)"}`,
            `- Selected references: ${selectedEntries.length}`,
            topRefs
              ? `- Top reference names: ${topRefs}${extraRefCount > 0 ? ` (+${extraRefCount} more)` : ""}`
              : "- Top reference names: (none selected)",
            `If you want, I can break this into goals, systems, and next implementation tasks.`
          ].join("\n");
        }
      } else {
        activeMcps = [];
      }

      // Fast path: high-level project overview questions should not run heavy MCP/dataweb/network orchestration.
      if (deterministicProjectReply) {
        setIrisStatus("responding");
        setThinkingStep("Applying backend rule...");
        updateTabMessages(requestTabId, msgs => insertLLMBubble(msgs, deterministicProjectReply));

        if (!stoppedRef.current) {
          try {
            setThinkingStep("Saving memory...");
            const tabObj = tabs.find(t => t.id === requestTabId);
            const nowTs = Math.floor(Date.now() / 1000);
            const uiMsgs = (tabObj?.messages || []).map((m: any) => ({
              role: m.role,
              text: m.text,
              time: (m as any).time ?? nowTs,
            }));
            const messagesForSnapshot: { role: 'user' | 'llm'; text: string; time: number }[] = [...uiMsgs];

            if (!messagesForSnapshot.length || messagesForSnapshot[messagesForSnapshot.length - 1].role !== 'user' ||
                messagesForSnapshot[messagesForSnapshot.length - 1].text !== userDisplayText) {
              messagesForSnapshot.push({ role: 'user', text: userDisplayText, time: nowTs } as any);
            }
            if (!messagesForSnapshot.length || messagesForSnapshot[messagesForSnapshot.length - 1].role !== 'llm' ||
                messagesForSnapshot[messagesForSnapshot.length - 1].text !== deterministicProjectReply) {
              messagesForSnapshot.push({ role: 'llm', text: deterministicProjectReply, time: nowTs });
            }

            await withTimeout(persistSnapshot(requestTabId, {
              title: tabObj?.title ?? `Tab #${requestTabId}`,
              messages: capSnapshotMessages(messagesForSnapshot),
              associatedProjectId: tabProjectMap[requestTabId] ?? null,
              microSummary: microSummary ?? "",
              dialogueBullets: compiledDialogueBullets ?? "",
              summary: serializeMemoryLanes(memoryLanes),
              artifacts: [],
              promptHistory: promptHistoryForTurn,
              last_updated: nowTs,
            }), 3200, "persistSnapshot(project-fast-path)");
          } catch (e) {
            console.warn("[project-fast-path] snapshot save failed (non-fatal):", e);
          }
        }

        setIrisStatus("idle");
        setThinkingStep("");
        setRespondingTab(null);
        setPendingImages([]);
        return;
      }

      if (!projectIdeationFastPath && !quickMode && selectedProject && projectRegistryContext.trim()) {
        projectContext = `${projectContext ? `${projectContext}\n\n` : ""}Project registry (enabled):\n${projectRegistryContext}`;
      }

      let projectDatawebText = "";
      let universalDatawebText = "";
      if (!projectIdeationFastPath && !quickMode && selectedProject?.datawebEnabled) {
        try {
          projectDatawebText = await withTimeout(
            invoke("read_project_dataweb", { projectId: selectedProject.id }) as Promise<string>,
            1500,
            "read_project_dataweb"
          );
        } catch {}
      }
      if (!projectIdeationFastPath && !quickMode && universalDatawebEnabled) {
        try {
          universalDatawebText = await withTimeout(
            invoke("read_universal_dataweb") as Promise<string>,
            1500,
            "read_universal_dataweb"
          );
        } catch {}
      }

      let mcpToolCatalog: McpToolDescriptor[] = [];
      let mcpToolExecutionLog = "";
      let fsExecutionLog = "";
      let sshExecutionLog = "";
      let desktopExecutionLog = "";
      let projectExecutionLog = "";
      const inferredActions = inferConversationalActions(text);
      const projectAction = parseExplicitProjectCall(text) || inferredActions.project;
      const fsAction = parseExplicitFsCall(text) || inferredActions.fs;
      const sshAction = parseExplicitSshCall(text) || inferredActions.ssh;
      const desktopAction = parseExplicitDesktopCall(text) || inferredActions.desktop;
      if (projectAction) {
        const rootPath = String(projectAction.args.root || projectAction.args.projectRoot || activeManipulationRoot?.path || "").trim();
        if (!selectedProject) {
          projectExecutionLog = "- Project checkpoint command requested, but no enabled project is selected for this tab.";
        } else if (!rootPath) {
          projectExecutionLog = "- Project checkpoint command requested, but no project manipulation root is configured.";
        } else {
          try {
            if (projectAction.action === "checkpoint") {
              const label = String(projectAction.args.label || projectAction.args.name || "").trim() || null;
              const meta = await invoke("capture_project_checkpoint", {
                projectId: selectedProject.id,
                root: rootPath,
                label,
              }) as any;
              projectExecutionLog = `- Captured checkpoint ${String(meta?.id || "(unknown)")} for ${selectedProject.name} (${Number(meta?.fileCount || 0)} files).`;
              appendActionTrace(requestTabId, {
                action: "project.checkpoint",
                reason: "User requested project state capture",
                argsSummary: JSON.stringify({ projectId: selectedProject.id, root: rootPath, label }),
                outcome: "ok",
              });
            } else if (projectAction.action === "checkpoints") {
              const items = await invoke("list_project_checkpoints", {
                projectId: selectedProject.id,
                root: rootPath,
              }) as any[];
              const preview = (Array.isArray(items) ? items : []).slice(0, 8).map((cp) => {
                const label = String(cp?.label || "").trim();
                const ts = Number(cp?.createdAt || 0);
                const when = ts > 0 ? new Date(ts * 1000).toLocaleString() : "unknown time";
                return `- ${String(cp?.id || "(unknown)")}${label ? ` (${label})` : ""} | ${when} | files=${Number(cp?.fileCount || 0)}`;
              }).join("\n");
              projectExecutionLog = `- Available checkpoints for ${selectedProject.name}:\n${preview || "(none)"}`;
              appendActionTrace(requestTabId, {
                action: "project.checkpoints",
                reason: "User requested available checkpoints",
                argsSummary: JSON.stringify({ projectId: selectedProject.id, root: rootPath }),
                outcome: "ok",
              });
            } else if (projectAction.action === "restore") {
              let checkpointId = String(projectAction.args.checkpointId || projectAction.args.id || "").trim();
              if (!checkpointId || checkpointId.toLowerCase() === "latest") {
                const items = await invoke("list_project_checkpoints", {
                  projectId: selectedProject.id,
                  root: rootPath,
                }) as any[];
                const pick = Array.isArray(items) ? items[0] : null;
                checkpointId = String(pick?.id || "").trim();
              }
              if (!checkpointId) {
                projectExecutionLog = `- Restore requested for ${selectedProject.name}, but no checkpoints were found.`;
              } else {
                const clean = !!(projectAction.args.clean ?? false);
                const result = await invoke("restore_project_checkpoint", {
                  projectId: selectedProject.id,
                  root: rootPath,
                  checkpointId,
                  clean,
                }) as string;
                projectExecutionLog = `- ${result}`;
                appendActionTrace(requestTabId, {
                  action: "project.restore",
                  reason: "User requested rollback/restore",
                  argsSummary: JSON.stringify({ projectId: selectedProject.id, checkpointId, clean }),
                  outcome: "ok",
                });
              }
            }
          } catch (err: any) {
            projectExecutionLog = `- Project ${projectAction.action} failed in ${selectedProject.name}: ${String(err?.message || err || "unknown error")}`;
            appendActionTrace(requestTabId, {
              action: `project.${projectAction.action}`,
              reason: "Action execution failed",
              argsSummary: JSON.stringify(projectAction.args || {}),
              outcome: String(err?.message || err || "error"),
            });
          }
        }
      }

      if (fsAction) {
        const rootPath = String(fsAction.args.root || fsAction.args.projectRoot || activeManipulationRoot?.path || "").trim();
        const rootName = String(fsAction.args.project || fsAction.args.projectName || activeManipulationRoot?.name || "project").trim();
        if (!rootPath) {
          fsExecutionLog = "- FS command requested, but no project manipulation root is configured for the current project.";
        } else {
          const relPath = String(fsAction.args.path || fsAction.args.relPath || fsAction.args.file || "");
          try {
            if (fsAction.action === "list") {
              const items = await invoke("fs_list_dir", { root: rootPath, path: relPath || "." }) as RepoEntry[];
              const preview = (Array.isArray(items) ? items : []).slice(0, 80).map((i) => `${i.isDir ? "[DIR]" : "[FILE]"} ${i.path}`).join("\n");
              fsExecutionLog = `- FS list on ${rootName}${relPath ? `/${relPath}` : ""}:\n${preview || "(empty)"}`;
            } else if (fsAction.action === "read") {
              const maxChars = Number(fsAction.args.maxChars ?? fsAction.args.max_chars ?? 20000);
              const content = await invoke("fs_read_text", { root: rootPath, path: relPath, maxChars }) as string;
              fsExecutionLog = `- FS read ${rootName}/${relPath}:\n${content}`;
            } else if (fsAction.action === "write") {
              const content = String(fsAction.args.content ?? "");
              const overwrite = fsAction.args.overwrite == null ? true : !!fsAction.args.overwrite;
              const createDirs = fsAction.args.createDirs == null ? true : !!fsAction.args.createDirs;
              const result = await invoke("fs_write_text", { root: rootPath, path: relPath, content, overwrite, createDirs }) as string;
              fsExecutionLog = `- ${result}`;
            } else if (fsAction.action === "delete") {
              const recursive = !!(fsAction.args.recursive ?? false);
              const result = await invoke("fs_delete_path", { root: rootPath, path: relPath, recursive }) as string;
              fsExecutionLog = `- ${result}`;
            } else if (fsAction.action === "move") {
              const fromPath = String(fsAction.args.from || fsAction.args.fromPath || "");
              const toPath = String(fsAction.args.to || fsAction.args.toPath || "");
              const result = await invoke("fs_move_path", { root: rootPath, fromPath, toPath }) as string;
              fsExecutionLog = `- ${result}`;
            } else if (fsAction.action === "mkdir") {
              const result = await invoke("fs_make_dir", { root: rootPath, path: relPath }) as string;
              fsExecutionLog = `- ${result}`;
            }
            appendActionTrace(requestTabId, {
              action: `filesystem.${fsAction.action}`,
              reason: "Action inferred from user request",
              argsSummary: JSON.stringify(fsAction.args || {}),
              outcome: "ok",
            });
          } catch (err: any) {
            fsExecutionLog = `- FS ${fsAction.action} failed in ${rootName}: ${String(err?.message || err || "unknown error")}`;
            appendActionTrace(requestTabId, {
              action: `filesystem.${fsAction.action}`,
              reason: "Action execution failed",
              argsSummary: JSON.stringify(fsAction.args || {}),
              outcome: String(err?.message || err || "error"),
            });
          }
        }
      }

      if (desktopAction) {
        if (!desktopToolsEnabled) {
          desktopExecutionLog = "- Desktop command requested, but Desktop Tools is disabled.";
        } else if (!activeManipulationRoot?.path) {
          desktopExecutionLog = "- Desktop command requested, but no project manipulation root is configured.";
        } else {
          const rootPath = activeManipulationRoot.path;
          const relPath = String(desktopAction.args.path || desktopAction.args.relPath || desktopAction.args.file || "");
          try {
            if (desktopAction.action === "list") {
              const items = await invoke("fs_list_dir", { root: rootPath, path: relPath || "." }) as RepoEntry[];
              const preview = (Array.isArray(items) ? items : []).slice(0, 80).map((i) => `${i.isDir ? "[DIR]" : "[FILE]"} ${i.path}`).join("\n");
              desktopExecutionLog = `- Desktop list ${relPath || "."}:\n${preview || "(empty)"}`;
            } else if (desktopAction.action === "read") {
              const maxChars = Number(desktopAction.args.maxChars ?? desktopAction.args.max_chars ?? 20000);
              const content = await invoke("fs_read_text", { root: rootPath, path: relPath, maxChars }) as string;
              desktopExecutionLog = `- Desktop read ${relPath}:\n${content}`;
            } else if (desktopAction.action === "write") {
              const content = String(desktopAction.args.content ?? "");
              const overwrite = desktopAction.args.overwrite == null ? true : !!desktopAction.args.overwrite;
              const createDirs = desktopAction.args.createDirs == null ? true : !!desktopAction.args.createDirs;
              const result = await invoke("fs_write_text", { root: rootPath, path: relPath, content, overwrite, createDirs }) as string;
              desktopExecutionLog = `- ${result}`;
            } else if (desktopAction.action === "delete") {
              const recursive = !!(desktopAction.args.recursive ?? false);
              const result = await invoke("fs_delete_path", { root: rootPath, path: relPath, recursive }) as string;
              desktopExecutionLog = `- ${result}`;
            } else if (desktopAction.action === "move") {
              const fromPath = String(desktopAction.args.from || desktopAction.args.fromPath || "");
              const toPath = String(desktopAction.args.to || desktopAction.args.toPath || "");
              const result = await invoke("fs_move_path", { root: rootPath, fromPath, toPath }) as string;
              desktopExecutionLog = `- ${result}`;
            } else if (desktopAction.action === "mkdir") {
              const result = await invoke("fs_make_dir", { root: rootPath, path: relPath }) as string;
              desktopExecutionLog = `- ${result}`;
            }
            appendActionTrace(requestTabId, {
              action: `desktop.${desktopAction.action}`,
              reason: "Action inferred from user request",
              argsSummary: JSON.stringify(desktopAction.args || {}),
              outcome: "ok",
            });
          } catch (err: any) {
            desktopExecutionLog = `- Desktop ${desktopAction.action} failed: ${String(err?.message || err || "unknown error")}`;
            appendActionTrace(requestTabId, {
              action: `desktop.${desktopAction.action}`,
              reason: "Action execution failed",
              argsSummary: JSON.stringify(desktopAction.args || {}),
              outcome: String(err?.message || err || "error"),
            });
          }
        }
      }

      const activeSshs = !projectIdeationFastPath && !quickMode && mcpContextActive && selectedProject
        ? repoStore.sshs.filter((s) => (selectedProject.sshIds || []).includes(s.id) && s.enabled)
        : [];

      if (sshAction) {
        const requestedConnection = String(sshAction.args.connectionId || sshAction.args.connection || sshAction.args.sshId || "").trim();
        const pick = requestedConnection
          ? activeSshs.find((s) => s.id === requestedConnection || s.name.toLowerCase() === requestedConnection.toLowerCase())
          : activeSshs[0];
        if (!pick) {
          sshExecutionLog = "- SSH command requested, but no enabled SSH connection is associated with the current project (or requested connection was not found).";
        } else {
          try {
            const raw = await withTimeout(invoke("ssh_tool_call", {
              connection: pick,
              action: sshAction.action,
              arguments: sshAction.args,
            }) as Promise<any>, 12000, `ssh_tool_call:${sshAction.action}`);
            const pretty = (() => {
              try {
                return JSON.stringify(raw, null, 2);
              } catch {
                return String(raw);
              }
            })();
            sshExecutionLog = `- Executed SSH ${pick.name}.${sshAction.action} with args ${JSON.stringify(sshAction.args)}\n${pretty}`;
            appendActionTrace(requestTabId, {
              action: `ssh.${sshAction.action}`,
              reason: `Action inferred for SSH connection ${pick.name}`,
              argsSummary: JSON.stringify(sshAction.args || {}),
              outcome: "ok",
            });
          } catch (err: any) {
            sshExecutionLog = `- SSH ${pick.name}.${sshAction.action} failed: ${String(err?.message || err || "unknown error")}`;
            appendActionTrace(requestTabId, {
              action: `ssh.${sshAction.action}`,
              reason: `Action failed for SSH connection ${pick.name}`,
              argsSummary: JSON.stringify(sshAction.args || {}),
              outcome: String(err?.message || err || "error"),
            });
          }
        }
      }

      if (!projectIdeationFastPath && !quickMode && mcpContextActive && activeMcps.length) {
        setThinkingProgress(requestTabId, "Connecting MCP tools...");
        for (const mcp of activeMcps) {
          const parsed = parseMcpTarget(mcp.target);
          const connectionType = parsed.type;
          const command = mcp.launchCommand || parsed.command;
          const args = Array.isArray(mcp.launchArgs) ? mcp.launchArgs.map(String) : (parsed.args ?? []);
          const target = typeof mcp.target === "string" ? mcp.target : "";
          try {
            const cacheKey = `${mcp.id}::${command || ""}::${args.join(" ")}`;
            const cached = mcpToolCacheRef.current[cacheKey];
            const recentlyCached = !!cached && (Date.now() - cached.ts) < 120000;
            if (recentlyCached && cached.tools.length) {
              appendWorkLog(requestTabId, `MCP ${mcp.name}: using cached tool list (${cached.tools.length})`);
              mcpToolCatalog = [...mcpToolCatalog, ...cached.tools];
              setMcpHealthStatus((prev) => ({
                ...prev,
                [mcp.id]: { state: "connected", message: `connected (${cached.tools.length} tools · cache)`, toolCount: cached.tools.length },
              }));
              continue;
            }

            const conn = await withTimeout(invoke("connect_mcp_server", {
              mcpId: mcp.id,
              target,
              connectionType,
              command: connectionType === "stdio" ? command : (parsed.url || target),
              args,
            }) as Promise<{ connected: boolean; pid?: number | null }>, 1600, `connect_mcp_server:${mcp.name}`);
            if (conn?.connected && typeof conn?.pid === "number" && conn.pid > 0) {
              setMcpLaunchStatus((prev) => ({ ...prev, [mcp.id]: { pid: conn.pid!, launched: true } }));
            }
            if (conn?.connected) {
              appendWorkLog(requestTabId, `MCP ${mcp.name}: connected, listing tools...`);
              setMcpHealthStatus((prev) => ({ ...prev, [mcp.id]: { state: "bridge_up", message: "connected, checking tools..." } }));
            }

            const tools = await withTimeout(invoke("mcp_list_tools", {
              mcpId: mcp.id,
              target,
              connectionType,
              command: connectionType === "stdio" ? command : (parsed.url || target),
              args,
            }) as Promise<Array<{ name: string; description?: string; inputSchema?: any }>>, 2200, `mcp_list_tools:${mcp.name}`);
            const normalized = (Array.isArray(tools) ? tools : []).map((t) => ({
              mcpId: mcp.id,
              mcpName: mcp.name,
              connectionType,
              target,
              command,
              args,
              toolName: String(t?.name || "").trim(),
              description: String(t?.description || ""),
              inputSchema: t?.inputSchema,
            })).filter((t) => t.toolName.length > 0);
            mcpToolCacheRef.current[cacheKey] = { tools: normalized, ts: Date.now() };
            mcpToolCatalog = [...mcpToolCatalog, ...normalized];
            appendWorkLog(requestTabId, `MCP ${mcp.name}: ${normalized.length} tools ready`);
            setMcpHealthStatus((prev) => ({
              ...prev,
              [mcp.id]: {
                state: "connected",
                message: normalized.length ? `connected (${normalized.length} tools)` : "connected (0 tools)",
                toolCount: normalized.length,
              },
            }));
          } catch (err: any) {
            const msg = String(err?.message || err || "unknown MCP error");
            mcpToolExecutionLog += `${mcpToolExecutionLog ? "\n" : ""}- ${mcp.name}: connection failed (${msg})`;
            appendWorkLog(requestTabId, `MCP ${mcp.name}: connection failed (${msg})`);
            setMcpHealthStatus((prev) => ({ ...prev, [mcp.id]: { state: "error", message: msg } }));
          }
        }

        const explicitCall = parseExplicitMcpCall(text) || inferredActions.mcp;
        if (explicitCall) {
          const match = resolveMcpToolMatch(mcpToolCatalog, explicitCall.toolName);
          if (!match) {
            const available = mcpToolCatalog.map((t) => `${t.mcpName}.${t.toolName}`).join(", ");
            mcpToolExecutionLog += `${mcpToolExecutionLog ? "\n" : ""}- Requested MCP tool '${explicitCall.toolName}' was not found. Available tools: ${available || "(none)"}`;
          } else {
            setThinkingProgress(requestTabId, `Running MCP tool: ${match.toolName}`);
            try {
              const raw = await withTimeout(invoke("mcp_call_tool", {
                mcpId: match.mcpId,
                target: match.target,
                connectionType: match.connectionType,
                command: match.connectionType === "stdio" ? match.command : match.target,
                args: match.args ?? [],
                toolName: match.toolName,
                arguments: explicitCall.args,
              }), 6000, `mcp_call_tool:${match.toolName}`);
              const pretty = (() => {
                try {
                  return JSON.stringify(raw, null, 2);
                } catch {
                  return String(raw);
                }
              })();
              mcpToolExecutionLog += `${mcpToolExecutionLog ? "\n" : ""}- Executed ${match.mcpName}.${match.toolName} with args ${JSON.stringify(explicitCall.args)}\n${pretty}`;
              appendWorkLog(requestTabId, `MCP tool done: ${match.mcpName}.${match.toolName}`);
              appendActionTrace(requestTabId, {
                action: `mcp.${match.toolName}`,
                reason: `Tool execution on ${match.mcpName}`,
                argsSummary: JSON.stringify(explicitCall.args || {}),
                outcome: "ok",
              });
            } catch (err: any) {
              const msg = String(err?.message || err || "tool call failed");
              mcpToolExecutionLog += `${mcpToolExecutionLog ? "\n" : ""}- Tool call ${match.mcpName}.${match.toolName} failed: ${msg}`;
              appendWorkLog(requestTabId, `MCP tool failed: ${match.mcpName}.${match.toolName}`);
              appendActionTrace(requestTabId, {
                action: `mcp.${match.toolName}`,
                reason: `Tool execution failed on ${match.mcpName}`,
                argsSummary: JSON.stringify(explicitCall.args || {}),
                outcome: msg,
              });
            }
          }
        }
      }

      // If an engineering task was requested but every MCP failed to connect, tell the user
      // how to fix it rather than letting the LLM loop endlessly with no tools.
      if (engineeringTaskRequest && mcpContextActive && activeMcps.length > 0 && mcpToolCatalog.length === 0) {
        const mcpNames = activeMcps.map((m) => m.name).join(", ");
        const warn = [
          `⚠️ **MCP bridge not running** — I can't reach the project tools for **${mcpNames}**.`,
          "",
          "To execute tasks on your Godot project I need the bridge active:",
          "1. Open **Bridge Servers** in the top chat bar",
          `2. Find \`${mcpNames}\` → click **▶ Launch**`,
          "3. Send your message again",
          "",
          "Without the bridge I can answer questions but cannot create or edit project files.",
        ].join("\n");
        updateTabMessages(requestTabId, (msgs) => insertLLMBubble(msgs, warn));
        setThinkingStep("");
        setIrisStatus("idle");
        clearWorkLog(requestTabId);
        return;
      }

      if (DEBUG_MEMORY) {
        console.log("[DEBUG] Compiled context:", {
          microSummaryLen: microSummary?.length,
          dialogueBulletsLen: compiledDialogueBullets?.length,
          recentTranscriptLen: recentTranscript?.length,
          recentTranscriptPreview: recentTranscript?.substring(0, 200)
        });
      }

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
      setThinkingModel((primaryModel || "iris-organizer:latest").replace(/:latest$/i, ""));
      setThinkingProgress(requestTabId, "Preparing response...");

      let prompt = plannerPrompt;
      if (projectIdeationFastPath && selectedProject) {
        prompt = [
          `You are helping brainstorm creative game ideas for an in-game environment.`,
          `Project: ${selectedProject.name}`,
          `Project description: ${selectedProject.description || "(none provided)"}`,
          `User request: ${text}`,
          `Answer directly with 8 concise environment ideas or directions for the game world.`,
          `Focus on in-game setting, mood, landmarks, traversal, hazards, and visual identity.`,
          `Do not switch into project planning, roadmaps, implementation plans, or process advice unless the user asks for that explicitly.`
        ].join("\n\n");
      }
      if (!prompt) {
        // Minimal fallback when planner is unavailable; no client-side resolver logic.
        if (quickMode) {
          prompt = `User request:\n${text || "Give a short helpful response."}\n\nReply directly and naturally. If asked for a joke or random number, provide one immediately. Do not redirect to project planning unless the user explicitly asks for project help.`;
        } else {
          prompt = `Conversation memory:\n${recentTranscript || "(empty)"}\n\nUser request:\n${text || "Describe the attached image(s) and provide basic useful observations."}`;
        }
      }

      if (hasImages) {
        const imageGuidance = text.trim()
          ? `Attached image input is present. First inspect the image carefully, then answer the user's request using visible evidence. Call out uncertainty briefly.`
          : `Attached image input is present. Describe the image briefly, identify obvious UI/object/layout details, and mention any readable text or notable uncertainty.`;
        prompt = `${prompt}\n\n${imageGuidance}`;
        if (!plannerRoutedModels.includes("iris-vision")) {
          setPlannerRoutedModels(["iris-organizer", "iris-vision", "iris-summarizer"]);
          setPlannerRouteSummary("Planner route: iris-organizer -> iris-vision -> iris-summarizer");
        }
      }

      if (windowHint) {
        const enabledMcpSummary = mcpContextActive
          ? repoStore.mcps
              .filter((m) => m.enabled)
              .map((m) => `${m.name} (${m.target})`)
              .join(", ")
          : "";
        prompt = `${prompt}\n\nDesktop context:\n- ${windowHint}\n- Use visible screenshot evidence first, then combine with project/MCP context.${enabledMcpSummary ? `\n- Enabled MCP connections: ${enabledMcpSummary}` : ""}`;
      }

      if (mcpContextActive && mcpToolCatalog.length) {
        const toolLines = mcpToolCatalog
          .map((t) => `- ${t.mcpName}.${t.toolName}${t.description ? `: ${t.description}` : ""}`)
          .join("\n");
        const toolCallNote = engineeringTaskRequest
          ? `\nTo call a tool, output exactly this on its own line:\nTOOL_CALL: {"tool": "exact_tool_name", "args": {"key": "value"}}\nYou will receive a TOOL_RESULT for each call. Use tool calls to ACT — do not just describe what to do.`
          : `\nOnly claim MCP tool output if it appears under MCP execution results.`;
        prompt = `${prompt}\n\nLive MCP tools available right now:\n${toolLines}${toolCallNote}`;
      }

      if (activeManipulationRoot?.path) {
        prompt = `${prompt}\n\nDirect project access:\n- Project manipulation root: ${activeManipulationRoot.path}\n- Prefer direct filesystem actions through the manipulation root for normal file edits, file moves, and folder operations.\n- Prefer MCP only when it gives project-engine-specific leverage that direct filesystem access does not.`;
      }

      const actionTraceLines = (actionTraceByTab[requestTabId] || [])
        .slice(-8)
        .map((a) => `- ${new Date(a.ts * 1000).toLocaleTimeString()}: ${a.action} | reason=${a.reason} | outcome=${a.outcome}`)
        .join("\n");
      const flowAdjustments = flowAdjustmentsByTab[requestTabId] || [];
      const allFlowAdjustments = [
        ...(flowAdjustments || []),
        ...(flowAdjustmentNote ? [flowAdjustmentNote] : []),
      ];
      prompt = `${prompt}\n\nIris internal action repository:\n${IRIS_ACTION_REPOSITORY.map((a) => `- ${a}`).join("\n")}\nUse these actions based on intent and context; slash commands are optional and should not be required.`;
      if (actionTraceLines.trim()) {
        prompt = `${prompt}\n\nRecent internal action trace:\n${actionTraceLines}`;
      }
      if (allFlowAdjustments.length) {
        prompt = `${prompt}\n\nUser feedback adjustments currently active:\n${allFlowAdjustments.map((n) => `- ${n}`).join("\n")}`;
      }

      if (fsExecutionLog.trim()) {
        prompt = `${prompt}\n\nFilesystem execution results:\n${fsExecutionLog}`;
      }

      if (mcpToolExecutionLog.trim()) {
        prompt = `${prompt}\n\nMCP execution results:\n${mcpToolExecutionLog}`;
      }

      if (activeSshs.length) {
        const sshLines = activeSshs
          .map((s) => `- ${s.name}: ${s.username ? `${s.username}@` : ""}${s.host}:${s.port || 22}${s.remoteRoot ? ` (root: ${s.remoteRoot})` : ""}`)
          .join("\n");
        prompt = `${prompt}\n\nProject SSH connections available right now:\n${sshLines}\nUse SSH call results when present; do not claim SSH output that was not executed.`;
      }

      if (sshExecutionLog.trim()) {
        prompt = `${prompt}\n\nSSH execution results:\n${sshExecutionLog}`;
      }

      if (desktopExecutionLog.trim()) {
        prompt = `${prompt}\n\nDesktop execution results:\n${desktopExecutionLog}`;
      }

      if (projectExecutionLog.trim()) {
        prompt = `${prompt}\n\nProject checkpoint results:\n${projectExecutionLog}`;
      }

      if (projectContext && !projectIdeationFastPath) {
        const gatingRules = !reposContextActive && !mcpContextActive
          ? "- Repos and MCP context toggles are OFF; do not fabricate tool outputs."
          : !reposContextActive
            ? "- Repo context toggle is OFF; do not use repo excerpts or file-grounded claims."
            : !mcpContextActive
              ? "- MCP context toggle is OFF; avoid MCP/tool orchestration."
          : runtimeCaps.fullRagEnabled
            ? "- Full RAG and multi-MCP are enabled; combine repo excerpts + MCP context for grounded execution."
            : "- Basic repo context + single-tool MCP only; keep plans concise and avoid deep multi-tool fan-out.";

        const loopDepthRule = runtimeCaps.deepReasoningEnabled
          ? "- Use deeper reasoning loops (plan -> execute -> self-check -> iterate) until a satisfactory result is reached."
          : "- Use one short plan/execute/check loop and ask before deeper iteration.";

        prompt = `${prompt}\n\nProject Dataweb Context:\n${projectContext}\n\nContext routing rules:\n- If user asks for project overview/status, answer from project description + registry + selected references summary.\n- Do not output raw tutorial steps unless user explicitly asks for implementation steps.\n- Use repository excerpts only for concrete implementation/debug requests.\n- Use MCP mentions only when execution/integration is requested.\n${gatingRules}\n\nExecution loop contract:\n- Plan with project dataweb (repos + MCPs when enabled).\n- Execute one concrete step.\n- Self-check results.\n${loopDepthRule}\n- Provide concise progress updates while working, then summarize completed work and next steps.\n${engineeringTaskRequest ? "- CRITICAL: Use TOOL_CALL directives to actually execute actions — do not just describe what to do. Format: TOOL_CALL: {\\\"tool\\\": \\\"exact_tool_name\\\", \\\"args\\\": {\\\"key\\\": \\\"value\\\"}} — one per line. Phases: Inspect (list_directory/read_file) → Plan → Execute (write_file/create_scene/etc via TOOL_CALLs) → Verify → Polish. Keep issuing TOOL_CALLs until the task is done." : ""}\n\nUse this project context for grounding when relevant.`;
      }

      if (!projectIdeationFastPath && selectedProject?.datawebEnabled && projectDatawebText.trim()) {
        prompt = `${prompt}\n\nProject dataweb memory (${selectedProject.name}):\n${projectDatawebText.slice(-5000)}`;
      }
      if (!projectIdeationFastPath && universalDatawebEnabled && universalDatawebText.trim()) {
        prompt = `${prompt}\n\nUniversal dataweb memory:\n${universalDatawebText.slice(-5000)}`;
      }

      let networkHits: NetworkHit[] = [];
      const networkQuery = normalizeConversationalQuery(text);
      if (!projectIdeationFastPath && networkEnabled && shouldUseNetworkForPrompt(networkQuery)) {
        try {
          setThinkingProgress(requestTabId, "Searching network sources...");
          const net = await withTimeout(invoke("network_search", {
            query: networkQuery,
            projectContext: projectContext || "",
          }) as Promise<any>, 7000, "network_search");
          networkHits = Array.isArray(net?.hits) ? net.hits as NetworkHit[] : [];
          const evidence = networkHits
            .slice(0, 6)
            .map((h, i) => `${i + 1}. ${h.title || "Source"}\nSnippet: ${h.snippet || "(none)"}\nURL: ${h.url || "(none)"}`)
            .join("\n\n");
          if (evidence.trim()) {
            prompt = `${prompt}\n\nNetwork Assist (fresh internet context):\n${evidence}\n\nWhen the user asks for weather/news/current events, answer directly from these snippets first and do not claim you lack real-time access.`;
            setThinkingProgress(requestTabId, "Network context merged");
          }
        } catch (e) {
          console.warn("network_lookup failed", e);
          setThinkingProgress(requestTabId, "Network lookup unavailable; continuing offline");
        }
      }

      const plannerBridgeNote = String(plannerV2?.bridgeNote || plannerV2?.bridge_note || "").trim();
      if (plannerBridgeNote) {
        prompt = `${prompt}\n\nPlanner bridge-back note:\n${plannerBridgeNote}`;
      }

      let fullText = "";
      let streamedText = "";
      let resolverUsed: ResolverKind = plannerResolverUsed;

      const deterministicReply = String(deterministicProjectReply || plannerV2?.deterministicReply || plannerV2?.deterministic_reply || "").trim();
      if (deterministicReply) {
        setIrisStatus("responding");
        setThinkingProgress(requestTabId, "Applying backend rule...");
        streamedText = deterministicReply;
        updateTabMessages(requestTabId, msgs => insertLLMBubble(msgs, deterministicReply));
      } else {
        setThinkingProgress(requestTabId, "Generating response...");
        await stream({
          model,
          prompt,
          images: hasImages ? allImageDataUrls.map((img) => img.split(",")[1]).filter(Boolean) : undefined,
          options,
          signal: controller.signal,
          onHeaders: () => setIrisStatus(hasImages ? "responding" : (coderish ? "coding" : "responding")),
          onFirstToken: (first) => {
            streamedText += first;
            updateTabMessages(requestTabId, msgs => insertLLMBubble(msgs, first));
          },
          onTokens: (delta) => {
            streamedText += delta;
            updateTabMessages(requestTabId, msgs => patchLastLLMBubble(msgs, delta));
          },
          onDone: () => {
            // No message replacement here; streamedText already has the full reply
          },
        });

        // --- Agentic MCP tool-use loop ---
        // If the model emitted TOOL_CALL directives, execute them and continue.
        if (!stoppedRef.current && engineeringTaskRequest && mcpToolCatalog.length > 0) {
          const MAX_AGENT_ITERS = 5;
          let agentContext = prompt;
          let agentResponse = streamedText;
          let agentActionsRan = 0;
          for (let agentIter = 0; agentIter < MAX_AGENT_ITERS; agentIter++) {
            if (stoppedRef.current) break;
            let pendingCalls = parseToolCallsFromText(agentResponse);
            if (pendingCalls.length === 0 && agentIter === 0) {
              setThinkingProgress(requestTabId, "Planning concrete tool calls...");
              const toolInventory = mcpToolCatalog
                .slice(0, 40)
                .map((t) => `- ${t.mcpName}.${t.toolName}${t.description ? `: ${t.description}` : ""}`)
                .join("\n");
              let forcedCallsText = "";
              await stream({
                model: "iris-organizer:latest",
                prompt: [
                  "You are an execution planner for MCP tools.",
                  "Return only tool calls. Do not return prose.",
                  `User request:\n${text}`,
                  `Available MCP tools:\n${toolInventory}`,
                  "Output format (one per line, max 3 lines):",
                  "TOOL_CALL: {\"tool\":\"exact_tool_name\",\"args\":{}}",
                  "If no tool can help, output exactly: NO_TOOL_CALL",
                ].join("\n\n"),
                options: {
                  num_ctx: 1024,
                  num_keep: 24,
                  num_predict: 220,
                  temperature: 0.1,
                  top_p: 0.9,
                  top_k: 40,
                  repeat_penalty: 1.06,
                },
                signal: controller.signal,
                onFirstToken: (first) => {
                  forcedCallsText += first;
                },
                onTokens: (delta) => {
                  forcedCallsText += delta;
                },
              });
              pendingCalls = parseToolCallsFromText(forcedCallsText);
              if (pendingCalls.length === 0) {
                appendWorkLog(requestTabId, "No actionable tool calls generated");
                break;
              }
            }
            if (pendingCalls.length === 0) break;

            // Execute each tool call and collect results
            let toolResultsBlock = "";
            for (const tc of pendingCalls) {
              const match = resolveMcpToolMatch(mcpToolCatalog, tc.toolName);
              if (!match) {
                const avail = mcpToolCatalog.map((t) => `${t.mcpName}.${t.toolName}`).slice(0, 20).join(", ");
                toolResultsBlock += `\nTOOL_RESULT [${tc.toolName}]: Error - tool not found. Available: ${avail || "(none)"}`;
                appendWorkLog(requestTabId, `Tool not found: ${tc.toolName}`);
                continue;
              }
              setThinkingProgress(requestTabId, `Executing: ${match.toolName}...`);
              try {
                const raw = await withTimeout(
                  invoke("mcp_call_tool", {
                    mcpId: match.mcpId,
                    target: match.target,
                    connectionType: match.connectionType,
                    command: match.connectionType === "stdio" ? match.command : match.target,
                    args: match.args ?? [],
                    toolName: match.toolName,
                    arguments: tc.args,
                  }) as Promise<unknown>,
                  14000,
                  `agentic_tool:${match.toolName}`
                );
                const resultText = typeof raw === "string" ? raw : JSON.stringify(raw, null, 2);
                toolResultsBlock += `\nTOOL_RESULT [${tc.toolName}]:\n${resultText.slice(0, 3000)}`;
                appendWorkLog(requestTabId, `ok ${match.toolName}`);
                agentActionsRan += 1;
                appendActionTrace(requestTabId, {
                  action: `mcp.${match.toolName}`,
                  reason: "Agentic tool execution",
                  argsSummary: JSON.stringify(tc.args || {}),
                  outcome: "ok",
                });
              } catch (err: unknown) {
                const errMsg = String((err as any)?.message || err || "tool failed");
                toolResultsBlock += `\nTOOL_RESULT [${tc.toolName}]: Error - ${errMsg}`;
                appendWorkLog(requestTabId, `fail ${match.toolName}: ${errMsg.slice(0, 80)}`);
                appendActionTrace(requestTabId, {
                  action: `mcp.${match.toolName}`,
                  reason: "Agentic tool failed",
                  argsSummary: JSON.stringify(tc.args || {}),
                  outcome: errMsg,
                });
              }
            }
            if (!toolResultsBlock.trim()) break;

            // Append tool results divider to the displayed bubble
            const divider = `\n\n---\n${toolResultsBlock.trim()}\n\n`;
            updateTabMessages(requestTabId, (msgs) => {
              const copy = [...msgs];
              for (let i = copy.length - 1; i >= 0; --i) {
                if (copy[i].role === "llm") {
                  copy[i] = { ...copy[i], text: copy[i].text + divider };
                  break;
                }
              }
              return copy;
            });

            // Build continuation prompt
            agentContext = [
              agentContext,
              `\nPrevious assistant response:\n${agentResponse.slice(0, 2000)}`,
              `\nTool execution results:${toolResultsBlock}`,
              `\nContinue. Output more TOOL_CALL lines if needed, or provide a final completion summary when the task is verified done.`,
            ].join("\n");

            setThinkingProgress(requestTabId, `Agent pass ${agentIter + 2}...`);
            let nextText = "";
            await stream({
              model,
              prompt: agentContext,
              options: { ...options, num_predict: 700 },
              signal: controller.signal,
              onFirstToken: (t) => {
                nextText += t;
                updateTabMessages(requestTabId, (msgs) => patchLastLLMBubble(msgs, t));
              },
              onTokens: (delta) => {
                nextText += delta;
                updateTabMessages(requestTabId, (msgs) => patchLastLLMBubble(msgs, delta));
              },
            });
            if (!nextText.trim()) break;
            streamedText += divider + nextText;
            agentResponse = nextText;
            if (!agentResponse.includes("TOOL_CALL:")) break;
          }

          if (agentActionsRan === 0) {
            appendWorkLog(requestTabId, "No MCP actions executed; returned guidance only");
          }
        }

        const iterationBudget = iterationBudgetForProfile(llmProfile);
        if (!hasImages && !quickMode && iterationBudget > 1 && streamedText.trim()) {
          let workingDraft = streamedText;
          for (let iter = 2; iter <= iterationBudget; iter++) {
            if (stoppedRef.current) break;
            setThinkingProgress(requestTabId, `Self-review pass ${iter}/${iterationBudget}...`);
            const reviewPrompt = [
              "You are performing an internal quality-control pass before finalizing a user response.",
              "Output strictly in one of these forms only:",
              "DONE",
              "REVISE:<final user-facing answer only>",
              "No analysis headings, no intent summaries, no commentary.",
              "",
              `User request:\n${text}`,
              "",
              `Current draft:\n${workingDraft}`,
            ].join("\n");

            let decision = "";
            await stream({
              model: "iris-organizer:latest",
              prompt: reviewPrompt,
              options: {
                num_ctx: 1024,
                num_keep: 24,
                num_predict: 600,
                temperature: 0.15,
                top_p: 0.9,
                top_k: 40,
                repeat_penalty: 1.06,
              },
              signal: controller.signal,
              onFirstToken: (first) => {
                decision += first;
              },
              onTokens: (delta) => {
                decision += delta;
              },
            });

            const trimmed = decision.replace(/<\/?final>/gi, "").trim();
            if (!trimmed || /^DONE\s*$/i.test(trimmed)) {
              break;
            }
            if (!/^REVISE:/i.test(trimmed)) {
              break;
            }
            const revised = sanitizeAssistantOutput(trimmed.replace(/^REVISE:\s*/i, "").trim());
            if (!revised) {
              break;
            }
            workingDraft = revised;
          }
          streamedText = workingDraft;
        }
      }

      // --- Artifact extraction and filename sniff ---
      fullText = streamedText;
      let deliveredText = sanitizeAssistantOutput(fullText);
      if (networkHits.length) {
        const sourcesBlock = networkHits
          .slice(0, 4)
          .map((h, i) => `- [${i + 1}] ${h.title || "Source"}${h.url ? ` - ${h.url}` : ""}`)
          .join("\n");
        deliveredText = `${deliveredText}\n\nSources consulted:\n${sourcesBlock}`;
      }
      let artifacts = extractArtifacts(deliveredText);
      const filenameMatch = deliveredText.match(/(?:here's|file|save as|edit)\s+`([^`]+)`/i);
      if (filenameMatch && artifacts[0]) artifacts[0].filename = filenameMatch[1];

      updateTabMessages(requestTabId, msgs => {
        const copy = [...msgs];
        for (let i = copy.length - 1; i >= 0; --i) {
          if (copy[i].role === "llm") {
            copy[i] = { ...copy[i], text: deliveredText };
            break;
          }
        }
        return copy;
      });

      const now = Math.floor(Date.now() / 1000);
      const artifactsWithTs = artifacts.map((a: Artifact) => ({ ...a, ts: now }));
      const activeArtifactAfterReply = pickActiveArtifact(
        artifactsWithTs,
        memorySnapshot?.artifacts as Artifact[] | undefined
      );
      const lanesSerialized = serializeMemoryLanes(memoryLanes);

      setMemoryDebug({
        lastResolver: resolverUsed,
        plannerPath,
        plannerStrategy: String(plannerV2?.strategy || "legacy"),
        plannerIntent: String(plannerV2?.primaryIntent || plannerV2?.primary_intent || "legacy"),
        suggestedGodotVersion: plannerSuggestedGodotVersion,
        lastNumericAnchor: extractLastAssistantNumber(recentTranscript),
        activeArtifactLabel: activeArtifactAfterReply
          ? `${activeArtifactAfterReply.filename || "(unsaved snippet)"}${activeArtifactAfterReply.lang ? ` [${activeArtifactAfterReply.lang}]` : ""}`
          : "(none)",
        laneProject: memoryLanes.project,
        laneCoding: memoryLanes.coding,
        laneRecall: memoryLanes.recall,
        transcriptChars: (recentTranscript || "").length,
        updatedAt: now,
      });

      // If the user hit Stop, don't write memory
      if (stoppedRef.current) return;

      // Persist project-specific and universal dataweb logs when enabled.
      try {
        const enablePostTurnDatawebWrites = false;
        if (!enablePostTurnDatawebWrites) {
          throw new Error("post-turn dataweb writes disabled temporarily");
        }
        const ts = new Date().toISOString();
        const compactAssistant = deliveredText.replace(/\s+/g, " ").slice(0, 1400);
        const entry = `[${ts}]\nUser: ${text || "(image-only message)"}\nIris: ${compactAssistant}\n\n`;
        if (selectedProject?.datawebEnabled) {
          const prev = await withTimeout(
            invoke("read_project_dataweb", { projectId: selectedProject.id }) as Promise<string>,
            1200,
            "read_project_dataweb(save)"
          );
          const next = `${String(prev || "")}${entry}`.slice(-120000);
          await withTimeout(
            invoke("write_project_dataweb", { projectId: selectedProject.id, content: next }) as Promise<any>,
            1200,
            "write_project_dataweb"
          );
        }
        if (universalDatawebEnabled) {
          const prevUniversal = await withTimeout(
            invoke("read_universal_dataweb") as Promise<string>,
            1200,
            "read_universal_dataweb(save)"
          );
          const nextUniversal = `${String(prevUniversal || "")}${entry}`.slice(-180000);
          await withTimeout(
            invoke("write_universal_dataweb", { content: nextUniversal }) as Promise<any>,
            1200,
            "write_universal_dataweb"
          );
        }
      } catch (e) {
        console.warn("dataweb persistence failed (non-fatal)", e);
      }

      // Persist memory to disk first (desktop-safe, crash-resistant).
      // Summaries are updated in a background step after raw transcript is safely written.
      try {
        setThinkingProgress(requestTabId, "Saving memory...");
        const tabObj = tabs.find(t => t.id === requestTabId);
        const nowTs = Math.floor(Date.now() / 1000);
        // Build from in-memory UI messages only to avoid disk read/write contention.
        const uiMsgs = (tabObj?.messages || []).map((m: any) => ({
          role: m.role,
          text: m.text,
          time: (m as any).time ?? nowTs,
        }));
        const baseMsgs = uiMsgs;

        const messagesForSnapshot: { role: 'user' | 'llm'; text: string; time: number }[] = [...baseMsgs];

        // Ensure last user message is present
        if (!messagesForSnapshot.length || messagesForSnapshot[messagesForSnapshot.length - 1].role !== 'user' ||
            messagesForSnapshot[messagesForSnapshot.length - 1].text !== userDisplayText) {
          messagesForSnapshot.push({ role: 'user', text: userDisplayText, time: nowTs, ...(allImageDataUrls.length ? { images: allImageDataUrls } : {}) } as any);
        }

        // Ensure last LLM message is present/updated
        if (!messagesForSnapshot.length || messagesForSnapshot[messagesForSnapshot.length - 1].role !== 'llm' ||
            messagesForSnapshot[messagesForSnapshot.length - 1].text !== deliveredText) {
          messagesForSnapshot.push({ role: 'llm', text: deliveredText, time: nowTs });
        }

        const boundedSnapshotMessages = capSnapshotMessages(messagesForSnapshot);

        await withTimeout(persistSnapshot(requestTabId, {
          title: tabObj?.title ?? `Tab #${requestTabId}`,
          messages: boundedSnapshotMessages,
          associatedProjectId: tabProjectMap[requestTabId] ?? null,
          microSummary: microSummary ?? "",
          dialogueBullets: compiledDialogueBullets ?? "",
          summary: lanesSerialized,
          artifacts: artifactsWithTs ?? [],
          promptHistory: promptHistoryForTurn,
          last_updated: nowTs,
        }), 3200, "persistSnapshot(primary)");

        // Background summarization: non-blocking and non-fatal.
        const shouldRunBackgroundSummary = false;

        if (shouldRunBackgroundSummary) {
          setIsSummarizing(true);
          setThinkingModel("iris-summarizer");
          setThinkingStep("Summarizing in background...");
        }

        void (async () => {
          if (!shouldRunBackgroundSummary) {
            setIsSummarizing(false);
            return;
          }
          try {
            await ensureModel(SUMMARY_MODEL);

            const transcriptWithLatest = [
              recentTranscript || "",
              `User: ${text}`,
              `Iris: ${(deliveredText || "").slice(0, 4000)}`,
            ].filter(Boolean).join("\n");

            const boundedTranscript = transcriptWithLatest.length > 9000
              ? transcriptWithLatest.slice(transcriptWithLatest.length - 9000)
              : transcriptWithLatest;

            const microSummaryPrompt = `
Current rolling memory summary:
${microSummary || "(empty)"}

Current memory lanes:
${lanesSerialized}

Recent transcript:
${boundedTranscript || "(empty)"}

Latest exchange:
User: ${text}
Iris: ${(deliveredText || "").slice(0, 4000)}

Update the rolling memory summary in 1-2 sentences.
Preserve important facts, prior values, user preferences, unresolved tasks, and references that future turns may depend on.
Do not discard earlier useful context just because a new message arrived.
`;

            const projectSummaryPrompt = `
Current project summary:
${memorySnapshot?.summary || lanesSerialized || "(empty)"}

Current compressed notes:
${compiledDialogueBullets || "(empty)"}

Recent transcript:
${boundedTranscript || "(empty)"}

Latest exchange:
User: ${text}
Iris: ${(deliveredText || "").slice(0, 4000)}

Return an updated project summary that preserves ongoing context and prior important facts.
Keep lane sections in this exact format with concise content:
[PROJECT]
...
[CODING]
...
[RECALL]
...
`;

            const dialogueBulletsPrompt = `
Current compressed notes:
${compiledDialogueBullets || "(empty)"}

Recent transcript:
${boundedTranscript || "(empty)"}

Latest exchange:
User: ${text}
Iris: ${(deliveredText || "").slice(0, 4000)}

Update the notes into <=6 bullets, preserving names, files, decisions, remembered values, and unresolved tasks.
`;

            const { microSummary: newMicroSummary, projectSummary: newProjectSummary, dialogueBullets: newDialogueBullets } =
              await withTimeout(
                summarizeExchange({ model: SUMMARY_MODEL, microSummaryPrompt, projectSummaryPrompt, dialogueBulletsPrompt }),
                7000,
                "summarizeExchange"
              );

            if (stoppedRef.current) return;

            const refreshedLanes = buildMemoryLanes({
              transcript: transcriptWithLatest,
              microSummary: newMicroSummary || microSummary || "",
              dialogueBullets: newDialogueBullets || compiledDialogueBullets || "",
              summary: newProjectSummary || lanesSerialized || "",
              activeArtifact: activeArtifactAfterReply,
            });

            await withTimeout(persistSnapshot(requestTabId, {
              title: tabObj?.title ?? `Tab #${requestTabId}`,
              messages: boundedSnapshotMessages,
              associatedProjectId: tabProjectMap[requestTabId] ?? null,
              microSummary: newMicroSummary || microSummary || "",
              dialogueBullets: newDialogueBullets || compiledDialogueBullets || "",
              summary: serializeMemoryLanes(refreshedLanes),
              artifacts: artifactsWithTs ?? [],
              promptHistory: promptHistoryForTurn,
              last_updated: Math.floor(Date.now() / 1000),
            }), 3200, "persistSnapshot(background)");

            setMemoryDebug((prev) => ({
              ...prev,
              laneProject: refreshedLanes.project,
              laneCoding: refreshedLanes.coding,
              laneRecall: refreshedLanes.recall,
              updatedAt: Math.floor(Date.now() / 1000),
            }));
          } catch (e) {
            console.warn("[background summarize] failed (non-fatal):", e);
          } finally {
            setIsSummarizing(false);
          }
        })();
        setPendingImages([]);
      } catch (e) {
        console.warn("[persistSnapshot] failed (non-fatal):", e);
      } finally {
        setIrisStatus("idle");
        setThinkingStep("");
        setRespondingTab(null);
      }

    } catch (err: any) {
      setThinking(false);
      setIsGenerating(false);
      setIrisStatus("idle");
      setThinkingStep("");
      setRespondingTab(null);
      setIsSummarizing(false);

      updateTabMessages(requestTabId, msgs => [
        ...msgs,
        { role: "llm", text: `[Error: ${err?.message || err}]` }
      ]);
    } finally {
      setThinking(false);
      setThinkingModel(null);
      setIsGenerating(false);
      setThinkingStep("");
      abortController.current = null;
    }
  }

  function handleStop(e: React.FormEvent) {
    e.preventDefault();
    // mark stopped so in-flight async work can bail early
    stoppedRef.current = true;
    if (abortController.current) {
      abortController.current.abort();
      setIsGenerating(false);
      setIrisStatus("idle");
      setThinkingStep("");
      setRespondingTab(null);
      setIsSummarizing(false);
    }
  }

  useEffect(() => {
    if (!thinking) {
      setThinkingSeconds(0);
      return;
    }
    const id = setInterval(() => {
      setThinkingSeconds((s) => s + 1);
    }, 1000);
    return () => clearInterval(id);
  }, [thinking]);

  async function handleNewTab() {
    if (menuLocked) return;
    const chatTabs = tabs.filter(tab => tab.type === "chat");
    if (chatTabs.length >= 8) return;

    const settingsIdx = tabs.findIndex(tab => tab.type === "settings");
    const newId = tabs.length ? Math.max(...tabs.map(t => t.id)) + 1 : 1;
    const labelNumber = nextChatLabelNumber(tabs);
    const newTab: Tab = { id: newId, title: `Tab #${labelNumber}`, type: "chat", messages: [], promptHistory: [] };

    if (settingsIdx === -1) {
      setTabs([...tabs, newTab]);
      setActiveTab(newId);
    } else {
      const newTabs = [...tabs];
      newTabs.splice(settingsIdx, 0, newTab);
      setTabs(newTabs);
      setActiveTab(newId);
    }
    // Persist an initial empty snapshot for the new tab
    try {
      await persistSnapshot(newId, {
        title: newTab.title,
        messages: [],
        associatedProjectId: null,
        microSummary: "",
        dialogueBullets: "",
        summary: "",
        artifacts: [],
        promptHistory: [],
        last_updated: Math.floor(Date.now() / 1000)
      });
    } catch {}
  }

  const [settingsTab, setSettingsTab] = useState<"General" | "Projects" | "Repos" | "Bridges" | "LLMs" | "Network" | "Controller">("General");

  function handleSettings() {
    if (menuLocked) return;
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

  async function handleOpenModelfiles() {
    try {
      await invoke("open_modelfiles_folder");
    } catch (e: any) {
      alert("Failed to open modelfiles folder: " + (e?.message || e));
    }
  }

  async function handleApplyModelsAndRestart() {
    try {
      setThinking(false);
      setIsGenerating(false);
      setIsSummarizing(false);
      setIrisStatus("idle");
      setThinkingStep("");
      setRespondingTab(null);
      await invoke("set_setup_flags", { args: { modelsVerified: false, modelProfile: llmProfile } });
      await invoke("restart_app");
    } catch (e: any) {
      alert("Failed to apply model changes: " + (e?.message || e));
    }
  }

  useEffect(() => {
    if (settingsTab !== "LLMs" || modelfileLoadedRef.current) return;
    let cancelled = false;

    (async () => {
      try {
        setModelfileLoadError(null);
        const listed = await invoke<string[]>("list_modelfiles");
        const preferredOrder = [
          "modelfile_organizer.txt",
          "modelfile_coder.txt",
          "modelfile_summarizer.txt",
          "modelfile_vision.txt",
        ];

        const normalized = Array.isArray(listed) ? listed : [];
        const ordered = [
          ...preferredOrder.filter((name) => normalized.includes(name)),
          ...normalized.filter((name) => !preferredOrder.includes(name)),
        ];

        const rows = await Promise.all(
          ordered.map(async (filename) => {
            const row = await invoke<any>("read_modelfile_data", { filename });
            return {
              filename: String(row?.filename ?? filename),
              displayName: String(row?.displayName ?? row?.display_name ?? filename),
              nickname: String(row?.nickname ?? row?.displayName ?? row?.display_name ?? filename),
              fromModel: String(row?.fromModel ?? row?.from_model ?? ""),
              systemPrompt: String(row?.systemPrompt ?? row?.system_prompt ?? ""),
              params: Array.isArray(row?.params)
                ? row.params.map((p: any) => ({ key: String(p?.key ?? ""), value: String(p?.value ?? "") }))
                : [],
            } as ModelfileDataFE;
          })
        );

        if (cancelled) return;

        const dataMap: Record<string, ModelfileDataFE> = {};
        const paramsMap: Record<string, ModelfileParamFE[]> = {};
        const fromMap: Record<string, string> = {};
        for (const row of rows) {
          dataMap[row.filename] = row;
          paramsMap[row.filename] = row.params.map((p) => ({ ...p }));
          fromMap[row.filename] = row.fromModel;
        }

        setModelfileDatas(dataMap);
        setModelfileEdits(paramsMap);
        setModelfileFromEdits(fromMap);
        setModelfileFilenames(ordered);
        if (ordered.length > 0) setModelfileSubTab(ordered[0]);
        modelfileLoadedRef.current = true;
      } catch (err: any) {
        if (!cancelled) {
          setModelfileLoadError(err?.message || String(err));
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [settingsTab]);

  function updateModelfileParam(filename: string, key: string, nextValue: string) {
    setModelfileEdits((prev) => ({
      ...prev,
      [filename]: (prev[filename] || []).map((p) => (p.key === key ? { ...p, value: nextValue } : p)),
    }));
  }

  async function saveModelfile(filename: string, opts?: { systemPrompt?: string; nickname?: string }) {
    const params = modelfileEdits[filename] || [];
    const fromModel = (modelfileFromEdits[filename] || "").trim();
    if (!fromModel) {
      alert("FROM model cannot be empty.");
      return;
    }
    try {
      setModelfileSaving((prev) => ({ ...prev, [filename]: true }));
      await invoke("save_modelfile_data", {
        args: {
          filename,
          fromModel,
          params,
          systemPrompt: opts?.systemPrompt,
          nickname: opts?.nickname,
        },
      });

      setModelfileDatas((prev) => {
        const current = prev[filename];
        if (!current) return prev;
        return {
          ...prev,
          [filename]: {
            ...current,
            nickname: opts?.nickname ?? current.nickname,
            displayName: opts?.nickname ?? current.displayName,
            fromModel,
            systemPrompt: opts?.systemPrompt ?? current.systemPrompt,
            params: params.map((p) => ({ ...p })),
          },
        };
      });
      setModelfileLoadError(null);
    } catch (err: any) {
      alert("Failed to save modelfile: " + (err?.message || String(err)));
    } finally {
      setModelfileSaving((prev) => ({ ...prev, [filename]: false }));
    }
  }

  async function addCustomModel() {
    const nickname = window.prompt("Nickname for the new model:", "Custom Model")?.trim();
    if (!nickname) return;
    const fileBase = nickname.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "custom_model";
    const filename = `modelfile_${fileBase}.txt`;
    try {
      await invoke("create_custom_modelfile", { filename, nickname });
      const row = await invoke<any>("read_modelfile_data", { filename });
      const loaded: ModelfileDataFE = {
        filename: String(row?.filename ?? filename),
        displayName: String(row?.displayName ?? row?.display_name ?? nickname),
        nickname: String(row?.nickname ?? nickname),
        fromModel: String(row?.fromModel ?? row?.from_model ?? ""),
        systemPrompt: String(row?.systemPrompt ?? row?.system_prompt ?? ""),
        params: Array.isArray(row?.params) ? row.params.map((p: any) => ({ key: String(p?.key ?? ""), value: String(p?.value ?? "") })) : [],
      };
      setModelfileDatas((prev) => ({ ...prev, [filename]: loaded }));
      setModelfileEdits((prev) => ({ ...prev, [filename]: loaded.params.map((p) => ({ ...p })) }));
      setModelfileFromEdits((prev) => ({ ...prev, [filename]: loaded.fromModel }));
      setModelfileFilenames((prev) => (prev.includes(filename) ? prev : [...prev, filename]));
      setModelfileSubTab(filename);

      const custom = [...modelConfig.customModels, { id: makeId("custom_model"), filename, nickname: loaded.nickname, enabled: true, note: "" }];
      await persistModelConfig({ ...modelConfig, customModels: custom });
    } catch (err: any) {
      alert("Failed to create custom model: " + (err?.message || String(err)));
    }
  }

  async function removeCustomModel(modelId: string) {
    const model = modelConfig.customModels.find((m) => m.id === modelId);
    if (!model) return;
    const proceed = await askCenteredConfirm({
      title: "Remove Custom Model",
      message: `Remove custom model \"${model.nickname || model.filename}\"?`,
      confirmLabel: "Remove",
      cancelLabel: "Cancel",
    });
    if (!proceed) return;
    try {
      await invoke("delete_custom_modelfile", { filename: model.filename });
      const custom = modelConfig.customModels.filter((m) => m.id !== modelId);
      await persistModelConfig({ ...modelConfig, customModels: custom });

      setModelfileDatas((prev) => {
        const next = { ...prev };
        delete next[model.filename];
        return next;
      });
      setModelfileEdits((prev) => {
        const next = { ...prev };
        delete next[model.filename];
        return next;
      });
      setModelfileFromEdits((prev) => {
        const next = { ...prev };
        delete next[model.filename];
        return next;
      });
      setModelfileFilenames((prev) => prev.filter((n) => n !== model.filename));
      if (modelfileSubTab === model.filename) {
        setModelfileSubTab("modelfile_organizer.txt");
      }
    } catch (err: any) {
      alert("Failed to remove custom model: " + (err?.message || String(err)));
    }
  }

  async function appendPendingImages(files: File[]) {
    const imageFiles = files.filter((file) => file.type.startsWith("image/"));
    if (!imageFiles.length) return;
    try {
      const rows = await Promise.all(
        imageFiles.slice(0, 4).map(async (file) => ({
          id: makeId("img"),
          name: file.name || "image",
          mimeType: file.type || "image/png",
          dataUrl: await fileToDataUrl(file),
        }))
      );
      setPendingImages((prev) => [...prev, ...rows].slice(0, 4));
    } catch (err) {
      console.warn("Failed to read pasted/dropped image", err);
    }
  }

  function removePendingImage(imageId: string) {
    setPendingImages((prev) => prev.filter((img) => img.id !== imageId));
  }

  async function handleChatPaste(e: React.ClipboardEvent<HTMLTextAreaElement>) {
    const imageFiles = Array.from(e.clipboardData.items)
      .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
      .map((item) => item.getAsFile())
      .filter((file): file is File => !!file);
    const raw = e.clipboardData.getData("text");

    if (!imageFiles.length) {
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
      return;
    }

    e.preventDefault();
    await appendPendingImages(imageFiles);
    if (raw) {
      const el = e.currentTarget;
      const start = el.selectionStart ?? 0;
      const end = el.selectionEnd ?? 0;
      setInput(prev => prev.slice(0, start) + raw + prev.slice(end));
    }
  }

  async function handleChatDrop(e: React.DragEvent<HTMLFormElement>) {
    e.preventDefault();
    setChatDragActive(false);
    const files = Array.from(e.dataTransfer.files || []);
    await appendPendingImages(files);
  }

  async function handleCloseTab(tabId: number) {
    const tabObj = tabs.find(t => t.id === tabId);
    if (tabObj?.type === "settings") {
      setTabs(prevTabs => {
        const idx = prevTabs.findIndex(tab => tab.id === tabId);
        const newTabs = prevTabs.filter(tab => tab.id !== tabId);
        if (tabId === activeTab && newTabs.length > 0) {
          const newIdx = Math.max(0, idx - 1);
          setActiveTab(newTabs[newIdx].id);
        }
        return newTabs;
      });
      return;
    }

    let backendOk = false;
    try {
      const nowTs = Math.floor(Date.now() / 1000);
      const snapshotPayload = {
        tab_id: tabId,
        title: tabObj?.title ?? `Tab #${tabId}`,
        messages: (tabObj?.messages || []).map((m: any) => ({
          role: m.role,
          text: m.text,
          time: (m?.time ?? nowTs),
        })),
        associatedProjectId: tabProjectMap[tabId] ?? null,
        microSummary: "",
        dialogueBullets: "",
        summary: "",
        artifacts: [],
        promptHistory: tabObj?.promptHistory ?? [],
        last_updated: nowTs,
      };

      const attempts: any[] = [
        { tabId: tabId, snapshot: snapshotPayload },
        { tab_id: tabId, snapshot: snapshotPayload },
        { args: { tabId: tabId, snapshot: snapshotPayload } },
        { args: { tab_id: tabId, snapshot: snapshotPayload } },
      ];
      for (const payload of attempts) {
        try {
          await invoke("close_tab_and_snapshot", payload);
          backendOk = true;
          break;
        } catch {}
      }
    } catch (e) {
      console.warn("close_tab_and_snapshot failed; closing UI anyway:", e);
    }

    // Always update UI (optimistic close)
    setTabs(prevTabs => {
      const idx = prevTabs.findIndex(tab => tab.id === tabId);
      let newTabs = prevTabs.filter(tab => tab.id !== tabId);

      // keep Settings pinned to the right if present
      const settings = newTabs.find(t => t.type === "settings");
      if (settings && newTabs[newTabs.length - 1]?.type !== "settings") {
        newTabs = [...newTabs.filter(t => t.type !== "settings"), settings];
      }

      if (tabId === activeTab && newTabs.length > 0) {
        const newIdx = Math.max(0, idx - 1);
        setActiveTab(newTabs[newIdx].id);
      }
      return newTabs;
    });

    // No browser localStorage fallback: desktop app uses backend snapshots only.
  }


  useEffect(() => {
    function handleClick(e: MouseEvent) {
      if (
        taskbarRef.current &&
        !taskbarRef.current.contains(e.target as Node)
      ) {
        setOpenMenu(null);
        setOpenSubmenu(null);
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

  function recallPromptHistory(direction: "older" | "newer") {
    if (currentTab?.type !== "chat") return;
    const history = currentTab.promptHistory || [];
    if (!history.length) return;

    if (promptHistoryIndexRef.current === -1) {
      promptHistoryDraftRef.current = input;
    }

    if (direction === "older") {
      const nextIndex = Math.min(promptHistoryIndexRef.current + 1, history.length - 1);
      promptHistoryIndexRef.current = nextIndex;
      setInput(history[nextIndex] || "");
      queueMicrotask(() => autoResize());
      return;
    }

    if (promptHistoryIndexRef.current <= 0) {
      promptHistoryIndexRef.current = -1;
      setInput(promptHistoryDraftRef.current || "");
      queueMicrotask(() => autoResize());
      return;
    }

    const nextIndex = promptHistoryIndexRef.current - 1;
    promptHistoryIndexRef.current = nextIndex;
    setInput(history[nextIndex] || "");
    queueMicrotask(() => autoResize());
  }

  const MODEL_STATUS_MESSAGES: Record<string, string> = {
    "iris-coder": "coding",
    "iris-organizer": "orchestrating",
    "iris-summarizer": "compressing",
    "iris-vision": "inspecting",
  };

  function statusVerbForModel(modelName: string | null): string {
    if (!modelName) return "thinking";
    const normalized = modelName.toLowerCase();
    const override = modelConfig.statusVerbs?.[normalized]?.trim();
    if (override) return override;
    if (MODEL_STATUS_MESSAGES[normalized]) return MODEL_STATUS_MESSAGES[normalized];
    return "processing";
  }

  function currentStatusText(): string {
    if (respondingTab !== activeTab) return `${assistantLabel} is responding in a different chat`;
    if (thinkingModel) return `${assistantLabel} is ${statusVerbForModel(thinkingModel)}`;
    if (irisStatus === "coding") return `${assistantLabel} is ${statusVerbForModel("iris-coder")}`;
    if (irisStatus === "summarizing") return `${assistantLabel} is ${statusVerbForModel("iris-summarizer")}`;
    if (irisStatus === "responding") return `${assistantLabel} is responding`;
    if (irisStatus === "thinking") return `${assistantLabel} is ${statusVerbForModel("iris-organizer")}`;
    return `${assistantLabel} is thinking`;
  }

  const lastSentRef = useRef<string>("");
  const stoppedRef = useRef<boolean>(false);

  useEffect(() => {
    if (!startupStatus.active) return;
    setThinking(false);
    setIsGenerating(false);
    setIsSummarizing(false);
    setIrisStatus("idle");
    setThinkingStep("");
    setRespondingTab(null);
  }, [startupStatus.active]);

  async function persistGeneralSettings(partial: { assistantName?: string; themeColor?: string; themePreset?: ThemePreset; colorMode?: "dark" | "light" }) {
    try {
      await invoke("set_setup_flags", {
        args: {
          assistantName: partial.assistantName ?? assistantLabel,
          themeColor: partial.themeColor ?? appBgColor,
          themePreset: partial.themePreset ?? themePreset,
          colorMode: partial.colorMode ?? colorMode,
        },
      });
    } catch (e) {
      console.warn("Failed to persist general settings", e);
    }
  }

  async function persistModelConfig(next: ModelConfigFE) {
    setModelConfig(next);
    setModelConfigDirty(true);
    try {
      await invoke("save_model_config", {
        config: {
          coderEnabled: next.coderEnabled,
          visionEnabled: next.visionEnabled,
          customModels: next.customModels,
          modelNotes: next.modelNotes,
          statusVerbs: next.statusVerbs,
        },
      });
      setModelConfigDirty(false);
    } catch (e) {
      console.warn("Failed to persist model config", e);
    }
  }

  function shouldUseNetworkForPrompt(userText: string): boolean {
    const t = normalizeConversationalQuery(userText).toLowerCase();
    if (
      t.includes("selected project") ||
      t.includes("current project") ||
      t.includes("currently selected") ||
      t.includes("project currently") ||
      t.includes("our project")
    ) {
      return false;
    }
    const triggers = [
      "latest", "recent", "today", "current events", "news", "update", "updated", "release",
      "version", "cve", "security advisory", "price", "market", "weather", "forecast", "tomorrow", "this week", "this month",
    ];
    return triggers.some((k) => t.includes(k));
  }

  async function saveNetworkSettings() {
    if (!networkDraftEnabled && networkEnabled) {
      // disabling does not need a warning confirmation
    }
    if (networkDraftEnabled && !networkEnabled) {
      const proceed = window.confirm(
        "Enabling Network features will allow Iris to contact internet services. This may improve freshness and breadth of answers, but introduces normal network risks such as data exposure to third-party services, metadata leakage, and attack surface from connected services. Continue?"
      );
      if (!proceed) return;
    }
    setNetworkEnabled(networkDraftEnabled);
    try {
      await invoke("set_setup_flags", { args: { networkEnabled: networkDraftEnabled } });
    } catch (e: any) {
      alert("Failed to save network setting: " + (e?.message || e));
    }
  }

  function isValidHttpUrl(value: string): boolean {
    const t = String(value || "").trim();
    return /^https?:\/\//i.test(t);
  }

  async function saveUpdateSettings() {
    if (manualDownloadUrl.trim() && !isValidHttpUrl(manualDownloadUrl)) {
      alert("Manual download URL must start with http:// or https://");
      return;
    }
    if (releaseNotesUrl.trim() && !isValidHttpUrl(releaseNotesUrl)) {
      alert("Release notes URL must start with http:// or https://");
      return;
    }
    if (updateFeedUrl.trim() && !isValidHttpUrl(updateFeedUrl)) {
      alert("Update feed URL must start with http:// or https://");
      return;
    }
    try {
      await invoke("set_setup_flags", {
        args: {
          manualDownloadUrl: manualDownloadUrl.trim(),
          releaseNotesUrl: releaseNotesUrl.trim(),
          updateFeedUrl: updateFeedUrl.trim(),
          autoUpdatesEnabled,
        },
      });
      alert("Update links saved.");
    } catch (e: any) {
      alert("Failed to save update settings: " + (e?.message || e));
    }
  }

  async function openExternalUpdateUrl(url: string) {
    const target = String(url || "").trim();
    if (!isValidHttpUrl(target)) return;
    try {
      await openUrl(target);
    } catch (e: any) {
      alert("Could not open link: " + (e?.message || e));
    }
  }

  async function resolveGithubReleaseInstaller() {
    try {
      const resolved = await invoke("resolve_github_release_asset", {
        repo: githubReleaseRepo.trim(),
        preferMsi: preferMsiInstaller,
        includePrerelease: includePrereleaseInstaller,
      }) as {
        repo?: string;
        tag?: string;
        prerelease?: boolean;
        releaseUrl?: string;
        assetName?: string;
        downloadUrl?: string;
      };
      const dl = String(resolved?.downloadUrl || "").trim();
      const rel = String(resolved?.releaseUrl || "").trim();
      if (dl) setManualDownloadUrl(dl);
      if (rel) setReleaseNotesUrl(rel);
      setUpdateFeedUrl(`https://api.github.com/repos/${String(resolved?.repo || githubReleaseRepo).trim()}/releases`);
      alert(`Resolved latest installer: ${String(resolved?.assetName || "(unknown)")} (${String(resolved?.tag || "")})`);
    } catch (e: any) {
      alert("Could not resolve GitHub release installer: " + (e?.message || e));
    }
  }

  async function persistRepoStore(next: RepoProjectStore) {
    setRepoStore(next);
    try {
      await invoke("save_repo_project_store", { store: next });
    } catch (e) {
      console.warn("Failed to save repo/project store", e);
    }
  }

  async function saveBridgeSettings() {
    // Enrich each MCP entry with parsed connectionType/launchCommand/launchArgs before persisting.
    const enriched: McpConnection[] = (Array.isArray(mcpDraft) ? mcpDraft : []).map((mcp) => {
      const parsed = parseMcpTarget(mcp?.target);
      const targetRaw = typeof mcp?.target === "string"
        ? mcp.target
        : (mcp?.target == null ? "" : String(mcp.target));
      const normalizedTarget = parsed.url && targetRaw.trim().startsWith("{") ? parsed.url : targetRaw;
      return {
        ...mcp,
        target: normalizedTarget,
        connectionType: parsed.type,
        launchCommand: parsed.command,
        launchArgs: parsed.args,
      };
    });
    const normalized: SshConnection[] = (Array.isArray(sshDraft) ? sshDraft : []).map((s) => ({
      ...s,
      name: String(s.name || "SSH").trim() || "SSH",
      host: String(s.host || "").trim(),
      port: Number(s.port || 22) || 22,
      username: String(s.username || "").trim(),
      privateKeyPath: String(s.privateKeyPath || "").trim(),
      knownHostsPath: String(s.knownHostsPath || "").trim(),
      remoteRoot: String(s.remoteRoot || "").trim(),
      strictHostKeyChecking: !!s.strictHostKeyChecking,
      extraArgs: Array.isArray(s.extraArgs) ? s.extraArgs.map((a) => String(a || "").trim()).filter(Boolean) : [],
      enabled: !!s.enabled,
      notes: String(s.notes || ""),
    }));
    setMcpDraft(enriched);
    setSshDraft(normalized);
    // Single atomic save prevents the two separate snapshots from clobbering each other.
    await persistRepoStore({ ...repoStore, mcps: enriched, sshs: normalized });
  }

  function appendActionTrace(tabId: number, entry: Omit<IrisActionTrace, "id" | "ts">) {
    const trace: IrisActionTrace = {
      id: makeId("act"),
      ts: Math.floor(Date.now() / 1000),
      ...entry,
    };
    setActionTraceByTab((prev) => {
      const existing = Array.isArray(prev[tabId]) ? prev[tabId] : [];
      return { ...prev, [tabId]: [...existing, trace].slice(-60) };
    });
  }

  function applyFlowAdjustmentFeedback(tabId: number, text: string): string | null {
    const note = detectFlowAdjustmentNote(text);
    if (!note) return null;
    setFlowAdjustmentsByTab((prev) => {
      const existing = Array.isArray(prev[tabId]) ? prev[tabId] : [];
      if (existing.includes(note)) return prev;
      return { ...prev, [tabId]: [...existing, note].slice(-10) };
    });
    return note;
  }

  async function probeMcpReadiness(mcp: McpConnection): Promise<{ connected: boolean; toolCount: number; message: string }> {
    const parsed = parseMcpTarget(mcp.target);
    const connectionType = parsed.type;
    const command = connectionType === "stdio"
      ? (mcp.launchCommand || parsed.command)
      : (parsed.url || mcp.target);
    const args = Array.isArray(mcp.launchArgs) ? mcp.launchArgs : (parsed.args ?? []);

    const conn = await withTimeout(
      invoke("connect_mcp_server", {
        mcpId: mcp.id,
        target: mcp.target,
        connectionType,
        command,
        args,
      }) as Promise<{ connected: boolean; pid?: number | null; protocol?: string | null }>,
      65000,
      `connect_mcp_server:${mcp.name}`
    );

    if (conn?.connected && typeof conn?.pid === "number" && conn.pid > 0) {
      setMcpLaunchStatus((prev) => ({ ...prev, [mcp.id]: { pid: conn.pid!, launched: true } }));
    }

    const tools = await withTimeout(
      invoke("mcp_list_tools", {
        mcpId: mcp.id,
        target: mcp.target,
        connectionType,
        command,
        args,
      }) as Promise<Array<{ name: string }>>,
      35000,
      `mcp_list_tools:${mcp.name}`
    );

    const toolCount = Array.isArray(tools) ? tools.length : 0;
    const proto = conn?.protocol ? ` · ${conn.protocol}` : "";
    return {
      connected: !!conn?.connected,
      toolCount,
      message: toolCount > 0 ? `connected (${toolCount} tools${proto})` : `connected (0 tools${proto})`,
    };
  }

  async function launchMcpServer(mcp: McpConnection) {
    const parsed = parseMcpTarget(mcp?.target);
    if (parsed.type !== "stdio" || !parsed.command) {
      alert("This MCP connection target is a URL, not a launch command. Enter a command string (e.g. 'uv run /path/server.py') to use Launch.");
      return;
    }
    // Clear any previous abort for this id; show Stop button immediately.
    mcpProbeAbortedRef.current.delete(mcp.id);
    setMcpLaunchStatus((prev) => ({ ...prev, [mcp.id]: { pid: 0, launched: true } }));
    setMcpHealthStatus((prev) => ({ ...prev, [mcp.id]: { state: "launching", message: "establishing connection..." } }));
    try {
      const readiness = await probeMcpReadiness(mcp);
      // If Stop was pressed during the probe, ignore result.
      if (mcpProbeAbortedRef.current.has(mcp.id)) return;
      if (readiness.connected) {
        setMcpHealthStatus((prev) => ({
          ...prev,
          [mcp.id]: { state: "connected", message: readiness.message, toolCount: readiness.toolCount },
        }));
      } else {
        setMcpHealthStatus((prev) => ({ ...prev, [mcp.id]: { state: "error", message: readiness.message } }));
        try { await invoke("stop_mcp_server", { mcpId: mcp.id }); } catch (_) { /* ignore */ }
        setMcpLaunchStatus((prev) => ({ ...prev, [mcp.id]: { pid: 0, launched: false } }));
      }
    } catch (probeErr: any) {
      if (mcpProbeAbortedRef.current.has(mcp.id)) return;
      const msg = String(probeErr?.message || probeErr || "connection failed");
      setMcpHealthStatus((prev) => ({ ...prev, [mcp.id]: { state: "error", message: msg } }));
      try { await invoke("stop_mcp_server", { mcpId: mcp.id }); } catch (_) { /* ignore */ }
      setMcpLaunchStatus((prev) => ({ ...prev, [mcp.id]: { pid: 0, launched: false } }));
    }
  }

  async function stopMcpServer(mcp: McpConnection) {
    const isLaunching = mcpHealthStatus[mcp.id]?.state === "launching";
    // Skip confirm dialog while still establishing connection.
    if (!isLaunching) {
      const proceed = await askCenteredConfirm({
        title: "Stop Bridge Server?",
        message: `Are you sure you want to stop "${mcp.name}"?`,
        confirmLabel: "Stop",
        cancelLabel: "Cancel",
      });
      if (!proceed) return;
    }

    // Mark any pending probe as cancelled so launchMcpServer doesn't overwrite UI state.
    mcpProbeAbortedRef.current.add(mcp.id);

    try {
      await invoke("stop_mcp_server", { mcpId: mcp.id });
    } catch (_) { /* ignore */ }
    setMcpLaunchStatus((prev) => ({ ...prev, [mcp.id]: { pid: 0, launched: false } }));
    setMcpHealthStatus((prev) => ({ ...prev, [mcp.id]: { state: "stopped", message: "stopped" } }));
  }

  async function toggleMcpServer(mcp: McpConnection) {
    if (mcpLaunchStatus[mcp.id]?.launched) {
      await stopMcpServer(mcp);
    } else {
      await launchMcpServer(mcp);
    }
  }

  async function connectSshBridge(ssh: SshConnection) {
    setSshConnectStatus((prev) => ({ ...prev, [ssh.id]: { connected: false, checking: true, message: "Checking..." } }));
    try {
      const raw = await invoke("ssh_tool_call", {
        connection: ssh,
        action: "list",
        arguments: { path: "." },
      }) as any;
      const ok = !!raw?.ok;
      setSshConnectStatus((prev) => ({
        ...prev,
        [ssh.id]: {
          connected: ok,
          checking: false,
          message: ok ? "Connected" : String(raw?.error || "Connection check failed"),
        },
      }));
    } catch (e: any) {
      setSshConnectStatus((prev) => ({
        ...prev,
        [ssh.id]: { connected: false, checking: false, message: String(e?.message || e || "Connection failed") },
      }));
    }
  }

  function disconnectSshBridge(ssh: SshConnection) {
    setSshConnectStatus((prev) => ({
      ...prev,
      [ssh.id]: { connected: false, checking: false, message: "Disconnected" },
    }));
  }

  async function toggleSshBridge(ssh: SshConnection) {
    if (sshConnectStatus[ssh.id]?.connected) {
      disconnectSshBridge(ssh);
      return;
    }
    await connectSshBridge(ssh);
  }

  async function saveProjectDescription(projectId: string) {
    const draft = (projectDescriptionDrafts[projectId] ?? "").trim();
    const manipulationRootPath = (projectManipulationRootDrafts[projectId] ?? "").trim();
    const next: RepoProjectStore = {
      ...repoStore,
      projects: repoStore.projects.map((p) => (p.id === projectId ? { ...p, description: draft, manipulationRootPath } : p)),
    };
    await persistRepoStore(next);
    setProjectDescriptionDrafts((prev) => ({ ...prev, [projectId]: draft }));
    setProjectManipulationRootDrafts((prev) => ({ ...prev, [projectId]: manipulationRootPath }));
    setProjectDescriptionDirty((prev) => ({ ...prev, [projectId]: false }));
  }

  function queueProjectDraftAutosave(projectId: string) {
    const existing = projectDraftSaveTimerRef.current[projectId];
    if (existing) clearTimeout(existing);
    projectDraftSaveTimerRef.current[projectId] = setTimeout(() => {
      void saveProjectDescription(projectId);
      delete projectDraftSaveTimerRef.current[projectId];
    }, 900);
  }

  async function saveUserPersonaPrompt() {
    try {
      await invoke("save_user_persona_prompt", { content: userPersonaPrompt });
      setUserPersonaSavedPrompt(userPersonaPrompt);
      setUserPersonaDirty(false);
    } catch (e: any) {
      alert("Failed to save user persona prompt: " + (e?.message || String(e)));
    }
  }

  async function persistTabProjectAssociation(tabId: number, projectId: string | null) {
    setTabProjectMap((prev) => ({ ...prev, [tabId]: projectId }));
    // Intentionally avoid immediate disk write here.
    // Project association is persisted during normal tab snapshot saves,
    // which avoids lock contention from read-then-write cycles.
  }

  async function sendRepoAssistantMessage() {
    const text = repoAssistantInput.trim();
    if (!text || repoAssistantBusy) return;
    setRepoAssistantInput("");
    setRepoAssistantBusy(true);
    setRepoAssistantMessages((prev) => [...prev, { role: "user", text }]);

    try {
      const urlMatch = text.match(/https?:\/\/[^\s]+/i);
      if (urlMatch && repoAssistantTargetRepoId) {
        const target = repoStore.repos.find((r) => r.id === repoAssistantTargetRepoId);
        if (target) {
          const result = await invoke("clone_repo_into_folder", {
            repoUrl: urlMatch[0],
            destinationRoot: target.path,
          }) as string;
          await handleRefreshRepo(target.id);
          setRepoAssistantMessages((prev) => [...prev, { role: "assistant", text: result }]);
          setRepoAssistantBusy(false);
          return;
        }
      }

      const repoSummary = repoStore.repos.map((r) => `${r.name}: ${r.path}`).join("\n") || "(no repos configured)";
      const prompt = `You are Iris repo assistant. Keep answers concise and actionable.\nConfigured repos:\n${repoSummary}\n\nUser request:\n${text}\n\nIf the user wants automatic install, ask for a direct Git URL and target repo folder.`;
      const controller = new AbortController();
      let response = "";
      await stream({
        model: "iris-organizer:latest",
        prompt,
        options: { temperature: 0.2, num_predict: 220 },
        signal: controller.signal,
        onFirstToken: (first) => { response += first; },
        onTokens: (delta) => { response += delta; },
      });
      setRepoAssistantMessages((prev) => [...prev, { role: "assistant", text: response.trim() || "I need a bit more detail." }]);
    } catch (e: any) {
      const errText = String(e?.message || e || "Unknown error");
      if (/git is not installed|not in path|dependency/i.test(errText.toLowerCase())) {
        const proceed = await askCenteredConfirm({
          title: "Install Dependencies",
          message: "A required dependency is missing. Should Iris try to install Git/repo dependencies automatically now?",
          confirmLabel: "Install",
          cancelLabel: "Not now",
        });
        if (proceed) {
          setRepoAssistantMessages((prev) => [...prev, { role: "assistant", text: "Installing dependencies now..." }]);
          try {
            const installResult = await invoke("install_repo_dependencies") as string;
            setRepoAssistantMessages((prev) => [...prev, { role: "assistant", text: installResult }]);

            const restartNow = await askCenteredConfirm({
              title: "Restart Iris?",
              message: "Dependencies finished installing. Restart Iris now to finalize setup and enable repo install?",
              confirmLabel: "Restart now",
              cancelLabel: "Later",
            });
            if (restartNow) {
              await invoke("restart_app");
            } else {
              setRepoAssistantMessages((prev) => [
                ...prev,
                { role: "assistant", text: "No restart selected. Repo install may continue failing until Iris is restarted." },
              ]);
            }
          } catch (installErr: any) {
            setRepoAssistantMessages((prev) => [
              ...prev,
              { role: "assistant", text: "Automatic dependency install failed: " + (installErr?.message || installErr) },
            ]);
          }
        } else {
          setRepoAssistantMessages((prev) => [...prev, { role: "assistant", text: "Repo assistant failed: " + errText }]);
        }
      } else {
        setRepoAssistantMessages((prev) => [...prev, { role: "assistant", text: "Repo assistant failed: " + errText }]);
      }
    } finally {
      setRepoAssistantBusy(false);
    }
  }

  async function handleAddRepoFolder() {
    try {
      const picked = await invoke("pick_repo_folder") as string | null;
      if (!picked) return;
      const entries = await invoke("scan_repo_entries", { path: picked }) as RepoEntry[];
      const repo: RepoFolder = {
        id: makeId("repo"),
        name: picked.split(/[\\/]/).filter(Boolean).pop() || "Repository",
        path: picked,
        enabled: true,
        entries: entries || [],
        selectedEntryIds: [],
      };
      const next = { ...repoStore, repos: [...repoStore.repos, repo] };
      await persistRepoStore(next);
    } catch (e: any) {
      alert("Failed to add repository folder: " + (e?.message || e));
    }
  }

  async function handleRefreshRepo(repoId: string) {
    const repo = repoStore.repos.find((r) => r.id === repoId);
    if (!repo) return;
    try {
      const entries = await invoke("scan_repo_entries", { path: repo.path }) as RepoEntry[];
      const validEntryIds = new Set((entries || []).map((e) => e.id));
      const next: RepoProjectStore = {
        ...repoStore,
        repos: repoStore.repos.map((r) =>
          r.id === repoId
            ? {
                ...r,
                entries: entries || [],
                selectedEntryIds: r.selectedEntryIds.filter((id) => validEntryIds.has(id)),
              }
            : r
        ),
        projects: repoStore.projects.map((p) => ({
          ...p,
          entryIds: p.entryIds.filter((id) => validEntryIds.has(id)),
        })),
      };
      await persistRepoStore(next);
    } catch (e: any) {
      alert("Failed to refresh repository: " + (e?.message || e));
    }
  }

  async function handleRefreshAllRepos() {
    for (const repo of repoStore.repos) {
      await handleRefreshRepo(repo.id);
    }
  }

  function isRepoNodeExpanded(repoId: string, nodePath: string, depth: number): boolean {
    const key = `${repoId}::${normalizeSlashes(nodePath)}`;
    if (key in repoTreeExpanded) {
      return !!repoTreeExpanded[key];
    }
    return depth < 1;
  }

  function toggleRepoNodeExpanded(repoId: string, nodePath: string) {
    const key = `${repoId}::${normalizeSlashes(nodePath)}`;
    setRepoTreeExpanded((prev) => ({ ...prev, [key]: !prev[key] }));
  }

  function getRepoNodeAffectedEntryIds(repo: RepoFolder, node: RepoTreeNode): string[] {
    const nodePath = normalizeSlashes(node.path);
    if (node.isDir) {
      const prefix = `${nodePath}/`;
      const affected = repo.entries
        .filter((e) => {
          const p = normalizeSlashes(e.path);
          return p === nodePath || p.startsWith(prefix);
        })
        .map((e) => e.id);
      return Array.from(new Set(affected));
    }
    return node.entryId ? [node.entryId] : [];
  }

  function isRepoNodeChecked(repo: RepoFolder, node: RepoTreeNode): boolean {
    const affected = getRepoNodeAffectedEntryIds(repo, node);
    if (!affected.length) return false;
    return affected.every((id) => repo.selectedEntryIds.includes(id));
  }

  async function toggleRepoNodeSelection(repoId: string, node: RepoTreeNode, checked: boolean) {
    const next: RepoProjectStore = {
      ...repoStore,
      repos: repoStore.repos.map((r) => {
        if (r.id !== repoId) return r;
        const affected = getRepoNodeAffectedEntryIds(r, node);
        if (!affected.length) return r;

        const selectedSet = new Set(r.selectedEntryIds);
        if (checked) {
          for (const id of affected) selectedSet.add(id);
        } else {
          for (const id of affected) selectedSet.delete(id);
        }

        return { ...r, selectedEntryIds: Array.from(selectedSet) };
      }),
    };
    await persistRepoStore(next);
  }

  function parseJsonArrayFromText(text: string): string[] {
    const start = text.indexOf("[");
    const end = text.lastIndexOf("]");
    if (start < 0 || end <= start) return [];
    try {
      const parsed = JSON.parse(text.slice(start, end + 1));
      if (!Array.isArray(parsed)) return [];
      return parsed.map((v) => String(v || "").trim()).filter(Boolean);
    } catch {
      return [];
    }
  }

  async function handleAutoSelectRepoEntries(repoId: string) {
    const repo = repoStore.repos.find((r) => r.id === repoId);
    if (!repo) return;
    setRepoAutoSelectingId(repoId);
    setRepoAutoStatus((prev) => ({ ...prev, [repoId]: "Analyzing repo entries..." }));

    try {
      await ensureOllamaServer();
      await ensureModel("iris-organizer:latest");
      setRepoAutoStatus((prev) => ({ ...prev, [repoId]: "Consulting project context..." }));

      const linkedProjects = repoStore.projects.filter((p) => p.enabled && p.repoIds.includes(repoId));
      const fallbackProjects = linkedProjects.length ? linkedProjects : repoStore.projects.filter((p) => p.enabled);
      const projectContext = fallbackProjects
        .map((p) => `- ${p.name}: ${p.description || "(no description)"}`)
        .join("\n");

      const entryLines = repo.entries.slice(0, 700).map((e) => `${e.id} | ${e.isDir ? "DIR" : "FILE"} | ${toRelativeRepoPath(repo.path, e.path)}`);
      const prompt = `Select the most relevant repo entries for active projects.\nReturn ONLY a JSON array of entry IDs from the list (max 120 IDs).\n\nProject context:\n${projectContext || "(none)"}\n\nEntries:\n${entryLines.join("\n")}`;

      const controller = new AbortController();
      let response = "";
      await stream({
        model: "iris-organizer:latest",
        prompt,
        options: { temperature: 0.05, num_predict: 420 },
        signal: controller.signal,
        onFirstToken: (first) => { response += first; },
        onTokens: (delta) => { response += delta; },
      });
      setRepoAutoStatus((prev) => ({ ...prev, [repoId]: "Applying recommendations..." }));

      let selectedIds = parseJsonArrayFromText(response).filter((id) => repo.entries.some((e) => e.id === id));

      if (!selectedIds.length) {
        const hints = (projectContext || "")
          .toLowerCase()
          .split(/[^a-z0-9_\-]+/)
          .filter((t) => t.length >= 3);
        const uniqHints = Array.from(new Set(hints));
        selectedIds = repo.entries
          .filter((e) => {
            const rel = toRelativeRepoPath(repo.path, e.path).toLowerCase();
            return uniqHints.some((h) => rel.includes(h));
          })
          .slice(0, 120)
          .map((e) => e.id);
      }

      const next: RepoProjectStore = {
        ...repoStore,
        repos: repoStore.repos.map((r) => (r.id === repoId ? { ...r, selectedEntryIds: selectedIds } : r)),
      };
      await persistRepoStore(next);
      setRepoAutoStatus((prev) => ({ ...prev, [repoId]: `Automatic complete: ${selectedIds.length} selected` }));
    } catch (e: any) {
      setRepoAutoStatus((prev) => ({ ...prev, [repoId]: "Automatic failed" }));
      alert("Automatic selection failed: " + (e?.message || e));
    } finally {
      setRepoAutoSelectingId(null);
    }
  }

  function isProjectNodeExpanded(projectId: string, repoId: string, nodePath: string, depth: number): boolean {
    const key = `${projectId}::${repoId}::${normalizeSlashes(nodePath)}`;
    if (key in projectTreeExpanded) {
      return !!projectTreeExpanded[key];
    }
    return depth < 1;
  }

  function toggleProjectNodeExpanded(projectId: string, repoId: string, nodePath: string) {
    const key = `${projectId}::${repoId}::${normalizeSlashes(nodePath)}`;
    setProjectTreeExpanded((prev) => ({ ...prev, [key]: !prev[key] }));
  }

  async function updateProjectEntrySelection(projectId: string, nextEntryIds: string[]) {
    const next: RepoProjectStore = {
      ...repoStore,
      projects: repoStore.projects.map((p) => (p.id === projectId ? { ...p, entryIds: nextEntryIds } : p)),
    };
    await persistRepoStore(next);
  }

  async function handleRefreshProject(project: ProjectDef) {
    setProjectAutoStatus((prev) => ({ ...prev, [project.id]: "Refreshing linked repositories..." }));
    for (const repoId of project.repoIds) {
      await handleRefreshRepo(repoId);
    }
    setProjectAutoStatus((prev) => ({ ...prev, [project.id]: "Refresh complete" }));
  }

  async function handleAutoSelectProjectEntries(project: ProjectDef, availableEntries: RepoEntry[]) {
    setProjectAutoSelectingId(project.id);
    setProjectAutoStatus((prev) => ({ ...prev, [project.id]: "Analyzing project relevance..." }));
    try {
      await ensureOllamaServer();
      await ensureModel("iris-organizer:latest");

      const entryLines = availableEntries.slice(0, 900).map((e) => `${e.id} | ${e.isDir ? "DIR" : "FILE"} | ${e.path}`);
      const prompt = `Pick the most relevant files/folders for this project. Return ONLY a JSON array of entry IDs from the list (max 140).\n\nProject name: ${project.name}\nProject description: ${project.description || "(none)"}\n\nEntries:\n${entryLines.join("\n")}`;

      const controller = new AbortController();
      let response = "";
      await stream({
        model: "iris-organizer:latest",
        prompt,
        options: { temperature: 0.05, num_predict: 520 },
        signal: controller.signal,
        onFirstToken: (first) => { response += first; },
        onTokens: (delta) => { response += delta; },
      });

      let entryIds = parseJsonArrayFromText(response).filter((id) => availableEntries.some((e) => e.id === id));
      if (!entryIds.length) {
        const hints = (project.description || project.name || "")
          .toLowerCase()
          .split(/[^a-z0-9_\-]+/)
          .filter((t) => t.length >= 3);
        const uniqHints = Array.from(new Set(hints));
        entryIds = availableEntries
          .filter((e) => uniqHints.some((h) => e.path.toLowerCase().includes(h)))
          .slice(0, 140)
          .map((e) => e.id);
      }

      await updateProjectEntrySelection(project.id, entryIds);
      setProjectAutoStatus((prev) => ({ ...prev, [project.id]: `Automatic complete: ${entryIds.length} selected` }));
    } catch (e: any) {
      setProjectAutoStatus((prev) => ({ ...prev, [project.id]: "Automatic failed" }));
      alert("Project automatic selection failed: " + (e?.message || e));
    } finally {
      setProjectAutoSelectingId(null);
    }
  }

  async function openProjectsTabFromChat() {
    handleSettings();
    setSettingsTab("Projects");
  }

  return (
    <>
      {setupResult === null && (
        <OllamaSetupModal onComplete={setSetupResult} />
      )}
      {centerDialog.open && (
        <div className="setup-overlay" role="dialog" aria-modal="true">
          <div className="setup-modal" style={{ maxWidth: 560 }}>
            <h2>{centerDialog.title}</h2>
            <p style={{ whiteSpace: "pre-wrap" }}>{centerDialog.message}</p>
            <div className="setup-buttons">
              <button className="setup-btn" onClick={() => resolveCenteredConfirm(false)}>
                {centerDialog.cancelLabel}
              </button>
              <button className="setup-btn primary" onClick={() => resolveCenteredConfirm(true)}>
                {centerDialog.confirmLabel}
              </button>
            </div>
          </div>
        </div>
      )}
        {hwToast && (
          <div className="hw-toast-root" role="status" onClick={() => setHwToast(null)}>
            {hwToast}
          </div>
        )}
      <div
        className={`chat-root${isLightMode ? " light-theme" : ""}`}
        style={{
          ["--app-bg" as any]: appBgColor,
          ["--app-surface" as any]: appSurfaceColor,
          ["--app-border" as any]: appBorderColor,
          ["--app-tab-bg" as any]: appTabBgColor,
          ["--app-tab-active-bg" as any]: appTabActiveBgColor,
          ["--app-input-bg" as any]: appInputBgColor,
          ["--app-panel-bg" as any]: appPanelBgColor,
          ["--app-text" as any]: appTextColor,
        }}
      >
      <div className="taskbar" ref={taskbarRef}>
        <div className="menu">
          <button
            className={`menu-btn${menuLocked ? " disabled" : ""}`}
            onClick={() => {
              if (menuLocked) return;
              setOpenMenu(openMenu === "file" ? null : "file");
            }}
            disabled={menuLocked}
          >
            File ▾
          </button>
          {!menuLocked && openMenu === "file" && (
            <div className="dropdown file-dropdown" onClick={e => e.stopPropagation()}>
              <div className="dropdown-item" onClick={handleNewTab}>New tab</div>
              <div className="dropdown-item" onClick={handleClose}>Close</div>
            </div>
          )}
        </div>
        <div className="menu">
          <button
            className={`menu-btn${menuLocked ? " disabled" : ""}`}
            onClick={() => {
              if (menuLocked) return;
              setOpenMenu(openMenu === "options" ? null : "options");
            }}
            disabled={menuLocked}
          >
            Options ▾
          </button>
          {!menuLocked && openMenu === "options" && (
            <div
              className="dropdown options-dropdown"
              onClick={e => e.stopPropagation()}
              onMouseLeave={() => { setOpenMenu(null); setOpenSubmenu(null); }}
            >
              <div className="dropdown-item" onClick={handleSettings}>Settings</div>

              <div
                className="dropdown-item"
                onMouseEnter={() => setOpenSubmenu("development")}
                onMouseLeave={() => setOpenSubmenu(null)}
                onClick={e => e.stopPropagation()}
                style={{ position: "relative" }}
              >
                <span>Development</span><span>▶</span>
                {openSubmenu === "development" && (
                  <div
                    className="submenu"
                    onMouseEnter={() => { /* keep parent open */ }}
                    onMouseLeave={() => setOpenSubmenu(null)}
                  >
                    <div
                      className="submenu-item"
                      onClick={async (e) => {
                        e.stopPropagation();
                        try {
                          const invoke = (window as any).__TAURI__?.core?.invoke ?? (window as any).__TAURI__?.invoke;
                          if (invoke) {
                            await invoke("show_devtools");
                          } else {
                            alert("Tauri not available (web mode?)");
                          }
                        } catch (err: any) {
                          alert("Failed to open devtools: " + err?.message);
                        }
                      }}
                    >
                      Open DevTools
                    </div>
                  </div>
                )}
              </div>

              <div
                className="dropdown-item"
                onClick={async (e) => {
                  e.stopPropagation();
                  const next = !desktopDashboardEnabled;
                  setDesktopDashboardEnabled(next);
                  try {
                    await invoke("set_setup_flags", { args: { desktopDashboardEnabled: next } });
                  } catch {}
                  setOpenSubmenu(null);
                  setOpenMenu(null);
                }}
              >
                <span>{desktopDashboardEnabled ? "✓ " : ""}Desktop Dashboard</span>
              </div>
            </div>
          )}
        </div>
      </div>

      <div className="tab-bar">
        {tabs.map(tab => (
          <div
            key={tab.id}
            className={`tab-wrapper${tab.id === activeTab ? " active" : ""}${draggingTabId === tab.id ? " dragging" : ""}`}
            draggable={tab.type === "chat"}
            onDragStart={() => {
              if (tab.type !== "chat") return;
              setDraggingTabId(tab.id);
            }}
            onDragOver={(e) => {
              if (tab.type !== "chat" || draggingTabId == null || draggingTabId === tab.id) return;
              e.preventDefault();
            }}
            onDrop={(e) => {
              if (tab.type !== "chat" || draggingTabId == null) return;
              e.preventDefault();
              moveChatTab(draggingTabId, tab.id);
              setDraggingTabId(null);
            }}
            onDragEnd={() => setDraggingTabId(null)}
          >
            <button
              className={tab.id === activeTab ? "tab active" : "tab"}
              onClick={() => setActiveTab(tab.id)}
              onDoubleClick={() => {
                if (tab.type === "chat") startTabRename(tab.id);
              }}
            >
              {editingTabId === tab.id ? (
                <input
                  className="tab-title-input"
                  value={editingTabTitle}
                  autoFocus
                  onChange={(e) => setEditingTabTitle(e.target.value)}
                  onClick={(e) => e.stopPropagation()}
                  onBlur={() => { void commitTabRename(tab.id); }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void commitTabRename(tab.id);
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      cancelTabRename();
                    }
                  }}
                />
              ) : (
                tab.title
              )}
            </button>
            {(tab.type === "chat" || tab.type === "settings") && (
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
        {currentTab?.type === "chat" && (
          <div style={{ marginLeft: "auto", paddingRight: 12, display: "flex", alignItems: "center", gap: 8, position: "relative" }}>
            {showChatBridgeControls && (
              <div style={{ position: "relative", display: "flex", alignItems: "center" }}>
                <button
                  className={`setup-btn${chatBridgePanelOpen ? " primary" : ""}`}
                  onClick={() => setChatBridgePanelOpen((v) => !v)}
                  disabled={menuLocked}
                >
                  Bridge Servers
                </button>
                {chatBridgePanelOpen && (
                  <div style={{ position: "absolute", top: "calc(100% + 6px)", right: 0, minWidth: 320, maxWidth: 460, maxHeight: 420, overflowY: "auto", border: "1px solid var(--app-border)", borderRadius: 8, padding: 8, background: "var(--app-panel-bg)", display: "flex", flexDirection: "column", gap: 6, boxShadow: "0 6px 18px rgba(0,0,0,0.35)", zIndex: 280 }}>
                    {launchableProjectMcps.length > 1 && (
                      <div style={{ display: "flex", gap: 6, marginBottom: 4 }}>
                        <button
                          className="setup-btn"
                          onClick={() => {
                            for (const mcp of launchableProjectMcps) {
                              if (!mcpLaunchStatus[mcp.id]?.launched) {
                                void launchMcpServer(mcp);
                              }
                            }
                          }}
                        >
                          Launch All MCP
                        </button>
                        <button
                          className="setup-btn"
                          onClick={() => {
                            for (const mcp of launchableProjectMcps) {
                              if (mcpLaunchStatus[mcp.id]?.launched) {
                                void stopMcpServer(mcp);
                              }
                            }
                          }}
                        >
                          Stop All MCP
                        </button>
                      </div>
                    )}
                    {launchableProjectMcps.map((mcp) => (
                      <div key={`chat_mcp_${mcp.id}`} style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8 }}>
                        <div style={{ fontSize: 12, color: "var(--app-text)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                          <div>
                            {mcp.name}
                            <span style={{ marginLeft: 6, color: isLightMode ? "#4b627a" : "#9fb2c9" }}>
                              {mcpLaunchStatus[mcp.id]?.launched
                                ? `(PID ${mcpLaunchStatus[mcp.id].pid})`
                                : "(stopped)"}
                            </span>
                          </div>
                          {mcpHealthStatus[mcp.id]?.message && mcpHealthStatus[mcp.id]?.state !== "error" && (
                            <div style={{ color: isLightMode ? "#2f5f3b" : "#9be7b1" }}>
                              {mcpHealthStatus[mcp.id]?.message}
                            </div>
                          )}
                          {mcpHealthStatus[mcp.id]?.message && mcpHealthStatus[mcp.id]?.state === "error" && (
                            <div style={{ color: isLightMode ? "#8b2b2b" : "#ff9f9f" }}>
                              {mcpHealthStatus[mcp.id]?.message}
                            </div>
                          )}
                        </div>
                        <div style={{ display: "flex", gap: 6 }}>
                          <button className="setup-btn" onClick={() => toggleMcpServer(mcp)}>
                            {mcpLaunchStatus[mcp.id]?.launched ? "Stop" : "Launch"}
                          </button>
                          <button
                            className="setup-btn"
                            onClick={async () => {
                              setMcpHealthStatus((prev) => ({ ...prev, [mcp.id]: { state: "launching", message: "checking..." } }));
                              try {
                                const readiness = await probeMcpReadiness(mcp);
                                setMcpHealthStatus((prev) => ({
                                  ...prev,
                                  [mcp.id]: {
                                    state: readiness.connected ? "connected" : "bridge_up",
                                    message: readiness.message,
                                    toolCount: readiness.toolCount,
                                  },
                                }));
                              } catch (err: any) {
                                setMcpHealthStatus((prev) => ({ ...prev, [mcp.id]: { state: "error", message: String(err?.message || err || "not connected") } }));
                              }
                            }}
                          >
                            Check
                          </button>
                        </div>
                      </div>
                    ))}
                    {launchableProjectSsh.map((ssh) => {
                      const st = sshConnectStatus[ssh.id];
                      return (
                        <div key={`chat_ssh_${ssh.id}`} style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8 }}>
                          <div style={{ fontSize: 12, color: "var(--app-text)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                            {ssh.name}
                            <span style={{ marginLeft: 6, color: isLightMode ? "#4b627a" : "#9fb2c9" }}>
                              {st?.checking ? "(checking...)" : st?.connected ? "(connected)" : st?.message ? `(${st.message})` : "(disconnected)"}
                            </span>
                          </div>
                          <button className="setup-btn" onClick={() => { void toggleSshBridge(ssh); }} disabled={!!st?.checking}>
                            {st?.connected ? "Disconnect" : "Connect"}
                          </button>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            )}
            <span style={{ color: isLightMode ? "#344454" : "#bbb", fontSize: 12 }}>Project</span>
            <select
              value={activeProjectId || ""}
              onChange={(e) => {
                const value = e.target.value;
                if (value === "__create__") {
                  openProjectsTabFromChat();
                  return;
                }
                void persistTabProjectAssociation(activeTab, value || null);
              }}
              disabled={menuLocked}
              style={{ minWidth: 220, padding: "6px 8px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
            >
              <option value="">No project</option>
              {repoStore.projects.filter((p) => p.enabled).map((p) => (
                <option key={p.id} value={p.id}>{p.name}</option>
              ))}
              <option value="__create__">+ Add new project...</option>
            </select>
          </div>
        )}
      </div>

      <header className="chat-header">
        <h2>
          {currentTab?.type === "settings" ? `${assistantLabel} • Settings` : `${assistantLabel} • Chat`}
        </h2>
        {currentTab?.type === "settings" && (
          <div className="settings-subtab-bar">
            {["General", "Projects", "Repos", "Bridges", "LLMs", "Network", "Controller"].map(subtab => {
              return (
                <button
                  key={subtab}
                  className={settingsTab === subtab ? "settings-subtab active" : "settings-subtab"}
                  onClick={() => setSettingsTab(subtab as any)}
                >
                  {subtab}
                </button>
              );
            })}
          </div>
        )}
        {startupStatus.active && (
          <span style={{ color: '#8fb2ff', fontSize: 13 }}>
            Startup {startupStatus.progress}% • {startupStatus.step}
          </span>
        )}
        {!startupStatus.active && modelStatus === "checking" && (
          <span style={{ color: '#aaa', fontSize: 14 }}>Checking model...</span>
        )}
        {!startupStatus.active && modelStatus === "loading" && (
          <span style={{ color: '#aaa', fontSize: 14 }}>Loading model...</span>
        )}
        {!startupStatus.active && modelStatus === "error" && (
          <span style={{ color: 'red', fontSize: 14 }}>Model error!</span>
        )}
      </header>
      {currentTab?.type === "chat" && !startupStatus.active && irisStatus !== "idle" && (
        <div className="thinking-indicator" style={{ marginTop: 12 }}>
          <strong>
            {currentStatusText()}
            {ellipsis}
          </strong>
          {thinkingStep && (
            <div style={{ marginTop: 4, opacity: 0.8, fontSize: 13 }}>
              {thinkingStep}
              {thinkingSeconds >= 14 ? " (first response after launch can take up to about a minute)" : ""}
            </div>
          )}
          {Array.isArray(workLogByTab[activeTab]) && workLogByTab[activeTab].length > 0 && (
            <div style={{ marginTop: 6, fontSize: 12, opacity: 0.85, lineHeight: 1.35 }}>
              {workLogByTab[activeTab].slice(-5).map((entry, idx) => (
                <div key={`${entry.ts}_${idx}`}>• {entry.text}</div>
              ))}
            </div>
          )}
        </div>
      )}

      {currentTab?.type === "chat" && activeRoutine && activeRoutineTabId === activeTab && (() => {
        const activeStep = activeRoutine.steps.find(s => s.status === "running") ??
          activeRoutine.steps.find(s => s.status === "pending") ?? null;
        const doneCount = activeRoutine.steps.filter(s => s.status === "done").length;
        return (
          <div className="routine-card">
            <div className="routine-header">
              <strong>Routine:</strong> {activeRoutine.goal}
              <span style={{ marginLeft: "auto", display: "flex", gap: 6, alignItems: "center" }}>
                <button
                  type="button"
                  className="setup-btn"
                  style={{ padding: "2px 7px", fontSize: 11 }}
                  onClick={() => setRoutineExpanded(e => !e)}
                  title={routineExpanded ? "Collapse steps" : "Expand all steps"}
                >
                  {routineExpanded ? "▲" : "▼"} {doneCount}/{activeRoutine.steps.length}
                </button>
                <button
                  type="button"
                  className="setup-btn"
                  style={{ padding: "2px 7px", fontSize: 11 }}
                  onClick={() => { setActiveRoutine(null); setActiveRoutineTabId(null); setRoutineExpanded(false); }}
                >
                  ✕
                </button>
              </span>
            </div>
            {!routineExpanded && activeStep && (
              <div className="routine-step running" style={{ marginTop: 4 }}>
                <span className="routine-step-label">{activeStep.label}</span>
                <span className="routine-step-status">{activeStep.status}</span>
              </div>
            )}
            {routineExpanded && (
              <>
                <div className="routine-meta" style={{ marginTop: 4 }}>
                  {activeRoutine.status.replace("_", " ")}
                  {activeRoutine.isLongRunning ? " · long-running" : ""}
                  {windowList.length ? ` · ${windowList.length} windows` : ""}
                </div>
                <div className="routine-steps">
                  {activeRoutine.steps.map((s) => (
                    <div key={s.id} className={`routine-step ${s.status}`}>
                      <span className="routine-step-label">{s.label}</span>
                      <span className="routine-step-status">{s.status}</span>
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        );
      })()}

      {currentTab?.type === "chat" && desktopDashboardEnabled && (() => {
        const latestImage = [...(currentTab.messages || [])]
          .reverse()
          .find((m: any) => Array.isArray(m?.images) && m.images.length > 0)?.images?.[0] as string | undefined;
        return (
          <div className="routine-card" style={{ marginTop: 8 }}>
            <div className="routine-header">
              <strong>Desktop Dashboard</strong>
              <span style={{ marginLeft: 8, fontSize: 12, opacity: 0.65 }}>·</span>
              <span style={{ marginLeft: 8, fontSize: 12, opacity: 0.8 }}>
                {windowList.length > 0 ? `${windowList.length} window${windowList.length !== 1 ? "s" : ""}` : "no windows detected"}
              </span>
            </div>
            <div className="routine-meta" style={{ marginTop: 4 }}>
              Project: {activeProject?.name || "none"} · MCP linked: {launchableProjectMcps.length}
            </div>
            {systemStats && (
              <div style={{ marginTop: 4, fontSize: 12, opacity: 0.8, display: "flex", gap: 12 }}>
                <span>CPU: {systemStats.cpuPercent.toFixed(1)}%</span>
                <span>RAM: {systemStats.memUsedMb.toLocaleString()} / {systemStats.memTotalMb.toLocaleString()} MB</span>
              </div>
            )}
            {windowList.length > 0 && (
              <div style={{ marginTop: 4, fontSize: 11, opacity: 0.65, lineHeight: 1.4 }}>
                {windowList.slice(0, 5).map((w, i) => (
                  <div key={i}>▸ {w.title}</div>
                ))}
                {windowList.length > 5 && <div>…and {windowList.length - 5} more</div>}
              </div>
            )}
            {latestImage && (
              <div style={{ marginTop: 6, display: "flex", alignItems: "center", gap: 8 }}>
                <img src={latestImage} alt="Latest capture" style={{ width: 110, height: 64, objectFit: "cover", borderRadius: 6, border: "1px solid var(--app-border)" }} />
                <span style={{ fontSize: 12, opacity: 0.85 }}>Latest captured frame</span>
              </div>
            )}
          </div>
        );
      })()}

      {currentTab?.type === "chat" && (
        <div className="chat-history" ref={historyRef}>
          {(currentTab.messages || []).map((msg, idx) => (
            <div className={`bubble ${msg.role}`} key={idx}>
              <strong>
                {msg.role === "user" ? "You:" : `${assistantLabel}:`}
              </strong>{" "}
              <ReactMarkdown>{msg.text}</ReactMarkdown>
              {Array.isArray((msg as any).images) && (msg as any).images.length > 0 && (
                <div className="bubble-image-strip">
                  {((msg as any).images as string[]).map((src, imgIdx) => (
                    <div key={`msg_${idx}_img_${imgIdx}`} className="bubble-image-card">
                      <img src={src} alt={`attachment ${imgIdx + 1}`} className="bubble-image-thumb" />
                    </div>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
      {!hasChatTabs && currentTab?.type !== "settings" && (
        <div className="chat-history" ref={historyRef}>
          <div className="bubble llm">
            <strong>{assistantLabel}:</strong>{" "}
            No chats are open. Press <strong>File</strong> and then <strong>New tab</strong> to start a new conversation.
          </div>
        </div>
      )}
      {currentTab?.type === "settings" && (
        <div className="settings-container">
          {settingsTab === "General" && (
            <div className="llm-settings-card">
              <h3>General</h3>
              <p>Customize appearance and persona wiring for this app instance.</p>

              <div style={{ marginBottom: 14 }}>
                <label style={{ display: "block", marginBottom: 6, color: "var(--app-text)" }}>Assistant name</label>
                <input
                  value={assistantName}
                  onChange={(e) => setAssistantName(e.target.value)}
                  onBlur={() => persistGeneralSettings({ assistantName })}
                  placeholder="Iris"
                  style={{ width: "min(420px, 100%)", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                />
              </div>

              <div style={{ marginBottom: 14 }}>
                <label style={{ display: "block", marginBottom: 6, color: "var(--app-text)" }}>Theme color (background + borders)</label>
                <select
                  value={themePreset}
                  onChange={async (e) => {
                    const next = e.target.value as ThemePreset;
                    setThemePreset(next);
                    const nextColor = next === "Custom" ? customThemeColor : THEME_PRESET_COLORS[next as Exclude<ThemePreset, "Custom">];
                    await persistGeneralSettings({ themePreset: next, themeColor: nextColor });
                  }}
                  style={{ minWidth: 240, padding: "8px 10px", borderRadius: 6, background: "var(--app-input-bg)", color: "var(--app-text)", border: "1px solid var(--app-border)" }}
                >
                  <option value="Black">Black</option>
                  <option value="Blue">Blue</option>
                  <option value="Pink">Pink</option>
                  <option value="Purple">Purple</option>
                  <option value="Silver">Silver</option>
                  <option value="Grey">Grey</option>
                  <option value="Custom">Custom</option>
                </select>
                {themePreset === "Custom" && (
                  <div style={{ marginTop: 10 }}>
                    <input
                      type="color"
                      value={customThemeColor}
                      onChange={(e) => {
                        const val = e.target.value;
                        setCustomThemeColor(val);
                        persistGeneralSettings({ themePreset: "Custom", themeColor: val });
                      }}
                      title="Pick custom app color"
                      style={{ width: 52, height: 34, borderRadius: 6, border: "1px solid #3d3d3d", background: "transparent", padding: 0 }}
                    />
                  </div>
                )}
              </div>

              <div style={{ marginBottom: 14 }}>
                <label style={{ display: "block", marginBottom: 6, color: "var(--app-text)" }}>Accent color mode</label>
                <div style={{ display: "flex", gap: 8 }}>
                  <button
                    className={`setup-btn${colorMode === "dark" ? " primary" : ""}`}
                    onClick={async () => {
                      setColorMode("dark");
                      await persistGeneralSettings({ colorMode: "dark" });
                    }}
                  >
                    Dark
                  </button>
                  <button
                    className={`setup-btn${colorMode === "light" ? " primary" : ""}`}
                    onClick={async () => {
                      setColorMode("light");
                      await persistGeneralSettings({ colorMode: "light" });
                    }}
                  >
                    Light
                  </button>
                </div>
                <p style={{ marginTop: 6, fontSize: 11, color: isLightMode ? "#526272" : "#888" }}>
                  Controls tabs, inputs, panels, and surface elements. App background and borders only change with the theme color above.
                </p>
              </div>

              <div style={{ marginBottom: 14 }}>
                <label style={{ display: "block", marginBottom: 6, color: "var(--app-text)" }}>Universal dataweb</label>
                <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                  <button
                    className={`setup-btn${universalDatawebEnabled ? " primary" : ""}`}
                    onClick={async () => {
                      const next = !universalDatawebEnabled;
                      setUniversalDatawebEnabled(next);
                      await invoke("set_setup_flags", { args: { universalDatawebEnabled: next } });
                    }}
                  >
                    Universal Dataweb: {universalDatawebEnabled ? "ON" : "OFF"}
                  </button>
                  <button
                    className="setup-btn"
                    onClick={async () => {
                      try {
                        await invoke("open_universal_dataweb");
                      } catch (e: any) {
                        alert("Failed to open universal dataweb: " + (e?.message || String(e)));
                      }
                    }}
                  >
                    Open Universal Dataweb
                  </button>
                </div>
                <p style={{ marginTop: 6, fontSize: 11, color: isLightMode ? "#526272" : "#888" }}>
                  When OFF, Iris will ignore and stop writing universal cross-project memory.
                </p>
              </div>

              <hr style={{ opacity: 0.2, margin: "12px 0" }} />
              <div style={{ marginBottom: 8, fontSize: 13, color: "var(--app-text)" }}>Core persona prompt</div>
              <textarea
                value={corePersonaPrompt}
                readOnly
                style={{ width: "100%", minHeight: 170, background: isLightMode ? "rgba(86, 112, 138, 0.10)" : "rgba(18, 24, 31, 0.95)", color: "var(--app-text)", border: "1px solid var(--app-border)", borderRadius: 8, padding: 10, boxSizing: "border-box", opacity: 0.92 }}
              />
              <div style={{ marginTop: 14, marginBottom: 8, fontSize: 13, color: "var(--app-text)" }}>User persona overlay</div>
              <textarea
                value={userPersonaPrompt}
                onChange={(e) => {
                  const next = e.target.value;
                  setUserPersonaPrompt(next);
                  setUserPersonaDirty(next !== userPersonaSavedPrompt);
                }}
                rows={8}
                placeholder="Add your style preferences, behavior constraints, and response preferences here."
                style={{ width: "100%", minHeight: 180, background: "var(--app-input-bg)", color: "var(--app-text)", border: "1px solid var(--app-border)", borderRadius: 8, padding: 10, boxSizing: "border-box" }}
              />
              <div style={{ marginTop: 8, display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                <button className={`setup-btn${userPersonaDirty ? " primary" : ""}`} disabled={!userPersonaDirty} onClick={saveUserPersonaPrompt}>
                  {userPersonaDirty ? "Save User Persona" : "Saved"}
                </button>
                <button
                  className="setup-btn"
                  onClick={async () => {
                    try {
                      const user = await invoke("get_user_persona_prompt") as string;
                      setUserPersonaPrompt(String(user || ""));
                      setUserPersonaSavedPrompt(String(user || ""));
                      setUserPersonaDirty(false);
                    } catch (e: any) {
                      alert("Failed to reload user persona prompt: " + (e?.message || e));
                    }
                  }}
                >
                  Reload
                </button>
              </div>
            </div>
          )}
          {settingsTab === "Repos" && (
            <div className="llm-settings-card">
              <h3>Repositories</h3>
              <p>Add one or more repository folders, then choose files/folders Iris should reference. Repositories are reference-only and are not used as the direct write root.</p>
              <div style={{ marginBottom: 12, border: "1px solid var(--app-border)", background: isLightMode ? "rgba(86, 112, 138, 0.14)" : "#17202a", borderRadius: 8, padding: "9px 10px" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
                  <button
                    className={`setup-btn${reposContextActive ? " primary" : ""}`}
                    onClick={async () => {
                      const next = !reposEnabled;
                      setReposEnabled(next);
                      await invoke("set_setup_flags", { args: { reposEnabled: next } });
                    }}
                  >
                    Repo Context: {reposContextActive ? "ON" : "OFF"}
                  </button>
                  <span style={{ fontSize: 12, color: isLightMode ? "#2f4359" : "#cfd9e8" }}>
                    Toggle whether Iris should regard or disregard your configured repository context during responses.
                  </span>
                </div>
                <div style={{ marginTop: 6, fontSize: 12, color: isLightMode ? "#4b627a" : "#9fb2c9" }}>
                  Turning this off can significantly improve speed and reduce cognitive bandwidth usage, especially on lower-powered devices.
                </div>
              </div>
              <div className="llm-settings-actions" style={{ marginBottom: 12 }}>
                <button className="setup-btn primary" onClick={handleAddRepoFolder}>Add Repository Folder</button>
                <button className="setup-btn" onClick={handleRefreshAllRepos}>Refresh All</button>
                <button
                  className="setup-btn"
                  onClick={async () => {
                    const next: RepoProjectStore = {
                      ...repoStore,
                      repos: repoStore.repos.map((r) => ({ ...r, selectedEntryIds: r.entries.map((e) => e.id) })),
                    };
                    await persistRepoStore(next);
                  }}
                >
                  Select All
                </button>
                <button
                  className="setup-btn"
                  onClick={async () => {
                    const next: RepoProjectStore = {
                      ...repoStore,
                      repos: repoStore.repos.map((r) => ({ ...r, selectedEntryIds: [] })),
                    };
                    await persistRepoStore(next);
                  }}
                >
                  Select None
                </button>
              </div>

              <div style={{ display: "flex", flexDirection: "column", gap: 12, maxHeight: 440, overflowY: "auto", paddingRight: 4 }}>
                {repoStore.repos.map((repo) => (
                  <div key={repo.id} style={{ border: "1px solid var(--app-border)", borderRadius: 8, padding: 10, background: "var(--app-panel-bg)" }}>
                    <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8, flexWrap: "wrap" }}>
                      <input
                        value={repo.name}
                        onChange={async (e) => {
                          const name = e.target.value;
                          const next: RepoProjectStore = {
                            ...repoStore,
                            repos: repoStore.repos.map((r) => r.id === repo.id ? { ...r, name } : r),
                          };
                          await persistRepoStore(next);
                        }}
                        style={{ minWidth: 210, padding: "6px 8px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                      />
                      <label style={{ fontSize: 12, color: "var(--app-text)" }}>
                        <input
                          type="checkbox"
                          checked={repo.enabled}
                          onChange={async (e) => {
                            const enabled = e.target.checked;
                            const next: RepoProjectStore = {
                              ...repoStore,
                              repos: repoStore.repos.map((r) => r.id === repo.id ? { ...r, enabled } : r),
                            };
                            await persistRepoStore(next);
                          }}
                          style={{ marginRight: 6 }}
                        />
                        Enabled
                      </label>
                      <button
                        className="setup-btn"
                        onClick={async () => {
                          const proceed = await askCenteredConfirm({
                            title: "Remove Repository",
                            message: `Remove repository "${repo.name}" from settings?`,
                            confirmLabel: "Remove",
                            cancelLabel: "Cancel",
                          });
                          if (!proceed) return;
                          const nextRepos = repoStore.repos.filter((r) => r.id !== repo.id);
                          const nextProjects = repoStore.projects.map((p) => ({
                            ...p,
                            repoIds: p.repoIds.filter((rid) => rid !== repo.id),
                            entryIds: p.entryIds.filter((eid) => !repo.entries.some((e) => e.id === eid)),
                            mcpIds: p.mcpIds || [],
                          }));
                          await persistRepoStore({ repos: nextRepos, mcps: repoStore.mcps, sshs: repoStore.sshs, projects: nextProjects });
                        }}
                      >
                        Remove
                      </button>
                    </div>
                    <div style={{ fontSize: 12, color: isLightMode ? "#495a6a" : "#a7a7a7", marginBottom: 8 }}>{repo.path}</div>
                    {!!repoAutoStatus[repo.id] && (
                      <div style={{ fontSize: 12, color: "#8fb2ff", marginBottom: 8 }}>{repoAutoStatus[repo.id]}</div>
                    )}
                    <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", marginBottom: 8 }}>
                      <button
                        className="setup-btn"
                        onClick={async () => {
                          await handleRefreshRepo(repo.id);
                        }}
                      >
                        Refresh
                      </button>
                      <button
                        className="setup-btn"
                        onClick={async () => {
                          await handleAutoSelectRepoEntries(repo.id);
                        }}
                        disabled={repoAutoSelectingId === repo.id}
                      >
                        {repoAutoSelectingId === repo.id ? "Automatic..." : "Automatic"}
                      </button>
                      <button
                        className="setup-btn"
                        onClick={async () => {
                          const next: RepoProjectStore = {
                            ...repoStore,
                            repos: repoStore.repos.map((r) => r.id === repo.id ? { ...r, selectedEntryIds: r.entries.map((e) => e.id) } : r),
                          };
                          await persistRepoStore(next);
                        }}
                      >
                        All
                      </button>
                      <button
                        className="setup-btn"
                        onClick={async () => {
                          const next: RepoProjectStore = {
                            ...repoStore,
                            repos: repoStore.repos.map((r) => r.id === repo.id ? { ...r, selectedEntryIds: [] } : r),
                          };
                          await persistRepoStore(next);
                        }}
                      >
                        None
                      </button>
                    </div>
                    <div style={{ border: "1px solid #323232", borderRadius: 8, maxHeight: 220, overflowY: "auto", padding: 8 }}>
                      {buildRepoTree(repo).map((node) => {
                        const renderNode = (n: RepoTreeNode, depth: number) => {
                          const expanded = n.isDir ? isRepoNodeExpanded(repo.id, n.path, depth) : false;
                          const isChecked = isRepoNodeChecked(repo, n);
                          return (
                            <div key={`${repo.id}_${n.id}_${depth}`}>
                              <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 13, color: "var(--app-text)", padding: "4px 0", paddingLeft: depth * 14 }}>
                                {n.isDir ? (
                                  <button
                                    className="setup-btn"
                                    style={{ padding: "1px 8px", minWidth: 28 }}
                                    onClick={() => toggleRepoNodeExpanded(repo.id, n.path)}
                                  >
                                    {expanded ? "-" : "+"}
                                  </button>
                                ) : (
                                  <span style={{ width: 28 }} />
                                )}
                                <input
                                  type="checkbox"
                                  checked={isChecked}
                                  disabled={!n.isDir && !n.entryId}
                                  onChange={async (e) => {
                                    await toggleRepoNodeSelection(repo.id, n, e.target.checked);
                                  }}
                                />
                                <span style={{ minWidth: 44, color: "#9db8ff" }}>{n.isDir ? "DIR" : "FILE"}</span>
                                <span style={{ flex: 1 }}>{n.name}</span>
                                <span style={{ color: isLightMode ? "#56697c" : "#9f9f9f" }}>{formatBytes(n.sizeBytes || 0)}</span>
                              </div>
                              {n.isDir && expanded && n.children.map((c) => renderNode(c, depth + 1))}
                            </div>
                          );
                        };
                        return renderNode(node, 0);
                      })}
                    </div>
                  </div>
                ))}
                {!repoStore.repos.length && (
                  <div style={{ color: "#aaa", fontSize: 13 }}>No repository folders added yet.</div>
                )}
              </div>

              <div style={{ marginTop: 14, marginBottom: 14, borderTop: "1px solid #2f2f2f", paddingTop: 12 }}>
                <h4 style={{ margin: "0 0 8px 0", color: "var(--app-text)" }}>Iris Repo Assistant</h4>
                {!networkEnabled && (
                  <div style={{ marginBottom: 10, padding: "8px 12px", borderRadius: 7, background: isLightMode ? "rgba(170, 131, 50, 0.12)" : "#1e1a12", border: `1px solid ${isLightMode ? "rgba(170, 131, 50, 0.34)" : "#4a3e1a"}`, color: isLightMode ? "#70531a" : "#d9c27a", fontSize: 12 }}>
                    Network is disabled. Enable Network in the <strong>Network</strong> tab to use Iris Repo Assistant.
                  </div>
                )}
                <div style={{ opacity: networkEnabled ? 1 : 0.38, pointerEvents: networkEnabled ? "auto" : "none", transition: "opacity 0.2s" }}>
                  <p style={{ marginTop: 0, fontSize: 12, color: isLightMode ? "#455566" : "#b9b9b9" }}>
                    Ask Iris which repositories to add, or paste a Git URL and ask it to install into a selected repository folder.
                  </p>
                  <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", marginBottom: 8 }}>
                    <span style={{ color: "var(--app-text)", fontSize: 12 }}>Install target</span>
                    <select
                      value={repoAssistantTargetRepoId}
                      onChange={(e) => setRepoAssistantTargetRepoId(e.target.value)}
                      style={{ minWidth: 220, padding: "6px 8px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                    >
                      <option value="">No target selected</option>
                      {repoStore.repos.map((repo) => (
                        <option key={repo.id} value={repo.id}>{repo.name}</option>
                      ))}
                    </select>
                  </div>
                  <div style={{ border: "1px solid var(--app-border)", borderRadius: 8, maxHeight: 170, overflowY: "auto", padding: 8, background: "var(--app-panel-bg)", marginBottom: 8 }}>
                    {repoAssistantMessages.map((m, i) => (
                      <div key={i} style={{ fontSize: 12, color: m.role === "assistant" ? (isLightMode ? "#29435e" : "#d8e7ff") : "var(--app-text)", marginBottom: 6 }}>
                        <strong>{m.role === "assistant" ? "Iris" : "You"}:</strong> {m.text}
                      </div>
                    ))}
                  </div>
                  <div style={{ display: "flex", gap: 8 }}>
                    <input
                      value={repoAssistantInput}
                      onChange={(e) => setRepoAssistantInput(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          sendRepoAssistantMessage();
                        }
                      }}
                      placeholder="Ask about repos, or paste a Git URL..."
                      style={{ flex: 1, padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                    />
                    <button className={`setup-btn${repoAssistantBusy ? "" : " primary"}`} onClick={sendRepoAssistantMessage} disabled={repoAssistantBusy}>
                      {repoAssistantBusy ? "Working..." : "Send"}
                    </button>
                  </div>
                </div>
              </div>
            </div>
          )}
          {settingsTab === "Projects" && (
            <div className="llm-settings-card">
              <h3>Projects</h3>
              <p>
                Projects combine reference context from Repos with bridges and an optional direct manipulation root. Repos stay reference-only; the chosen root project folder is where Iris performs direct filesystem edits when available.
              </p>
              {(!reposContextActive || !mcpContextActive) && (
                <div style={{ marginBottom: 12, fontSize: 12, color: isLightMode ? "#74591f" : "#d9c58d", border: `1px solid ${isLightMode ? "rgba(170, 131, 50, 0.34)" : "#5a5136"}`, background: isLightMode ? "rgba(170, 131, 50, 0.10)" : "#2b2517", borderRadius: 8, padding: "8px 10px" }}>
                  Context toggles are currently limiting project tooling: Repos {reposContextActive ? "ON" : "OFF"}, MCP {mcpContextActive ? "ON" : "OFF"}.
                  Your project selections remain saved and will be used again when these toggles are turned back ON.
                </div>
              )}
              <div className="llm-settings-actions" style={{ marginBottom: 12 }}>
                <button
                  className="setup-btn primary"
                  disabled={menuLocked}
                  onClick={async () => {
                    const p: ProjectDef = {
                      id: makeId("proj"),
                      name: `Project ${repoStore.projects.length + 1}`,
                      enabled: true,
                      description: "",
                      manipulationRootPath: "",
                      datawebEnabled: true,
                      repoIds: [],
                      entryIds: [],
                      mcpIds: [],
                      sshIds: [],
                    };
                    await persistRepoStore({ ...repoStore, projects: [...repoStore.projects, p] });
                  }}
                >
                  New Project
                </button>
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 12, maxHeight: "min(72vh, 760px)", overflowY: "auto", overflowX: "hidden", paddingRight: 4 }}>
                {repoStore.projects.map((project) => {
                  const linkedRepos = repoStore.repos.filter((r) => project.repoIds.includes(r.id));
                  const availableEntries = linkedRepos.flatMap((r) => r.entries.filter((e) => r.selectedEntryIds.includes(e.id)));
                  return (
                    <div key={project.id} style={{ border: "1px solid var(--app-border)", borderRadius: 8, padding: 10, background: "var(--app-panel-bg)", width: "100%", boxSizing: "border-box", minWidth: 0 }}>
                      <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8, flexWrap: "wrap" }}>
                        <input
                          value={project.name}
                          onChange={async (e) => {
                            const name = e.target.value;
                            await persistRepoStore({
                              ...repoStore,
                              projects: repoStore.projects.map((p) => p.id === project.id ? { ...p, name } : p),
                            });
                          }}
                          style={{ minWidth: 210, padding: "6px 8px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                        />
                        <label style={{ fontSize: 12, color: "var(--app-text)" }}>
                          <input
                            type="checkbox"
                            checked={project.enabled}
                            onChange={async (e) => {
                              const enabled = e.target.checked;
                              await persistRepoStore({
                                ...repoStore,
                                projects: repoStore.projects.map((p) => p.id === project.id ? { ...p, enabled } : p),
                              });
                            }}
                            style={{ marginRight: 6 }}
                          />
                          Enabled
                        </label>
                        <label style={{ fontSize: 12, color: "var(--app-text)", border: "1px solid var(--app-border)", borderRadius: 6, padding: "4px 8px" }}>
                          <input
                            type="checkbox"
                            checked={project.datawebEnabled !== false}
                            onChange={async (e) => {
                              const datawebEnabled = e.target.checked;
                              await persistRepoStore({
                                ...repoStore,
                                projects: repoStore.projects.map((p) => p.id === project.id ? { ...p, datawebEnabled } : p),
                              });
                            }}
                            style={{ marginRight: 6 }}
                          />
                          Project dataweb ON
                        </label>
                        <button
                          className="setup-btn"
                          onClick={async () => {
                            try {
                              await invoke("open_project_dataweb", { projectId: project.id });
                            } catch (e: any) {
                              alert("Failed to open project dataweb: " + (e?.message || String(e)));
                            }
                          }}
                        >
                          Open Dataweb
                        </button>
                        <button
                          className="setup-btn"
                          onClick={async () => {
                            const proceed = await askCenteredConfirm({
                              title: "Remove Project",
                              message: `Remove project \"${project.name}\" from settings?`,
                              confirmLabel: "Remove",
                              cancelLabel: "Cancel",
                            });
                            if (!proceed) return;
                            const nextProjects = repoStore.projects.filter((p) => p.id !== project.id);
                            const nextMap = { ...tabProjectMap };
                            for (const [k, v] of Object.entries(nextMap)) {
                              if (v === project.id) nextMap[Number(k)] = null;
                            }
                            setTabProjectMap(nextMap);
                            await persistRepoStore({ ...repoStore, projects: nextProjects });
                          }}
                        >
                          Remove
                        </button>
                      </div>
                      <div style={{ marginBottom: 8 }}>
                        <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Description</label>
                        <textarea
                          value={projectDescriptionDrafts[project.id] ?? project.description}
                          onChange={(e) => {
                            const draft = e.target.value;
                            setProjectDescriptionDrafts((prev) => ({ ...prev, [project.id]: draft }));
                            setProjectDescriptionDirty((prev) => ({ ...prev, [project.id]: true }));
                            queueProjectDraftAutosave(project.id);
                          }}
                          rows={3}
                          style={{ width: "100%", boxSizing: "border-box", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)", padding: 8 }}
                        />
                        <div style={{ marginTop: 8 }}>
                          <button
                            className={`setup-btn${projectDescriptionDirty[project.id] ? " primary" : ""}`}
                            disabled={!projectDescriptionDirty[project.id]}
                            onClick={async () => {
                              await saveProjectDescription(project.id);
                            }}
                          >
                            {projectDescriptionDirty[project.id] ? "Save Project" : "Saved"}
                          </button>
                        </div>
                      </div>

                      <div style={{ marginBottom: 8 }}>
                        <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Manipulation root folder</label>
                        <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
                          <input
                            value={projectManipulationRootDrafts[project.id] ?? project.manipulationRootPath ?? ""}
                            onChange={(e) => {
                              const draft = e.target.value;
                              setProjectManipulationRootDrafts((prev) => ({ ...prev, [project.id]: draft }));
                              setProjectDescriptionDirty((prev) => ({ ...prev, [project.id]: true }));
                              queueProjectDraftAutosave(project.id);
                            }}
                            placeholder="Choose the root project folder for Iris to edit directly"
                            style={{ flex: 1, minWidth: 280, padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                          />
                          <button
                            className="setup-btn"
                            onClick={async () => {
                              try {
                                const picked = await invoke("pick_repo_folder") as string | null;
                                if (!picked) return;
                                setProjectManipulationRootDrafts((prev) => ({ ...prev, [project.id]: picked }));
                                setProjectDescriptionDirty((prev) => ({
                                  ...prev,
                                  [project.id]: picked.trim() !== (project.manipulationRootPath || "").trim() || (projectDescriptionDrafts[project.id] ?? project.description ?? "").trim() !== (project.description || "").trim(),
                                }));
                              } catch (e: any) {
                                alert("Failed to pick manipulation root: " + (e?.message || String(e)));
                              }
                            }}
                          >
                            Pick Folder
                          </button>
                          <button
                            className="setup-btn"
                            onClick={() => {
                              setProjectManipulationRootDrafts((prev) => ({ ...prev, [project.id]: "" }));
                              setProjectDescriptionDirty((prev) => ({
                                ...prev,
                                [project.id]: (projectDescriptionDrafts[project.id] ?? project.description ?? "").trim() !== (project.description || "").trim() || !!(project.manipulationRootPath || "").trim(),
                              }));
                            }}
                          >
                            Clear
                          </button>
                        </div>
                        <div style={{ marginTop: 6, fontSize: 12, color: isLightMode ? "#4b627a" : "#9fb2c9" }}>
                          When both a manipulation root and MCP are available, Iris should usually prefer direct filesystem work here and reserve MCP for engine-specific actions.
                        </div>
                      </div>

                      <div style={{ marginBottom: 8 }}>
                        <div style={{ fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Associated repositories</div>
                        <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
                          {repoStore.repos.map((repo) => (
                            <label key={repo.id} style={{ fontSize: 12, color: "var(--app-text)", border: "1px solid var(--app-border)", borderRadius: 6, padding: "4px 8px", background: isLightMode ? "rgba(255,255,255,0.12)" : "transparent" }}>
                              <input
                                type="checkbox"
                                checked={project.repoIds.includes(repo.id)}
                                onChange={async (e) => {
                                  const checked = e.target.checked;
                                  const repoIds = checked
                                    ? [...project.repoIds, repo.id]
                                    : project.repoIds.filter((id) => id !== repo.id);
                                  const allowed = new Set(
                                    repoStore.repos
                                      .filter((r) => repoIds.includes(r.id))
                                      .flatMap((r) => r.selectedEntryIds)
                                  );
                                  const entryIds = project.entryIds.filter((id) => allowed.has(id));
                                  await persistRepoStore({
                                    ...repoStore,
                                    projects: repoStore.projects.map((p) => p.id === project.id ? { ...p, repoIds, entryIds } : p),
                                  });
                                }}
                                style={{ marginRight: 6 }}
                              />
                              {repo.name}
                            </label>
                          ))}
                        </div>
                      </div>

                      {llmCapabilities.mcpAvailable ? (
                        <div style={{ marginBottom: 8 }}>
                          <div style={{ fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>
                            Associated MCP connections {llmCapabilities.multiMcpEnabled ? "(multi-tool profile mode)" : "(single-tool profile mode)"}
                          </div>
                          <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
                            {repoStore.mcps.map((mcp) => (
                              <label key={mcp.id} style={{ fontSize: 12, color: "var(--app-text)", border: "1px solid var(--app-border)", borderRadius: 6, padding: "4px 8px", background: isLightMode ? "rgba(255,255,255,0.12)" : "transparent" }}>
                                <input
                                  type="checkbox"
                                  checked={project.mcpIds?.includes(mcp.id)}
                                  onChange={async (e) => {
                                    const checked = e.target.checked;
                                    let mcpIds = checked
                                      ? Array.from(new Set([...(project.mcpIds || []), mcp.id]))
                                      : (project.mcpIds || []).filter((id) => id !== mcp.id);
                                    if (!llmCapabilities.multiMcpEnabled && mcpIds.length > 1) {
                                      mcpIds = [mcp.id];
                                    }
                                    await persistRepoStore({
                                      ...repoStore,
                                      projects: repoStore.projects.map((p) => p.id === project.id ? { ...p, mcpIds } : p),
                                    });
                                  }}
                                  style={{ marginRight: 6 }}
                                />
                                {mcp.name}
                              </label>
                            ))}
                            {!repoStore.mcps.length && (
                              <div style={{ fontSize: 12, color: "#9d9d9d" }}>No MCP connections configured yet.</div>
                            )}
                          </div>
                        </div>
                      ) : (
                        <div style={{ marginBottom: 8, fontSize: 12, color: "#9d9d9d" }}>
                          MCP linking is unavailable for this profile mode.
                        </div>
                      )}

                      <div style={{ marginBottom: 8 }}>
                        <div style={{ fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>
                          Associated SSH connections
                        </div>
                        <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
                          {repoStore.sshs.map((ssh) => (
                            <label key={ssh.id} style={{ fontSize: 12, color: "var(--app-text)", border: "1px solid var(--app-border)", borderRadius: 6, padding: "4px 8px", background: isLightMode ? "rgba(255,255,255,0.12)" : "transparent" }}>
                              <input
                                type="checkbox"
                                checked={project.sshIds?.includes(ssh.id)}
                                onChange={async (e) => {
                                  const checked = e.target.checked;
                                  const sshIds = checked
                                    ? Array.from(new Set([...(project.sshIds || []), ssh.id]))
                                    : (project.sshIds || []).filter((id) => id !== ssh.id);
                                  await persistRepoStore({
                                    ...repoStore,
                                    projects: repoStore.projects.map((p) => p.id === project.id ? { ...p, sshIds } : p),
                                  });
                                }}
                                style={{ marginRight: 6 }}
                              />
                              {ssh.name}
                            </label>
                          ))}
                          {!repoStore.sshs.length && (
                            <div style={{ fontSize: 12, color: "#9d9d9d" }}>No SSH connections configured yet.</div>
                          )}
                        </div>
                      </div>

                      <div>
                        <div style={{ fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Referenced files/folders</div>
                        <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", marginBottom: 8 }}>
                          <button
                            className="setup-btn"
                            onClick={async () => {
                              await handleRefreshProject(project);
                            }}
                          >
                            Refresh
                          </button>
                          <button
                            className="setup-btn"
                            onClick={async () => {
                              await handleAutoSelectProjectEntries(project, availableEntries);
                            }}
                            disabled={projectAutoSelectingId === project.id}
                          >
                            {projectAutoSelectingId === project.id ? "Automatic..." : "Automatic"}
                          </button>
                          <button
                            className="setup-btn"
                            onClick={async () => {
                              await updateProjectEntrySelection(project.id, availableEntries.map((e) => e.id));
                              setProjectAutoStatus((prev) => ({ ...prev, [project.id]: `All selected: ${availableEntries.length}` }));
                            }}
                          >
                            All
                          </button>
                          <button
                            className="setup-btn"
                            onClick={async () => {
                              await updateProjectEntrySelection(project.id, []);
                              setProjectAutoStatus((prev) => ({ ...prev, [project.id]: "None selected" }));
                            }}
                          >
                            None
                          </button>
                          {!!projectAutoStatus[project.id] && (
                            <span style={{ fontSize: 12, color: "#8fb2ff" }}>{projectAutoStatus[project.id]}</span>
                          )}
                        </div>
                        <div style={{ maxHeight: 260, overflowY: "auto", border: "1px solid #313131", borderRadius: 6, padding: 6 }}>
                          {linkedRepos.map((repo) => {
                            const repoEntries = repo.entries.filter((e) => repo.selectedEntryIds.includes(e.id));
                            if (!repoEntries.length) return null;
                            const repoTree = buildRepoTree({ ...repo, entries: repoEntries });
                            return (
                              <div key={`${project.id}_${repo.id}`} style={{ marginBottom: 8 }}>
                                <div style={{ fontSize: 12, color: isLightMode ? "#495a6a" : "#a4a4a4", marginBottom: 4 }}>{repo.name}</div>
                                {repoTree.map((node) => {
                                  const renderProjectNode = (n: RepoTreeNode, depth: number) => {
                                    const expanded = n.isDir ? isProjectNodeExpanded(project.id, repo.id, n.path, depth) : false;
                                    const isChecked = !!n.entryId && project.entryIds.includes(n.entryId);
                                    return (
                                      <div key={`${project.id}_${repo.id}_${n.id}_${depth}`}>
                                        <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, color: "var(--app-text)", padding: "3px 0", paddingLeft: depth * 14 }}>
                                          {n.isDir ? (
                                            <button
                                              className="setup-btn"
                                              style={{ padding: "1px 8px", minWidth: 26 }}
                                              onClick={() => toggleProjectNodeExpanded(project.id, repo.id, n.path)}
                                            >
                                              {expanded ? "-" : "+"}
                                            </button>
                                          ) : (
                                            <span style={{ width: 26 }} />
                                          )}
                                          <input
                                            type="checkbox"
                                            checked={isChecked}
                                            disabled={!n.entryId}
                                            onChange={async (e) => {
                                              if (!n.entryId) return;
                                              const checked = e.target.checked;
                                              const entryIds = checked
                                                ? Array.from(new Set([...project.entryIds, n.entryId]))
                                                : project.entryIds.filter((id) => id !== n.entryId);
                                              await updateProjectEntrySelection(project.id, entryIds);
                                            }}
                                          />
                                          <span style={{ minWidth: 40, color: "#9db8ff" }}>{n.isDir ? "DIR" : "FILE"}</span>
                                          <span style={{ flex: 1 }}>{n.name}</span>
                                        </div>
                                        {n.isDir && expanded && n.children.map((c) => renderProjectNode(c, depth + 1))}
                                      </div>
                                    );
                                  };
                                  return renderProjectNode(node, 0);
                                })}
                              </div>
                            );
                          })}
                          {!availableEntries.length && (
                            <div style={{ fontSize: 12, color: "#9d9d9d" }}>Select repos in Repos tab and attach them above to expose entries here.</div>
                          )}
                        </div>
                      </div>
                    </div>
                  );
                })}
                {!repoStore.projects.length && <div style={{ color: "#aaa", fontSize: 13 }}>No projects yet.</div>}
              </div>
            </div>
          )}
          {settingsTab === "Bridges" && (
            <div className="llm-settings-card">
              <h3>Bridge Connections</h3>
              <p>
                Configure external MCP and SSH endpoints Iris can use. Projects can opt into one or more bridge connections.
              </p>
              <div style={{ marginBottom: 12, border: "1px solid var(--app-border)", background: isLightMode ? "rgba(86, 112, 138, 0.14)" : "#17202a", borderRadius: 8, padding: "9px 10px" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
                  <button
                    className={`setup-btn${mcpContextActive ? " primary" : ""}`}
                    onClick={async () => {
                      const next = !mcpEnabled;
                      setMcpEnabled(next);
                      await invoke("set_setup_flags", { args: { mcpEnabled: next } });
                    }}
                  >
                    Bridge Context: {mcpContextActive ? "ON" : "OFF"}
                  </button>
                  <span style={{ fontSize: 12, color: isLightMode ? "#2f4359" : "#cfd9e8" }}>
                    Toggle whether Iris should regard or disregard your configured MCP connections while planning/executing.
                  </span>
                </div>
                <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap", marginTop: 8 }}>
                  <button
                    className={`setup-btn${desktopToolsEnabled ? " primary" : ""}`}
                    onClick={async () => {
                      const next = !desktopToolsEnabled;
                      setDesktopToolsEnabled(next);
                      await invoke("set_setup_flags", { args: { desktopToolsEnabled: next } });
                    }}
                  >
                    Desktop Tools: {desktopToolsEnabled ? "ON" : "OFF"}
                  </button>
                  <span style={{ fontSize: 12, color: isLightMode ? "#2f4359" : "#cfd9e8" }}>
                    Allows Iris to inspect open windows and capture screenshots for vision-enhanced assistance, alongside SSH-style read/manipulate workflows in project roots.
                  </span>
                </div>
                <div style={{ marginTop: 6, fontSize: 12, color: isLightMode ? "#4b627a" : "#9fb2c9" }}>
                  MCP OFF reduces tool orchestration overhead; Desktop Tools OFF prevents any automatic window/screenshot routine.
                </div>
              </div>
              <div className="llm-settings-actions" style={{ marginBottom: 12 }}>
                <button
                  className="setup-btn primary"
                  disabled={menuLocked}
                  onClick={async () => {
                    const next = {
                      ...repoStore,
                      mcps: [
                        ...repoStore.mcps,
                        { id: makeId("mcp"), name: `MCP ${repoStore.mcps.length + 1}`, target: "", enabled: true, notes: "" },
                      ],
                    };
                    await persistRepoStore(next);
                  }}
                >
                  Add MCP
                </button>
                <button
                  className="setup-btn primary"
                  disabled={menuLocked}
                  onClick={async () => {
                    const next = {
                      ...repoStore,
                      sshs: [
                        ...repoStore.sshs,
                        {
                          id: makeId("ssh"),
                          name: `SSH ${repoStore.sshs.length + 1}`,
                          host: "",
                          port: 22,
                          username: "",
                          privateKeyPath: "",
                          knownHostsPath: "",
                          remoteRoot: "",
                          strictHostKeyChecking: true,
                          extraArgs: [],
                          enabled: true,
                          notes: "",
                        },
                      ],
                    };
                    await persistRepoStore(next);
                  }}
                >
                  Add SSH
                </button>
                <button
                  className={`setup-btn${(mcpDirty || sshDirty) ? " primary" : ""}`}
                  disabled={!(mcpDirty || sshDirty)}
                  onClick={saveBridgeSettings}
                  title="Persist MCP and SSH draft edits"
                >
                  {(mcpDirty || sshDirty) ? "Save Bridges" : "Saved"}
                </button>
              </div>

              <div style={{ marginBottom: 10, fontSize: 12, color: isLightMode ? "#4b627a" : "#9fb2c9" }}>
                Target supports standard MCP formats: URL (`http://...`, `ws://...`) or stdio command (`uv run ...`). You can also paste JSON with `command/args`.
                Iris can infer SSH/Desktop/Filesystem/Project actions from conversational requests; slash commands remain optional for power users.
              </div>

              <div style={{ display: "flex", flexDirection: "column", gap: 12, maxHeight: "min(72vh, 760px)", overflowY: "auto", overflowX: "hidden", paddingRight: 4 }}>
                {(Array.isArray(mcpDraft) ? mcpDraft : []).map((mcp) => (
                  <div key={mcp.id} style={{ border: "1px solid var(--app-border)", borderRadius: 8, padding: 10, background: "var(--app-panel-bg)" }}>
                    <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8, flexWrap: "wrap" }}>
                      <input
                        value={mcp.name}
                        onChange={(e) => {
                          const name = e.target.value;
                          setMcpDraft(prev => prev.map((m) => m.id === mcp.id ? { ...m, name } : m));
                        }}
                        style={{ minWidth: 240, padding: "6px 8px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                      />
                      <label style={{ fontSize: 12, color: "var(--app-text)" }}>
                        <input
                          type="checkbox"
                          checked={mcp.enabled}
                          onChange={(e) => {
                            const enabled = e.target.checked;
                            setMcpDraft(prev => prev.map((m) => m.id === mcp.id ? { ...m, enabled } : m));
                          }}
                          style={{ marginRight: 6 }}
                        />
                        Enabled
                      </label>
                      {parseMcpTarget(mcp.target).type === "stdio" && (
                        <button
                          className="setup-btn"
                          onClick={() => toggleMcpServer(mcp)}
                          title={mcpLaunchStatus[mcp.id]?.launched ? "Stop launched bridge process" : "Launch MCP server process from target command"}
                        >
                          {mcpLaunchStatus[mcp.id]?.launched ? "Stop" : "Launch"}
                        </button>
                      )}
                      <button
                        className="setup-btn"
                        onClick={async () => {
                          const proceed = await askCenteredConfirm({
                            title: "Remove MCP",
                            message: `Remove MCP connection \"${mcp.name}\" from settings?`,
                            confirmLabel: "Remove",
                            cancelLabel: "Cancel",
                          });
                          if (!proceed) return;
                          const nextMcps = repoStore.mcps.filter((m) => m.id !== mcp.id);
                          const nextProjects = repoStore.projects.map((p) => ({
                            ...p,
                            mcpIds: (p.mcpIds || []).filter((id) => id !== mcp.id),
                          }));
                          await persistRepoStore({ ...repoStore, mcps: nextMcps, projects: nextProjects });
                          setMcpDraft(prev => prev.filter((m) => m.id !== mcp.id));
                        }}
                      >
                        Remove
                      </button>
                    </div>

                    {mcpLaunchStatus[mcp.id]?.launched && (
                      <div style={{ marginBottom: 8, fontSize: 12, color: isLightMode ? "#2f5f3b" : "#9be7b1" }}>
                        Running bridge process (PID {mcpLaunchStatus[mcp.id].pid})
                      </div>
                    )}
                    {mcpHealthStatus[mcp.id]?.message && mcpHealthStatus[mcp.id]?.state !== "error" && (
                      <div style={{ marginBottom: 8, fontSize: 12, color: isLightMode ? "#2f5f3b" : "#9be7b1" }}>
                        MCP: {mcpHealthStatus[mcp.id].message}
                      </div>
                    )}
                    {mcpHealthStatus[mcp.id]?.state === "error" && (
                      <div style={{ marginBottom: 8, fontSize: 12, color: isLightMode ? "#7e2a2a" : "#ff9f9f" }}>
                        MCP connection check failed: {mcpHealthStatus[mcp.id]?.message || "unknown error"}
                      </div>
                    )}

                    <div style={{ marginBottom: 8 }}>
                      <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Target</label>
                      <input
                        value={mcp.target}
                        placeholder="e.g. http://127.0.0.1:3000/mcp OR uv run D:/path/server.py OR JSON with mcpServers"
                        onChange={(e) => {
                          const target = e.target.value;
                          setMcpDraft(prev => prev.map((m) => m.id === mcp.id ? { ...m, target } : m));
                        }}
                        style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                      />
                      <div style={{ marginTop: 4, fontSize: 11, color: isLightMode ? "#4b627a" : "#9fb2c9" }}>
                        Detected format: {parseMcpTarget(mcp.target).type === "stdio" ? "Stdio command" : "Network URL"}
                      </div>
                    </div>

                    <div>
                      <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Notes</label>
                      <textarea
                        rows={2}
                        value={mcp.notes || ""}
                        onChange={(e) => {
                          const notes = e.target.value;
                          setMcpDraft(prev => prev.map((m) => m.id === mcp.id ? { ...m, notes } : m));
                        }}
                        style={{ width: "100%", boxSizing: "border-box", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)", padding: 8 }}
                      />
                    </div>
                  </div>
                ))}
                {(!Array.isArray(mcpDraft) || !mcpDraft.length) && (
                  <div style={{ color: "#aaa", fontSize: 13 }}>No MCP connections configured yet.</div>
                )}

                {(Array.isArray(sshDraft) ? sshDraft : []).map((ssh) => (
                  <div key={ssh.id} style={{ border: "1px solid var(--app-border)", borderRadius: 8, padding: 10, background: "var(--app-panel-bg)" }}>
                    <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8, flexWrap: "wrap" }}>
                      <input
                        value={ssh.name}
                        onChange={(e) => {
                          const name = e.target.value;
                          setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, name } : s));
                        }}
                        style={{ minWidth: 240, padding: "6px 8px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                      />
                      <label style={{ fontSize: 12, color: "var(--app-text)" }}>
                        <input
                          type="checkbox"
                          checked={ssh.enabled}
                          onChange={(e) => {
                            const enabled = e.target.checked;
                            setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, enabled } : s));
                          }}
                          style={{ marginRight: 6 }}
                        />
                        Enabled
                      </label>
                      <button
                        className="setup-btn"
                        onClick={async () => {
                          const proceed = await askCenteredConfirm({
                            title: "Remove SSH",
                            message: `Remove SSH connection "${ssh.name}" from settings?`,
                            confirmLabel: "Remove",
                            cancelLabel: "Cancel",
                          });
                          if (!proceed) return;
                          const nextSsh = repoStore.sshs.filter((s) => s.id !== ssh.id);
                          const nextProjects = repoStore.projects.map((p) => ({
                            ...p,
                            sshIds: (p.sshIds || []).filter((id) => id !== ssh.id),
                          }));
                          await persistRepoStore({ ...repoStore, sshs: nextSsh, projects: nextProjects });
                          setSshDraft(prev => prev.filter((s) => s.id !== ssh.id));
                        }}
                      >
                        Remove
                      </button>
                    </div>

                    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))", gap: 8, marginBottom: 8 }}>
                      <div>
                        <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Host</label>
                        <input
                          value={ssh.host}
                          placeholder="e.g. 192.168.1.20"
                          onChange={(e) => setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, host: e.target.value } : s))}
                          style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                        />
                      </div>
                      <div>
                        <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Port</label>
                        <input
                          value={String(ssh.port || 22)}
                          onChange={(e) => setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, port: Number(e.target.value || 22) || 22 } : s))}
                          style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                        />
                      </div>
                      <div>
                        <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Username</label>
                        <input
                          value={ssh.username}
                          placeholder="e.g. pi"
                          onChange={(e) => setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, username: e.target.value } : s))}
                          style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                        />
                      </div>
                      <div>
                        <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Remote root</label>
                        <input
                          value={ssh.remoteRoot}
                          placeholder="e.g. /home/pi/project"
                          onChange={(e) => setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, remoteRoot: e.target.value } : s))}
                          style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                        />
                      </div>
                    </div>

                    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(260px, 1fr))", gap: 8, marginBottom: 8 }}>
                      <div>
                        <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Private key path (optional)</label>
                        <input
                          value={ssh.privateKeyPath}
                          placeholder="e.g. C:/Users/you/.ssh/id_ed25519"
                          onChange={(e) => setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, privateKeyPath: e.target.value } : s))}
                          style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                        />
                      </div>
                      <div>
                        <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Known hosts path (optional)</label>
                        <input
                          value={ssh.knownHostsPath}
                          placeholder="e.g. C:/Users/you/.ssh/known_hosts"
                          onChange={(e) => setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, knownHostsPath: e.target.value } : s))}
                          style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                        />
                      </div>
                    </div>

                    <div style={{ marginBottom: 8 }}>
                      <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Extra SSH args (optional)</label>
                      <input
                        value={Array.isArray(ssh.extraArgs) ? ssh.extraArgs.join(" ") : ""}
                        placeholder="e.g. -o ConnectTimeout=8"
                        onChange={(e) => {
                          const extraArgs = e.target.value.split(/\s+/).map((s) => s.trim()).filter(Boolean);
                          setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, extraArgs } : s));
                        }}
                        style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                      />
                    </div>

                    <div style={{ marginBottom: 8 }}>
                      <label style={{ fontSize: 12, color: "var(--app-text)" }}>
                        <input
                          type="checkbox"
                          checked={ssh.strictHostKeyChecking}
                          onChange={(e) => setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, strictHostKeyChecking: e.target.checked } : s))}
                          style={{ marginRight: 6 }}
                        />
                        Strict host key checking
                      </label>
                    </div>

                    <div>
                      <label style={{ display: "block", fontSize: 12, color: "var(--app-text)", marginBottom: 4 }}>Notes</label>
                      <textarea
                        rows={2}
                        value={ssh.notes || ""}
                        onChange={(e) => {
                          const notes = e.target.value;
                          setSshDraft(prev => prev.map((s) => s.id === ssh.id ? { ...s, notes } : s));
                        }}
                        style={{ width: "100%", boxSizing: "border-box", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)", padding: 8 }}
                      />
                    </div>
                  </div>
                ))}
                {(!Array.isArray(sshDraft) || !sshDraft.length) && (
                  <div style={{ color: "#aaa", fontSize: 13 }}>No SSH connections configured yet.</div>
                )}
              </div>
            </div>
          )}
          {settingsTab === "Network" && (
            <div className="llm-settings-card">
              <h3>Network</h3>
              <p style={{ color: isLightMode ? "#7a531b" : "#ffd9a3", fontWeight: 700 }}>
                Privacy Warning: enabling network features allows Iris to connect to internet services. This app is offline-first and does not require network access.
              </p>
              <p>
                Network assist can improve time-accurate and broad information (for example, recent updates/news), but introduces normal internet risks like
                metadata leakage to third-party providers and potential exposure to untrusted online content.
              </p>
              <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 12 }}>
                <input
                  id="network-enabled"
                  type="checkbox"
                  checked={networkDraftEnabled}
                  onChange={(e) => setNetworkDraftEnabled(e.target.checked)}
                />
                <label htmlFor="network-enabled" style={{ color: "var(--app-text)" }}>Enable network-assisted responses</label>
              </div>
              <div className="llm-settings-actions">
                <button
                  className={`setup-btn${networkDraftEnabled !== networkEnabled ? " primary" : ""}`}
                  onClick={saveNetworkSettings}
                  disabled={networkDraftEnabled === networkEnabled}
                >
                  {networkDraftEnabled !== networkEnabled ? "Save Network Setting" : "Saved"}
                </button>
              </div>
              <div style={{ marginTop: 10, fontSize: 12, color: isLightMode ? "#4c6074" : "#b1b1b1", lineHeight: 1.45 }}>
                Behavior: Iris checks local project context first. For time-sensitive or internet-only topics, Iris may run a lightweight network lookup and
                blend those hints into its response.
              </div>

              <div style={{ marginTop: 18, borderTop: "1px solid var(--app-border)", paddingTop: 14 }}>
                <h4 style={{ margin: "0 0 8px", color: "var(--app-text)" }}>Manual App Updates</h4>
                <p style={{ marginTop: 0, fontSize: 12, color: isLightMode ? "#4c6074" : "#b1b1b1", lineHeight: 1.45 }}>
                  Host your installer on your Google Site, then paste those links here. Users can install/update by downloading the latest installer manually.
                </p>

                <div style={{ marginBottom: 10, border: "1px solid var(--app-border)", borderRadius: 8, padding: 10 }}>
                  <div style={{ marginBottom: 6, fontSize: 12, color: "var(--app-text)", fontWeight: 700 }}>GitHub release source</div>
                  <div style={{ marginBottom: 8 }}>
                    <label style={{ display: "block", marginBottom: 4, fontSize: 12, color: "var(--app-text)" }}>GitHub repo (owner/repo or release URL)</label>
                    <input
                      value={githubReleaseRepo}
                      onChange={(e) => setGithubReleaseRepo(e.target.value)}
                      placeholder="KennyCrowPixels/Iris_for_Godot"
                      style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                    />
                  </div>
                  <div style={{ display: "flex", gap: 14, flexWrap: "wrap", marginBottom: 8 }}>
                    <label style={{ fontSize: 12, color: "var(--app-text)", display: "flex", alignItems: "center", gap: 6 }}>
                      <input type="checkbox" checked={preferMsiInstaller} onChange={(e) => setPreferMsiInstaller(e.target.checked)} />
                      Prefer MSI (enterprise)
                    </label>
                    <label style={{ fontSize: 12, color: "var(--app-text)", display: "flex", alignItems: "center", gap: 6 }}>
                      <input type="checkbox" checked={includePrereleaseInstaller} onChange={(e) => setIncludePrereleaseInstaller(e.target.checked)} />
                      Include pre-releases
                    </label>
                  </div>
                  <button className="setup-btn" onClick={() => void resolveGithubReleaseInstaller()}>
                    Resolve Latest Installer From GitHub
                  </button>
                </div>

                <div style={{ marginBottom: 10 }}>
                  <label style={{ display: "block", marginBottom: 4, fontSize: 12, color: "var(--app-text)" }}>Installer download URL</label>
                  <input
                    value={manualDownloadUrl}
                    onChange={(e) => setManualDownloadUrl(e.target.value)}
                    placeholder="https://sites.google.com/.../iris-installer.exe"
                    style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                  />
                </div>

                <div style={{ marginBottom: 10 }}>
                  <label style={{ display: "block", marginBottom: 4, fontSize: 12, color: "var(--app-text)" }}>Release notes URL (optional)</label>
                  <input
                    value={releaseNotesUrl}
                    onChange={(e) => setReleaseNotesUrl(e.target.value)}
                    placeholder="https://sites.google.com/.../iris-release-notes"
                    style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                  />
                </div>

                <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                  <button className="setup-btn primary" onClick={saveUpdateSettings}>Save Update Links</button>
                  <button className="setup-btn" disabled={!isValidHttpUrl(manualDownloadUrl)} onClick={() => void openExternalUpdateUrl(manualDownloadUrl)}>
                    Open Installer Download
                  </button>
                  <button className="setup-btn" disabled={!isValidHttpUrl(releaseNotesUrl)} onClick={() => void openExternalUpdateUrl(releaseNotesUrl)}>
                    Open Release Notes
                  </button>
                </div>
              </div>

              <div style={{ marginTop: 18, borderTop: "1px solid var(--app-border)", paddingTop: 14, opacity: 0.62 }}>
                <h4 style={{ margin: "0 0 8px", color: "var(--app-text)" }}>Automatic Updates (Coming Soon)</h4>
                <p style={{ marginTop: 0, fontSize: 12, color: isLightMode ? "#4c6074" : "#b1b1b1", lineHeight: 1.45 }}>
                  Planned feature: Iris checks an update feed and downloads the newest installer automatically.
                </p>
                <div style={{ marginBottom: 10 }}>
                  <label style={{ display: "block", marginBottom: 4, fontSize: 12, color: "var(--app-text)" }}>Update feed URL (reserved)</label>
                  <input
                    value={updateFeedUrl}
                    onChange={(e) => setUpdateFeedUrl(e.target.value)}
                    placeholder="https://your-site.example.com/iris/update-feed.json"
                    style={{ width: "100%", boxSizing: "border-box", padding: "8px 10px", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)" }}
                  />
                </div>
                <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
                  <input type="checkbox" checked={autoUpdatesEnabled} disabled readOnly />
                  <span style={{ fontSize: 12, color: "var(--app-text)" }}>Enable automatic updates (disabled until update feed is finalized)</span>
                </div>
                <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                  <button className="setup-btn" disabled>Check for Updates (Coming Soon)</button>
                  <button className="setup-btn" onClick={saveUpdateSettings}>Save Reserved Feed URL</button>
                </div>
              </div>
            </div>
          )}
          {settingsTab === "LLMs" && (
            <div className="llm-settings-card">
              <h3>Model Files</h3>
              <p>
                Choose a default performance profile, then optionally fine-tune files manually.
                Apply and Restart recreates the Iris models from the current modelfiles.
              </p>
              <div style={{ marginBottom: 12, fontSize: 12, color: isLightMode ? "#516579" : "#9fb2c9", border: `1px solid ${isLightMode ? "rgba(89, 118, 150, 0.32)" : "#344a63"}`, background: isLightMode ? "rgba(93, 128, 168, 0.10)" : "#142131", borderRadius: 8, padding: "8px 10px" }}>
                Vision functions (paste/drag image analysis) require the Vision model to be correctly created from its modelfile and enabled in Active Models.
                If Vision is disabled, image sends are blocked.
              </div>

              {/* ── Auto-Detect ── */}
              <div style={{ display: "flex", alignItems: "center", gap: 12, marginBottom: 16, flexWrap: "wrap" }}>
                <button
                  className="setup-btn primary"
                  onClick={async () => {
                    try {
                      const hw = await invoke<HardwareProfile>("detect_hardware_profile");
                      const next = hw.detectedProfile as LlmProfile;
                      setLlmProfile(next);
                      await invoke("apply_model_profile", { profile: next });
                      await invoke("set_setup_flags", { args: { modelProfile: next } });
                      const msg = `Detected ${hw.gpuName}: Recommending ${next}. ${hw.detectionNote}`;
                      setHwToast(msg);
                      if (hwToastTimerRef.current) clearTimeout(hwToastTimerRef.current);
                      hwToastTimerRef.current = setTimeout(() => setHwToast(null), 7000);
                    } catch (err: any) {
                      setHwToast("Hardware scan failed: " + (err?.message || String(err)));
                      if (hwToastTimerRef.current) clearTimeout(hwToastTimerRef.current);
                      hwToastTimerRef.current = setTimeout(() => setHwToast(null), 5000);
                    }
                  }}
                >
                  Scan Hardware &amp; Recommend
                </button>
                {hwToast && (
                  <div className="hw-toast">{hwToast}</div>
                )}
              </div>

              {/* ── Profile selector ── */}
              <div style={{ marginBottom: 18 }}>
                <label htmlFor="llm-profile" style={{ display: "block", marginBottom: 6, color: "var(--app-text)" }}>
                  Default profile <span style={{ fontSize: 11, color: "#777" }}>(manual override)</span>
                </label>
                <select
                  id="llm-profile"
                  value={llmProfile}
                  onChange={async (e) => {
                    const next = e.target.value as LlmProfile;
                    setLlmProfile(next);
                    try {
                      await invoke("apply_model_profile", { profile: next });
                      await invoke("set_setup_flags", { args: { modelProfile: next } });
                    } catch (err: any) {
                      alert("Failed to apply profile defaults: " + (err?.message || err));
                    }
                  }}
                  style={{ minWidth: 240, padding: "8px 10px", borderRadius: 6, background: "var(--app-input-bg)", color: "var(--app-text)", border: "1px solid var(--app-border)" }}
                >
                  <option value="Ultra">Ultra — VRAM ≥ 16 GB</option>
                  <option value="High">High — VRAM ≥ 12 GB</option>
                  <option value="MediumHigh">Medium-High — 8 GB VRAM + 32 GB RAM (Developer Baseline)</option>
                  <option value="Medium">Medium — 6–8 GB VRAM + 16 GB RAM</option>
                  <option value="Low">Low — VRAM ≤ 4 GB or RAM &lt; 16 GB</option>
                  <option value="Minimal">Minimal — CPU only</option>
                </select>
                <div style={{ marginTop: 8, fontSize: 12, color: isLightMode ? "#4e6175" : "#9e9e9e", lineHeight: 1.4 }}>
                  Profile presets update base models and inference parameters across organizer, coder, summarizer, and vision modelfiles
                  (context window, generation length, sampling, and repetition controls).
                  You can always override the Auto-Detect recommendation by changing this manually.
                </div>
              </div>

              {/* ── Modelfile editor ── */}
              <div style={{ marginBottom: 18, border: "1px solid rgba(122,148,187,0.35)", borderRadius: 8, padding: 12, background: "rgba(14,20,30,0.35)" }}>
                <div style={{ marginBottom: 8, color: isLightMode ? "#26384d" : "#dbe9ff", fontWeight: 700 }}>Dynamic Modelfile Editor</div>
                <div style={{ marginBottom: 10, fontSize: 12, color: isLightMode ? "#4b627a" : "#9fb2c9" }}>
                  Organizer and Summarizer are always active. Coder and Vision can be toggled. Add custom models, then set per-model note text that is appended to organizer routing instructions.
                </div>

                {modelfileLoadError && (
                  <div style={{ marginBottom: 10, fontSize: 12, color: "#ffb2b2" }}>
                    Could not load modelfiles: {modelfileLoadError}
                  </div>
                )}

                <div style={{ marginBottom: 12, border: "1px solid rgba(122,148,187,0.25)", borderRadius: 8, padding: 10 }}>
                  <div style={{ marginBottom: 8, color: "#cde0ff", fontWeight: 600, fontSize: 12 }}>Active Models</div>
                  <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))", gap: 8 }}>
                    <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <input type="checkbox" checked disabled />
                      Organizer (always active)
                    </label>
                    <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <input type="checkbox" checked disabled />
                      Summarizer (always active)
                    </label>
                    <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <input
                        type="checkbox"
                        checked={modelConfig.coderEnabled}
                        onChange={async (e) => await persistModelConfig({ ...modelConfig, coderEnabled: e.target.checked })}
                      />
                      Coder
                    </label>
                    <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <input
                        type="checkbox"
                        checked={modelConfig.visionEnabled}
                        onChange={async (e) => await persistModelConfig({ ...modelConfig, visionEnabled: e.target.checked })}
                      />
                      Vision
                    </label>
                    {modelConfig.customModels.map((m) => (
                      <label key={`active_${m.id}`} style={{ display: "flex", alignItems: "center", gap: 8 }}>
                        <input
                          type="checkbox"
                          checked={m.enabled}
                          onChange={async (e) => {
                            const nextCustom = modelConfig.customModels.map((x) => x.id === m.id ? { ...x, enabled: e.target.checked } : x);
                            await persistModelConfig({ ...modelConfig, customModels: nextCustom });
                          }}
                        />
                        {m.nickname || m.filename}
                      </label>
                    ))}
                  </div>
                </div>

                <div style={{ marginBottom: 12, border: "1px solid rgba(122,148,187,0.25)", borderRadius: 8, padding: 10 }}>
                  <div style={{ marginBottom: 8, color: "#cde0ff", fontWeight: 600, fontSize: 12 }}>Iris is... Status Verbs</div>
                  <div style={{ marginBottom: 8, fontSize: 11, color: isLightMode ? "#3f556d" : "#9fb2c9" }}>
                    Optional verbs used by the status indicator. Leave empty to use automatic defaults.
                  </div>
                  {["iris-organizer", "iris-coder", "iris-summarizer", "iris-vision"].map((modelKey) => (
                    <div key={`verb_${modelKey}`} style={{ marginBottom: 6 }}>
                      <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#c8d5ea" }}>{modelKey}</label>
                      <input
                        value={modelConfig.statusVerbs?.[modelKey] || ""}
                        onChange={(e) => {
                          const next = {
                            ...modelConfig,
                            statusVerbs: { ...modelConfig.statusVerbs, [modelKey]: e.target.value },
                          };
                          setModelConfig(next);
                        }}
                        onBlur={async () => {
                          await persistModelConfig(modelConfig);
                        }}
                        placeholder="e.g. orchestrating"
                        style={{ width: "100%", boxSizing: "border-box", padding: "7px 8px", borderRadius: 6, background: "var(--app-input-bg)", color: "var(--app-text)", border: "1px solid var(--app-border)" }}
                      />
                    </div>
                  ))}
                </div>

                <div style={{ marginBottom: 12, border: "1px solid rgba(122,148,187,0.25)", borderRadius: 8, padding: 10 }}>
                  <div style={{ marginBottom: 8, color: "#cde0ff", fontWeight: 600, fontSize: 12 }}>Routing Preview</div>
                  <div style={{ marginBottom: 8, fontSize: 11, color: isLightMode ? "#3f556d" : "#9fb2c9" }}>
                    Configured orchestration route based on enabled models. The backend planner may skip stages per request intent.
                  </div>
                  <div style={{ fontSize: 12, color: "#d8e6ff", marginBottom: 6 }}>
                    {configuredRouteSummary}
                  </div>
                  {!!plannerRouteSummary && plannerRouteSummary !== configuredRouteSummary && (
                    <div style={{ fontSize: 11, color: isLightMode ? "#3f556d" : "#9fb2c9", marginBottom: 8 }}>
                      Last planner-selected route: {plannerRouteSummary.replace(/^Planner route:\s*/i, "")}
                    </div>
                  )}
                  <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                    {configuredRouteModels.map((m, i) => (
                      <span key={`route_${m}_${i}`} style={{ border: "1px solid rgba(151,176,214,0.45)", borderRadius: 999, padding: "2px 8px", fontSize: 11, color: "#d4e4ff" }}>{m}</span>
                    ))}
                  </div>
                </div>

                <div style={{ marginBottom: 12, border: "1px solid rgba(122,148,187,0.25)", borderRadius: 8, padding: 10 }}>
                  <div style={{ marginBottom: 8, color: "#cde0ff", fontWeight: 600, fontSize: 12 }}>Organizer Dispatch Notes</div>
                  <div style={{ marginBottom: 8, fontSize: 11, color: isLightMode ? "#3f556d" : "#9fb2c9" }}>
                    Notes here are appended to organizer routing instructions before it iterates through enabled models.
                  </div>
                  {["modelfile_organizer.txt", "modelfile_coder.txt", "modelfile_summarizer.txt", "modelfile_vision.txt"].map((filename) => (
                    <div key={filename} style={{ marginBottom: 6 }}>
                      <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#c8d5ea" }}>
                        {(modelfileDatas[filename]?.displayName || filename)} note
                      </label>
                      <input
                        value={modelConfig.modelNotes[filename] || ""}
                        onChange={async (e) => {
                          const next = {
                            ...modelConfig,
                            modelNotes: { ...modelConfig.modelNotes, [filename]: e.target.value },
                          };
                          setModelConfig(next);
                        }}
                        onBlur={async () => {
                          await persistModelConfig(modelConfig);
                        }}
                        style={{ width: "100%", boxSizing: "border-box", padding: "7px 8px", borderRadius: 6, background: "var(--app-input-bg)", color: "var(--app-text)", border: "1px solid var(--app-border)" }}
                      />
                    </div>
                  ))}
                </div>

                <div style={{ marginBottom: 12, border: "1px solid rgba(122,148,187,0.25)", borderRadius: 8, padding: 10 }}>
                  <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 8 }}>
                    <div style={{ color: "#cde0ff", fontWeight: 600, fontSize: 12 }}>Custom Models</div>
                    <button className="setup-btn" onClick={addCustomModel}>Add Model</button>
                  </div>
                  {modelConfig.customModels.length === 0 && (
                    <div style={{ fontSize: 12, color: isLightMode ? "#3f556d" : "#9fb2c9" }}>No custom models yet.</div>
                  )}
                  {modelConfig.customModels.map((m) => (
                    <div key={m.id} style={{ border: "1px solid rgba(122,148,187,0.25)", borderRadius: 6, padding: 8, marginBottom: 8 }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                        <label style={{ display: "flex", alignItems: "center", gap: 6 }}>
                          <input
                            type="checkbox"
                            checked={m.enabled}
                            onChange={async (e) => {
                              const nextCustom = modelConfig.customModels.map((x) => x.id === m.id ? { ...x, enabled: e.target.checked } : x);
                              await persistModelConfig({ ...modelConfig, customModels: nextCustom });
                            }}
                          />
                          Enabled
                        </label>
                        <span style={{ fontWeight: 600 }}>{m.nickname || m.filename}</span>
                        <button className="setup-btn" onClick={async () => removeCustomModel(m.id)}>Remove</button>
                      </div>
                      <div style={{ marginBottom: 6, fontSize: 12, color: "#a6bbdc" }}>{m.filename}</div>
                      <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#c8d5ea" }}>Organizer note for this model</label>
                      <input
                        value={m.note || ""}
                        onChange={(e) => {
                          const nextCustom = modelConfig.customModels.map((x) => x.id === m.id ? { ...x, note: e.target.value } : x);
                          setModelConfig({ ...modelConfig, customModels: nextCustom });
                        }}
                        onBlur={async () => {
                          await persistModelConfig(modelConfig);
                        }}
                        style={{ width: "100%", boxSizing: "border-box", padding: "7px 8px", borderRadius: 6, background: "var(--app-input-bg)", color: "var(--app-text)", border: "1px solid var(--app-border)", marginBottom: 6 }}
                      />
                    </div>
                  ))}
                </div>

                <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 10 }}>
                  {modelfileFilenames.map((filename) => {
                    const item = modelfileDatas[filename];
                    return (
                      <button
                        key={filename}
                        className="setup-btn"
                        style={{
                          opacity: modelfileSubTab === filename ? 1 : 0.85,
                          borderColor: modelfileSubTab === filename ? "#87b4ff" : undefined,
                        }}
                        onClick={() => setModelfileSubTab(filename)}
                      >
                        {item?.nickname || item?.displayName || filename}
                      </button>
                    );
                  })}
                </div>

                {!!modelfileSubTab && modelfileDatas[modelfileSubTab] && (
                  <div>
                    <div style={{ marginBottom: 10 }}>
                      <label style={{ display: "block", marginBottom: 6, color: "var(--app-text)" }}>FROM model</label>
                      <input
                        value={modelfileFromEdits[modelfileSubTab] || ""}
                        onChange={(e) =>
                          setModelfileFromEdits((prev) => ({
                            ...prev,
                            [modelfileSubTab]: e.target.value,
                          }))
                        }
                        style={{ width: "100%", maxWidth: 520, padding: "8px 10px", borderRadius: 6, background: "var(--app-input-bg)", color: "var(--app-text)", border: "1px solid var(--app-border)" }}
                      />
                      {!(["modelfile_organizer.txt", "modelfile_coder.txt", "modelfile_summarizer.txt", "modelfile_vision.txt"].includes(modelfileSubTab)) && (
                        <div style={{ marginTop: 8 }}>
                          <label style={{ display: "block", marginBottom: 6, color: "var(--app-text)" }}>Display nickname (from modelfile note)</label>
                          <input
                            value={modelfileDatas[modelfileSubTab]?.nickname || ""}
                            onChange={(e) => {
                              const nextNick = e.target.value;
                              setModelfileDatas((prev) => ({
                                ...prev,
                                [modelfileSubTab]: {
                                  ...(prev[modelfileSubTab] || { filename: modelfileSubTab, displayName: modelfileSubTab, nickname: "", fromModel: "", systemPrompt: "", params: [] }),
                                  nickname: nextNick,
                                  displayName: nextNick || prev[modelfileSubTab]?.displayName || modelfileSubTab,
                                },
                              }));
                            }}
                            style={{ width: "100%", maxWidth: 520, padding: "8px 10px", borderRadius: 6, background: "var(--app-input-bg)", color: "var(--app-text)", border: "1px solid var(--app-border)" }}
                          />

                          <label style={{ display: "block", margin: "8px 0 6px", color: "var(--app-text)" }}>System prompt (custom models only)</label>
                          <textarea
                            rows={6}
                            value={modelfileDatas[modelfileSubTab]?.systemPrompt || ""}
                            onChange={(e) => {
                              const nextPrompt = e.target.value;
                              setModelfileDatas((prev) => ({
                                ...prev,
                                [modelfileSubTab]: {
                                  ...(prev[modelfileSubTab] || { filename: modelfileSubTab, displayName: modelfileSubTab, nickname: "", fromModel: "", systemPrompt: "", params: [] }),
                                  systemPrompt: nextPrompt,
                                },
                              }));
                            }}
                            style={{ width: "100%", boxSizing: "border-box", borderRadius: 6, border: "1px solid var(--app-border)", background: "var(--app-input-bg)", color: "var(--app-text)", padding: 8 }}
                          />
                        </div>
                      )}
                      {["modelfile_organizer.txt", "modelfile_coder.txt", "modelfile_summarizer.txt", "modelfile_vision.txt"].includes(modelfileSubTab) && (
                        <div style={{ marginTop: 6, fontSize: 11, color: "#8fa4c6" }}>
                          For default models, system prompt and nickname should be edited directly in their modelfiles.
                        </div>
                      )}
                    </div>

                    <div
                      style={{
                        display: "grid",
                        gridTemplateColumns: "minmax(160px, 220px) minmax(160px, 1fr)",
                        gap: 8,
                        alignItems: "center",
                        marginBottom: 10,
                      }}
                    >
                      {(modelfileEdits[modelfileSubTab] || []).map((p) => (
                        <div key={`${modelfileSubTab}_${p.key}`} style={{ display: "contents" }}>
                          <div style={{ fontSize: 12, color: "#c8d5ea" }}>{p.key}</div>
                          <input
                            value={p.value}
                            onChange={(e) => updateModelfileParam(modelfileSubTab, p.key, e.target.value)}
                            style={{ padding: "7px 9px", borderRadius: 6, background: "#131313", color: "#ebebeb", border: "1px solid #3a3a3a" }}
                          />
                        </div>
                      ))}
                    </div>

                    <button
                      className="setup-btn primary"
                      disabled={!!modelfileSaving[modelfileSubTab]}
                      onClick={() => saveModelfile(modelfileSubTab, {
                        systemPrompt: ["modelfile_organizer.txt", "modelfile_coder.txt", "modelfile_summarizer.txt", "modelfile_vision.txt"].includes(modelfileSubTab)
                          ? undefined
                          : modelfileDatas[modelfileSubTab]?.systemPrompt,
                        nickname: ["modelfile_organizer.txt", "modelfile_coder.txt", "modelfile_summarizer.txt", "modelfile_vision.txt"].includes(modelfileSubTab)
                          ? undefined
                          : modelfileDatas[modelfileSubTab]?.nickname,
                      })}
                    >
                      {modelfileSaving[modelfileSubTab] ? "Saving..." : "Save Modelfile"}
                    </button>
                  </div>
                )}
              </div>

              {/* ── Performance Guide ── */}
              <div style={{ marginBottom: 18 }}>
                <div style={{ marginBottom: 8, fontSize: 12, color: "#a8d7ff" }}>
                  Current capability mode: Repos {reposContextActive ? "ON" : "OFF"}, MCP {mcpContextActive ? "ON" : "OFF"},
                  {llmCapabilities.fullRagEnabled ? " Full RAG" : " Basic RAG"}, {llmCapabilities.multiMcpEnabled ? "Multi-MCP" : "Single-MCP"}.
                </div>
                <div className="footprint-card" style={{ marginBottom: 10 }}>
                  <div className="footprint-top-row">
                    <span style={{ color: isLightMode ? "#1d2b3a" : "#dbe9ff", fontWeight: 700 }}>Effective Context Footprint</span>
                    <span className="footprint-badge">{effectiveFootprint.level}</span>
                  </div>
                  <div style={{ marginTop: 4, fontSize: 12, color: isLightMode ? "#2b3c52" : "#b9cae7" }}>
                    Score {effectiveFootprint.score.toFixed(2)} / 6.50
                  </div>
                  <div style={{ marginTop: 4, fontSize: 12, color: isLightMode ? "#314a66" : "#9bb1d3" }}>
                    {effectiveFootprint.rationale}
                  </div>
                  <div style={{ marginTop: 6, fontSize: 12, color: isLightMode ? "#2f465f" : "#9fb2c9" }}>
                    This score estimates cognitive bandwidth load for the active profile and tools. Inputs:
                    profile base + live token budget (`num_ctx` from Organizer, if edited) + repo load + MCP load + planner + network.
                    Lower scores favor responsiveness on constrained hardware; higher scores favor richer context handling.
                  </div>
                </div>
                <div style={{ marginBottom: 8, color: "var(--app-text)", fontWeight: 600, fontSize: 13 }}>Performance Guide</div>
                <div style={{ marginBottom: 8, fontSize: 12, color: isLightMode ? "#4e6175" : "#9e9e9e" }}>
                  Use these baselines with online tools to compare your specs and find your best fit.
                </div>
                <div className="perf-guide-table">
                  <div className="perf-guide-header">Profile</div>
                  <div className="perf-guide-header">Recommended Specs</div>
                  <div className="perf-guide-header">Performance Expectation</div>
                  <div className="perf-guide-header">Enabled Capabilities</div>

                  <div className="perf-guide-cell perf-guide-profile ultra">Ultra</div>
                  <div className="perf-guide-cell">VRAM ≥ 16 GB (e.g., RTX 3090 / 4090)</div>
                  <div className="perf-guide-cell">Fastest, highest quality, max context (8K+)</div>
                  <div className="perf-guide-cell">Full RAG · Multi-MCP · Deep reasoning loops</div>

                  <div className="perf-guide-cell perf-guide-profile high">High</div>
                  <div className="perf-guide-cell">VRAM ≥ 12 GB (e.g., RTX 3060 12 GB / 4070 Ti)</div>
                  <div className="perf-guide-cell">Excellent quality, large context (4K)</div>
                  <div className="perf-guide-cell">Full RAG · Multi-MCP · Extended loops</div>

                  <div className="perf-guide-cell perf-guide-profile mediumhigh">Medium-High</div>
                  <div className="perf-guide-cell">VRAM ≈ 8 GB + RAM ≥ 32 GB — Developer Baseline</div>
                  <div className="perf-guide-cell">Solid 7B performance, 3–4K context window</div>
                  <div className="perf-guide-cell">Full RAG · Multi-MCP · Standard loops</div>

                  <div className="perf-guide-cell perf-guide-profile medium">Medium</div>
                  <div className="perf-guide-cell">VRAM 6–8 GB + RAM ≥ 16 GB</div>
                  <div className="perf-guide-cell">Balanced 3B–7B models, 1.5–2K context</div>
                  <div className="perf-guide-cell">Basic Repo context · Single-tool MCP</div>

                  <div className="perf-guide-cell perf-guide-profile low">Low</div>
                  <div className="perf-guide-cell">VRAM ≤ 4 GB or RAM &lt; 16 GB</div>
                  <div className="perf-guide-cell">Lightweight 1.5–3B models, 1K context</div>
                  <div className="perf-guide-cell">Optional lightweight Repo/MCP via ON/OFF toggles</div>

                  <div className="perf-guide-cell perf-guide-profile minimal">Minimal</div>
                  <div className="perf-guide-cell">CPU only / no dedicated GPU</div>
                  <div className="perf-guide-cell">Smallest footprint, slow generation</div>
                  <div className="perf-guide-cell">Keep Repo/MCP OFF by default; enable only when needed</div>
                </div>
              </div>

              <div className="llm-settings-actions">
                <button className="setup-btn" onClick={handleOpenModelfiles}>
                  Open Modelfiles Folder
                </button>
                <button className="setup-btn primary" onClick={handleApplyModelsAndRestart}>
                  Apply and Restart
                </button>
              </div>
            </div>
          )}
          {settingsTab === "Controller" && (
            <div className="llm-settings-card">
              <h3>Bi-directional AI Control</h3>
              <p>
                This controller defines how Iris receives instructions from you and how it routes focused objectives to each internal specialist model.
                Use this as the bridge between human intent and model-level behavior.
              </p>

              <div style={{ marginBottom: 10, fontSize: 13, lineHeight: 1.55 }}>
                <div style={{ color: isLightMode ? "#26384d" : "#dbe9ff", fontWeight: 700, marginBottom: 4 }}>Outbound: You to Iris</div>
                <div style={{ color: "#cfdcf1" }}>
                  User prompts establish intent, constraints, and priority. Controller settings shape how that intent is interpreted,
                  whether planner memory lanes are emphasized, and how much context load is allowed.
                </div>
              </div>

              <div style={{ marginBottom: 10, fontSize: 13, lineHeight: 1.55 }}>
                <div style={{ color: isLightMode ? "#26384d" : "#dbe9ff", fontWeight: 700, marginBottom: 4 }}>Inbound: Iris to Internal Model Stack</div>
                <div style={{ color: "#cfdcf1" }}>
                  Iris routes tasks bi-directionally across Organizer, Coder, Summarizer, and Vision according to the selected profile,
                  modelfile parameters, and available tool context (Repos, MCP, Network). This keeps each model focused while maintaining continuity.
                </div>
              </div>

              <div style={{ marginBottom: 12, fontSize: 13, lineHeight: 1.55 }}>
                <div style={{ color: isLightMode ? "#26384d" : "#dbe9ff", fontWeight: 700, marginBottom: 4 }}>Task Directive Layer</div>
                <div style={{ color: "#cfdcf1" }}>
                  Use this section to verify the active reasoning route and memory signals. If behavior is too broad, reduce context load;
                  if too narrow, increase token budget and keep planner lanes enabled.
                </div>
              </div>

              <div style={{ marginBottom: 12 }}>
                <button
                  className="setup-btn"
                  onClick={async () => {
                    const next = !interpretV2Enabled;
                    try {
                      await invoke("set_setup_flags", { args: { interpretV2Enabled: next } });
                      setInterpretV2Enabled(next);
                    } catch (e) {
                      console.warn("Failed to toggle interpret v2", e);
                    }
                  }}
                >
                  Interpret V2: {interpretV2Enabled ? "ON" : "OFF"}
                </button>
              </div>

              <details>
                <summary style={{ cursor: "pointer", color: "#b9cae7", marginBottom: 8 }}>Memory Debug Signals</summary>
                <div style={{ fontSize: 13, lineHeight: 1.5, marginTop: 8 }}>
                  <div><strong>Planner path:</strong> {memoryDebug.plannerPath}</div>
                  <div><strong>Planner strategy:</strong> {memoryDebug.plannerStrategy || "(n/a)"}</div>
                  <div><strong>Planner intent:</strong> {memoryDebug.plannerIntent || "(n/a)"}</div>
                  <div><strong>Suggested Godot:</strong> {memoryDebug.suggestedGodotVersion || "unspecified"}</div>
                  <div><strong>Last resolver:</strong> {memoryDebug.lastResolver}</div>
                  <div><strong>Last numeric anchor:</strong> {memoryDebug.lastNumericAnchor ?? "(none)"}</div>
                  <div><strong>Active artifact:</strong> {memoryDebug.activeArtifactLabel}</div>
                  <div><strong>Transcript chars:</strong> {memoryDebug.transcriptChars}</div>
                  <div><strong>Updated:</strong> {memoryDebug.updatedAt ? new Date(memoryDebug.updatedAt * 1000).toLocaleTimeString() : "(not yet)"}</div>
                </div>

                <hr style={{ opacity: 0.2, margin: "12px 0" }} />
                <div style={{ fontSize: 13, lineHeight: 1.5 }}>
                  <div><strong>Lane • Project</strong></div>
                  <div style={{ opacity: 0.9 }}>{memoryDebug.laneProject || "(empty)"}</div>
                  <div style={{ marginTop: 8 }}><strong>Lane • Coding</strong></div>
                  <div style={{ opacity: 0.9 }}>{memoryDebug.laneCoding || "(empty)"}</div>
                  <div style={{ marginTop: 8 }}><strong>Lane • Recall</strong></div>
                  <div style={{ opacity: 0.9 }}>{memoryDebug.laneRecall || "(empty)"}</div>
                </div>
              </details>
            </div>
          )}
        </div>
      )}

      {currentTab?.type === "chat" && (
        <form
          className="chat-input-row"
          onSubmit={isGenerating ? handleStop : sendMessage}
          autoComplete="off"
          onDragOver={(e) => {
            e.preventDefault();
            if (!chatDragActive) setChatDragActive(true);
          }}
          onDragLeave={(e) => {
            if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
            setChatDragActive(false);
          }}
          onDrop={(e) => { void handleChatDrop(e); }}
          style={chatDragActive ? { boxShadow: "inset 0 0 0 2px rgba(91, 143, 214, 0.55)" } : undefined}
        >
          {!!pendingImages.length && (
            <div className="image-attachment-strip">
              {pendingImages.map((img) => (
                <div key={img.id} className="image-attachment-card">
                  <img src={img.dataUrl} alt={img.name} className="image-attachment-preview" />
                  <div className="image-attachment-meta">{img.name}</div>
                  <button type="button" className="image-attachment-remove" onClick={() => removePendingImage(img.id)}>×</button>
                </div>
              ))}
            </div>
          )}
          <textarea
            className="chat-input"
            value={input}
            onChange={(e) => {
              promptHistoryIndexRef.current = -1;
              promptHistoryDraftRef.current = "";
              setInput(e.target.value);
            }}
            placeholder={pendingImages.length ? "Add a prompt, or send to let Iris inspect the image..." : "Type your message..."}
            autoFocus
            disabled={isGenerating || menuLocked}
            rows={1}
            style={{ resize: "none" }}
            ref={inputRef}
            onInput={autoResize}
            onPaste={(e) => { void handleChatPaste(e); }}
            onKeyDown={(e) => {
              if (e.key === "ArrowUp" && !e.shiftKey) {
                e.preventDefault();
                recallPromptHistory("older");
                return;
              }
              if (e.key === "ArrowDown" && !e.shiftKey && promptHistoryIndexRef.current >= 0) {
                e.preventDefault();
                recallPromptHistory("newer");
                return;
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                if (menuLocked) return;
                if (input.trim() || pendingImages.length) sendMessage(e);
              }
            }}
          />
          <button
            className="send-btn"
            type="submit"
            disabled={
              menuLocked ||
              (respondingTab !== null && respondingTab !== activeTab && !isGenerating)
            }
          >
            {isGenerating ? "Stop" : "Send"}
          </button>
        </form>
      )}
    </div>
    </>
  );
}

export default App;