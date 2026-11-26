// Sign-in for the phone page, with GitHub's web flow: a redirect to GitHub,
// and back with a one-time code. The code is traded for a user access token,
// and from there it is the app's sign-in (`verifyAndRevoke`, `newSession`):
// the token is checked, read for the email, revoked, and a session of ours
// goes into a cookie. The page never sees a GitHub token.
//
// The `state` ties the callback to the browser that started it. Without it,
// a foreign page could send a victim's browser to the callback with the
// attacker's code and sign the victim in as the attacker.

import { cookie, GITHUB_TIMEOUT_MS, MAX_ACCESS_TOKEN, MAX_SESSION_DAYS, newSession, SESSION_COOKIE, SESSION_TOKEN, signInAllowed, verifyAndRevoke } from "./auth";
import { recheckAccount } from "./machines";
import { DAY, b64url, discard, hex, isObject, sha256, type Env } from "./util";

/// `__Host-`, as the session cookie: a sibling host under the same domain
/// cannot plant a state of its own and sign the browser in to its account.
/// The prefix needs `Path=/`, so every request carries it for the ten minutes
/// it lives.
const OAUTH_COOKIE = "__Host-arakun_oauth";
/// Ten minutes to get through GitHub's page.
const OAUTH_SECONDS = 600;
/// GitHub's codes are 20 characters; this leaves room for a change.
const MAX_CODE = 256;
const CALLBACK = "/auth/github/callback";

/// The browser comes to both routes by itself, so every failure sends it back
/// to the page, which says the sign-in failed: JSON would be shown as it is.
const FAILED = "/?sign_in=failed";

/// Not counted against the sign-in limit: it asks nothing of GitHub or D1.
/// The callback counts, so one sign-in counts once.
export async function webAuthStart(request: Request, env: Env): Promise<Response> {
  if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) return redirect(FAILED);
  const state = b64url(crypto.getRandomValues(new Uint8Array(32)));
  const url = new URL("https://github.com/login/oauth/authorize");
  url.searchParams.set("client_id", env.GITHUB_CLIENT_ID);
  url.searchParams.set("state", state);
  url.searchParams.set("scope", "user:email");
  url.searchParams.set("redirect_uri", `${new URL(request.url).origin}${CALLBACK}`);
  const res = new Response(null, { status: 302, headers: { Location: url.toString() } });
  // Lax, so the cookie comes back with GitHub's redirect, a top-level GET.
  res.headers.append("Set-Cookie", `${OAUTH_COOKIE}=${state}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${OAUTH_SECONDS}`);
  return res;
}

export async function webAuthCallback(request: Request, env: Env): Promise<Response> {
  // The state cookie is single use: every outcome clears it.
  const done = (res: Response): Response => {
    res.headers.append("Set-Cookie", `${OAUTH_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0`);
    return res;
  };
  // What the page learns on a failure is only that it failed: no answer of
  // GitHub, and nothing the request sent, goes back.
  const failed = () => done(redirect(FAILED));
  try {
    return await callback(request, env, done, failed);
  } catch (e) {
    // The router would answer 500 for a throw, without the cookie cleared.
    console.error("GET /auth/github/callback:", e);
    return failed();
  }
}

async function callback(request: Request, env: Env, done: (res: Response) => Response, failed: () => Response): Promise<Response> {
  const url = new URL(request.url);
  // A stale or doubled callback (the back button, a second tab) ends here.
  if (!(await sameState(url.searchParams.get("state"), cookie(request, OAUTH_COOKIE)))) return failed();
  if (!(await signInAllowed(request, env))) return failed();
  if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) return failed();

  // No code when the user refused on GitHub's page.
  const code = url.searchParams.get("code");
  if (!code || code.length > MAX_CODE) return failed();
  const token = await exchangeCode(code, `${url.origin}${CALLBACK}`, env.GITHUB_CLIENT_ID, env.GITHUB_CLIENT_SECRET);
  if (!token) return failed();
  const user = await verifyAndRevoke(token, env);
  if (user instanceof Response) {
    await discard(user);
    return failed();
  }

  const made = await newSession(env, user, platform(request.headers.get("User-Agent") ?? ""), "phone");
  // After the new session, which is what tells us the account: a sign-in by a
  // GitHub user with no account yet has no old session of it to end.
  await endReplacedSession(request, env, made.account_id, made.session);
  const res = redirect("/");
  // The session's row says when it ends, and use renews it. The cookie lives
  // as long as a session can at most, so the browser does not drop it first
  // and leave a row behind that nobody can use.
  res.headers.append("Set-Cookie", `${SESSION_COOKIE}=${made.session}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${MAX_SESSION_DAYS * DAY}`);
  return done(res);
}

/// A browser that signs in again over a session cookie it still holds: that
/// session's row goes, or it would stay listed as a signed-in device no
/// browser can use. Only a session of the account signing in: one of
/// another account is not this sign-in's to end.
async function endReplacedSession(request: Request, env: Env, accountId: number, current: string): Promise<void> {
  const old = cookie(request, SESSION_COOKIE);
  if (!old || old === current || !SESSION_TOKEN.test(old)) return;
  const result = await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ? AND account_id = ?").bind(hex(await sha256(old)), accountId).run();
  if (result.meta.changes > 0) await recheckAccount(env, accountId);
}

function redirect(location: string): Response {
  return new Response(null, { status: 302, headers: { Location: location } });
}

/// Both present and equal. Compared as SHA-256 hashes, which are all one
/// length and leave nothing to time.
async function sameState(state: string | null, saved: string | null): Promise<boolean> {
  if (!state || !saved) return false;
  const [a, b] = await Promise.all([sha256(state), sha256(saved)]);
  return hex(a) === hex(b);
}

/// The user access token for a code, or null for any other answer. GitHub
/// answers 200 with `{ error }` for a bad code.
async function exchangeCode(code: string, redirectUri: string, clientId: string, clientSecret: string): Promise<string | null> {
  try {
    const res = await fetch("https://github.com/login/oauth/access_token", {
      // A followed redirect would carry the client secret on.
      redirect: "manual",
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json", "User-Agent": "arakun-teams" },
      body: JSON.stringify({ client_id: clientId, client_secret: clientSecret, code, redirect_uri: redirectUri }),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
    if (!res.ok) {
      await discard(res);
      return null;
    }
    const body: unknown = await res.json();
    const token = isObject(body) ? body["access_token"] : null;
    return typeof token === "string" && token.length > 0 && token.length <= MAX_ACCESS_TOKEN ? token : null;
  } catch {
    return null;
  }
}

/// A name for the signed-in device from the browser's User-Agent. The mobile
/// systems are asked first: their agents also say "Mac OS X" or "Linux".
function platform(userAgent: string): string {
  for (const [pattern, name] of [
    [/iPhone/, "iPhone"],
    [/iPad/, "iPad"],
    [/Android/, "Android"],
    [/Mac/, "Mac"],
    [/Windows/, "Windows"],
    [/Linux/, "Linux"],
  ] as const) {
    if (pattern.test(userAgent)) return name;
  }
  return "Browser";
}
