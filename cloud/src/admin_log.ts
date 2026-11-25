// The record of what the operator changed: one row per change, kept by
// `record()` and read at GET /admin/log. Nothing here edits or deletes a row.
//
//   GET /admin/log?github_id=&before=&limit=  → { entries, next }, newest first
//
// The operator's name comes from two headers that the admin panel sets after
// it has checked who is signed in. A call with the secret alone has none.

import { GITHUB_ID, GITHUB_LOGIN, error, now, type Env } from "./util";

export type LogAction =
  | "flag.grant"
  | "flag.revoke"
  | "sessions.close"
  | "block"
  | "unblock"
  | "plan.set"
  | "notice.create"
  | "notice.delete"
  | "cloud.grant"
  | "cloud.decline";

export interface Operator {
  id: number | null;
  login: string | null;
}

const DEFAULT_LIMIT = 50;
const MAX_LIMIT = 100;

/// From X-Arakun-Operator-Id and X-Arakun-Operator-Login. Both or neither: a
/// name without its ID, or the reverse, could name the wrong person.
export function operator(request: Request): Operator {
  const idText = request.headers.get("X-Arakun-Operator-Id") ?? "";
  const login = request.headers.get("X-Arakun-Operator-Login") ?? "";
  const id = GITHUB_ID.test(idText) ? Number(idText) : NaN;
  if (!Number.isSafeInteger(id) || !GITHUB_LOGIN.test(login)) return { id: null, login: null };
  return { id, login };
}

/// Writes one row. Call it after the change succeeded; if the write fails it
/// throws, and the router answers 500 for a change that did happen.
export async function record(
  env: Env,
  op: Operator,
  action: LogAction,
  githubId: number | null,
  detail?: Record<string, string | number | null>,
): Promise<void> {
  await env.DB.prepare("INSERT INTO admin_log (at, operator_id, operator_login, action, github_id, detail) VALUES (?, ?, ?, ?, ?, ?)")
    .bind(now(), op.id, op.login, action, githubId, detail ? JSON.stringify(detail) : null)
    .run();
}

interface Row {
  id: number;
  at: number;
  operator_id: number | null;
  operator_login: string | null;
  action: string;
  github_id: number | null;
  login: string | null;
  detail: string | null;
}

export async function listLog(env: Env, url: URL): Promise<Response> {
  const query = url.searchParams;
  const limit = query.has("limit") ? positive(query.get("limit")!) : DEFAULT_LIMIT;
  const before = query.has("before") ? positive(query.get("before")!) : Number.MAX_SAFE_INTEGER;
  const account = query.has("github_id") ? positive(query.get("github_id")!) : undefined;
  if (limit === null || limit > MAX_LIMIT) return error(400, `limit must be 1 to ${MAX_LIMIT}`);
  if (before === null) return error(400, "before must be an entry ID");
  if (account === null) return error(400, "github_id must be a positive integer");

  // One more than asked, to know whether another page follows.
  const rows = await env.DB.prepare(
    `SELECT l.id, l.at, l.operator_id, l.operator_login, l.action, l.github_id, u.login, l.detail
     FROM admin_log l LEFT JOIN users u ON u.github_id = l.github_id
     WHERE l.id < ?1 AND (?2 IS NULL OR l.github_id = ?2)
     ORDER BY l.id DESC LIMIT ?3`,
  )
    .bind(before, account ?? null, limit + 1)
    .all<Row>();
  const page = rows.results.slice(0, limit);
  return Response.json({
    entries: page.map((row) => ({ ...row, detail: row.detail === null ? null : (JSON.parse(row.detail) as unknown) })),
    next: rows.results.length > limit ? page[page.length - 1]!.id : null,
  });
}

/// A positive integer from a query value, or null.
function positive(text: string): number | null {
  const n = GITHUB_ID.test(text) ? Number(text) : NaN;
  return Number.isSafeInteger(n) ? n : null;
}
