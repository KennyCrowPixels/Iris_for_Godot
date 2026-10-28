type Message = { role: "user" | "assistant"; content: string };

export async function readOllamaStream(body: ReadableStream<Uint8Array>, onToken: (token: string) => void): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let pending = "";
  let completed = false;
  const parseLine = (line: string) => {
    if (!line.trim()) return;
    const value = JSON.parse(line);
    if (value.error) throw new Error(String(value.error));
    const token = value.response ?? value.message?.content ?? "";
    if (token) onToken(String(token));
    if (value.done) completed = true;
  };
  try {
    while (!completed) {
      const { value, done } = await reader.read();
      pending += done ? decoder.decode() : decoder.decode(value, { stream: true });
      let boundary = pending.indexOf("\n");
      while (boundary >= 0) {
        parseLine(pending.slice(0, boundary));
        pending = pending.slice(boundary + 1);
        if (completed) break;
        boundary = pending.indexOf("\n");
      }
      if (done) {
        if (!completed) parseLine(pending);
        if (!completed) throw new Error("Ollama stream ended before its completion record");
        break;
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export default function useOllamaStream() {
  async function stream({
    model,
    images,
    prompt,
    messages,
    options,
    signal,
    onHeaders,
    onFirstToken,
    onTokens,
    onDone,
  }: {
    model: string;
    images?: string[];
    prompt?: string;
    messages?: Message[];
    options?: Record<string, any>;
    signal: AbortSignal;
    onHeaders?: () => void;
    onFirstToken?: (first: string) => void;
    onTokens?: (delta: string) => void;
    onDone?: () => void;
  }) {
    let llmInserted = false;

    // Determine which endpoint to use: /api/chat for messages, /api/generate for prompt
    const useChat = messages && messages.length > 0;
    const endpoint = useChat ? 'http://127.0.0.1:11434/api/chat' : 'http://127.0.0.1:11434/api/generate';
    const requestBody = useChat
      ? {
          model,
          messages,
          stream: true,
          keep_alive: "90s",
          ...(options ? { options } : {}),
        }
      : {
          model,
          ...(images && images.length ? { images } : {}),
          prompt: prompt || "",
          stream: true,
          keep_alive: "90s",
          ...(options ? { options } : {}),
        };

    const fetchOptions = {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(requestBody),
      signal,
    };

    let response: Response | null = null;
    let lastErr: any = null;
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        response = await fetch(endpoint, fetchOptions);
        if (response && response.ok && response.body) break;
        // read body text on error to help debug
        const txt = response ? await response.text().catch(() => "") : "";
        lastErr = new Error(`Ollama responded with status ${response?.status}: ${txt}`);
        response = null;
      } catch (err) {
        if (signal.aborted) throw err;
        lastErr = err;
        response = null;
      }
      // retry once after a short backoff
      await new Promise(r => setTimeout(r, 300));
    }
    if (!response) throw lastErr || new Error("No response body from Ollama");
    if (onHeaders) onHeaders();

    const body = response.body;
    if (!body) throw new Error("Ollama response missing body");
    await readOllamaStream(body, (token) => {
      if (!llmInserted) { llmInserted = true; onFirstToken?.(token); }
      else onTokens?.(token);
    });
    if (onDone) onDone();
  }

  return { stream };
}