export type Message = { role: "user" | "llm"; text: string };

export function updateMessagesAppendUser(prev: Message[], text: string, images?: string[]): Message[] {
  return [...prev, { role: "user", text, ...(images && images.length ? { images } : {}) } as any];
}

export function insertLLMBubble(prev: Message[], first: string): Message[] {
  return [...prev, { role: "llm", text: first }];
}

export function patchLastLLMBubble(prev: Message[], delta: string): Message[] {
  if (!delta) return prev;
  const copy = [...prev];
  for (let i = copy.length - 1; i >= 0; --i) {
    if (copy[i].role === "llm") {
      copy[i] = { ...copy[i], text: copy[i].text + delta };
      return copy;
    }
  }
  return [...copy, { role: "llm", text: delta }];
}

// Artifact extractor (string → artifacts[])
export function extractArtifacts(text: string): Array<{ lang: string; filename?: string; content: string }> {
  const out: Array<{ lang: string; filename?: string; content: string }> = [];
  const re = /```(\w*)\s*([\s\S]*?)```/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    const lang = (m[1] || "").trim();
    const content = (m[2] || "").trim();
    if (content) out.push({ lang, content });
  }
  return out;
}