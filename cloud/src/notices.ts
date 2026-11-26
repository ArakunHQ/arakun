// Notices: a short message the operator writes for one account or for
// everyone. /me carries the unread ones; a person dismisses one by reading it.
//
//   POST /notices/<id>/read   → 204, for a notice the caller can see
//
// The operator's side (`POST /admin/notices`, `DELETE /admin/notices/<id>`) is
// in admin.ts and uses the functions here.

import { record, type Operator } from "./admin_log";
import { session } from "./auth";
import { error, hex, isObject, jsonBody, now, type Env } from "./util";

export const MAX_NOTICES = 5;
const MAX_TEXT = 500;
const MAX_URL = 2048;
const MAX_BODY = 4096;
const READ = /^\/notices\/([^/]+)\/read$/;

export interface Notice {
  id: string;
  text: string;
  url: string | null;
}

/// A notice is visible to the account it names, or to everyone when it names
/// none, until it expires. One condition for /me and for a read.
const VISIBLE = "(n.account_id IS NULL OR n.account_id = ?1) AND (n.expires_at IS NULL OR n.expires_at > ?2)";

/// The answer for a /notices route, or null when the path is not one.
export async function notices(request: Request, env: Env, url: URL): Promise<Response | null> {
  const match = READ.exec(url.pathname);
  if (!match || request.method !== "POST") return null;
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  const id = match[1]!;
  // One statement: a notice that is not visible writes nothing, and a second
  // read of one changes nothing.
  const result = await env.DB.prepare(
    `INSERT INTO notice_reads (account_id, notice_id) SELECT ?1, n.id FROM notices n WHERE n.id = ?3 AND ${VISIBLE} ON CONFLICT DO NOTHING`,
  )
    .bind(s.accountId, now(), id)
    .run();
  if (result.meta.changes === 0) {
    const seen = await env.DB.prepare(
      "SELECT 1 FROM notice_reads r JOIN notices n ON n.id = r.notice_id WHERE r.account_id = ?1 AND r.notice_id = ?3 AND (n.expires_at IS NULL OR n.expires_at > ?2)",
    )
      .bind(s.accountId, now(), id)
      .first();
    if (!seen) return error(404, "no such notice");
  }
  return new Response(null, { status: 204 });
}

/// The account's unread notices for /me, oldest first.
export async function unreadNotices(env: Env, accountId: number): Promise<Notice[]> {
  const rows = await env.DB.prepare(
    `SELECT n.id, n.text, n.url FROM notices n
     WHERE ${VISIBLE} AND NOT EXISTS (SELECT 1 FROM notice_reads r WHERE r.notice_id = n.id AND r.account_id = ?1)
     ORDER BY n.created_at, n.id LIMIT ?3`,
  )
    .bind(accountId, now(), MAX_NOTICES)
    .all<Notice>();
  return rows.results;
}

/// `{ account_id?, text, url?, expires_at? }` from the operator. The text is
/// plain, and the URL is https and stored as the URL parser writes it
/// (`https://host/...`), so a client can open it without a check.
export async function createNotice(request: Request, env: Env, op: Operator): Promise<Response> {
  const body = await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, "expected a JSON object");
  const { account_id: accountId, text, url, expires_at: expiresAt } = body;
  if (typeof text !== "string" || text.length < 1 || text.length > MAX_TEXT) return error(400, `text must be 1 to ${MAX_TEXT} characters`);
  if (url !== undefined && url !== null && !isHttps(url)) return error(400, "url must be an https:// address");
  if (expiresAt !== undefined && expiresAt !== null && (!Number.isSafeInteger(expiresAt) || (expiresAt as number) <= now())) {
    return error(400, "expires_at must be Unix seconds in the future");
  }
  if (accountId !== undefined && accountId !== null && (!Number.isSafeInteger(accountId) || (accountId as number) < 1)) {
    return error(400, "account_id must be a positive integer");
  }
  if (typeof accountId === "number") {
    const user = await env.DB.prepare("SELECT 1 FROM accounts WHERE id = ?").bind(accountId).first();
    if (!user) return error(404, "no such account");
  }
  const id = hex(crypto.getRandomValues(new Uint8Array(16)));
  await env.DB.prepare("INSERT INTO notices (id, account_id, text, url, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?)")
    .bind(id, accountId ?? null, text, typeof url === "string" ? new URL(url).href : null, now(), expiresAt ?? null)
    .run();
  await record(env, op, "notice.create", typeof accountId === "number" ? accountId : null, { notice_id: id });
  return Response.json({ id });
}

export async function deleteNotice(env: Env, id: string, op: Operator): Promise<Response> {
  // Read first for the log: the row is gone after the delete.
  const notice = await env.DB.prepare("SELECT account_id FROM notices WHERE id = ?").bind(id).first<{ account_id: number | null }>();
  // Its reads go with it, in one transaction.
  const [, result] = await env.DB.batch([
    env.DB.prepare("DELETE FROM notice_reads WHERE notice_id = ?").bind(id),
    env.DB.prepare("DELETE FROM notices WHERE id = ?").bind(id),
  ]);
  if (result!.meta.changes === 0) return error(404, "no such notice");
  await record(env, op, "notice.delete", notice?.account_id ?? null, { notice_id: id });
  return new Response(null, { status: 204 });
}

function isHttps(value: unknown): value is string {
  if (typeof value !== "string" || value.length > MAX_URL) return false;
  try {
    return new URL(value).protocol === "https:";
  } catch {
    return false;
  }
}
