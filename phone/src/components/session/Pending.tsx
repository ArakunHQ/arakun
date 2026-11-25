// A sent message shows at once, faded, until the computer's own copy of it
// arrives: each prompt entry after the moment of sending takes one away.
// What was written while the computer was away waits here and goes out once
// it is back.

import { useEffect, useRef, useState } from "react";
import type { Link } from "../../lib/link";
import type { Session } from "../../lib/model";
import "./transcript.css";

export interface PendingMessage {
  text: string;
  /** The entry index the computer's copy will have at least. */
  after: number;
  /** Not sent yet: the computer was away. */
  queued: boolean;
}

/** The messages on their way for session `s`, and `add` for a new one. */
export function usePending(link: Link, s: Session | null) {
  const [pending, setPending] = useState<PendingMessage[]>([]);
  // The newest list for the link's events; updaters must not send.
  const now = useRef(pending);
  now.current = pending;

  // Settled by the user entries that came in after each was sent. Only news
  // from the computer settles: a message added here is not its own copy.
  const entries = s?.entries;
  const from = s?.from ?? 0;
  useEffect(() => {
    if (!entries) return;
    setPending((all) => {
      if (!all.length) return all;
      let users = 0;
      entries.forEach((e, i) => {
        if (e.type === "user" && from + i >= all[0].after) users++;
      });
      return users ? all.slice(users) : all;
    });
  }, [entries, from]);

  const id = s?.id;
  useEffect(
    () =>
      link.subscribeEvents((e) => {
        if (e.type !== "linked" || !id || !now.current.some((p) => p.queued)) return;
        for (const p of now.current) if (p.queued) link.send({ type: "send", session: id, text: p.text });
        setPending((all) => all.map((p) => (p.queued ? { ...p, queued: false } : p)));
      }),
    [link, id],
  );

  const add = (p: PendingMessage) => setPending((all) => [...all, p]);
  return { pending, add };
}

export function Pending({ pending }: { pending: PendingMessage[] }) {
  return (
    <>
      {pending.map((p, i) => (
        <div key={i} className="entry user">
          <div className="bubble">
            <div className="text">{p.text}</div>
          </div>
          <div className="meta">
            <span>{p.queued ? "Waiting to send · sends when reconnected" : "Sending…"}</span>
          </div>
        </div>
      ))}
    </>
  );
}
