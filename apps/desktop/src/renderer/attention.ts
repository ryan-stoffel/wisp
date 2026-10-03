import type { AgentRun, Thread } from "../protocol/generated/protocol";
import { isRunning } from "./transcript";

/**
 * What a sidebar row asks of the user (0033): its agent is working, it waits on the user, it
 * stopped since the user last looked (done or failed), or there is nothing new (settled).
 */
export type Attention = "working" | "needsYou" | "done" | "failed" | "settled";

/**
 * A thread's attention, from its run, how many permission requests it waits on, and when the
 * user last saw it. A run stopped after `seenAt` (or never seen) is unseen.
 */
export function attentionOf(thread: Thread, run: AgentRun | undefined, asks: number): Attention {
  if (asks > 0) return "needsYou";
  if (!run) return "settled";
  if (isRunning(run.status)) return "working";
  const unseen = !thread.seenAt || Date.parse(run.updatedAt) > Date.parse(thread.seenAt);
  if (!unseen) return "settled";
  return run.status === "failed" ? "failed" : "done";
}

/** A Project's attention, from its runs: any waiting on the user, else any working. */
export function projectAttention(runs: AgentRun[], asks: (runId: string) => number): Attention {
  if (runs.some((r) => asks(r.id) > 0)) return "needsYou";
  return runs.some((r) => isRunning(r.status)) ? "working" : "settled";
}

// Most urgent first: what waits on the user, then news, then work still going.
const urgency: Attention[] = ["needsYou", "failed", "done", "working", "settled"];

/** The most urgent of `attentions`, as a collapsed group of threads shows it; settled for none. */
export const mostUrgent = (attentions: Attention[]): Attention =>
  urgency.find((a) => attentions.includes(a)) ?? "settled";

/** Whether a thread is snoozed at `now`. One that needs the user wakes early. */
export const snoozed = (thread: Thread, attention: Attention, now = Date.now()) =>
  !!thread.snoozedUntil && Date.parse(thread.snoozedUntil) > now && attention !== "needsYou";

/** When a thread's newest message was sent, for the sidebar's order. */
export const lastPrompt = (thread: Thread) => thread.lastPromptAt ?? thread.createdAt;

/** A repo's default icon: the first letters of its name's first two words, or its first two. */
export function initials(name: string): string {
  const words = name.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  const letters = words.length > 1 ? words[0]![0]! + words[1]![0]! : (words[0] ?? "?").slice(0, 2);
  return letters.toUpperCase();
}

/** The snooze menu's choices, each a label and its time from `now`. */
export function snoozeChoices(now = new Date()): { label: string; until: Date }[] {
  const at = (days: number, hour: number) => {
    const d = new Date(now);
    d.setDate(d.getDate() + days);
    d.setHours(hour, 0, 0, 0);
    return d;
  };
  // Next Monday, a week out if today is Monday.
  const toMonday = (8 - now.getDay()) % 7 || 7;
  return [
    { label: "In 1 hour", until: new Date(now.getTime() + 3_600_000) },
    { label: "In 3 hours", until: new Date(now.getTime() + 3 * 3_600_000) },
    { label: "Tomorrow", until: at(1, 9) },
    { label: "Next week", until: at(toMonday, 9) },
  ];
}
