import { invoke } from '@tauri-apps/api/core';


export function createTabMemory(tabId: number) {
  return invoke("create_tab_memory", { tabId });
}
export function updateTabMemory(args: {
  tabId: number; summary: string; microSummary: string; dialogueBullets: string; newMessage: string; artifacts: any[];
}) {
  return invoke("update_tab_memory", args);
}
export function restoreFullTabMemory(args: {
  tabId: number; title: string; messages: any[]; artifacts: any[]; microSummary: string; dialogueBullets: string; summary: string; lastUpdated?: number;
}) {
  return invoke("restore_full_tab_memory", args);
}
export function listOpenTabs() {
  return invoke<any[]>("list_open_tabs");
}