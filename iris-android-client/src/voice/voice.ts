// Mobile Voice Mode — speech-to-text and text-to-speech run entirely on the phone's native OS APIs.
// No audio ever crosses the network: the phone transcribes locally and sends only text; replies are
// spoken locally with the phone's TTS voices, keeping the desktop GPU free for inference.

import * as Speech from "expo-speech";
import { ExpoSpeechRecognitionModule } from "expo-speech-recognition";

const SILENCE_MS = 10000;

/** Strip markdown/code/URLs so TTS reads a natural sentence. Pure + mirrors the desktop logic. */
export function sanitizeForSpeech(markdown: string): string {
  if (!markdown) return "";
  let text = String(markdown);
  text = text.replace(/```[\s\S]*?```/g, " ");
  text = text.replace(/`([^`]+)`/g, "$1");
  text = text.replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1");
  text = text.replace(/\[([^\]]+)\]\(([^)]*)\)/g, "$1");
  text = text.replace(/<?https?:\/\/[^\s>]+>?/gi, " ");
  text = text.replace(/\n\s*Sources:[\s\S]*$/i, " ");
  text = text.replace(/^\s{0,3}#{1,6}\s+/gm, "");
  text = text.replace(/^\s{0,3}>\s?/gm, "");
  text = text.replace(/^\s{0,3}[-*+]\s+/gm, "");
  text = text.replace(/(\*\*|__|\*|_|~~)/g, "");
  text = text.replace(/\|/g, " ");
  return text.replace(/\s+/g, " ").trim();
}

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

/** Immediately interrupt and cancel any active or queued spoken output. */
export function cancelSpeech(): void {
  Speech.stop().catch(() => {});
}

export function speak(markdown: string, opts: { onStart?: () => void; onDone?: () => void } = {}): void {
  const chunks = chunkForSpeech(sanitizeForSpeech(markdown));
  if (chunks.length === 0) {
    opts.onDone?.();
    return;
  }
  Speech.stop().catch(() => {});
  chunks.forEach((chunk, index) => {
    Speech.speak(chunk, {
      onStart: index === 0 ? opts.onStart : undefined,
      onDone: index === chunks.length - 1 ? opts.onDone : undefined,
      onStopped: index === chunks.length - 1 ? opts.onDone : undefined,
    });
  });
}

export type VoiceHandlers = {
  onTranscript?: (text: string) => void;
  onEnd?: (finalText: string) => void;
  onError?: (message: string) => void;
};

/** Microphone speech recognition with a 10s silence auto-stop, using the phone's native recognizer. */
export class MobileVoiceRecognizer {
  private subscriptions: { remove: () => void }[] = [];
  private silenceTimer: ReturnType<typeof setTimeout> | null = null;
  private finalText = "";
  private active = false;

  get isActive(): boolean {
    return this.active;
  }

  async start(handlers: VoiceHandlers): Promise<boolean> {
    try {
      const permission = await ExpoSpeechRecognitionModule.requestPermissionsAsync();
      if (!permission.granted) {
        handlers.onError?.("Microphone permission was denied.");
        return false;
      }
      this.finalText = "";
      this.active = true;
      const add = (event: string, cb: (payload: any) => void) =>
        this.subscriptions.push(ExpoSpeechRecognitionModule.addListener(event as any, cb));

      add("result", (event: any) => {
        const transcript = event?.results?.[0]?.transcript ?? "";
        if (event?.isFinal) this.finalText = transcript.trim();
        this.resetSilence();
        handlers.onTranscript?.(transcript);
      });
      add("error", (event: any) => {
        if (event?.error && event.error !== "no-speech") handlers.onError?.(String(event.error));
      });
      add("end", () => {
        this.clearSilence();
        this.active = false;
        this.cleanup();
        handlers.onEnd?.(this.finalText.trim());
      });

      ExpoSpeechRecognitionModule.start({ lang: "en-US", interimResults: true, continuous: true });
      this.resetSilence();
      return true;
    } catch (error) {
      this.active = false;
      handlers.onError?.(String(error));
      return false;
    }
  }

  stop(): void {
    this.clearSilence();
    try {
      ExpoSpeechRecognitionModule.stop();
    } catch {
      /* the 'end' event will still finalize, or state resets on next start */
    }
  }

  private resetSilence(): void {
    this.clearSilence();
    this.silenceTimer = setTimeout(() => this.stop(), SILENCE_MS);
  }

  private clearSilence(): void {
    if (this.silenceTimer) {
      clearTimeout(this.silenceTimer);
      this.silenceTimer = null;
    }
  }

  private cleanup(): void {
    for (const subscription of this.subscriptions) subscription.remove();
    this.subscriptions = [];
  }
}
