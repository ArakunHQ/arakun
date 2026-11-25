// Over the composer while the agent works: the clock, what it costs so far,
// and Stop. Stopping takes two taps a few seconds apart: a thumb brushes the
// screen too easily. With something written, Stop stops the turn and sends it.

import { useEffect, useReducer } from "react";
import { Icon } from "../../lib/icons";
import type { Session } from "../../lib/model";
import { useArmed } from "./PermissionCard";

export const STOP_WINDOW = 3000;

/** mm:ss, hours when it comes to that. */
export function fmtClock(ms: number): string {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const sec = total % 60;
  const two = (n: number) => String(n).padStart(2, "0");
  return h ? `${h}:${two(m)}:${two(sec)}` : `${two(m)}:${two(sec)}`;
}

/** How long the turn has run: what the computer last said, plus the time since. */
export function liveElapsed(s: Session, now = Date.now()): number | null {
  if (s.elapsed_ms == null) return null;
  return s.elapsed_ms + (s.elapsedAt ? now - s.elapsedAt : 0);
}

export function usageSummary(s: Session): string {
  const u = s.usage as { summary?: string } | null | undefined;
  return u?.summary || "";
}

export interface StopRowProps {
  session: Session;
  /** Something is written: Stop also sends it. */
  hasDraft: boolean;
  /** The second tap in time. */
  onStop: () => void;
}

export function StopRow({ session: s, hasDraft, onStop }: StopRowProps) {
  const working = s.status === "working";
  const [armed, tap, disarm] = useArmed(STOP_WINDOW);
  const [, tick] = useReducer((n: number) => n + 1, 0);
  // The clock runs on the phone between the computer's updates.
  useEffect(() => {
    if (!working) return disarm();
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, [working, disarm]);
  if (!working) return null;
  const bits: string[] = [];
  const ms = liveElapsed(s);
  if (ms != null) bits.push(fmtClock(ms));
  const summary = usageSummary(s);
  if (summary) bits.push(summary);
  const [word, again, hint] = hasDraft ? ["Stop and send", "Send now?", "Tap again to stop and send"] : ["Stop", "Stop?", "Tap again to stop"];
  return (
    <div className="stoprow">
      <span className="live">{bits.join(" · ") || "Working…"}</span>
      <button type="button" className={`btn danger small${armed ? " armed" : ""}`} aria-label={armed ? hint : word} onClick={() => tap() && onStop()}>
        <Icon name="stop" />
        <span>{armed ? again : word}</span>
      </button>
    </div>
  );
}
