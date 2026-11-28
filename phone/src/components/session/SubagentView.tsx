// A subagent's transcript, opened from its card: read-only, over the session,
// with a way back to it. It is drawn by the session's own Transcript.

import { useMemo } from "react";
import { Icon } from "../../lib/icons";
import type { Session, Subagent, SubagentTranscript } from "../../lib/model";
import { ranFor, stateLabel, useTicking } from "./Subagents";
import { Transcript } from "./Transcript";
import "./Subagents.css";

export interface SubagentViewProps {
  /** The session's agent, to fall back on for a name. */
  agent: string;
  /** From the list; missing when the computer has not listed it (yet). */
  subagent: Subagent | undefined;
  transcript: SubagentTranscript;
  loadingOlder: boolean;
  onLoadOlder: () => void;
  onBack: () => void;
}

/** A subagent's transcript over the session, read-only, with a way back. */
export function SubagentView({ agent, subagent: a, transcript: t, loadingOlder, onLoadOlder, onBack }: SubagentViewProps) {
  const running = a?.state === "running";
  useTicking(running);
  const name = a?.name || "Subagent";
  // The transcript draws a session; this one has no title or composer.
  const s: Session = useMemo(
    () => ({
      id: t.id,
      title: name,
      agent: name || agent,
      status: running ? "working" : "idle",
      count: t.count,
      from: t.from,
      entries: t.entries,
      permission: null,
      question: null,
      place: "",
    }),
    [t, name, agent, running],
  );
  const time = a ? ranFor(a) : "";
  const empty = !t.loaded ? "Loading…" : t.count ? "" : running ? "Waiting for its first step…" : "Nothing in it.";
  return (
    <div className="subview" role="dialog" aria-label={`${name} transcript`}>
      <div className="hdr">
        <button type="button" className="iconbtn" aria-label="Back to the session" onClick={onBack}>
          <Icon name="back" />
        </button>
        <div className="grow">
          <div className="title">{name}</div>
          <div className="sub">{a ? [stateLabel(a.state), time].filter(Boolean).join(" · ") : ""}</div>
        </div>
      </div>
      <div className="older" style={t.from > 0 ? undefined : { display: "none" }}>
        {t.from > 0 ? (loadingOlder ? "Loading…" : `${t.from} earlier`) : ""}
      </div>
      {empty ? <div className="subempty">{empty}</div> : null}
      <Transcript session={s} loadingOlder={loadingOlder} onLoadOlder={onLoadOlder} pending={null} onHoldBubble={noop} />
    </div>
  );
}

function noop() {}
