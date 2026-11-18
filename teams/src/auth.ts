// Sign-in: the desktop app runs GitHub's device flow itself and hands us the
// user access token once. We ask GitHub whether our OAuth App issued it and
// to whom, keep only the numeric ID and the login, and answer with a session
// token of our own. The GitHub token is dropped as soon as the request ends.
//
// Asking /user would accept any token of the user's (the gh CLI's, a personal
// one, another app's), so whoever held one could sign in as them. The
// Applications API only answers for tokens our app issued, and it needs the
// app's client secret, which lives only here as a Worker secret: the desktop
// app's device flow has no secret.

import { DAY, b64url, bearer, error, flagList, flagsQuery, hex, now, sha256, type Env } from "./util";

const SESSION_DAYS = 90;
/// 32 bytes in base64url, as `signIn` makes them.
const SESSION_TOKEN = /^[A-Za-z0-9_-]{43}$/;
/// GitHub tokens are at most 255 characters; this leaves room for a change.
const MAX_ACCESS_TOKEN = 1024;
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
  if (!token) return error(400, 'expected {"access_token": "..."}');

  if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) return error(503, "sign-in is not set up");
  const user = await githubUser(token, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  if (user === "rejected") return error(401, "GitHub rejected the token");
  if (user === "unreachable") return error(502, "could not reach GitHub");

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
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(s.hash).run();
  return new Response(null, { status: 204 });
}

/// The session the request's bearer token names, if it is still valid. Every
/// endpoint that needs a user goes through here, and team endpoints will check
/// their flag after it.
export async function session(request: Request, env: Env): Promise<Session | null> {
  const token = bearer(request);
  if (!token || !SESSION_TOKEN.test(token)) return null;
  const hash = hex(await sha256(token));
  // A lookup by the hash itself, so no secret is compared here.
  const row = await env.DB.prepare(
    `SELECT s.github_id, s.expires_at, s.last_used_at, u.login
     FROM sessions s JOIN users u ON u.github_id = s.github_id
     WHERE s.token_hash = ?`,
  )
    .bind(hash)
    .first<{ github_id: number; expires_at: number; last_used_at: number; login: string }>();
  if (!row) return null;
  const t = now();
  if (row.expires_at <= t) {
    await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(hash).run();
    return null;
  }
  // Extending on every request would write to D1 on every request.
  if (t - row.last_used_at >= DAY) {
    await env.DB.batch([
      env.DB.prepare("UPDATE sessions SET expires_at = ?, last_used_at = ? WHERE token_hash = ?").bind(
        t + SESSION_DAYS * DAY,
        t,
        hash,
      ),
      env.DB.prepare("UPDATE users SET last_seen_at = ? WHERE github_id = ?").bind(t, row.github_id),
    ]);
  }
  return { hash, githubId: row.github_id, login: row.login };
}

async function accessToken(request: Request): Promise<string | null> {
  let body: unknown;
  try {
    body = await request.json();
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
  let res: Response;
  try {
    res = await fetch(`https://api.github.com/applications/${encodeURIComponent(clientId)}/token`, {
      method: "POST",
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
    return "unreachable";
  }
  // 404: no such token for this app (wrong app, revoked or expired). 422: not a token.
  if (res.status === 404 || res.status === 422) {
    await res.body?.cancel();
    return "rejected";
  }
  // 401 here means our own client ID or secret is wrong, not the user's token.
  if (!res.ok) {
    if (res.status === 401) console.error("GitHub refused the app's client credentials");
    await res.body?.cancel();
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
  // GitHub logins: letters, digits and hyphens, at most 39.
  if (typeof login !== "string" || !/^[A-Za-z0-9-]{1,39}$/.test(login)) return "unreachable";
  return { id, login };
}
