// A sent message shows at once, faded, until the computer's own copy of it
// arrives: each prompt entry after the moment of sending takes one away.
// What was written while the computer was away waits here and goes out once
// it is back. One the computer holds in its queue shows there instead. One
// that never reached a server's session stays, marked, to send again or take back.

import { useEffect, useRef, useState } from "react";
import type { Link } from "../../lib/link";
import type { Session } from "../../lib/model";
import type { Shot } from "../../lib/shots";
import "./transcript.css";

export interface PendingMessage {
  text: string;
  /** The entry index the computer's copy will have at least. */
  after: number;
  /** Not sent yet: the computer was away. */
  queued: boolean;
  /** This phone's id for the prompt, which the computer names when it did not get there. */
  sent?: string;
  /** Why it did not reach the session; set, it waits for Retry or Take back. */
  failed?: string;
  /** What was written, without the stand-in a message of photos alone gets. */
  typed?: string;
  /** The photos it carried, sent again on Retry. */
  shots?: Shot[];
}

/** A new id for a prompt this phone sends. */
export function sentId(): string {
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

/** The messages on their way for session `s`, and `add` for a new one. */
export function usePending(link: Link, s: Session | null) {
  const [pending, setPending] = useState<PendingMessage[]>([]);
  // The newest list for the link's events; updaters must not send.
  const now = useRef(pending);
  now.current = pending;

  // Settled by the user entries that came in after each was sent. Only news
  // from the computer settles: a message added here is not its own copy. A
  // failed one has no copy coming, so it is not counted.
  const entries = s?.entries;
  const from = s?.from ?? 0;
  useEffect(() => {
    if (!entries) return;
    setPending((all) => {
      const first = all.find((p) => !p.failed);
      if (!first) return all;
      let users = 0;
      entries.forEach((e, i) => {
        if (e.type === "user" && from + i >= first.after) users++;
      });
      if (!users) return all;
      return all.filter((p) => p.failed || users-- <= 0);
    });
  }, [entries, from]);

  const id = s?.id;
  useEffect(
    () =>
      link.subscribeEvents((e) => {
        if (e.type === "unsent") {
          if (e.session !== id) return;
          setPending((all) => all.map((p) => (p.sent === e.sent ? { ...p, failed: e.message || "Not sent" } : p)));
          return;
        }
        if (e.type !== "linked" || !id || !now.current.some((p) => p.queued)) return;
        for (const p of now.current) {
          if (p.queued) link.send({ type: "send", session: id, text: p.text, ...(p.sent ? { sent: p.sent } : {}) });
        }
        setPending((all) => all.map((p) => (p.queued ? { ...p, queued: false } : p)));
      }),
    [link, id],
  );

  const add = (p: PendingMessage) => setPending((all) => [...all, p]);
  // Taken back from the queue: its copy will never come.
  const drop = (text: string) =>
    setPending((all) => {
      const i = all.findIndex((p) => !p.queued && !p.failed && p.text === text);
      return i < 0 ? all : all.filter((_, k) => k !== i);
    });
  // A failed message taken off the list, for Retry or Take back.
  const take = (sent: string) => {
    const p = now.current.find((m) => m.failed && m.sent === sent);
    if (p) setPending((all) => all.filter((m) => m.sent !== sent));
    return p;
  };
  return { pending, add, drop, take };
}

/**
 * `queued`: what the computer's queue holds, shown over the composer and not here.
 * `onRetry` and `onTakeBack` act on a message that did not get there.
 */
export function Pending({
  pending,
  queued = [],
  onRetry,
  onTakeBack,
}: {
  pending: PendingMessage[];
  queued?: string[];
  onRetry?: (p: PendingMessage) => void;
  onTakeBack?: (p: PendingMessage) => void;
}) {
  const held = [...queued];
  const shown = pending.filter((p) => {
    const i = p.queued || p.failed ? -1 : held.indexOf(p.text);
    if (i >= 0) held.splice(i, 1);
    return i < 0;
  });
  return (
    <>
      {shown.map((p, i) => (
        <div key={p.sent ?? i} className={p.failed ? "entry user unsent" : "entry user"}>
          <div className="bubble">
            <div className="text">{p.text}</div>
          </div>
          <div className="meta">
            {p.failed ? (
              <>
                <span className="why">{p.failed}</span>
                {onTakeBack && (
                  <button type="button" className="btn outline small" aria-label={`Take back: ${p.text}`} onClick={() => onTakeBack(p)}>
                    Take back
                  </button>
                )}
                {onRetry && (
                  <button type="button" className="btn outline small" aria-label={`Retry: ${p.text}`} onClick={() => onRetry(p)}>
                    Retry
                  </button>
                )}
              </>
            ) : (
              <span>{p.queued ? "Waiting to send · sends when reconnected" : "Sending…"}</span>
            )}
          </div>
        </div>
      ))}
    </>
  );
}
