// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Sign-in for operators, with GitHub's web flow, after ../../src/web_auth.ts:
// a redirect to GitHub, and back with a one-time code that is traded for a
// user access token. The token is used for one thing, to learn who the
// account is, and is revoked in every outcome. Nothing but GitHub ever sees it.
//
// GitHub is the first step only. An operator then gives a code from an
// authenticator app (worker/totp.ts), so a taken-over GitHub account is not
// enough to get in. An operator with no key yet sets one up first
// (worker/setup.ts).
//
// The `state` ties the callback to the browser that started it. Without it, a
// foreign page could send an operator's browser to the callback with the
// attacker's code and sign the operator in as the attacker.

import {
  allowed,
  clearedCodeStepCookie,
  clearedCookie,
  codeStepCookie,
  readCodeStep,
  sessionCookie,
  signCodeStep,
  signSession,
  type Operator,
} from "./session";
import { loadKey, useStep, type StoredKey } from "./keys";
import { clearedSetupCookie } from "./setup";
import { checkCode } from "./totp";
import { b64url, cookie, error } from "./util";

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
/// The operator's stored key does not open: SESSION_SECRET changed.
const RESET = "/?sign_in=reset";
/// GitHub said yes; the sign-in screen asks for the code.
const CODE_STEP = "/sign-in?step=code";
/// GitHub said yes to an operator with no key; the screen sets one up.
const SETUP_STEP = "/sign-in?step=setup";

/// What wrangler.toml holds until a person creates the OAuth App and sets the
/// real ID. GitHub would be asked with a client that does not exist.
const UNSET_CLIENT_ID = "PLACEHOLDER";

function configured(env: Env): env is Env & { GITHUB_CLIENT_SECRET: string } {
  return !!env.GITHUB_CLIENT_ID && env.GITHUB_CLIENT_ID !== UNSET_CLIENT_ID && !!env.GITHUB_CLIENT_SECRET;
}

/// Not counted against the sign-in limit: it asks nothing of GitHub. The
/// callback counts, so one sign-in counts once.
export async function authStart(request: Request, env: Env): Promise<Response> {
  if (!configured(env)) return redirect(FAILED);
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
  if (!configured(env) || !env.SESSION_SECRET || !env.ADMIN_SECRET) return redirect(FAILED);

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
  const op = { githubId: user.id, login: user.login };
  const stored = await loadKey(env, op);
  if (stored === "error") return redirect(FAILED);
  if (stored === "unreadable") return redirect(RESET);

  const value = await signCodeStep(op, env.SESSION_SECRET, Math.floor(Date.now() / 1000));
  const res = redirect(stored === "none" ? SETUP_STEP : CODE_STEP);
  res.headers.append("Set-Cookie", codeStepCookie(value));
  return res;
}

/// POST /auth/code with JSON { code }: the second step. A right code trades
/// the code step's cookie for a session. Only the panel's own page may ask.
export async function authCode(request: Request, env: Env): Promise<Response> {
  if (request.headers.get("Origin") !== new URL(request.url).origin) return error(403, "the request comes from another origin");
  const now = Math.floor(Date.now() / 1000);
  const op = await readCodeStep(request, env, now);
  // Expired, or a browser that never went through GitHub.
  if (!op || !env.SESSION_SECRET) return error(401, "sign-in expired");
  if (!(await codeAllowed(env, op))) return error(429, "too many codes; wait a minute");
  const code = text((await smallJson(request))?.["code"]);
  if (code === null) return error(400, "no code");

  const stored = await loadKey(env, op);
  if (stored === "none") return error(409, "no key");
  if (stored === "unreadable") return error(409, "key unreadable");
  if (stored === "error") return error(502, "the cloud did not answer");
  const result = await takeCode(env, op, stored, code, now);
  if (result !== "ok") return error(result === "used" ? 409 : 401, result === "used" ? "code used" : "wrong code");

  const res = new Response(null, { status: 204, headers: { "Cache-Control": "no-store" } });
  res.headers.append("Set-Cookie", sessionCookie(await signSession(op, env.SESSION_SECRET, now)));
  res.headers.append("Set-Cookie", clearedCodeStepCookie());
  return res;
}

/// Checks a code against the stored key and takes its step, so the same code
/// does not work twice, not even in two requests at once.
export async function takeCode(env: Env, op: Operator, stored: StoredKey, code: string, now: number): Promise<"ok" | "wrong" | "used"> {
  const step = await checkCode(stored.key, code, now);
  if (step === null) return "wrong";
  if (step <= stored.lastStep || !(await useStep(env, op, step))) return "used";
  return "ok";
}

/// Counted per operator, not per IP: a million codes are few enough to try
/// from many addresses. Every request that checks a code counts.
export async function codeAllowed(env: Env, op: Operator): Promise<boolean> {
  const { success } = await env.CODE_LIMIT.limit({ key: String(op.githubId) });
  return success;
}

/// Codes, and room for spaces an app shows in the middle.
const MAX_CODE_BODY = 256;

/// The request's small JSON object, or null.
export async function smallJson(request: Request): Promise<Record<string, unknown> | null> {
  const body = await request.text().catch(() => "");
  if (body.length > MAX_CODE_BODY) return null;
  try {
    const value: unknown = JSON.parse(body);
    return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

export function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

/// POST /auth/logout. Only the panel's own page may ask, so another site
/// cannot sign an operator out.
export function logout(request: Request): Response {
  if (request.headers.get("Origin") !== new URL(request.url).origin) return Response.json({ error: "wrong origin" }, { status: 403 });
  const res = new Response(null, { status: 204 });
  res.headers.append("Set-Cookie", clearedCookie());
  res.headers.append("Set-Cookie", clearedCodeStepCookie());
  res.headers.append("Set-Cookie", clearedSetupCookie());
  return res;
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
