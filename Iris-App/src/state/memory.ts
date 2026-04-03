// src/state/memory.ts
// Thin, resilient wrappers around Tauri commands.
// If Tauri isn't injected yet, we return safe defaults instead of throwing.

export type ChatMessage = { role: "user" | "llm"; text: string; time?: number };

export type Artifact = {
  filename?: string | null;
  lang?: string | null;
  content: string;
  ts?: number;
};

export type Snapshot = {
  tab_id?: number;
  title: string;
  messages: ChatMessage[];
  associatedProjectId?: string | null;
  microSummary: string;
  dialogueBullets: string;
  summary: string;
  artifacts: Artifact[];
  promptHistory?: string[];
  last_updated?: number;
};

type RestoreArgs = {
  tabId: number;
  title: string;
  messages: ChatMessage[];
  artifacts: Artifact[];
  microSummary: string;
  dialogueBullets: string;
  summary: string;
  lastUpdated?: number;
};

type UpdateArgs = {
  tabId: number;
  summary: string;
  microSummary: string;
  dialogueBullets: string;
  newMessage: string;
  artifacts: Artifact[];
};

// ---------------------------
// internal helpers
// ---------------------------
function tauriInvoke<T = unknown>(cmd: string, args?: any): Promise<T> {
  // Tauri may inject `invoke` directly on `window.__TAURI__` or under `window.__TAURI__.core`
  const tauri = (window as any).__TAURI__;
  const inv = tauri?.core?.invoke ?? tauri?.invoke;
  if (!inv || typeof inv !== "function") {
    // Soft fallback: pretend it succeeded (or return empty data)
    // so startup code doesn't break while Tauri injects. Callers that "need" data should handle empty results.
    console.warn("[tauriInvoke] Tauri invoke not available yet (frontend falling back). Cmd:", cmd, "args:", args);
    return Promise.resolve((Array.isArray(null) ? [] : undefined) as unknown as T);
  }
  try {
    return inv(cmd, args);
  } catch (e) {
    // Some environments expose invoke but require call signature via core.invoke; try to call via tauri.core.invoke
    if (tauri?.core?.invoke && typeof tauri.core.invoke === "function") {
      return tauri.core.invoke(cmd, args);
    }
    throw e;
  }
}

// ---------------------------
// exported API
// ---------------------------

export async function createTabMemory(tabId: number): Promise<void> {
  await tauriInvoke("create_tab_memory", { tab_id: tabId });
}

export async function updateTabMemory(args: UpdateArgs): Promise<void> {
  // Tauri 2: command functions receive a struct parameter named `args`,
  // so we must wrap the payload under that key at the transport level
  await tauriInvoke("update_tab_memory", {
    args: {
      tab_id: args.tabId,
      summary: args.summary,
      micro_summary: args.microSummary,
      dialogue_bullets: args.dialogueBullets,
      new_message: args.newMessage,
      artifacts: args.artifacts,
    }
  });
}

export async function restoreFullTabMemory(args: RestoreArgs): Promise<void> {
  // Pass the payload directly as the command takes RestoreFullTabMemoryArgs
  await tauriInvoke("restore_full_tab_memory", {
    tab_id: args.tabId,
    title: args.title,
    messages: args.messages,
    artifacts: args.artifacts,
    micro_summary: args.microSummary,
    dialogue_bullets: args.dialogueBullets,
    summary: args.summary,
    last_updated: args.lastUpdated,
  });
}

export async function listOpenTabs(): Promise<Snapshot[]> {
  try {
      const snaps = await tauriInvoke<Snapshot[]>("list_open_tabs");
      return Array.isArray(snaps) ? snaps : [];
    } catch (e) {
      console.warn("[listOpenTabs] error:", e);
      return [];
  }
}

// Optional per-tab read fallback. Returns Snapshot or null on failure.
export async function readTabSnapshot(key: string | number): Promise<Snapshot | null> {
  try {
    const snap = await tauriInvoke<Snapshot>("read_tab_snapshot", { 
      args: { key: String(key) } 
    });
    return (snap && Array.isArray((snap as any).messages)) ? snap : null;
  } catch {
    return null;
  }
}

// New helper: persist a full Snapshot to backend open-tabs (calls update_snapshot_memory)
export async function persistSnapshot(tabId: number, snapshotLike: Snapshot): Promise<void> {
  try {
    const now = Math.floor(Date.now() / 1000);
    const msgsWithTime = (snapshotLike.messages || []).map(m => ({
      role: m.role,
      text: m.text,
      time: typeof m.time === "number" ? m.time : now,
    }));
    const snapToSend: Snapshot = {
      tab_id: tabId,
      title: snapshotLike.title,
      messages: msgsWithTime,
      associatedProjectId: snapshotLike.associatedProjectId ?? null,
      microSummary: snapshotLike.microSummary,
      dialogueBullets: snapshotLike.dialogueBullets,
      summary: snapshotLike.summary,
      artifacts: snapshotLike.artifacts ?? [],
      promptHistory: Array.isArray(snapshotLike.promptHistory) ? snapshotLike.promptHistory.map(String) : [],
      last_updated: snapshotLike.last_updated ?? now,
    };
    await tauriInvoke("update_snapshot_memory", { 
      args: {
        tab_id: tabId,
        snapshot: snapToSend,
      }
    });
  } catch (e) {
    console.warn("[persistSnapshot] failed:", e);
  }
}

// Debug helpers
export async function debugMemoryDir(): Promise<string | null> {
  try {
    return await tauriInvoke<string>("debug_memory_dir");
  } catch {
    return null;
  }
}

export async function debugListOpenTabFiles(): Promise<string[] | null> {
  try {
    return await tauriInvoke<string[]>("debug_list_open_tab_files");
  } catch {
    return null;
  }
}

export async function seedOpenTabsFromDevDir(): Promise<number> {
  try {
    return await tauriInvoke<number>("seed_open_tabs_from_dev_dir");
  } catch {
    return 0;
  }
}

export async function migrateOpenTabsToSnapshotFormat(): Promise<number> {
  try {
    return await tauriInvoke<number>("migrate_open_tabs_to_snapshot_format");
  } catch {
    return 0;
  }
}
export async function sanitizeTabTitles(): Promise<number> {
  try {
    return await tauriInvoke<number>("sanitize_tab_titles");
  } catch {
    return 0;
  }
}