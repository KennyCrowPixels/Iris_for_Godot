// IrisClient — thin REST wrapper over the desktop controller server. The phone never runs the model;
// it pairs with a PIN, submits turns, and polls for the final reply. All inference stays on the desktop.

import type { ChatDetail, ChatSummary, Connection, JobStatus } from "./types";

const REQUEST_TIMEOUT_MS = 15000;

type ApiInit = {
  method?: "GET" | "POST";
  body?: string;
  token?: string;
  timeoutMs?: number;
};

async function request<T>(url: string, init: ApiInit = {}): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), init.timeoutMs ?? REQUEST_TIMEOUT_MS);
  try {
    const headers: Record<string, string> = { "content-type": "application/json" };
    if (init.token) headers.authorization = `Bearer ${init.token}`;
    const response = await fetch(url, { method: init.method ?? "GET", body: init.body, headers, signal: controller.signal });
    const text = await response.text();
    const body = text ? JSON.parse(text) : {};
    if (!response.ok) throw new Error(body?.error || `HTTP ${response.status}`);
    return body as T;
  } finally {
    clearTimeout(timer);
  }
}

const base = (host: string, port: number) => `http://${host}:${port}`;

/** Probe a candidate host without a token to confirm it is an Iris companion server. */
export async function ping(host: string, port: number): Promise<{ service: string; requiresPin: boolean; pairingLocked: boolean }> {
  return request(`${base(host, port)}/v1/ping`, { method: "GET", timeoutMs: 4000 });
}

/** Exchange the one-time PIN for a session bearer token. */
export async function pair(host: string, port: number, pin: string): Promise<{ token: string }> {
  return request(`${base(host, port)}/v1/pair`, { method: "POST", body: JSON.stringify({ pin }), timeoutMs: 6000 });
}

export async function listChats(c: Connection): Promise<ChatSummary[]> {
  const body = await request<{ chats: ChatSummary[] }>(`${base(c.host, c.port)}/v1/chats`, { token: c.token });
  return body.chats;
}

export async function createChat(c: Connection, title: string): Promise<{ id: number; title: string }> {
  return request(`${base(c.host, c.port)}/v1/chats`, { method: "POST", token: c.token, body: JSON.stringify({ title }) });
}

export async function getChat(c: Connection, tabId: number): Promise<ChatDetail> {
  const body = await request<{ chat: ChatDetail }>(`${base(c.host, c.port)}/v1/chats/${tabId}`, { token: c.token });
  return body.chat;
}

export async function submitTurn(
  c: Connection,
  tabId: number,
  input: string,
  options: { networkAllowed?: boolean; images?: string[] } = {},
): Promise<{ jobId: string; statusUrl: string }> {
  return request(`${base(c.host, c.port)}/v1/chats/${tabId}/turns`, {
    method: "POST",
    token: c.token,
    timeoutMs: 30000,
    body: JSON.stringify({ input, networkAllowed: options.networkAllowed ?? false, images: options.images ?? [] }),
  });
}

export async function getJob(c: Connection, jobId: string): Promise<JobStatus> {
  const body = await request<{ job: JobStatus }>(`${base(c.host, c.port)}/v1/jobs/${jobId}`, { token: c.token });
  return body.job;
}

export async function cancelJob(c: Connection, jobId: string): Promise<void> {
  await request(`${base(c.host, c.port)}/v1/jobs/${jobId}/cancel`, { method: "POST", token: c.token });
}

/** Poll a job until it leaves the running state (or the signal aborts). The desktop emits Done
 *  internally; we detect completion via the job status, then the caller fetches the final chat. */
export async function waitForJob(
  c: Connection,
  jobId: string,
  onTick: (status: JobStatus) => void,
  shouldStop: () => boolean,
): Promise<JobStatus> {
  for (;;) {
    if (shouldStop()) {
      await cancelJob(c, jobId).catch(() => {});
      throw new Error("cancelled");
    }
    const status = await getJob(c, jobId);
    onTick(status);
    if (status.status !== "running") return status;
    await new Promise((resolve) => setTimeout(resolve, 600));
  }
}
