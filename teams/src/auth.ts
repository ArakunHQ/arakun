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

import { DAY, b64url, bearer, bodyText, discard, error, flagList, flagsQuery, hex, now, sha256, type Env } from "./util";

const SESSION_DAYS = 90;
/// Renewal never takes a session past this many days after its sign-in, so a
/// stolen session ends even while it is in use.
const MAX_SESSION_DAYS = 180;
/// 32 bytes in base64url, as `signIn` makes them.
const SESSION_TOKEN = /^[A-Za-z0-9_-]{43}$/;
/// GitHub tokens are at most 255 characters; this leaves room for a change.
const MAX_ACCESS_TOKEN = 1024;
/// Room for the token and some JSON around it. Anyone can call sign-in, so the
/// body is cut off here, before it is parsed.
const MAX_SIGN_IN_BODY = 4096;
const GITHUB_TIMEOUT_MS = 10_000;

export interface Session {
  hash: string;
  githubId: number;
  login: string;
}

interface GitHubUser {
  id: number;
  login: string;
}

export async function signIn(request: Request, env: Env): Promise<Response> {
  // Before sign-in the caller's IP is the only thing we know about it.
  const ip = request.headers.get("CF-Connecting-IP") ?? "unknown";
  const { success } = await env.SIGN_IN_LIMIT.limit({ key: ip });
  if (!success) return error(429, "too many sign-ins, try again in a minute");

  const token = await accessToken(request);
  if (token === TOO_BIG) return error(413, "the body is too big");
  if (!token) return error(400, 'expected {"access_token": "..."}');

  if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) return error(503, "sign-in is not set up");
  const user = await githubUser(token, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  if (user === "rejected") return error(401, "GitHub rejected the token");
  if (user === "unreachable") return error(502, "could not reach GitHub");
  // Before the session exists: a token that still works after sign-in could
  // make more sessions for whoever gets it later.
  const revoked = await revokeToken(token, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  if (revoked === "rejected") return error(401, "GitHub rejected the token");
  if (revoked === "unreachable") return error(502, "could not reach GitHub");

  const t = now();
  const session = b64url(crypto.getRandomValues(new Uint8Array(32)));
  const expiresAt = t + SESSION_DAYS * DAY;
  const db = env.DB;
  // One batch is one transaction: no session without its user row.
  const results = await db.batch([
    // Fills in the login of a row an admin made before the first sign-in.
    db
      .prepare(
        `INSERT INTO users (github_id, login, created_at, last_seen_at) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT (github_id) DO UPDATE SET login = excluded.login, last_seen_at = excluded.last_seen_at`,
      )
      .bind(user.id, user.login, t),
    // Each sign-in adds a session, so old ones are cleared here.
    db.prepare("DELETE FROM sessions WHERE github_id = ? AND expires_at <= ?").bind(user.id, t),
    db
      .prepare("INSERT INTO sessions (token_hash, github_id, created_at, expires_at, last_used_at) VALUES (?, ?, ?, ?, ?)")
      .bind(hex(await sha256(session)), user.id, t, expiresAt, t),
    flagsQuery(db, user.id),
  ]);
  const flags = flagList(results[3]!);
  return Response.json({ session, expires_at: expiresAt, github_id: user.id, login: user.login, flags });
}

export async function me(request: Request, env: Env): Promise<Response> {
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  const flags = flagList(await flagsQuery(env.DB, s.githubId).run());
  return Response.json({ github_id: s.githubId, login: s.login, flags });
}

export async function logout(request: Request, env: Env): Promise<Response> {
  // No renewal: a failed delete must not leave the session living longer.
  const s = await session(request, env, { renew: false });
  if (!s) return error(401, "not signed in");
  await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(s.hash).run();
  return new Response(null, { status: 204 });
}

/// The session the request's bearer token names, if it is still valid. Every
/// endpoint that needs a user goes through here, and team endpoints will check
/// their flag after it.
export async function session(request: Request, env: Env, { renew = true } = {}): Promise<Session | null> {
  const token = bearer(request);
  if (!token || !SESSION_TOKEN.test(token)) return null;
  const hash = hex(await sha256(token));
  // A lookup by the hash itself, so no secret is compared here.
  const row = await env.DB.prepare(
    `SELECT s.github_id, s.created_at, s.expires_at, s.last_used_at, u.login
     FROM sessions s JOIN users u ON u.github_id = s.github_id
     WHERE s.token_hash = ?`,
  )
    .bind(hash)
    .first<{ github_id: number; created_at: number; expires_at: number; last_used_at: number; login: string }>();
  if (!row) return null;
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
      env.DB.prepare("UPDATE users SET last_seen_at = ? WHERE github_id = ? AND last_seen_at < ?").bind(t, row.github_id, t),
    ]);
  }
  return { hash, githubId: row.github_id, login: row.login };
}

/// A body over the limit. A symbol, so no token string can be mistaken for it.
const TOO_BIG = Symbol("too big");

async function accessToken(request: Request): Promise<string | typeof TOO_BIG | null> {
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
  const token = (body as Record<string, unknown>)["access_token"];
  if (typeof token !== "string" || token.length === 0 || token.length > MAX_ACCESS_TOKEN) return null;
  return token;
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
  if (typeof user !== "object" || user === null) return "unreachable";
  const { id, login } = user as Record<string, unknown>;
  if (typeof id !== "number" || !Number.isSafeInteger(id) || id <= 0) return "unreachable";
  // GitHub logins: letters, digits and hyphens, at most 39. Enterprise Managed
  // Users also have an underscore before their enterprise's short code.
  if (typeof login !== "string" || !/^[A-Za-z0-9_-]{1,39}$/.test(login)) return "unreachable";
  return { id, login };
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
      headers: {
        Authorization: `Basic ${btoa(`${clientId}:${clientSecret}`)}`,
        Accept: "application/vnd.github+json",
        "Content-Type": "application/json",
        "User-Agent": "arakun-teams",
        "X-GitHub-Api-Version": "2022-11-28",
      },
      body: JSON.stringify({ access_token: token }),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
  } catch {
    return null;
  }
}
