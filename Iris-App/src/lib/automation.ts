export type TaskClock = {
  status: "idle" | "waiting" | "running" | "paused" | "error";
  scheduleKind: "none" | "countdown" | "datetime";
  remainingMs: number;
  nextRunAt: number;
  repeatMs: number;
  updatedAt: number;
};

export function intervalMilliseconds(value: string, unit: "seconds" | "minutes" | "hours" | "days"): number {
  const amount = Number(value);
  if (!Number.isSafeInteger(amount) || amount <= 0) return 0;
  const result = amount * { seconds: 1000, minutes: 60000, hours: 3600000, days: 86400000 }[unit];
  return Number.isSafeInteger(result) ? result : 0;
}

export function advanceTaskClock<Runtime extends TaskClock>(runtime: Runtime, now: number): Runtime {
  if (runtime.status !== "waiting" || runtime.scheduleKind !== "countdown") return runtime;
  const nextRunAt = runtime.nextRunAt || runtime.updatedAt + runtime.remainingMs;
  return { ...runtime, nextRunAt, remainingMs: Math.max(0, nextRunAt - now), updatedAt: now };
}

export function taskIsDue(runtime: TaskClock, now: number): boolean {
  return runtime.status === "waiting" && runtime.scheduleKind !== "none"
    && (runtime.nextRunAt > 0 ? now >= runtime.nextRunAt : runtime.scheduleKind === "countdown" && runtime.remainingMs <= 0);
}

export function nextRecurringRun(previous: number, interval: number, now: number): number {
  if (interval <= 0) return 0;
  if (previous > now) return previous;
  return previous > 0 ? previous + (Math.floor((now - previous) / interval) + 1) * interval : now + interval;
}

export function parseTaskOutcome(text: string): { status: "complete" | "blocked" | "incomplete" | "unknown"; reason: string } {
  const lines = text.trim().split(/\r?\n/);
  const lastLine = lines[lines.length - 1] || "";
  const marker = lastLine.match(/^AUTONOMOUS_STATUS\s*:\s*(COMPLETE|BLOCKED|INCOMPLETE)\s*-\s*(.+)$/i);
  if (!marker) return { status: "unknown", reason: "The task did not explicitly evaluate its completion criteria." };
  return { status: marker[1].toLowerCase() as "complete" | "blocked" | "incomplete", reason: marker[2].trim() };
}