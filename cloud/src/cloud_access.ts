// Access to cloud environments. They run on CPU we pay for, so they are given
// by hand, one grant per GitHub account: a person with many accounts must not
// get many allowances. An account asks here; the operator grants or declines
// at /admin/cloud/requests (admin.ts calls the functions at the bottom).
//
//   GET  /cloud/access  → { status, requested_at, decided_at, retry_at }
//   POST /cloud/access  → 201 with the same, after asking, { reason? }
//
// `status` is "none", "pending", "granted" or "declined"; the times are null
// when they do not apply. `retry_at` is set only for "declined": when the
// account may ask again. The `cloud` flag is the truth for "granted": a
// request marked granted whose flag was revoked later reads as "none", and the
// account may ask again. To keep someone out, block the account, or decline
// the request they make next.

import { record, type Operator } from "./admin_log";
import { githubHeaders, githubIdOf, GITHUB_TIMEOUT_MS, session } from "./auth";
import { DAY, GITHUB_ID_OF, NUMERIC_ID, GITHUB_LOGIN, discard, error, isObject, jsonBody, now, type Env } from "./util";

/// The flag that lets an account make cloud environments.
export const CLOUD_FLAG = "cloud";
/// How long a declined account waits before it may ask again. Long enough that
/// asking again and again does not fill the operator's list; short enough that
/// a new account, declined for having no history yet, can come back once it
/// has some.
export const DECLINE_COOLDOWN_DAYS = 30;
export const MAX_REASON = 500;
/// Who decided, as the operator writes it.
const MAX_BY = 100;
/// The operator's list stops here; older decisions stay in the table.
const MAX_LIST = 500;
const MAX_BODY = 4096;
const STATUSES = ["pending", "granted", "declined"] as const;

export type AccessStatus = "none" | (typeof STATUSES)[number];

/// What GitHub says about an account, for the reviewer. Null fields when it did
/// not answer.
export interface Signals {
  login: string | null;
  created_at: number | null;
  public_repos: number | null;
  followers: number | null;
  following: number | null;
}

/// What the account sees about its own access.
export interface Access {
  status: AccessStatus;
  requested_at: number | null;
  decided_at: number | null;
  retry_at: number | null;
}

/// The account's request, if any, and its flag. The request's fields are null
/// when it has none.
interface Row {
  status: (typeof STATUSES)[number] | null;
  requested_at: number | null;
  decided_at: number | null;
  flagged: number;
}

/// The answer for a /cloud route, or null when the path is not one.
export async function cloudAccess(request: Request, env: Env, url: URL, ctx: ExecutionContext): Promise<Response | null> {
  if (url.pathname !== "/cloud/access") return null;
  if (request.method === "GET") return getAccess(request, env);
  if (request.method === "POST") return requestAccess(request, env, ctx);
  return error(404, "not found");
}

async function getAccess(request: Request, env: Env): Promise<Response> {
  // A cloud session counts as none here, as everywhere but its own door.
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  return Response.json(answer(await readRow(env, s.accountId)));
}

async function requestAccess(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  if (s.blocked) return error(403, "blocked");
  let limited: boolean;
  try {
    limited = !(await env.CLOUD_ACCESS_LIMIT.limit({ key: String(s.accountId) })).success;
  } catch (e) {
    console.error("cloud access limit failed:", e);
    return error(503, "the server could not check the request: try again");
  }
  if (limited) return error(429, "too many requests, try again in a minute");
  const body = request.headers.get("Content-Length") === "0" || !request.body ? {} : await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, 'expected {"reason": "..."} or no body');
  const reason = body["reason"];
  if (reason !== undefined && reason !== null && (typeof reason !== "string" || reason.length > MAX_REASON)) {
    return error(400, `reason must be text of at most ${MAX_REASON} characters`);
  }

  const current = answer(await readRow(env, s.accountId));
  const refused = refusal(current);
  if (refused) return refused;

  const t = now();
  const signals = await githubSignals(await githubIdOf(env, s.accountId), env);
  const login = signals.login ?? s.login;
  const text = typeof reason === "string" && reason.trim().length > 0 ? reason.trim() : null;
  // The conditions are those checked above, again in the one statement that
  // writes, so two requests at once make one request and one notification.
  const result = await env.DB.prepare(
    `INSERT INTO cloud_access_requests
       (account_id, status, requested_at, decided_at, decided_by, reason, gh_login, gh_created_at, gh_public_repos, gh_followers, gh_following)
     VALUES (?1, 'pending', ?2, NULL, NULL, ?3, ?4, ?5, ?6, ?7, ?8)
     ON CONFLICT (account_id) DO UPDATE SET
       status = 'pending', requested_at = excluded.requested_at, decided_at = NULL, decided_by = NULL, reason = excluded.reason,
       gh_login = excluded.gh_login, gh_created_at = excluded.gh_created_at, gh_public_repos = excluded.gh_public_repos,
       gh_followers = excluded.gh_followers, gh_following = excluded.gh_following
     WHERE NOT EXISTS (SELECT 1 FROM user_flags WHERE account_id = ?1 AND flag = ?9)
       AND (cloud_access_requests.status = 'granted'
         OR (cloud_access_requests.status = 'declined' AND cloud_access_requests.decided_at <= ?10))`,
  )
    .bind(
      s.accountId,
      t,
      text,
      login,
      signals.created_at,
      signals.public_repos,
      signals.followers,
      signals.following,
      CLOUD_FLAG,
      t - DECLINE_COOLDOWN_DAYS * DAY,
    )
    .run();
  if (result.meta.changes === 0) {
    // Another request got there first.
    const latest = answer(await readRow(env, s.accountId));
    return refusal(latest) ?? Response.json(latest);
  }
  // After the answer, so a slow or broken webhook never holds or fails it.
  if (env.CLOUD_REQUESTS_WEBHOOK) ctx.waitUntil(notify(env.CLOUD_REQUESTS_WEBHOOK, s.accountId, login, signals, t));
  return Response.json(answer({ status: "pending", requested_at: t, decided_at: null, flagged: 0 }), { status: 201 });
}

/// 409 with the account's access, when it may not ask now: it has asked
/// already, has access, or was declined less than the cooldown ago.
function refusal(current: Access): Response | null {
  const why =
    current.status === "pending"
      ? "already pending"
      : current.status === "granted"
        ? "already granted"
        : current.status === "declined" && current.retry_at! > now()
          ? "declined recently"
          : null;
  return why ? Response.json({ error: why, ...current }, { status: 409 }) : null;
}

async function readRow(env: Env, accountId: number): Promise<Row> {
  // One row even without a request, so the flag is read in the same query.
  const row = await env.DB.prepare(
    `SELECT r.status, r.requested_at, r.decided_at,
            EXISTS (SELECT 1 FROM user_flags f WHERE f.account_id = ?1 AND f.flag = ?2) AS flagged
     FROM (SELECT ?1 AS account_id) me LEFT JOIN cloud_access_requests r ON r.account_id = me.account_id`,
  )
    .bind(accountId, CLOUD_FLAG)
    .first<Row>();
  return row!;
}

/// The status an account sees. The flag wins over the row both ways: a flag
/// granted by hand without a request is "granted", and a granted row whose
/// flag was revoked is "none".
function answer(row: Row): Access {
  const times = { requested_at: row.requested_at, decided_at: row.decided_at, retry_at: null };
  if (row.flagged) return { status: "granted", ...times };
  if (row.status === null || row.status === "granted") return { status: "none", requested_at: null, decided_at: null, retry_at: null };
  if (row.status === "declined") return { status: "declined", ...times, retry_at: retryAt(row.decided_at!) };
  return { status: row.status, ...times };
}

function retryAt(decidedAt: number): number {
  return decidedAt + DECLINE_COOLDOWN_DAYS * DAY;
}

/// GitHub's public profile of the account's GitHub user, by its numeric ID,
/// which stays when the login changes; none for an account without one. Asked with the app's credentials when there are some, so
/// it counts against the app's limit rather than the Worker's shared IP. Any
/// failure gives null signals: the request does not depend on GitHub.
export async function githubSignals(githubId: number | null, env: Env): Promise<Signals> {
  const none: Signals = { login: null, created_at: null, public_repos: null, followers: null, following: null };
  if (githubId === null) return none;
  const headers: Record<string, string> =
    env.GITHUB_CLIENT_ID && env.GITHUB_CLIENT_SECRET
      ? githubHeaders(env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET)
      : { Accept: "application/vnd.github+json", "User-Agent": "arakun-teams", "X-GitHub-Api-Version": "2022-11-28" };
  let body: unknown;
  try {
    const res = await fetch(`https://api.github.com/user/${githubId}`, {
      // A followed redirect would carry the credentials to wherever it points.
      redirect: "manual",
      headers,
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
    if (!res.ok) {
      await discard(res);
      return none;
    }
    body = await res.json();
  } catch {
    return none;
  }
  // An answer about another account is no answer.
  if (!isObject(body) || body["id"] !== githubId) return none;
  const created = typeof body["created_at"] === "string" ? Date.parse(body["created_at"]) : NaN;
  return {
    login: typeof body["login"] === "string" && GITHUB_LOGIN.test(body["login"]) ? body["login"] : null,
    created_at: Number.isFinite(created) ? Math.floor(created / 1000) : null,
    public_repos: count(body["public_repos"]),
    followers: count(body["followers"]),
    following: count(body["following"]),
  };
}

function count(v: unknown): number | null {
  return typeof v === "number" && Number.isSafeInteger(v) && v >= 0 ? v : null;
}

/// Whole days from the account's creation on GitHub to `at`, or null.
export function accountAgeDays(createdAt: number | null, at: number): number | null {
  return createdAt === null ? null : Math.max(0, Math.floor((at - createdAt) / DAY));
}

/// Tells the operator's Discord channel about a new request. Only public facts
/// about the account: no email, no reason (the user wrote it, and it could ping
/// or link anything), no token.
async function notify(webhook: string, accountId: number, login: string, signals: Signals, at: number): Promise<void> {
  await postToWebhook(webhook, "cloud requests", [`New cloud access request from \`${login}\` (account ${accountId})`, ...profileLines(login, signals, at)]);
}

/// The public GitHub facts a webhook message shows about an account.
export function profileLines(login: string, signals: Signals, at: number): string[] {
  const age = accountAgeDays(signals.created_at, at);
  return [
    `account age: ${age === null ? "unknown" : `${age} days`}`,
    `public repos: ${signals.public_repos ?? "unknown"}`,
    `followers: ${signals.followers ?? "unknown"}`,
    `<https://github.com/${login}>`,
  ];
}

/// Posts lines to a Discord webhook, with no mentions. Failures are logged by
/// `name`, never with the URL, which is itself a secret.
export async function postToWebhook(webhook: string, name: string, lines: string[]): Promise<void> {
  try {
    const res = await fetch(webhook, {
      method: "POST",
      redirect: "manual",
      headers: { "Content-Type": "application/json" },
      // No mentions: nothing in the message may ping anyone.
      body: JSON.stringify({ content: lines.join("\n"), allowed_mentions: { parse: [] } }),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
    await discard(res);
    if (!res.ok) console.warn(`the ${name} webhook answered ${res.status}`);
  } catch {
    console.warn(`the ${name} webhook could not be reached`);
  }
}

// The operator's side, behind the admin door.
//
//   GET /admin/cloud/requests[?status=pending|granted|declined]
//       → [request], pending first (oldest first), then the decided ones, newest decision first
//   PUT /admin/cloud/requests/<account_id>/grant    { by? } → request, and the `cloud` flag
//   PUT /admin/cloud/requests/<account_id>/decline  { by? } → request; it may ask again at retry_at

/// What the operator sees about one request.
const REQUEST_COLUMNS = `r.account_id, ${GITHUB_ID_OF("r.account_id")} AS github_id, u.login, r.status, r.reason, r.requested_at, r.decided_at, r.decided_by,
  r.gh_login, r.gh_created_at, r.gh_public_repos, r.gh_followers, r.gh_following,
  EXISTS (SELECT 1 FROM user_flags f WHERE f.account_id = r.account_id AND f.flag = ?1) AS flagged,
  u.blocked_at IS NOT NULL AS blocked
  FROM cloud_access_requests r JOIN accounts u ON u.id = r.account_id`;

interface RequestRow {
  account_id: number;
  github_id: number | null;
  login: string;
  status: (typeof STATUSES)[number];
  reason: string | null;
  requested_at: number;
  decided_at: number | null;
  decided_by: string | null;
  gh_login: string;
  gh_created_at: number | null;
  gh_public_repos: number | null;
  gh_followers: number | null;
  gh_following: number | null;
  flagged: number;
  blocked: number;
}

export type RequestJson = ReturnType<typeof requestJson>;

function requestJson(r: RequestRow) {
  return {
    account_id: r.account_id,
    // The linked GitHub user, for the avatar; null without one.
    github_id: r.github_id,
    login: r.login,
    status: r.status,
    reason: r.reason,
    requested_at: r.requested_at,
    decided_at: r.decided_at,
    decided_by: r.decided_by,
    retry_at: r.status === "declined" ? retryAt(r.decided_at!) : null,
    has_flag: r.flagged === 1,
    blocked: r.blocked === 1,
    // As GitHub said when the account asked.
    github: {
      login: r.gh_login,
      created_at: r.gh_created_at,
      age_days: accountAgeDays(r.gh_created_at, r.requested_at),
      public_repos: r.gh_public_repos,
      followers: r.gh_followers,
      following: r.gh_following,
    },
  };
}

/// The request as GET /admin/cloud/requests lists it, or null.
export async function requestFor(env: Env, accountId: number): Promise<RequestJson | null> {
  const row = await env.DB.prepare(`SELECT ${REQUEST_COLUMNS} WHERE r.account_id = ?2`).bind(CLOUD_FLAG, accountId).first<RequestRow>();
  return row ? requestJson(row) : null;
}

export async function listRequests(env: Env, url: URL): Promise<Response> {
  const status = url.searchParams.get("status");
  if (status !== null && !(STATUSES as readonly string[]).includes(status)) {
    return error(400, `status must be one of ${STATUSES.join(", ")}`);
  }
  const rows = await env.DB.prepare(
    `SELECT ${REQUEST_COLUMNS}
     WHERE ?2 IS NULL OR r.status = ?2
     ORDER BY r.status = 'pending' DESC,
              CASE WHEN r.status = 'pending' THEN r.requested_at END ASC,
              r.decided_at DESC, r.account_id
     LIMIT ?3`,
  )
    .bind(CLOUD_FLAG, status, MAX_LIST)
    .all<RequestRow>();
  return Response.json(rows.results.map(requestJson));
}

/// Grant sets the request and the `cloud` flag together, the flag as the flag
/// route does; granting a declined request is how the operator changes their
/// mind. Decline takes only a pending request.
export async function decideRequest(request: Request, env: Env, idText: string, decision: "grant" | "decline", op: Operator): Promise<Response> {
  const id = NUMERIC_ID.test(idText) ? Number(idText) : NaN;
  if (!Number.isSafeInteger(id)) return error(400, "the account ID must be a positive integer");
  const body = request.headers.get("Content-Length") === "0" || !request.body ? {} : await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, 'expected {"by": "..."} or no body');
  const by = body["by"];
  if (by !== undefined && by !== null && (typeof by !== "string" || by.length > MAX_BY)) {
    return error(400, `by must be text of at most ${MAX_BY} characters`);
  }
  const db = env.DB;
  const t = now();
  if (decision === "grant") {
    const [updated, flagged] = await db.batch([
      // A second grant changes nothing: it keeps the time of the first, and
      // the log gets no second entry.
      db
        .prepare("UPDATE cloud_access_requests SET status = 'granted', decided_at = ?2, decided_by = ?3 WHERE account_id = ?1 AND status != 'granted'")
        .bind(id, t, by ?? null),
      db
        .prepare(
          `INSERT INTO user_flags (account_id, flag, granted_at)
           SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM cloud_access_requests WHERE account_id = ?1)
           ON CONFLICT DO NOTHING`,
        )
        .bind(id, CLOUD_FLAG, t),
    ]);
    if (updated!.meta.changes === 0 && flagged!.meta.changes === 0) {
      const row = await db.prepare("SELECT 1 FROM cloud_access_requests WHERE account_id = ?").bind(id).first();
      if (!row) return error(404, "no request from this account");
    } else {
      // A grant that only restores a revoked flag is a change too.
      await record(env, op, "cloud.grant", id);
    }
  } else {
    // Only a pending request: a granted one is ended by revoking the flag.
    const updated = await db
      .prepare("UPDATE cloud_access_requests SET status = 'declined', decided_at = ?2, decided_by = ?3 WHERE account_id = ?1 AND status = 'pending'")
      .bind(id, t, by ?? null)
      .run();
    if (updated.meta.changes === 0) {
      const row = await db.prepare("SELECT status FROM cloud_access_requests WHERE account_id = ?").bind(id).first<{ status: string }>();
      if (!row) return error(404, "no request from this account");
      return error(409, `the request is ${row.status}, not pending`);
    }
    await record(env, op, "cloud.decline", id);
  }
  const row = await db.prepare(`SELECT ${REQUEST_COLUMNS} WHERE r.account_id = ?2`).bind(CLOUD_FLAG, id).first<RequestRow>();
  return Response.json(requestJson(row!));
}
