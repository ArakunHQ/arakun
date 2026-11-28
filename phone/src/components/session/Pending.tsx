// A sent message shows at once, faded, until the computer's own copy of it
// arrives: each prompt entry after the moment of sending takes one away.
// What was written while the computer was away waits here and goes out once
// it is back. One the computer holds in its queue shows there instead.

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
  // Taken back from the queue: its copy will never come.
  const drop = (text: string) =>
    setPending((all) => {
      const i = all.findIndex((p) => !p.queued && p.text === text);
      return i < 0 ? all : all.filter((_, k) => k !== i);
    });
  return { pending, add, drop };
}

/** `queued`: what the computer's queue holds, shown over the composer and not here. */
export function Pending({ pending, queued = [] }: { pending: PendingMessage[]; queued?: string[] }) {
  const held = [...queued];
  const shown = pending.filter((p) => {
    const i = p.queued ? -1 : held.indexOf(p.text);
    if (i >= 0) held.splice(i, 1);
    return i < 0;
  });
  return (
    <>
      {shown.map((p, i) => (
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
