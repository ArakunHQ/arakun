// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// What happens to a Linear delivery the computer never took. It lives apart
// from linear.ts because the relay calls it, and linear.ts reaches the relay
// through machines.ts: importing it from the relay would make a cycle.

import { LinearRefused, postActivity } from "./linear_api";
import { now, type Env } from "./util";

const dropped = (machine: string) => `${machine} stayed offline for 7 days, so this was dropped. Delegate it again when the computer is on.`;

/// The relay dropped delivery `id` after a week unanswered. The row says so,
/// and a delegation the computer never took says so in its session. A row
/// the computer accepted is left alone: the relay can still hold one whose
/// drop failed, and marking it would refuse the session's follow-ups. Never
/// throws: the relay's alarm must go on.
export async function expired(env: Env, id: string): Promise<void> {
  try {
    const row = await env.DB.prepare(
      `SELECT d.kind, d.agent_session_id, d.organization_id, d.accepted_at, d.expired_at, m.name AS machine
       FROM linear_deliveries d LEFT JOIN machines m ON m.label = d.label AND m.account_id = d.account_id
       WHERE d.id = ?`,
    )
      .bind(id)
      .first<{ kind: string; agent_session_id: string; organization_id: string; accepted_at: number | null; expired_at: number | null; machine: string | null }>();
    if (!row || row.expired_at !== null || row.accepted_at !== null) return;
    // Only the call that marks it speaks, so the session hears it once. The
    // accept may have landed since the read.
    const marked = await env.DB.prepare("UPDATE linear_deliveries SET expired_at = ? WHERE id = ? AND expired_at IS NULL AND accepted_at IS NULL")
      .bind(now(), id)
      .run();
    if (marked.meta.changes === 0) return;
    // A follow-up's session already heard from Arakun.
    if (row.kind !== "created") return;
    await postActivity(env, row.organization_id, row.agent_session_id, "error", dropped(row.machine ?? "This computer"));
  } catch (e) {
    // The status or the error's name only: a message could quote Linear.
    const why = e instanceof LinearRefused ? e.status : e instanceof Error ? e.name : "unknown";
    console.error(`linear expiry of ${id}: ${why}`);
  }
}
