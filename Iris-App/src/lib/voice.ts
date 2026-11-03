// Voice Mode — speech-to-text (STT) and text-to-speech (TTS) using built-in, free browser APIs
// (webkitSpeechRecognition / SpeechRecognition and window.speechSynthesis). No external deps.
//
// The browser-facing wrappers are thin and defensive (feature-detected, try/caught) so the app
// degrades gracefully in webviews that do not expose these APIs. The text-normalization used before
// speaking is a pure function so it can be unit-tested without a browser.

/** Strip markdown/code/URLs so TTS reads a natural sentence instead of syntax. Pure + testable. */
export function sanitizeForSpeech(markdown: string): string {
  if (!markdown) return "";
  let text = String(markdown);
  // Remove fenced code blocks entirely — reading code aloud is noise.
  text = text.replace(/```[\s\S]*?```/g, " ");
  // Inline code: keep the words, drop the backticks.
  text = text.replace(/`([^`]+)`/g, "$1");
  // Markdown images ![alt](url) -> alt; links [text](url) -> text.
  text = text.replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1");
  text = text.replace(/\[([^\]]+)\]\(([^)]*)\)/g, "$1");
  // Bare URLs (including the <https://...> citation style Iris emits).
  text = text.replace(/<?https?:\/\/[^\s>]+>?/gi, " ");
  // Drop a trailing "Sources:" citation list — it does not read well aloud.
  text = text.replace(/\n\s*Sources:[\s\S]*$/i, " ");
  // Heading / blockquote / list markers at line starts.
  text = text.replace(/^\s{0,3}#{1,6}\s+/gm, "");
  text = text.replace(/^\s{0,3}>\s?/gm, "");
  text = text.replace(/^\s{0,3}[-*+]\s+/gm, "");
  // Emphasis markers and stray table pipes.
  text = text.replace(/(\*\*|__|\*|_|~~)/g, "");
  text = text.replace(/\|/g, " ");
  // Collapse whitespace.
  return text.replace(/\s+/g, " ").trim();
}

/** Split long text into speakable chunks at sentence boundaries to avoid the speechSynthesis
 *  long-utterance cutoff bug. Pure + testable. */
export function chunkForSpeech(text: string, maxChars = 220): string[] {
  const clean = text.trim();
  if (!clean) return [];
  const sentences = clean.match(/[^.!?]+[.!?]+|\S[^.!?]*$/g) || [clean];
  const chunks: string[] = [];
  let current = "";
  for (const sentence of sentences) {
    const piece = sentence.trim();
    if (!piece) continue;
    if (current.length + piece.length + 1 > maxChars && current) {
      chunks.push(current.trim());
      current = "";
    }
    current = current ? `${current} ${piece}` : piece;
    while (current.length > maxChars) {
      chunks.push(current.slice(0, maxChars).trim());
      current = current.slice(maxChars);
    }
  }
  if (current.trim()) chunks.push(current.trim());
  return chunks;
}

type SpeechRecognitionCtor = new () => any;

export function getSpeechRecognitionCtor(): SpeechRecognitionCtor | null {
  if (typeof window === "undefined") return null;
  const w = window as any;
  return (w.SpeechRecognition || w.webkitSpeechRecognition || null) as SpeechRecognitionCtor | null;
}

export function isSpeechRecognitionSupported(): boolean {
  return getSpeechRecognitionCtor() !== null;
}

export function isSpeechSynthesisSupported(): boolean {
  return typeof window !== "undefined" && "speechSynthesis" in window;
}

/** Immediately interrupt and cancel any active or queued spoken output. */
export function cancelSpeech(): void {
  if (!isSpeechSynthesisSupported()) return;
  try {
    window.speechSynthesis.cancel();
  } catch {
    /* ignore */
  }
}

/** Speak text aloud (sanitized + chunked). Returns true if speech was started. */
export function speak(
  markdown: string,
  opts: { onStart?: () => void; onEnd?: () => void; rate?: number; pitch?: number } = {},
): boolean {
  if (!isSpeechSynthesisSupported()) {
    opts.onEnd?.();
    return false;
  }
  const chunks = chunkForSpeech(sanitizeForSpeech(markdown));
  if (chunks.length === 0) {
    opts.onEnd?.();
    return false;
  }
  cancelSpeech();
  let started = false;
  chunks.forEach((chunk, index) => {
    const utterance = new SpeechSynthesisUtterance(chunk);
    utterance.rate = opts.rate ?? 1;
    utterance.pitch = opts.pitch ?? 1;
    if (index === 0) {
      utterance.onstart = () => {
        if (!started) {
          started = true;
          opts.onStart?.();
        }
      };
    }
    if (index === chunks.length - 1) {
      utterance.onend = () => opts.onEnd?.();
      utterance.onerror = () => opts.onEnd?.();
    }
    try {
      window.speechSynthesis.speak(utterance);
    } catch {
      if (index === chunks.length - 1) opts.onEnd?.();
    }
  });
  return true;
}

export type VoiceRecognizerHandlers = {
  /** Interim/accumulated transcript as the user speaks. */
  onTranscript?: (text: string, isFinal: boolean) => void;
  /** Called once when recognition ends (manual stop, silence timeout, or natural end). */
  onEnd?: (finalText: string) => void;
  onError?: (message: string) => void;
};

/**
 * Microphone speech recognition with an automatic silence timeout. Recording stops when `stop()` is
 * called or when no speech is detected for `silenceMs`. Defensive against unsupported webviews.
 */
export class VoiceRecognizer {
  private recognition: any = null;
  private silenceTimer: ReturnType<typeof setTimeout> | null = null;
  private finalText = "";
  private active = false;
  private handlers: VoiceRecognizerHandlers = {};

  constructor(private silenceMs = 10000) {}

  get isActive(): boolean {
    return this.active;
  }

  start(handlers: VoiceRecognizerHandlers): boolean {
    const Ctor = getSpeechRecognitionCtor();
    if (!Ctor) {
      handlers.onError?.("Speech recognition is not available in this environment.");
      return false;
    }
    if (this.active) return true;
    this.handlers = handlers;
    this.finalText = "";
    try {
      const recognition = new Ctor();
      recognition.continuous = true;
      recognition.interimResults = true;
      recognition.lang = (typeof navigator !== "undefined" && navigator.language) || "en-US";
      recognition.onresult = (event: any) => {
        let interim = "";
        for (let i = event.resultIndex; i < event.results.length; i++) {
          const result = event.results[i];
          const transcript = result[0]?.transcript ?? "";
          if (result.isFinal) this.finalText += (this.finalText ? " " : "") + transcript.trim();
          else interim += transcript;
        }
        this.resetSilence();
        const combined = `${this.finalText}${interim ? ` ${interim}` : ""}`.trim();
        this.handlers.onTranscript?.(combined, interim.length === 0);
      };
      recognition.onerror = (event: any) => {
        if (event?.error && event.error !== "no-speech" && event.error !== "aborted") {
          this.handlers.onError?.(String(event.error));
        }
      };
      recognition.onend = () => {
        this.clearSilence();
        this.active = false;
        this.recognition = null;
        this.handlers.onEnd?.(this.finalText.trim());
      };
      this.recognition = recognition;
      this.active = true;
      recognition.start();
      this.resetSilence();
      return true;
    } catch (error) {
      this.active = false;
      this.recognition = null;
      handlers.onError?.(String(error));
      return false;
    }
  }

  stop(): void {
    this.clearSilence();
    if (this.recognition) {
      try {
        this.recognition.stop();
      } catch {
        /* onend will still fire or state resets below */
      }
    }
  }

  private resetSilence(): void {
    this.clearSilence();
    this.silenceTimer = setTimeout(() => this.stop(), this.silenceMs);
  }

  private clearSilence(): void {
    if (this.silenceTimer) {
      clearTimeout(this.silenceTimer);
      this.silenceTimer = null;
    }
  }
}
