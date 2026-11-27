// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The daily job that keeps a quiet Linear workspace's install working.
// Linear's refresh token lasts 30 days from its last use, and the Worker
// otherwise uses it only when someone delegates an issue or a computer
// reports progress.

import { configured } from "./linear";
import { keepAlive, LinearRefused } from "./linear_api";
import { now, type Env } from "./util";

/// A workspace whose tokens are older than this is refreshed. Well inside 30
/// days, so a few days of failed runs still leave time.
export const KEEP_SECONDS = 7 * 24 * 3600;
/// The most refreshed in one run, each one call to Linear: a Worker run has a
/// limit on its calls. The oldest go first; the rest wait a day.
const MAX_PER_RUN = 50;

/// Refreshes the workspaces that have not been for KEEP_SECONDS. One that
/// Linear refuses is marked by the refresh; one that fails otherwise is tried
/// again tomorrow. Never throws, so one workspace cannot stop the rest.
export async function keepWorkspaces(env: Env): Promise<void> {
  if (!configured(env)) return;
  const rows = await env.DB.prepare(
    `SELECT organization_id FROM linear_workspaces WHERE refused_at IS NULL AND refreshed_at < ?
     ORDER BY refreshed_at, organization_id LIMIT ${MAX_PER_RUN}`,
  )
    .bind(now() - KEEP_SECONDS)
    .all<{ organization_id: string }>();
  for (const { organization_id: org } of rows.results) {
    try {
      await keepAlive(env, org);
    } catch (e) {
      // The status or the error's name only: a message could quote Linear.
      const why = e instanceof LinearRefused ? e.status : e instanceof Error ? e.name : "unknown";
      console.error(`linear keep-alive of ${org}: ${why}`);
    }
  }
}
