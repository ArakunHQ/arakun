// Over the composer: the prompts the computer holds until the running turn
// ends. Each can be taken back into the box to change or drop. After a Stop
// the queue waits paused, and Resume lets it go again.

import type { Session } from "../../lib/model";
import "./Queue.css";

export interface QueueProps {
  session: Session;
  /** Takes prompt `index` out of the queue, with its text. */
  onTakeBack: (index: number, text: string) => void;
  onResume: () => void;
}

export function Queue({ session: s, onTakeBack, onResume }: QueueProps) {
  const queue = s.queue ?? [];
  if (!queue.length) return null;
  const paused = !!s.queue_paused;
  return (
    <section className="queue" aria-label="Queued prompts">
      <div className="qhead">
        <span>{paused ? "Paused" : "Queued"}</span>
        <span className="grow">{paused ? "Waits until you resume" : "Sends when this turn ends"}</span>
        {paused ? (
          <button type="button" className="btn outline small" onClick={onResume}>
            Resume
          </button>
        ) : null}
      </div>
      {queue.map((text, i) => (
        <div key={`${i}:${text}`} className="qitem">
          <span className="qtext">{text}</span>
          <button type="button" className="btn ghost small" aria-label={`Take back: ${text}`} onClick={() => onTakeBack(i, text)}>
            Take back
          </button>
        </div>
      ))}
    </section>
  );
}
