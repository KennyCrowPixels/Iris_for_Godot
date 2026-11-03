// Shared types for the desktop controller contract (mirrors src-tauri/src/controller_server.rs).

export type Connection = {
  host: string; // LAN IP or hostname of the desktop
  port: number;
  token: string; // session bearer token obtained via PIN pairing
  name?: string;
};

export type DiscoveredHost = {
  name: string;
  host: string;
  port: number;
};

export type ChatSummary = {
  id: number;
  title: string;
  projectId: string | null;
  updatedAt: number;
};

export type ChatMessage = {
  role: "user" | "llm";
  text: string;
  time?: number;
  images?: string[];
};

export type ChatDetail = {
  id?: number;
  title: string;
  messages: ChatMessage[];
};

export type JobStatus = {
  id: string;
  tabId: number;
  status: "running" | "completed" | "paused" | "blocked" | "cancelled" | "error" | string;
  error: string | null;
};
