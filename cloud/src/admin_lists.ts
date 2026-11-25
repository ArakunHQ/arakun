// What the operator reads about accounts. These routes only read, so they
// write nothing to the admin log.
//
//   GET /admin/users?q=&after=&limit=  → { users, next }, last seen first
//   GET /admin/users/<github_id>       → one account with its sessions, machines,
//                                        cloud request and notices
//   GET /admin/notices                 → every notice, newest first, at most 500
//   GET /admin/plans                   → { plans }, the plan names in the KV `plans`
//
// A session's token hash never leaves the Worker.

import { requestFor } from "./cloud_access";
import { GITHUB_ID, GITHUB_LOGIN, error, type Env } from "./util";

const DEFAULT_LIMIT = 50;
const MAX_LIMIT = 100;
const MAX_NOTICES = 500;
/// A plan's name, as in admin.ts.
const PLAN = /^[a-z][a-z0-9_]{0,31}$/;
const CURSOR = /^(0|[1-9][0-9]{0,15})_([1-9][0-9]{0,15})$/;

export interface NoticeRow {
  id: string;
  github_id: number | null;
  login: string | null;
  text: string;
  url: string | null;
  created_at: number;
  expires_at: number | null;
  /// How many accounts marked it read.
  reads: number;
}

interface UserDb {
  github_id: number;
  login: string;
  plan: string;
  blocked_at: number | null;
  blocked_reason: string | null;
  created_at: number;
  last_seen_at: number;
  machines: number;
  flags: string | null;
}

const USER_COLUMNS = `u.github_id, u.login, u.plan, u.blocked_at, u.blocked_reason, u.created_at, u.last_seen_at,
  (SELECT count(*) FROM machines m WHERE m.github_id = u.github_id) AS machines,
  (SELECT group_concat(flag) FROM (SELECT flag FROM user_flags f WHERE f.github_id = u.github_id ORDER BY flag)) AS flags`;

function userRow({ flags, ...row }: UserDb) {
  return { ...row, flags: flags === null ? [] : flags.split(",") };
}

export async function listUsers(env: Env, url: URL): Promise<Response> {
  const query = url.searchParams;
  const limit = query.has("limit") ? (GITHUB_ID.test(query.get("limit")!) ? Number(query.get("limit")) : 0) : DEFAULT_LIMIT;
  if (limit < 1 || limit > MAX_LIMIT) return error(400, `limit must be 1 to ${MAX_LIMIT}`);
  let after: RegExpExecArray | null = null;
  if (query.has("after")) {
    after = CURSOR.exec(query.get("after")!);
    // 16 digits can pass MAX_SAFE_INTEGER, and a rounded cursor would skip rows.
    if (!after || !Number.isSafeInteger(Number(after[1])) || !Number.isSafeInteger(Number(after[2]))) {
      return error(400, "after must be a cursor from a previous answer");
    }
  }
  const q = query.get("q");
  if (q !== null && !GITHUB_LOGIN.test(q)) return error(400, "q is 1 to 39 of A-Z, a-z, 0-9, _ and -");

  // `_` is a wildcard in LIKE; a login may hold it. `\` and `%` cannot pass the check above.
  const prefix = q === null ? null : `${q.replaceAll("_", "\\_")}%`;
  const exactId = q !== null && GITHUB_ID.test(q) && Number.isSafeInteger(Number(q)) ? Number(q) : null;
  // One more than asked, to know whether another page follows.
  const rows = await env.DB.prepare(
    `SELECT ${USER_COLUMNS} FROM users u
     WHERE (?1 IS NULL OR u.login LIKE ?1 ESCAPE '\\' OR u.github_id = ?2)
       AND (?3 IS NULL OR u.last_seen_at < ?3 OR (u.last_seen_at = ?3 AND u.github_id < ?4))
     ORDER BY u.last_seen_at DESC, u.github_id DESC LIMIT ?5`,
  )
    .bind(prefix, exactId, after ? Number(after[1]) : null, after ? Number(after[2]) : null, limit + 1)
    .all<UserDb>();
  const page = rows.results.slice(0, limit).map(userRow);
  const last = page[page.length - 1];
  return Response.json({ users: page, next: rows.results.length > limit && last ? `${last.last_seen_at}_${last.github_id}` : null });
}

export async function getUser(env: Env, idText: string): Promise<Response> {
  const id = GITHUB_ID.test(idText) ? Number(idText) : NaN;
  if (!Number.isSafeInteger(id)) return error(400, "the GitHub ID must be a positive integer");
  const db = env.DB;
  const [user, sessions, machines, notices] = await db.batch([
    db.prepare(`SELECT ${USER_COLUMNS}, u.email FROM users u WHERE u.github_id = ?`).bind(id),
    db
      .prepare("SELECT id, name, kind, created_at, last_used_at, expires_at, machine_label FROM sessions WHERE github_id = ? ORDER BY last_used_at DESC, id")
      .bind(id),
    db.prepare("SELECT label, name, kind, created_at, last_seen_at FROM machines WHERE github_id = ? ORDER BY last_seen_at DESC, label").bind(id),
    db
      .prepare(
        `SELECT n.id, n.github_id, u.login, n.text, n.url, n.created_at, n.expires_at,
           (SELECT count(*) FROM notice_reads r WHERE r.notice_id = n.id) AS reads
         FROM notices n LEFT JOIN users u ON u.github_id = n.github_id
         WHERE n.github_id = ? ORDER BY n.created_at DESC, n.id`,
      )
      .bind(id),
  ]);
  const found = user!.results[0] as (UserDb & { email: string | null }) | undefined;
  if (!found) return error(404, "no such user");
  return Response.json({
    user: userRow(found),
    sessions: sessions!.results,
    machines: machines!.results,
    cloud_request: await requestFor(env, id),
    notices: notices!.results as unknown as NoticeRow[],
  });
}

export async function listNotices(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    `SELECT n.id, n.github_id, u.login, n.text, n.url, n.created_at, n.expires_at,
       (SELECT count(*) FROM notice_reads r WHERE r.notice_id = n.id) AS reads
     FROM notices n LEFT JOIN users u ON u.github_id = n.github_id
     ORDER BY n.created_at DESC, n.id LIMIT ?`,
  )
    .bind(MAX_NOTICES)
    .all<NoticeRow>();
  return Response.json(rows.results);
}

export async function listPlans(env: Env): Promise<Response> {
  // A KV that cannot be read must not hide `free`, which every account can fall back to.
  const plans = await env.CONFIG.get<Record<string, unknown>>("plans", "json").catch(() => null);
  const names = plans && typeof plans === "object" ? Object.keys(plans).filter((name) => PLAN.test(name) && name !== "free") : [];
  return Response.json({ plans: ["free", ...names.sort()] });
}
