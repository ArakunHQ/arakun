// Sign-in: the desktop app runs GitHub's device flow itself and hands us the
// user access token once. We ask GitHub whether our OAuth App issued it and
// to whom, keep only the numeric ID and the login, and answer with a session
// token of our own. Then we revoke the GitHub token: OAuth App tokens never
// expire, and it was only needed to learn who the user is.
//
// Asking /user would accept any token of the user's (the gh CLI's, a personal
// one, another app's), so whoever held one could sign in as them. The
// Applications API only answers for tokens our app issued, and it needs the
// app's client secret, which lives only here as a Worker secret: the desktop
// app's device flow has no secret.

import { recheckAccount } from "./machines";
import { unreadNotices } from "./notices";
import { DAY, GITHUB_LOGIN, b64url, bearer, bodyText, discard, error, flagList, flagsQuery, hex, isObject, now, sha256, type Env } from "./util";

const SESSION_DAYS = 90;
/// Renewal never takes a session past this many days after its sign-in, so a
/// stolen session ends even while it is in use.
export const MAX_SESSION_DAYS = 180;
/// 32 bytes in base64url, as `signIn` makes them.
export const SESSION_TOKEN = /^[A-Za-z0-9_-]{43}$/;
/// GitHub tokens are at most 255 characters; this leaves room for a change.
export const MAX_ACCESS_TOKEN = 1024;
/// Room for the token and some JSON around it. Anyone can call sign-in, so the
/// body is cut off here, before it is parsed.
const MAX_SIGN_IN_BODY = 4096;
export const GITHUB_TIMEOUT_MS = 10_000;

export interface Session {
  hash: string;
  /// The session's public id, which `GET /sessions` lists and
  /// `DELETE /sessions/<id>` takes. Not a secret.
  id: string;
  accountId: number;
  login: string;
  name: string;
  kind: string;
  plan: string;
  /// The account is blocked. `session()` still answers, so each caller
  /// decides what a block means for it.
  blocked: boolean;
  /// Whether the user has the flag `session()` was asked about; false when
  /// it was asked about none.
  flagged: boolean;
}

export interface GitHubUser {
  id: number;
  login: string;
  /// The primary verified address; null when GitHub has none. Undefined when
  /// we could not ask, so the stored address stays.
  email?: string | null;
}

/// What a session can be for: the app, `arakun-server`, the phone page, and
/// the native apps. Sign-in takes only these. `cloud` is not among them: only
/// `newCloudSession` makes one.
const KINDS = ["app", "server", "phone", "ios", "android"];
/// A session for an `arakun-server` in a container we host. An agent there
/// may be talked into reading the token, so it is good for one door only:
/// `/machines/<label>/pc` of the label it was made for.
export const CLOUD_KIND = "cloud";
/// A machine label, as the relay derives it from the computer's key.
const LABEL = /^[a-z2-7]{16}$/;
const MAX_NAME = 100;
/// How many sessions an account keeps. Sign-in is limited per client IP, not
/// per account, so without this the list has no end.
export const MAX_SESSIONS = 100;
/// `__Host-`: the browser keeps it only when set `Secure`, with `Path=/` and
/// no `Domain`, so a sibling host cannot plant one for this host.
export const SESSION_COOKIE = "__Host-arakun_session";

/// Thrown by `session()` for a cookie on a request from another origin, so a
/// caller that does not catch it cannot treat it as "not signed in". The
/// router answers it with 403.
export class ForeignOrigin extends Error {
  constructor() {
    super("the request comes from another origin");
  }
}

export function authConfig(env: Env): Response {
  return Response.json({ github_client_id: env.GITHUB_CLIENT_ID ?? null });
}

export async function signIn(request: Request, env: Env): Promise<Response> {
  if (!(await signInAllowed(request, env))) return error(429, "too many sign-ins, try again in a minute");

  const body = await signInBody(request);
  if (body === TOO_BIG) return error(413, "the body is too big");
  if (!body) return error(400, 'expected {"access_token": "..."}');
  if (body === "name") return error(400, `name must be 1 to ${MAX_NAME} characters`);
  if (body === "kind") return error(400, `kind must be one of ${KINDS.join(", ")}`);
  const user = await verifyAndRevoke(body.token, env);
  if (user instanceof Response) return user;

  const made = await newSession(env, user, body.name ?? user.login, body.kind);
  const [flags, plan] = await env.DB.batch([
    flagsQuery(env.DB, made.account_id),
    env.DB.prepare("SELECT plan FROM accounts WHERE id = ?").bind(made.account_id),
  ]);
  return Response.json({
    session: made.session,
    session_id: made.session_id,
    expires_at: made.expires_at,
    account_id: made.account_id,
    // The account ID under its old name, for clients from before accounts had
    // their own. For accounts made before then it is their GitHub ID.
    github_id: made.account_id,
    login: user.login,
    flags: flagList(flags!),
    plan: (plan!.results[0] as { plan: string }).plan,
  });
}

/// Sign-in of any kind counts against one limit per client IP. Before sign-in
/// the IP is the only thing we know about the caller.
export async function signInAllowed(request: Request, env: Env): Promise<boolean> {
  const ip = request.headers.get("CF-Connecting-IP") ?? "unknown";
  const { success } = await env.SIGN_IN_LIMIT.limit({ key: ip });
  return success;
}

/// The steps both sign-ins share, for a GitHub user access token of our app:
/// ask who it belongs to, read the email, revoke the token. The user, or the
/// error answer to give.
export async function verifyAndRevoke(token: string, env: Env): Promise<GitHubUser | Response> {
  if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) return error(503, "sign-in is not set up");
  const user = await githubUser(token, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  if (user === "rejected") return error(401, "GitHub rejected the token");
  if (user === "unreachable") return error(502, "could not reach GitHub");
  // Before the revoke, which ends the token's use.
  user.email = await primaryEmail(token);
  // Before the session exists: a token that still works after sign-in could
  // make more sessions for whoever gets it later.
  const revoked = await revokeToken(token, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  if (revoked === "rejected") return error(401, "GitHub rejected the token");
  if (revoked === "unreachable") return error(502, "could not reach GitHub");
  return user;
}

/// The account linked to a GitHub user, as a subquery on `?1`, the GitHub
/// user's numeric ID as text. Null when none is.
export const GITHUB_ACCOUNT = "(SELECT account_id FROM identities WHERE provider = 'github' AND subject = ?1)";

/// The lowest ID a new account gets. GitHub's user IDs are far below it, so a
/// new account's ID is never also some GitHub user's, and an account made
/// before accounts had their own IDs (whose ID was its GitHub ID) is told
/// apart by its size alone.
export const MIN_NEW_ACCOUNT_ID = 2 ** 32;

/// A random ID for a new account, from MIN_NEW_ACCOUNT_ID up to 2^53: random
/// so none is ever reused or tells how many accounts there are, and below 2^53
/// so JSON numbers hold it exactly in every client.
export function newAccountId(): number {
  const [hi, lo] = crypto.getRandomValues(new Uint32Array(2));
  const n = (hi! % 2 ** 21) * 2 ** 32 + lo!;
  return MIN_NEW_ACCOUNT_ID + (n % (Number.MAX_SAFE_INTEGER - MIN_NEW_ACCOUNT_ID));
}

/// Two statements that make an account for a GitHub user and link it, unless
/// one is linked already. Run them first in a batch: a batch is one
/// transaction, so both happen or neither. Its login stays empty and
/// `last_seen_at` 0 until it signs in, which is how an account an admin made
/// before the first sign-in looks. An ID that is taken already fails the
/// batch rather than link the GitHub user to someone else's account.
export function accountFor(db: D1Database, githubId: number, t: number): D1PreparedStatement[] {
  const subject = String(githubId);
  const id = newAccountId();
  return [
    db
      .prepare(`INSERT INTO accounts (id, login, created_at, last_seen_at) SELECT ?3, '', ?2, 0 WHERE ${GITHUB_ACCOUNT} IS NULL`)
      .bind(subject, t, id),
    db
      .prepare(`INSERT INTO identities (provider, subject, account_id, linked_at) SELECT 'github', ?1, ?3, ?2 WHERE ${GITHUB_ACCOUNT} IS NULL`)
      .bind(subject, t, id),
  ];
}

/// The account's GitHub user ID, or null if it has no GitHub identity.
export async function githubIdOf(env: Env, accountId: number): Promise<number | null> {
  const subject = await env.DB.prepare("SELECT subject FROM identities WHERE provider = 'github' AND account_id = ?")
    .bind(accountId)
    .first<string>("subject");
  return subject === null ? null : Number(subject);
}

/// Makes the account if the GitHub user has none and gives it a session.
/// Callers have checked who the user is already, and `name` and `kind` too.
export async function newSession(
  env: Env,
  user: GitHubUser,
  name: string,
  kind: string,
): Promise<{ session: string; session_id: string; expires_at: number; account_id: number }> {
  const t = now();
  const session = b64url(crypto.getRandomValues(new Uint8Array(32)));
  const id = hex(crypto.getRandomValues(new Uint8Array(16)));
  const expiresAt = t + SESSION_DAYS * DAY;
  const db = env.DB;
  const hash = hex(await sha256(session));
  const subject = String(user.id);
  // One batch is one transaction: no session without its account.
  const results = await db.batch([
    ...accountFor(db, user.id, t),
    // Fills in the login of an account an admin made before the first sign-in.
    db
      .prepare(
        `UPDATE accounts SET login = ?2, email = CASE WHEN ?4 THEN ?3 ELSE email END, last_seen_at = ?5
         WHERE id = ${GITHUB_ACCOUNT}`,
      )
      .bind(subject, user.login, user.email ?? null, user.email === undefined ? 0 : 1, t),
    // Each sign-in adds a session, so old ones are cleared here.
    db.prepare(`DELETE FROM sessions WHERE account_id = ${GITHUB_ACCOUNT} AND expires_at <= ?2`).bind(subject, t),
    db
      .prepare(
        `INSERT INTO sessions (token_hash, id, name, kind, account_id, created_at, expires_at, last_used_at)
         SELECT ?2, ?3, ?4, ?5, ${GITHUB_ACCOUNT}, ?6, ?7, ?6`,
      )
      .bind(subject, hash, id, name, kind, t, expiresAt),
    // Past the cap, the sessions used longest ago make room for the new one.
    // Cloud sessions neither count nor go: one per machine bounds them, and
    // signing in on a new phone must not cut off a machine we run.
    db
      .prepare(
        `DELETE FROM sessions WHERE account_id = ${GITHUB_ACCOUNT} AND token_hash IN (
           SELECT token_hash FROM sessions WHERE account_id = ${GITHUB_ACCOUNT} AND token_hash != ?2 AND kind != 'cloud'
           ORDER BY last_used_at DESC, created_at DESC, token_hash LIMIT -1 OFFSET ?3)`,
      )
      .bind(subject, hash, MAX_SESSIONS - 1),
    db.prepare(`SELECT ${GITHUB_ACCOUNT} AS id`).bind(subject),
  ]);
  const accountId = (results[6]!.results[0] as { id: number }).id;
  // A session that made room may have sockets open: they close now.
  if (results[5]!.meta.changes > 0) await recheckAccount(env, accountId);
  return { session, session_id: id, expires_at: expiresAt, account_id: accountId };
}

/// A session for the machine with this label, for a user who has signed in
/// before and is not blocked; null otherwise. The token is returned once and
/// only its hash is kept. It lives as long as any session, and use renews it
/// the same way, so a token copied out of the container ends too. A machine has
/// one cloud session: a new one ends the one before, which keeps them bounded
/// without the cap, and a restarted container leaves no live token behind.
export async function newCloudSession(
  env: Env,
  accountId: number,
  label: string,
  name: string,
): Promise<{ session: string; session_id: string; expires_at: number } | null> {
  if (!LABEL.test(label)) throw new Error("not a machine label");
  if (name.length === 0 || name.length > MAX_NAME) throw new Error(`name must be 1 to ${MAX_NAME} characters`);
  const t = now();
  const session = b64url(crypto.getRandomValues(new Uint8Array(32)));
  const id = hex(crypto.getRandomValues(new Uint8Array(16)));
  const expiresAt = t + SESSION_DAYS * DAY;
  const hash = hex(await sha256(session));
  const db = env.DB;
  const results = await db.batch([
    db.prepare("DELETE FROM sessions WHERE account_id = ? AND (expires_at <= ? OR machine_label = ?)").bind(accountId, t, label),
    // No row without a user who exists and is not blocked.
    db
      .prepare(
        `INSERT INTO sessions (token_hash, id, name, kind, machine_label, account_id, created_at, expires_at, last_used_at)
         SELECT ?1, ?2, ?3, ?4, ?5, id, ?6, ?7, ?6 FROM accounts WHERE id = ?8 AND blocked_at IS NULL`,
      )
      .bind(hash, id, name, CLOUD_KIND, label, t, expiresAt, accountId),
  ]);
  // The session it replaced may hold the machine's socket.
  if (results[0]!.meta.changes > 0) await recheckAccount(env, accountId);
  if (results[1]!.meta.changes === 0) return null;
  return { session, session_id: id, expires_at: expiresAt };
}

export async function me(request: Request, env: Env): Promise<Response> {
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  const flags = flagList(await flagsQuery(env.DB, s.accountId).run());
  return Response.json({ account_id: s.accountId, github_id: s.accountId, login: s.login, flags, plan: s.plan, notices: await unreadNotices(env, s.accountId) });
}

export async function logout(request: Request, env: Env): Promise<Response> {
  // No renewal: a failed delete must not leave the session living longer.
  const s = await session(request, env, { renew: false });
  // A cookie that names no session is dead: the page should drop it.
  if (!s) return withoutCookie(request, error(401, "not signed in"));
  await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(s.hash).run();
  await recheckAccount(env, s.accountId);
  return withoutCookie(request, new Response(null, { status: 204 }));
}

/// Clears the session cookie on an answer, only when the request came with
/// it: a bearer client has no cookie to clear.
function withoutCookie(request: Request, res: Response): Response {
  if (!bearer(request) && !request.headers.has("Authorization") && cookie(request, SESSION_COOKIE)) {
    res.headers.append("Set-Cookie", `${SESSION_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0`);
  }
  return res;
}

/// The account's signed-in devices, the calling one marked.
export async function listSessions(request: Request, env: Env): Promise<Response> {
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  const rows = await env.DB.prepare(
    "SELECT id, name, kind, created_at, last_used_at FROM sessions WHERE account_id = ? AND expires_at > ? ORDER BY created_at, id",
  )
    .bind(s.accountId, now())
    .all<{ id: string; name: string; kind: string; created_at: number; last_used_at: number }>();
  return Response.json(rows.results.map((r) => ({ ...r, current: r.id === s.id })));
}

/// Signs one of the account's devices out. Another account's id is not found.
export async function deleteSession(request: Request, env: Env, id: string): Promise<Response> {
  const s = await session(request, env, { renew: false });
  if (!s) return error(401, "not signed in");
  const result = await env.DB.prepare("DELETE FROM sessions WHERE id = ? AND account_id = ?").bind(id, s.accountId).run();
  if (result.meta.changes === 0) return error(404, "no such session");
  await recheckAccount(env, s.accountId);
  const res = new Response(null, { status: 204 });
  // Signing out the session that made this request ends the cookie too.
  return id === s.id ? withoutCookie(request, res) : res;
}

/// The session the request names, if it is still valid: by the bearer token
/// or by the cookie. Every endpoint that needs a user goes through here. Team
/// endpoints pass `flag`, so the flag is read in the same query rather than
/// in a second round trip.
///
/// A cloud session counts as no session, unless the caller is the computer
/// door and passes `machine`, the label it serves, and that is the session's
/// label. Refusing here rather than in each caller means a new endpoint is
/// closed to it without anyone remembering to.
export async function session(
  request: Request,
  env: Env,
  { renew = true, flag = null, machine = null }: { renew?: boolean; flag?: string | null; machine?: string | null } = {},
): Promise<Session | null> {
  let token = bearer(request);
  if (token === null && !request.headers.has("Authorization")) {
    // A page's script cannot read the cookie, but a foreign page can make the
    // browser send it. So whatever changes something, and any socket, must
    // come from our own origin.
    token = cookie(request, SESSION_COOKIE);
    if (token !== null && (!["GET", "HEAD"].includes(request.method) || request.headers.has("Upgrade"))) {
      if (request.headers.get("Origin") !== new URL(request.url).origin) throw new ForeignOrigin();
    }
  }
  if (!token || !SESSION_TOKEN.test(token)) return null;
  const hash = hex(await sha256(token));
  // A lookup by the hash itself, so no secret is compared here.
  const row = await env.DB.prepare(
    `SELECT s.id, s.name, s.kind, s.machine_label, s.account_id, s.created_at, s.expires_at, s.last_used_at, u.login, u.plan, u.blocked_at IS NOT NULL AS blocked,
            f.flag IS NOT NULL AS flagged
     FROM sessions s JOIN accounts u ON u.id = s.account_id
     LEFT JOIN user_flags f ON f.account_id = s.account_id AND f.flag = ?2
     WHERE s.token_hash = ?1`,
  )
    .bind(hash, flag)
    .first<{
      id: string | null;
      name: string;
      kind: string;
      machine_label: string | null;
      account_id: number;
      created_at: number;
      expires_at: number;
      last_used_at: number;
      login: string;
      plan: string;
      blocked: number;
      flagged: number;
    }>();
  if (!row) return null;
  // Before the renewal: a refused request must not keep the session alive.
  if ((row.kind === CLOUD_KIND || row.machine_label !== null) && (machine === null || row.machine_label !== machine)) return null;
  const t = now();
  if (row.expires_at <= t) {
    // Only while it is still expired: a request that renewed it meanwhile wins.
    await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ? AND expires_at <= ?").bind(hash, t).run();
    return null;
  }
  // Extending on every request would write to D1 on every request. The
  // conditions keep two requests from renewing twice, and an older one from
  // moving the times back.
  if (renew && t - row.last_used_at >= DAY) {
    await env.DB.batch([
      env.DB.prepare("UPDATE sessions SET expires_at = ?, last_used_at = ? WHERE token_hash = ? AND last_used_at = ?").bind(
        Math.min(t + SESSION_DAYS * DAY, row.created_at + MAX_SESSION_DAYS * DAY),
        t,
        hash,
        row.last_used_at,
      ),
      env.DB.prepare("UPDATE accounts SET last_seen_at = ? WHERE id = ? AND last_seen_at < ?").bind(t, row.account_id, t),
    ]);
  }
  // A row from before sessions had ids: the migration fills them in, but a
  // sign-in the Worker of before served between the migration and the deploy
  // has none, nor a name.
  let { id, name } = row;
  if (id === null) {
    const given = await giveId(env, hash, row.login);
    if (!given) return null;
    ({ id, name } = given);
  }
  return {
    hash,
    id,
    accountId: row.account_id,
    login: row.login,
    name,
    kind: row.kind,
    plan: row.plan,
    blocked: row.blocked === 1,
    flagged: row.flagged === 1,
  };
}

/// Gives a session without an id one, and its user's login for a name, as
/// the migration did for the rows it found. Two requests at once agree: only
/// the first write lands, and both read what it wrote. Null when the session
/// ended meanwhile.
async function giveId(env: Env, hash: string, login: string): Promise<{ id: string; name: string } | null> {
  const [, read] = await env.DB.batch<{ id: string; name: string }>([
    env.DB.prepare("UPDATE sessions SET id = ?2, name = CASE WHEN name = '' THEN ?3 ELSE name END WHERE token_hash = ?1 AND id IS NULL").bind(
      hash,
      hex(crypto.getRandomValues(new Uint8Array(16))),
      login,
    ),
    env.DB.prepare("SELECT id, name FROM sessions WHERE token_hash = ?").bind(hash),
  ]);
  return read!.results[0] ?? null;
}

/// A body over the limit. A symbol, so no token string can be mistaken for it.
const TOO_BIG = Symbol("too big");

async function signInBody(
  request: Request,
): Promise<{ token: string; name: string | null; kind: string } | "name" | "kind" | typeof TOO_BIG | null> {
  let body: unknown;
  try {
    // A body stream that breaks off is a bad request too, not a server error.
    const text = await bodyText(request, MAX_SIGN_IN_BODY);
    if (text === null) return TOO_BIG;
    body = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof body !== "object" || body === null) return null;
  const { access_token: token, name, kind } = body as Record<string, unknown>;
  if (typeof token !== "string" || token.length === 0 || token.length > MAX_ACCESS_TOKEN) return null;
  if (name !== undefined && (typeof name !== "string" || name.trim().length === 0 || name.length > MAX_NAME)) return "name";
  if (kind !== undefined && (typeof kind !== "string" || !KINDS.includes(kind))) return "kind";
  return { token, name: name === undefined ? null : name.trim(), kind: kind ?? "app" };
}

/// The value of one cookie, or null. Null too when it comes more than once:
/// a second one may have been planted (by a sibling host, or a path of its
/// own) and nothing says which one is ours.
export function cookie(request: Request, name: string): string | null {
  let found: string | null = null;
  for (const part of (request.headers.get("Cookie") ?? "").split(";")) {
    const [key, ...value] = part.trim().split("=");
    if (key !== name) continue;
    if (found !== null) return null;
    found = value.join("=");
  }
  return found;
}

/// The primary verified address of the token's user, null if there is none,
/// undefined if GitHub does not answer: sign-in does not depend on it, and
/// the stored address is kept.
async function primaryEmail(token: string): Promise<string | null | undefined> {
  try {
    const res = await fetch("https://api.github.com/user/emails", {
      redirect: "manual",
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: "application/vnd.github+json",
        "User-Agent": "arakun-teams",
        "X-GitHub-Api-Version": "2022-11-28",
      },
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
    if (!res.ok) {
      await discard(res);
      return undefined;
    }
    const list: unknown = await res.json();
    if (!Array.isArray(list)) return undefined;
    for (const e of list) {
      if (isObject(e) && e["primary"] === true && e["verified"] === true && typeof e["email"] === "string") return e["email"];
    }
  } catch {
    return undefined;
  }
  return null;
}

/// Who GitHub says owns the token, if our app issued it. Anything other than a
/// clear yes or a clear no counts as GitHub being unreachable, so a GitHub
/// outage never signs anyone out of the app by mistake.
async function githubUser(token: string, clientId: string, clientSecret: string): Promise<GitHubUser | "rejected" | "unreachable"> {
  const res = await applicationToken("POST", token, clientId, clientSecret);
  if (!res) return "unreachable";
  // 404: no such token for this app (wrong app, revoked or expired). GitHub
  // also answers 422 when it is spammed, so 422 is not a clear no.
  if (res.status === 404) {
    // Logged in case GitHub ever answers 404 to a wrong client ID or secret
    // too: then every sign-in would end here, and this line would show it.
    console.warn("GitHub answered 404 to a sign-in token: not issued by this app, revoked or expired");
    await discard(res);
    return "rejected";
  }
  // 401 here means our own client ID or secret is wrong, not the user's token:
  // api.github.com answers 401 "Bad credentials" to a wrong secret.
  if (!res.ok) {
    if (res.status === 401) console.error("GitHub refused the app's client credentials");
    await discard(res);
    return "unreachable";
  }
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    return "unreachable";
  }
  const user = typeof body === "object" && body !== null ? (body as Record<string, unknown>)["user"] : null;
  return asUser(user) ?? "unreachable";
}

/// The ID and login of a GitHub user object, if both look right.
function asUser(v: unknown): GitHubUser | null {
  if (typeof v !== "object" || v === null) return null;
  const { id, login } = v as Record<string, unknown>;
  if (typeof id !== "number" || !Number.isSafeInteger(id) || id <= 0) return null;
  if (typeof login !== "string" || !GITHUB_LOGIN.test(login)) return null;
  return { id, login };
}

/// The user who has this login on GitHub now. Asked with the app's
/// credentials, so it counts against the app's limit rather than the
/// Worker's IP, which Cloudflare shares with everyone. "missing" when no user
/// has it: GitHub answers 404, or the login belongs to an organization.
export async function githubAccount(login: string, env: Env): Promise<GitHubUser | "missing" | "unreachable"> {
  if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) return "unreachable";
  let res: Response;
  try {
    res = await fetch(`https://api.github.com/users/${encodeURIComponent(login)}`, {
      // As in applicationToken: a redirect must not carry the credentials.
      redirect: "manual",
      headers: githubHeaders(env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
  } catch {
    return "unreachable";
  }
  if (res.status === 404) {
    await discard(res);
    return "missing";
  }
  if (!res.ok) {
    if (res.status === 401) console.error("GitHub refused the app's client credentials");
    await discard(res);
    return "unreachable";
  }
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    return "unreachable";
  }
  if (typeof body === "object" && body !== null && (body as Record<string, unknown>)["type"] !== "User") return "missing";
  const user = asUser(body);
  // GitHub answers for the login asked about, in any case; anything else is
  // not an answer we can trust.
  if (!user || user.login.toLowerCase() !== login.toLowerCase()) return "unreachable";
  return user;
}

export function githubHeaders(clientId: string, clientSecret: string): Record<string, string> {
  return {
    Authorization: `Basic ${btoa(`${clientId}:${clientSecret}`)}`,
    Accept: "application/vnd.github+json",
    "User-Agent": "arakun-teams",
    "X-GitHub-Api-Version": "2022-11-28",
  };
}

/// Revokes this one token, not the user's other tokens of our app. 404 means
/// it is gone already: another sign-in with the same token got there first,
/// so this one gets no session. Anything else unclear keeps the user out too,
/// since the token would still work.
async function revokeToken(token: string, clientId: string, clientSecret: string): Promise<"revoked" | "rejected" | "unreachable"> {
  const res = await applicationToken("DELETE", token, clientId, clientSecret);
  if (!res) return "unreachable";
  await discard(res);
  if (res.status === 204) return "revoked";
  if (res.status === 404) return "rejected";
  console.error(`GitHub answered ${res.status} when asked to revoke a sign-in token`);
  return "unreachable";
}

/// One call to GitHub's Applications API about one token, with the app's
/// credentials. Null when GitHub cannot be reached.
async function applicationToken(method: "POST" | "DELETE", token: string, clientId: string, clientSecret: string): Promise<Response | null> {
  try {
    return await fetch(`https://api.github.com/applications/${encodeURIComponent(clientId)}/token`, {
      // A followed redirect would carry the client secret to wherever it points.
      // A 3xx is not ok, so it counts as GitHub being unreachable.
      redirect: "manual",
      method,
      headers: { ...githubHeaders(clientId, clientSecret), "Content-Type": "application/json" },
      body: JSON.stringify({ access_token: token }),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
  } catch {
    return null;
  }
}
