import { invoke } from '@tauri-apps/api/core';

export const OLLAMA_URL = 'http://127.0.0.1:11434/api/generate';

export async function ensureOllamaServer(): Promise<void> {
  try { await invoke("ensure_ollama_server"); } catch {}
}

export async function ensureModel(name: string): Promise<void> {
  await invoke("ensure_model", { name });
}

export async function pingOllama(): Promise<boolean> {
  try { const r = await fetch("http://127.0.0.1:11434/api/tags"); return r.ok; }
  catch { return false; }
}

export async function fetchJson(url: string, init: RequestInit): Promise<any> {
  const res = await fetch(url, init);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

export async function waitForTauri(timeout = 5000): Promise<void> {
  const start = Date.now();
  while (true) {
    const tauri = (window as any).__TAURI__;
    const ready = !!(tauri && (typeof tauri.invoke === "function" || (tauri.core && typeof tauri.core.invoke === "function")));
    if (ready) return;
    if (Date.now() - start > timeout) throw new Error("Tauri not ready after timeout");
    await new Promise(res => setTimeout(res, 50));
  }
}