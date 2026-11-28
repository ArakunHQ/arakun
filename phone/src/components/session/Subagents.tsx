// A session's subagents: the card a Task or Agent call becomes, and the line
// over the composer while some run in the background. A computer that sends
// no subagents leaves its tool rows as they were.

import { createContext, useContext, useEffect, useReducer } from "react";
import { fmtElapsed, type SessionStatus } from "../../lib/format";
import { Icon } from "../../lib/icons";
import type { Entry, Subagent, SubagentTranscript } from "../../lib/model";
import "./Subagents.css";

/** What a subagent card needs from the screen. */
export interface SubagentHandlers {
  subagents: Subagent[];
  /** The entries the card's latest step is found in. */
  entries: Entry[];
  /** The transcript open, which knows the latest step of its own subagent. */
  open: SubagentTranscript | null;
  onOpen: (id: string) => void;
}

export const SubagentContext = createContext<SubagentHandlers | null>(null);

const LABELS: Record<string, string> = { running: "Running", completed: "Done", failed: "Failed", cancelled: "Stopped" };

/** "Running", "Done", "Failed", "Stopped", as the desktop says it. */
export function stateLabel(state: string): string {
  return LABELS[state] ?? state;
}

/** How long it ran, or has run so far: "28s", "4m 12s". */
export function ranFor(a: Subagent, now: number = Date.now() / 1000): string {
  if (!a.started) return "";
  const end = a.ended ?? (a.state === "running" ? now : null);
  return end == null ? "" : fmtElapsed(Math.max(0, end - a.started) * 1000);
}

const str = (v: unknown) => (typeof v === "string" ? v : "");

/** One line for what entry `e` does or says. */
function stepLine(e: Entry): string {
  switch (e.type) {
    case "tool":
      return e.command ? `Ran ${str(e.command)}` : str(e.title);
    case "thought":
      return "Thinking";
    case "agent":
      return str(e.text).trim().split("\n")[0] ?? "";
    default:
      return "";
  }
}

/**
 * The subagent's latest step: the end of its transcript when it is open, else
 * the last call made under it in the session (OpenCode's and Droid's Task).
 * Claude's run apart, so their steps are only known once opened.
 */
export function latestStep(a: Subagent, entries: Entry[], open: SubagentTranscript | null): string {
  const list = open?.id === a.id ? open.entries : entries.filter((e) => e.type === "tool" && (e.parent === a.call || e.parent === a.id));
  for (let i = list.length - 1; i >= 0; i--) {
    const line = stepLine(list[i]);
    if (line) return line;
  }
  return "";
}

/** A tick each second while `on`, so a running clock moves. */
export function useTicking(on: boolean) {
  const [, tick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (!on) return;
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, [on]);
}

/** The tool row of a Task or Agent call, as a card that opens its subagent's transcript. */
export function SubagentCard({ a }: { a: Subagent }) {
  const h = useContext(SubagentContext);
  const running = a.state === "running";
  useTicking(running);
  const step = h ? latestStep(a, h.entries, h.open) : "";
  const time = ranFor(a);
  const name = a.name || "Subagent";
  return (
    <button type="button" className={`subcard ${a.state}`} aria-label={`${name}, ${stateLabel(a.state)}. Open its transcript`} onClick={() => h?.onOpen(a.id)}>
      <span className="sctop">
        <span className="st" />
        <span className="scname">{name}</span>
        <span className="scstate">{[stateLabel(a.state), time].filter(Boolean).join(" · ")}</span>
        <Icon name="chevron" size={14} />
      </span>
      {a.task ? <span className="sctask">{a.task}</span> : null}
      {step ? <span className="scstep">{step}</span> : null}
    </button>
  );
}

/** How many subagents run on once the turn is over, as the desktop footer says it. */
export function backgroundCount(subagents: Subagent[], status: SessionStatus): number {
  return status === "working" ? 0 : subagents.filter((a) => a.state === "running").length;
}

/** Over the composer: subagents left running after the turn was handed back. */
export function BackgroundLine({ subagents, status }: { subagents: Subagent[]; status: SessionStatus }) {
  const n = backgroundCount(subagents, status);
  if (!n) return null;
  return (
    <div className="bgsub" role="status">
      {n === 1 ? "1 subagent running in the background" : `${n} subagents running in the background`}
    </div>
  );
}
