// Sign-in for operators, with GitHub's web flow, after ../../src/web_auth.ts:
// a redirect to GitHub, and back with a one-time code that is traded for a
// user access token. The token is used for one thing, to learn who the
// account is, and is revoked in every outcome. Nothing but GitHub ever sees it.
//
// The `state` ties the callback to the browser that started it. Without it, a
// foreign page could send an operator's browser to the callback with the
// attacker's code and sign the operator in as the attacker.

import { allowed, clearedCookie, sessionCookie, signSession } from "./session";
import { b64url, cookie } from "./util";

/// `__Host-`, as the session cookie: a sibling host cannot plant a state of
/// its own. The prefix needs `Path=/`.
const OAUTH_COOKIE = "__Host-arakun_admin_oauth";
/// Ten minutes to get through GitHub's page.
const OAUTH_SECONDS = 600;
/// GitHub's codes are 20 characters; this leaves room for a change.
const MAX_CODE = 256;
const MAX_ACCESS_TOKEN = 1024;
const GITHUB_TIMEOUT_MS = 10_000;
const CALLBACK = "/auth/github/callback";
const GITHUB_LOGIN = /^[A-Za-z0-9-]{1,39}$/;

/// The browser comes to both routes by itself, so every outcome sends it back
/// to the page. What the page learns is only that it failed, or was denied:
/// nothing GitHub or the request said goes back.
const FAILED = "/?sign_in=failed";
const DENIED = "/?sign_in=denied";

/// Not counted against the sign-in limit: it asks nothing of GitHub. The
/// callback counts, so one sign-in counts once.
export async function authStart(request: Request, env: Env): Promise<Response> {
  if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) return redirect(FAILED);
  const state = b64url(crypto.getRandomValues(new Uint8Array(32)));
  const url = new URL("https://github.com/login/oauth/authorize");
  url.searchParams.set("client_id", env.GITHUB_CLIENT_ID);
  url.searchParams.set("state", state);
  url.searchParams.set("redirect_uri", `${new URL(request.url).origin}${CALLBACK}`);
  const res = redirect(url.toString());
  // Lax, so the cookie comes back with GitHub's redirect, a top-level GET.
  res.headers.append("Set-Cookie", `${OAUTH_COOKIE}=${state}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${OAUTH_SECONDS}`);
  return res;
}

export async function authCallback(request: Request, env: Env): Promise<Response> {
  // The state cookie is single use: every outcome clears it.
  const done = (res: Response): Response => {
    res.headers.append("Set-Cookie", `${OAUTH_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0`);
    return res;
  };
  try {
    return done(await callback(request, env));
  } catch (e) {
    // The router would answer 500 for a throw, without the cookie cleared.
    console.error("GET /auth/github/callback:", e instanceof Error ? e.message : "failed");
    return done(redirect(FAILED));
  }
}

async function callback(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  // A stale or doubled callback (the back button, a second tab) ends here.
  if (!(await sameState(url.searchParams.get("state"), cookie(request, OAUTH_COOKIE)))) return redirect(FAILED);
  if (!(await signInAllowed(request, env))) return redirect(FAILED);
  if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET || !env.SESSION_SECRET) return redirect(FAILED);

  // No code when the user refused on GitHub's page.
  const code = url.searchParams.get("code");
  if (!code || code.length > MAX_CODE) return redirect(FAILED);
  const token = await exchangeCode(code, `${url.origin}${CALLBACK}`, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  if (!token) return redirect(FAILED);

  // Once there is a token it is revoked whatever the answer, so none is left
  // behind for an account that is not let in.
  const user = await githubUser(token, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  const revoked = await revokeToken(token, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  // A token that still works is not a sign-in either.
  if (!user || !revoked) return redirect(FAILED);
  if (!allowed(env.ADMIN_GITHUB_IDS, user.id)) return redirect(DENIED);

  const value = await signSession({ githubId: user.id, login: user.login }, env.SESSION_SECRET, Math.floor(Date.now() / 1000));
  const res = redirect("/");
  res.headers.append("Set-Cookie", sessionCookie(value));
  return res;
}

/// POST /auth/logout. Only the panel's own page may ask, so another site
/// cannot sign an operator out.
export function logout(request: Request): Response {
  if (request.headers.get("Origin") !== new URL(request.url).origin) return Response.json({ error: "wrong origin" }, { status: 403 });
  return new Response(null, { status: 204, headers: { "Set-Cookie": clearedCookie() } });
}

function redirect(location: string): Response {
  return new Response(null, { status: 302, headers: { Location: location } });
}

/// Sign-in counts against one limit per client IP, as the cloud Worker's does.
async function signInAllowed(request: Request, env: Env): Promise<boolean> {
  const { success } = await env.SIGN_IN_LIMIT.limit({ key: request.headers.get("CF-Connecting-IP") ?? "unknown" });
  return success;
}

async function sha256(text: string): Promise<string> {
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return b64url(hash);
}

/// Both present and equal. Compared as SHA-256 hashes, which are all one
/// length and leave nothing to time.
async function sameState(state: string | null, saved: string | null): Promise<boolean> {
  if (!state || !saved) return false;
  const [a, b] = await Promise.all([sha256(state), sha256(saved)]);
  return a === b;
}

async function drop(res: Response): Promise<void> {
  await res.body?.cancel().catch(() => {});
}

/// The user access token for a code, or null for any other answer. GitHub
/// answers 200 with `{ error }` for a bad code.
async function exchangeCode(code: string, redirectUri: string, clientId: string, clientSecret: string): Promise<string | null> {
  try {
    const res = await fetch("https://github.com/login/oauth/access_token", {
      // A followed redirect would carry the client secret on.
      redirect: "manual",
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json", "User-Agent": "arakun-admin" },
      body: JSON.stringify({ client_id: clientId, client_secret: clientSecret, code, redirect_uri: redirectUri }),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
    if (!res.ok) {
      await drop(res);
      return null;
    }
    const body: unknown = await res.json();
    const token = typeof body === "object" && body !== null ? (body as Record<string, unknown>)["access_token"] : null;
    return typeof token === "string" && token.length > 0 && token.length <= MAX_ACCESS_TOKEN ? token : null;
  } catch {
    return null;
  }
}

/// One call to GitHub's Applications API about one token, with the app's
/// credentials. Null when GitHub cannot be reached.
async function applicationToken(method: "POST" | "DELETE", token: string, clientId: string, clientSecret: string): Promise<Response | null> {
  try {
    return await fetch(`https://api.github.com/applications/${encodeURIComponent(clientId)}/token`, {
      // A followed redirect would carry the client secret to wherever it points.
      redirect: "manual",
      method,
      headers: {
        Authorization: `Basic ${btoa(`${clientId}:${clientSecret}`)}`,
        Accept: "application/vnd.github+json",
        "Content-Type": "application/json",
        "User-Agent": "arakun-admin",
        "X-GitHub-Api-Version": "2022-11-28",
      },
      body: JSON.stringify({ access_token: token }),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
  } catch {
    return null;
  }
}

/// Who GitHub says owns the token, if our app issued it; null for any other
/// answer. The ID and login are checked: both end up in a cookie.
async function githubUser(token: string, clientId: string, clientSecret: string): Promise<{ id: number; login: string } | null> {
  const res = await applicationToken("POST", token, clientId, clientSecret);
  if (!res) return null;
  if (!res.ok) {
    if (res.status === 401) console.error("GitHub refused the app's client credentials");
    await drop(res);
    return null;
  }
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    return null;
  }
  const user = typeof body === "object" && body !== null ? (body as Record<string, unknown>)["user"] : null;
  if (typeof user !== "object" || user === null) return null;
  const { id, login } = user as Record<string, unknown>;
  if (typeof id !== "number" || !Number.isSafeInteger(id) || id <= 0) return null;
  if (typeof login !== "string" || !GITHUB_LOGIN.test(login)) return null;
  return { id, login };
}

/// Revokes this one token. Only a 204 counts: a 404 means another request
/// got there first, and anything else leaves the token working.
async function revokeToken(token: string, clientId: string, clientSecret: string): Promise<boolean> {
  const res = await applicationToken("DELETE", token, clientId, clientSecret);
  if (!res) return false;
  await drop(res);
  if (res.status === 204) return true;
  if (res.status !== 404) console.error(`GitHub answered ${res.status} when asked to revoke a sign-in token`);
  return false;
}
