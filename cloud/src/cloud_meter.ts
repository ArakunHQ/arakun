// Metering of cloud machines by awake time. The clock runs from a container's
// start to the stop after its snapshot, and not while it sleeps on one. The
// machine's object (cloud_machine.ts) adds what ran to `cloud_usage` at every
// check and at the sleep, and stops the container at the first limit it meets:
//
//   stopped_all     the operator stopped every cloud machine (KV `cloud_stopped`)
//   held            the operator stopped this account's (`cloud_holds`)
//   allowance_used  the account used its plan's hours for this UTC month
//   task_cap        one task ran its plan's hours: a task is one stretch
//                   awake, from a wake to the sleep
//
// The first three keep the machine asleep until they lift. After the task cap a
// device that knocks does not wake it, or a phone left open would start the
// next task at once; the owner wakes it with POST /cloud/machines/<label>/wake.
//
// The allowance and the cap come from the account's plan (relay/limits.ts),
// so KV moves them without a deploy.

import { planFor } from "./relay/limits";
import { hex, now as nowSeconds, type Env } from "./util";

export type Limit = "stopped_all" | "held" | "allowance_used" | "task_cap";

/// The KV key of the switch for everyone: any text stops every cloud machine
/// and is the reason shown; `true` stops them with none.
export const STOP_ALL_KEY = "cloud_stopped";

const MINUTE_MS = 60_000;
/// How long the notice about a stop stays up unread.
const NOTICE_DAYS = 7;

/// What the account may still use, and whether the operator stopped it.
export interface Budget {
  /// Why every cloud machine is stopped, or null while they may run.
  stoppedAll: string | null;
  /// Whether this account's are stopped, and why (the reason may be null).
  held: { reason: string | null } | null;
  /// The UTC month, such as `2026-11`.
  month: string;
  allowanceMs: number;
  usedMs: number;
  taskCapMs: number;
}

/// The UTC month of a time in Unix milliseconds, such as `2026-11`.
export function monthOf(t: number): string {
  return new Date(t).toISOString().slice(0, 7);
}

/// When the UTC month after `t` begins, in Unix milliseconds.
export function nextMonthStart(t: number): number {
  const d = new Date(t);
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1);
}

/// The milliseconds between `from` and `to` in each UTC month they cross, so
/// a container awake at midnight on the 1st counts in both months.
export function byMonth(from: number, to: number): [string, number][] {
  const out: [string, number][] = [];
  let at = from;
  while (at < to) {
    const end = Math.min(to, nextMonthStart(at));
    out.push([monthOf(at), end - at]);
    at = end;
  }
  return out;
}

/// Adds the time awake between `from` and `to` to the account's months.
export async function addAwake(env: Env, accountId: number, from: number, to: number): Promise<void> {
  const parts = byMonth(from, to);
  if (parts.length === 0) return;
  await env.DB.batch(
    parts.map(([month, ms]) =>
      env.DB.prepare(
        `INSERT INTO cloud_usage (account_id, month, awake_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT (account_id, month) DO UPDATE SET awake_ms = awake_ms + ?3`,
      ).bind(accountId, month, ms),
    ),
  );
}

/// Why the operator stopped every cloud machine, or null while they may run.
export async function stoppedAll(kv: KVNamespace): Promise<string | null> {
  const text = await kv.get(STOP_ALL_KEY).catch(() => null);
  if (text === null || text === "" || text === "false") return null;
  return text === "true" ? "" : text;
}

/// The account's budget at `now` (Unix milliseconds) under its plan.
export async function budgetOf(env: Env, accountId: number, plan: string, now: number): Promise<Budget> {
  const month = monthOf(now);
  const [all, limits, rows] = await Promise.all([
    stoppedAll(env.CONFIG),
    planFor(env.CONFIG, plan),
    env.DB.batch([
      env.DB.prepare("SELECT awake_ms FROM cloud_usage WHERE account_id = ? AND month = ?").bind(accountId, month),
      env.DB.prepare("SELECT reason FROM cloud_holds WHERE account_id = ?").bind(accountId),
    ]),
  ]);
  const used = (rows[0]!.results[0] as { awake_ms: number } | undefined)?.awake_ms ?? 0;
  const hold = rows[1]!.results[0] as { reason: string | null } | undefined;
  return {
    stoppedAll: all,
    held: hold ? { reason: hold.reason } : null,
    month,
    allowanceMs: limits.cloudMinutes * MINUTE_MS,
    usedMs: used,
    taskCapMs: limits.cloudTaskMinutes * MINUTE_MS,
  };
}

/// The first limit the budget meets with `taskMs` run in this task, or null.
export function limitOf(b: Budget, taskMs: number): Limit | null {
  if (b.stoppedAll !== null) return "stopped_all";
  if (b.held) return "held";
  if (b.usedMs >= b.allowanceMs) return "allowance_used";
  if (taskMs >= b.taskCapMs) return "task_cap";
  return null;
}

/// How long the machine may still run before it meets a limit.
export function msLeft(b: Budget, taskMs: number): number {
  return Math.max(0, Math.min(b.allowanceMs - b.usedMs, b.taskCapMs - taskMs));
}

/// What the owner reads about a stop: on a refused wake, and in a notice.
export function limitText(limit: Limit, b: Budget): string {
  switch (limit) {
    case "stopped_all":
      return `Cloud environments are stopped for now${b.stoppedAll ? `: ${b.stoppedAll}` : "."}`;
    case "held":
      return `The operator stopped this account's cloud environments${b.held?.reason ? `: ${b.held.reason}` : "."}`;
    case "allowance_used":
      return `This account used its ${hours(b.allowanceMs)} of cloud time for this month. Cloud environments can run again on ${new Date(nextMonthStart(Date.parse(`${b.month}-01T00:00:00Z`))).toISOString().slice(0, 10)}.`;
    case "task_cap":
      return `The cloud environment stopped after ${hours(b.taskCapMs)}, the most one task may run. Wake it to go on.`;
  }
}

/// A limit as people say it: whole minutes under an hour, else hours to a tenth.
function hours(ms: number): string {
  if (ms < 3_600_000) {
    const m = Math.round(ms / MINUTE_MS);
    return m === 1 ? "1 minute" : `${m} minutes`;
  }
  const h = Math.round((ms / 3_600_000) * 10) / 10;
  return h === 1 ? "1 hour" : `${h} hours`;
}

/// Tells the owner why their machine stopped, for the stops they did not ask
/// for and may not be watching: the task cap and the allowance. The operator
/// writes their own notice when they stop someone.
export async function noticeStop(env: Env, accountId: number, limit: Limit, b: Budget): Promise<void> {
  if (limit !== "task_cap" && limit !== "allowance_used") return;
  const t = nowSeconds();
  await env.DB.prepare("INSERT INTO notices (id, account_id, text, url, created_at, expires_at) VALUES (?, ?, ?, NULL, ?, ?)")
    .bind(hex(crypto.getRandomValues(new Uint8Array(16))), accountId, limitText(limit, b), t, t + NOTICE_DAYS * 86_400)
    .run();
}
