export type SessionStatus = "working" | "needs_input" | "error" | "idle";

/** How long ago a unix time was: "now", "5m", "3h", "2d"; "" when missing. */
export function ago(unix: number, now: number = Date.now() / 1000): string {
  if (!unix) return "";
  const d = Math.max(0, now - unix);
  if (d < 60) return "now";
  if (d < 3600) return `${Math.floor(d / 60)}m`;
  if (d < 86400) return `${Math.floor(d / 3600)}h`;
  return `${Math.floor(d / 86400)}d`;
}

/** The time of day of a unix moment, as the phone's clock shows it. */
export function clock(unix: number): string {
  return new Date(unix * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** "28s", "4m", "4m 12s": how long a turn took, as the computer writes it. */
export function fmtElapsed(ms: number): string {
  const secs = Math.max(1, Math.round(ms / 1000));
  if (secs < 60) return `${secs}s`;
  return secs % 60 ? `${Math.floor(secs / 60)}m ${secs % 60}s` : `${secs / 60}m`;
}

const LABELS: Record<SessionStatus, string> = {
  working: "Working",
  needs_input: "Needs you",
  error: "Error",
  idle: "Done",
};

export function statusLabel(s: SessionStatus): string {
  return LABELS[s] ?? s;
}

/** "+3 −2" as plain text. The old one made HTML; a component styles the parts now. */
export function counts(added: number, removed: number): string {
  return [added ? `+${added}` : "", removed ? `−${removed}` : ""].filter(Boolean).join(" ");
}
