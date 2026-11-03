// Microphone capture for desktop dictation. Records 16 kHz mono PCM in the webview (getUserMedia
// works in desktop webviews even though the cloud Web Speech API does not), stops on its own after
// a pause in speech, and hands the samples to the local whisper.cpp engine for transcription.

export const WHISPER_SAMPLE_RATE = 16000;

export type RecorderCallbacks = {
  /** Called once when the recorder stops by itself (pause after speech, no speech, or time limit). */
  onAutoStop?: (reason: "silence" | "no-speech" | "limit") => void;
  onError?: (message: string) => void;
};

export function isMicCaptureSupported(): boolean {
  return typeof navigator !== "undefined" && !!navigator.mediaDevices?.getUserMedia
    && typeof window !== "undefined" && !!((window as any).AudioContext || (window as any).webkitAudioContext);
}

/** Averages input samples into 16 kHz frames. Pure + testable. */
export function downsampleTo16k(input: Float32Array, inputRate: number): Float32Array {
  if (inputRate === WHISPER_SAMPLE_RATE) return input;
  const ratio = inputRate / WHISPER_SAMPLE_RATE;
  const length = Math.floor(input.length / ratio);
  const output = new Float32Array(length);
  for (let index = 0; index < length; index++) {
    const start = Math.floor(index * ratio);
    const end = Math.min(input.length, Math.floor((index + 1) * ratio));
    let sum = 0;
    for (let cursor = start; cursor < end; cursor++) sum += input[cursor];
    output[index] = end > start ? sum / (end - start) : 0;
  }
  return output;
}

/** Float [-1, 1] samples to little-endian 16-bit PCM, base64-encoded. Pure + testable. */
export function pcm16Base64(samples: Float32Array): string {
  const bytes = new Uint8Array(samples.length * 2);
  const view = new DataView(bytes.buffer);
  for (let index = 0; index < samples.length; index++) {
    const clamped = Math.max(-1, Math.min(1, samples[index]));
    view.setInt16(index * 2, clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff, true);
  }
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return btoa(binary);
}

export class WhisperRecorder {
  private stream: MediaStream | null = null;
  private context: AudioContext | null = null;
  private processor: ScriptProcessorNode | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private chunks: Float32Array[] = [];
  private inputRate = WHISPER_SAMPLE_RATE;
  private startedAt = 0;
  private lastVoiceAt = 0;
  private heardSpeech = false;
  private noiseFloor = 0.004;
  private stopping = false;

  constructor(private readonly silenceMs = 1600, private readonly noSpeechMs = 8000, private readonly maxMs = 90000) {}

  get recording(): boolean { return this.stream !== null; }

  async start(callbacks: RecorderCallbacks = {}): Promise<void> {
    if (this.stream) return;
    this.chunks = [];
    this.heardSpeech = false;
    this.stopping = false;
    this.noiseFloor = 0.004;
    try {
      this.stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
    } catch (error: any) {
      this.stream = null;
      const name = String(error?.name || "");
      throw new Error(name === "NotAllowedError" || name === "SecurityError"
        ? "Microphone access was blocked. Allow microphone access for Iris in your system privacy settings and try again."
        : name === "NotFoundError" ? "No microphone was found. Connect one and try again." : `Could not open the microphone: ${error?.message || error}`);
    }
    const Context = (window as any).AudioContext || (window as any).webkitAudioContext;
    try {
      this.context = new Context({ sampleRate: WHISPER_SAMPLE_RATE });
    } catch {
      this.context = new Context();
    }
    const context = this.context!;
    this.inputRate = context.sampleRate;
    this.source = context.createMediaStreamSource(this.stream);
    this.processor = context.createScriptProcessor(4096, 1, 1);
    this.startedAt = performance.now();
    this.lastVoiceAt = this.startedAt;
    this.processor.onaudioprocess = (event) => {
      if (this.stopping) return;
      const samples = new Float32Array(event.inputBuffer.getChannelData(0));
      this.chunks.push(samples);
      let energy = 0;
      for (let index = 0; index < samples.length; index++) energy += samples[index] * samples[index];
      const rms = Math.sqrt(energy / Math.max(1, samples.length));
      const now = performance.now();
      if (rms > Math.max(0.012, this.noiseFloor * 3)) {
        this.heardSpeech = true;
        this.lastVoiceAt = now;
      } else {
        this.noiseFloor = this.noiseFloor * 0.95 + rms * 0.05;
      }
      const reason = now - this.startedAt > this.maxMs ? "limit"
        : this.heardSpeech && now - this.lastVoiceAt > this.silenceMs ? "silence"
        : !this.heardSpeech && now - this.startedAt > this.noSpeechMs ? "no-speech" : null;
      if (reason) {
        this.stopping = true;
        queueMicrotask(() => callbacks.onAutoStop?.(reason));
      }
    };
    this.source.connect(this.processor);
    this.processor.connect(context.destination);
    void context.resume?.();
  }

  /** Stops recording and returns base64 16 kHz PCM, or null when nothing usable was heard. */
  async stop(): Promise<{ pcmBase64: string; seconds: number } | null> {
    const heard = this.heardSpeech;
    this.stopping = true;
    try { this.processor?.disconnect(); } catch { /* already disconnected */ }
    try { this.source?.disconnect(); } catch { /* already disconnected */ }
    this.stream?.getTracks().forEach((track) => track.stop());
    try { await this.context?.close(); } catch { /* already closed */ }
    this.stream = null;
    this.context = null;
    this.processor = null;
    this.source = null;
    const total = this.chunks.reduce((sum, chunk) => sum + chunk.length, 0);
    const joined = new Float32Array(total);
    let offset = 0;
    for (const chunk of this.chunks) { joined.set(chunk, offset); offset += chunk.length; }
    this.chunks = [];
    if (!heard || total === 0) return null;
    const samples = downsampleTo16k(joined, this.inputRate);
    return { pcmBase64: pcm16Base64(samples), seconds: samples.length / WHISPER_SAMPLE_RATE };
  }
}
