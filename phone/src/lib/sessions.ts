// What the screens say about the rows of the inbox.

import type { InboxSession } from "./model";
import { pickedMachine } from "./account";
import { mode } from "./start";
import { clock } from "./format";

/** Whether the session waits on the person: a question, a permission, or a failed run. */
export function needsYou(s: Pick<InboxSession, "status">): boolean {
  return s.status === "needs_input" || s.status === "error";
}

/** Under a waiting session's title: what it asks, so the inbox alone says it. "" when it asks nothing. */
export function asksLine(s: Pick<InboxSession, "status" | "asks">): string {
  if (s.asks) return s.asks.kind === "permission" ? `Run: ${s.asks.text}` : s.asks.text;
  if (s.status === "error") return "Stopped with an error";
  return "";
}

/** The kind of the line `asksLine` gives, for its color. */
export function asksKind(s: Pick<InboxSession, "status" | "asks">): string {
  return s.asks ? s.asks.kind : "error";
}

/** Projects in the order of their freshest session. */
export function projects(rows: InboxSession[]): string[] {
  const seen: string[] = [];
  for (const s of rows) if (s.project !== undefined && !seen.includes(s.project)) seen.push(s.project);
  return seen;
}

/** What an empty screen says while the computer is not there. */
export function connecting(s: { pcOffline: number | null; host: string }): string {
  if (s.pcOffline) return `Arakun on ${s.host || "the computer"} has been away since ${clock(s.pcOffline)}`;
  const machine = mode() === "account" ? pickedMachine() : null;
  return machine ? `Reaching ${machine.name} through the relay` : `Connecting to ${location.hostname}`;
}

export function connectingHint(): string {
  if (mode() === "account") return "Arakun must be running on the computer, signed in to the same GitHub account.";
  if (location.protocol === "https:") return "Make sure Tailscale is on, on the phone and on the computer, and Arakun is running.";
  return "Make sure the phone is on the same network and Arakun is running.";
}
