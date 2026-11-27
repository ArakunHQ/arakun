// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Sign-ups for the phone app betas. The stores cap testers and each one is
// invited by email, so they are admitted by hand: an account signs up here for
// a platform, and the operator adds the email in App Store Connect (TestFlight)
// or Play Console (testing track), then marks the sign-up admitted at
// /admin/beta/signups (admin.ts calls the functions at the bottom).
//
//   GET  /beta             → { email, ios: signup, android: signup }
//   POST /beta/<platform>  → 201 with the signup, after signing up, { email? }
//
// `platform` is "ios" or "android". A signup is { status, email,
// requested_at, decided_at, retry_at }; `status` is "none", "pending",
// "admitted" or "declined", and the other fields are null when they do not
// apply. The top `email` is the account's verified GitHub address, which a
// sign-up uses when it names none. A pending sign-up may be sent again to
// change its email; that is not a new request and posts nothing.

import { record, type Operator } from "./admin_log";
import { githubIdOf, session } from "./auth";
import { DECLINE_COOLDOWN_DAYS, accountAgeDays, githubSignals, postToWebhook, profileLines } from "./cloud_access";
import { DAY, GITHUB_ID_OF, NUMERIC_ID, error, isObject, jsonBody, now, type Env } from "./util";

export const PLATFORMS = ["ios", "android"] as const;
export type Platform = (typeof PLATFORMS)[number];
const STATUSES = ["pending", "admitted", "declined"] as const;
type Status = (typeof STATUSES)[number];
/// The longest address SMTP carries.
export const MAX_EMAIL = 254;
const MAX_BY = 100;
const MAX_LIST = 500;
const MAX_BODY = 4096;
/// Enough to catch a typo or a name in the wrong field. The stores check the
/// rest when the operator adds it.
const EMAIL = /^[^\s@]{1,64}@[^\s@.]+(\.[^\s@.]+)+$/;
/// What the operator's list and the Discord post call each platform.
const STORE: Record<Platform, string> = { ios: "iOS (TestFlight)", android: "Android (Play testing)" };

/// What the account sees about one platform.
export interface Signup {
  status: "none" | Status;
  email: string | null;
  requested_at: number | null;
  decided_at: number | null;
  retry_at: number | null;
}

interface Row {
  platform: Platform;
  email: string;
  status: Status;
  requested_at: number;
  decided_at: number | null;
}

const NONE: Signup = { status: "none", email: null, requested_at: null, decided_at: null, retry_at: null };

/// The answer for a /beta route, or null when the path is not one.
export async function beta(request: Request, env: Env, url: URL, ctx: ExecutionContext): Promise<Response | null> {
  if (url.pathname === "/beta" && request.method === "GET") return getSignups(request, env);
  const one = /^\/beta\/([^/]+)$/.exec(url.pathname);
  if (one && request.method === "POST") {
    const platform = one[1]!;
    if (!isPlatform(platform)) return error(404, "not found");
    return signUp(request, env, platform, ctx);
  }
  if (url.pathname === "/beta" || url.pathname.startsWith("/beta/")) return error(404, "not found");
  return null;
}

function isPlatform(text: string): text is Platform {
  return (PLATFORMS as readonly string[]).includes(text);
}

async function getSignups(request: Request, env: Env): Promise<Response> {
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  const [rows, email] = await Promise.all([readRows(env, s.accountId), accountEmail(env, s.accountId)]);
  return Response.json({ email, ios: answer(rows.get("ios")), android: answer(rows.get("android")) });
}

async function signUp(request: Request, env: Env, platform: Platform, ctx: ExecutionContext): Promise<Response> {
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  if (s.blocked) return error(403, "blocked");
  let limited: boolean;
  try {
    limited = !(await env.BETA_LIMIT.limit({ key: String(s.accountId) })).success;
  } catch (e) {
    console.error("beta limit failed:", e);
    return error(503, "the server could not check the request: try again");
  }
  if (limited) return error(429, "too many requests, try again in a minute");
  const body = request.headers.get("Content-Length") === "0" || !request.body ? {} : await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, 'expected {"email": "..."} or no body');
  const given = body["email"];
  if (given !== undefined && given !== null && typeof given !== "string") return error(400, "email must be text");
  const typed = typeof given === "string" ? given.trim() : "";
  const email = typed.length > 0 ? typed : await accountEmail(env, s.accountId);
  if (email === null) return error(400, "this account has no verified email on GitHub: give one");
  if (email.length > MAX_EMAIL || !EMAIL.test(email)) return error(400, "that does not look like an email address");

  const current = answer((await readRows(env, s.accountId)).get(platform));
  if (current.status === "pending") {
    // Only the address changes; the place in the queue stays.
    await env.DB.prepare("UPDATE beta_signups SET email = ?3 WHERE account_id = ?1 AND platform = ?2 AND status = 'pending'")
      .bind(s.accountId, platform, email)
      .run();
    return Response.json({ ...current, email });
  }
  const refused = refusal(current);
  if (refused) return refused;

  const t = now();
  const signals = await githubSignals(await githubIdOf(env, s.accountId), env);
  const login = signals.login ?? s.login;
  // The conditions are those checked above, again in the one statement that
  // writes, so two sign-ups at once make one row and one post.
  const result = await env.DB.prepare(
    `INSERT INTO beta_signups
       (account_id, platform, email, status, requested_at, decided_at, decided_by, gh_login, gh_created_at, gh_public_repos, gh_followers, gh_following)
     VALUES (?1, ?2, ?3, 'pending', ?4, NULL, NULL, ?5, ?6, ?7, ?8, ?9)
     ON CONFLICT (account_id, platform) DO UPDATE SET
       email = excluded.email, status = 'pending', requested_at = excluded.requested_at, decided_at = NULL, decided_by = NULL,
       gh_login = excluded.gh_login, gh_created_at = excluded.gh_created_at, gh_public_repos = excluded.gh_public_repos,
       gh_followers = excluded.gh_followers, gh_following = excluded.gh_following
     WHERE beta_signups.status = 'declined' AND beta_signups.decided_at <= ?10`,
  )
    .bind(
      s.accountId,
      platform,
      email,
      t,
      login,
      signals.created_at,
      signals.public_repos,
      signals.followers,
      signals.following,
      t - DECLINE_COOLDOWN_DAYS * DAY,
    )
    .run();
  if (result.meta.changes === 0) {
    // Another sign-up got there first.
    const latest = answer((await readRows(env, s.accountId)).get(platform));
    return refusal(latest) ?? Response.json(latest);
  }
  // After the answer, so a slow or broken webhook never holds or fails it. No
  // email in the post: the channel is not the place for it.
  if (env.BETA_REQUESTS_WEBHOOK) {
    const lines = [`New ${STORE[platform]} beta sign-up from \`${login}\` (account ${s.accountId})`, ...profileLines(login, signals, t)];
    ctx.waitUntil(postToWebhook(env.BETA_REQUESTS_WEBHOOK, "beta requests", lines));
  }
  return Response.json({ status: "pending", email, requested_at: t, decided_at: null, retry_at: null } satisfies Signup, { status: 201 });
}

/// 409 with the sign-up, when the account may not sign up now: it is in
/// already, or was declined less than the cooldown ago.
function refusal(current: Signup): Response | null {
  const why =
    current.status === "admitted"
      ? "already admitted"
      : current.status === "declined" && current.retry_at! > now()
        ? "declined recently"
        : null;
  return why ? Response.json({ error: why, ...current }, { status: 409 }) : null;
}

async function readRows(env: Env, accountId: number): Promise<Map<Platform, Row>> {
  const rows = await env.DB.prepare("SELECT platform, email, status, requested_at, decided_at FROM beta_signups WHERE account_id = ?")
    .bind(accountId)
    .all<Row>();
  return new Map(rows.results.map((r) => [r.platform, r]));
}

/// The verified primary address GitHub gave at the last sign-in, or null.
async function accountEmail(env: Env, accountId: number): Promise<string | null> {
  const row = await env.DB.prepare("SELECT email FROM accounts WHERE id = ?").bind(accountId).first<{ email: string | null }>();
  return row?.email ?? null;
}

function answer(row: Row | undefined): Signup {
  if (!row) return NONE;
  return {
    status: row.status,
    email: row.email,
    requested_at: row.requested_at,
    decided_at: row.decided_at,
    retry_at: row.status === "declined" ? retryAt(row.decided_at!) : null,
  };
}

function retryAt(decidedAt: number): number {
  return decidedAt + DECLINE_COOLDOWN_DAYS * DAY;
}

// The operator's side, behind the admin door.
//
//   GET /admin/beta/signups[?platform=ios|android][&status=pending|admitted|declined]
//       → [signup], pending first (oldest first), then the decided ones, newest decision first
//   PUT /admin/beta/signups/<account_id>/<platform>/admit    { by? } → signup
//   PUT /admin/beta/signups/<account_id>/<platform>/decline  { by? } → signup; it may sign up again at retry_at
//
// Admitting does not invite anyone: add the email in App Store Connect or Play
// Console first, then admit, so the app says "admitted" only once the invite
// is on its way. Either decision takes a sign-up in any state: admitting a
// declined one is a change of mind, and declining an admitted one removes a
// tester (remove them in the store too).

const SIGNUP_COLUMNS = `b.account_id, ${GITHUB_ID_OF("b.account_id")} AS github_id, u.login, b.platform, b.email, b.status, b.requested_at, b.decided_at, b.decided_by,
  b.gh_login, b.gh_created_at, b.gh_public_repos, b.gh_followers, b.gh_following,
  u.blocked_at IS NOT NULL AS blocked
  FROM beta_signups b JOIN accounts u ON u.id = b.account_id`;

interface SignupRow {
  account_id: number;
  github_id: number | null;
  login: string;
  platform: Platform;
  email: string;
  status: Status;
  requested_at: number;
  decided_at: number | null;
  decided_by: string | null;
  gh_login: string;
  gh_created_at: number | null;
  gh_public_repos: number | null;
  gh_followers: number | null;
  gh_following: number | null;
  blocked: number;
}

function signupJson(r: SignupRow) {
  return {
    account_id: r.account_id,
    // The linked GitHub user, for the avatar; null without one.
    github_id: r.github_id,
    login: r.login,
    platform: r.platform,
    email: r.email,
    status: r.status,
    requested_at: r.requested_at,
    decided_at: r.decided_at,
    decided_by: r.decided_by,
    retry_at: r.status === "declined" ? retryAt(r.decided_at!) : null,
    blocked: r.blocked === 1,
    // As GitHub said when the account signed up.
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

export async function listSignups(env: Env, url: URL): Promise<Response> {
  const platform = url.searchParams.get("platform");
  if (platform !== null && !isPlatform(platform)) return error(400, `platform must be one of ${PLATFORMS.join(", ")}`);
  const status = url.searchParams.get("status");
  if (status !== null && !(STATUSES as readonly string[]).includes(status)) {
    return error(400, `status must be one of ${STATUSES.join(", ")}`);
  }
  const rows = await env.DB.prepare(
    `SELECT ${SIGNUP_COLUMNS}
     WHERE (?1 IS NULL OR b.platform = ?1) AND (?2 IS NULL OR b.status = ?2)
     ORDER BY b.status = 'pending' DESC,
              CASE WHEN b.status = 'pending' THEN b.requested_at END ASC,
              b.decided_at DESC, b.account_id, b.platform
     LIMIT ?3`,
  )
    .bind(platform, status, MAX_LIST)
    .all<SignupRow>();
  return Response.json(rows.results.map(signupJson));
}

export async function decideSignup(
  request: Request,
  env: Env,
  idText: string,
  platform: string,
  decision: "admit" | "decline",
  op: Operator,
): Promise<Response> {
  const id = NUMERIC_ID.test(idText) ? Number(idText) : NaN;
  if (!Number.isSafeInteger(id)) return error(400, "the account ID must be a positive integer");
  if (!isPlatform(platform)) return error(400, `platform must be one of ${PLATFORMS.join(", ")}`);
  const body = request.headers.get("Content-Length") === "0" || !request.body ? {} : await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, 'expected {"by": "..."} or no body');
  const by = body["by"];
  if (by !== undefined && by !== null && (typeof by !== "string" || by.length > MAX_BY)) {
    return error(400, `by must be text of at most ${MAX_BY} characters`);
  }
  const to: Status = decision === "admit" ? "admitted" : "declined";
  const db = env.DB;
  // Deciding the same way twice changes nothing: it keeps the time and name
  // of the first, and the log gets no second entry.
  const updated = await db
    .prepare("UPDATE beta_signups SET status = ?3, decided_at = ?4, decided_by = ?5 WHERE account_id = ?1 AND platform = ?2 AND status != ?3")
    .bind(id, platform, to, now(), by ?? null)
    .run();
  if (updated.meta.changes > 0) {
    await record(env, op, decision === "admit" ? "beta.admit" : "beta.decline", id, { platform });
  }
  const row = await db.prepare(`SELECT ${SIGNUP_COLUMNS} WHERE b.account_id = ?1 AND b.platform = ?2`).bind(id, platform).first<SignupRow>();
  if (!row) return error(404, "no sign-up from this account for this platform");
  return Response.json(signupJson(row));
}
