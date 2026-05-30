// filepath: d:\Iris_for_Godot\Iris-App\src\hooks\useOllamaStream.ts
import { OLLAMA_URL } from "../lib/ollama";

type Message = { role: "user" | "assistant"; content: string };

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
    let fullText = "";
    let buffer = "";
    let done = false;
    let llmInserted = false;
    let acc = "";

    // Determine which endpoint to use: /api/chat for messages, /api/generate for prompt
    const useChat = messages && messages.length > 0;
    const endpoint = useChat ? 'http://127.0.0.1:11434/api/chat' : 'http://127.0.0.1:11434/api/generate';
    
    const requestBody = useChat
      ? {
          model,
          messages,
          stream: true,
          keep_alive: "90s",
          ...(options || {}),
        }
      : {
          model,
          ...(images && images.length ? { images } : {}),
          prompt: prompt || "",
          stream: true,
          keep_alive: "90s",
          ...(model === "iris-coder:latest" ? {} : { options }),
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
    const reader = body.getReader();

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
            if (c === "{") depth++;
            else if (c === "}") {
              depth--;
              if (depth === 0) {
                end = i;
                break;
              }
            }
          }
          if (end === -1) break;

          const objStr = acc.slice(0, end + 1);
          acc = acc.slice(end + 1);

          try {
            const json = JSON.parse(objStr);

            // Handle both /api/generate (response field) and /api/chat (message.content field)
            const token = json.response || (json.message && json.message.content) || "";
            
            if (token) {
              buffer += token;

              // Insert the LLM bubble on the first actual token
              if (!llmInserted && buffer.length > 0) {
                llmInserted = true;
                if (onFirstToken) onFirstToken(buffer);
                buffer = "";
              } else if (llmInserted && buffer.length > 0) {
                if (onTokens) onTokens(buffer);
                buffer = "";
              }
            }
          } catch {
            // ignore and continue accumulating
          }
        }
      }
    }
    if (onDone) onDone();
  }

  return { stream };
}