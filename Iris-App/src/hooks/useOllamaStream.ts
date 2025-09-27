// filepath: d:\Iris_for_Godot\Iris-App\src\hooks\useOllamaStream.ts
import { OLLAMA_URL } from "../lib/ollama";

export default function useOllamaStream() {
  async function stream({
    model,
    prompt,
    options,
    signal,
    onHeaders,
    onFirstToken,
    onTokens,
    onDone,
  }: {
    model: string;
    prompt: string;
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

    const fetchOptions = {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model,
        prompt,
        stream: true,
        keep_alive: "90s",
        ...(model === "iris-coder:latest" ? {} : { options }),
      }),
      signal,
    };

    const response = await fetch(OLLAMA_URL, fetchOptions);
    if (!response.ok || !response.body) throw new Error("No response body from Ollama");
    if (onHeaders) onHeaders();

    const reader = response.body.getReader();

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

            if (json.response) {
              buffer += json.response;

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