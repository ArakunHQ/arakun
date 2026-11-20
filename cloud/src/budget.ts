// Running time includes boot and shutdown. Waiting for CI or a person has no lease.
export const HOUR = 3_600_000;
export const MONTH_LIMIT = 20 * HOUR;
export const TASK_LIMIT = 2 * HOUR;
// The two fixed rehearsal steps need seconds, not the full task allowance.
export const STEP_LIMIT = 120_000;
export const STANDARD1_USD_HOUR_MAX = 0.074016;

export function reservation(now: number, monthUsed: number, taskUsed: number, limit = TASK_LIMIT) {
  const date = new Date(now);
  const nextMonth = Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 1);
  return Math.max(0, Math.min(STEP_LIMIT, MONTH_LIMIT - monthUsed, Math.min(limit, TASK_LIMIT) - taskUsed, nextMonth - now));
}

export type Phase = "new" | "running" | "waiting_ci" | "needs_decision" | "paused" | "done" | "interrupted";
export type Action = "start" | "ci-green" | "ci-red" | "ack" | "retry";
export function workspaceTransition(phase: Phase, action: string, hasCheckpoint: boolean) {
  if (action === "start") return phase === "new" ? "capture" : "unchanged";
  if (action === "resume" && phase === "paused") return "restore";
  if (action === "resume" && phase === "done") return "unchanged";
  if (action === "retry" && phase === "interrupted") return hasCheckpoint ? "restore" : "capture";
  return "conflict";
}

export function transition(phase: Phase, action: Action, hasCheckpoint = false): "review" | "test" | "done" | "unchanged" | "conflict" {
  if (action === "retry" && phase === "interrupted") return hasCheckpoint ? "test" : "review";
  if (action === "start") return phase === "new" ? "review" : "unchanged";
  if (phase === "waiting_ci" && action === "ci-red") return "test";
  if ((phase === "waiting_ci" && action === "ci-green") || (phase === "needs_decision" && action === "ack")) return "done";
  if (phase === "done" || (phase === "needs_decision" && action === "ci-red")) return "unchanged";
  return "conflict";
}
