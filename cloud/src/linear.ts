// Linear delegation: Linear hands an issue to Arakun and the Worker passes it
// to the user's computer. Every path is under /linear.
//
//   GET /linear/link      → { links: [] }, for a signed-in user with the flag
//   GET /linear/install   → a redirect to Linear's consent page
//   GET /linear/callback  → where Linear sends the admin back to
//
// Without all four LINEAR_* secrets every /linear path is a 404. All routes
// but the webhook and the OAuth callback (which Linear calls) need a session
// with the `linear` flag.

import { cookie, session, type Session } from "./auth";
import { exchangeCode, LinearRefused, saveWorkspace, viewer } from "./linear_api";
import { b64url, error, sha256, type Env } from "./util";

export const FLAG = "linear";

/// The ids Linear uses are UUIDs.
export const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/// Whether the app is set up: a Worker without its secrets does not pretend
/// to take part.
export function configured(env: Env): boolean {
  return Boolean(env.LINEAR_CLIENT_ID && env.LINEAR_CLIENT_SECRET && env.LINEAR_WEBHOOK_SECRET && env.LINEAR_TOKEN_KEY);
}

/// The session, or the 401 or 403 to send.
export async function signedIn(request: Request, env: Env): Promise<Session | Response> {
  const s = await session(request, env, { flag: FLAG });
  if (!s) return error(401, "not signed in");
  if (s.blocked || !s.flagged) return error(403, "Linear is not available for this account");
  return s;
}

/// The answer for a /linear route, or null when the path is not one.
export async function linear(request: Request, env: Env, url: URL): Promise<Response | null> {
  const { pathname } = url;
  if (pathname !== "/linear" && !pathname.startsWith("/linear/")) return null;
  if (!configured(env)) return error(404, "not found");
  const key = `${request.method} ${pathname}`;
  // Linear's own calls carry no session; later tasks add them here. The
  // callback is a browser's, and does its own check of session and flag.
  if (key === "GET /linear/callback") return callback(request, env, url);
  const who = await signedIn(request, env);
  if (who instanceof Response) return who;
  switch (key) {
    case "GET /linear/link":
      return Response.json({ links: [] });
    case "GET /linear/install":
      return install(env, url, who);
    default:
      return error(404, "not found");
  }
}

/// `__Host-`, so a sibling host cannot plant a state of its own.
const STATE_COOKIE = "__Host-arakun_linear_state";
const STATE_SECONDS = 600;
const MAX_CODE = 512;
const MAX_NAME = 100;

const callbackUri = (url: URL) => `https://${url.host}/linear/callback`;

function install(env: Env, url: URL, who: Session): Response {
  const state = b64url(crypto.getRandomValues(new Uint8Array(16)));
  const to = new URL("https://linear.app/oauth/authorize");
  to.searchParams.set("client_id", env.LINEAR_CLIENT_ID!);
  to.searchParams.set("redirect_uri", callbackUri(url));
  to.searchParams.set("response_type", "code");
  to.searchParams.set("scope", "read,write,app:assignable,app:mentionable");
  to.searchParams.set("actor", "app");
  to.searchParams.set("state", state);
  const res = new Response(null, { status: 302, headers: { Location: to.toString() } });
  // Lax, so the cookie comes back with Linear's redirect, a top-level GET.
  res.headers.append("Set-Cookie", `${STATE_COOKIE}=${state}.${who.githubId}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${STATE_SECONDS}`);
  return res;
}

/// Equal as SHA-256 hashes, which are all one length.
async function same(a: string, b: string): Promise<boolean> {
  const [x, y] = await Promise.all([sha256(a), sha256(b)]);
  return crypto.subtle.timingSafeEqual(x, y);
}

async function callback(request: Request, env: Env, url: URL): Promise<Response> {
  // A state is used once: every outcome clears it.
  const done = (res: Response): Response => {
    res.headers.append("Set-Cookie", `${STATE_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0`);
    return res;
  };
  const saved = cookie(request, STATE_COOKIE);
  const state = url.searchParams.get("state");
  const dot = saved?.lastIndexOf(".") ?? -1;
  if (!saved || !state || dot < 1 || !(await same(saved.slice(0, dot), state))) return done(error(400, "bad state"));
  const who = await signedIn(request, env);
  if (who instanceof Response) return done(who);
  if (saved.slice(dot + 1) !== String(who.githubId)) return done(error(400, "bad state"));
  const code = url.searchParams.get("code");
  if (url.searchParams.has("error") || !code || code.length > MAX_CODE) return done(error(400, "linear refused the install"));
  try {
    const tokens = await exchangeCode(env, code, callbackUri(url));
    const app = await viewer(tokens.access);
    await saveWorkspace(env, {
      organizationId: app.organizationId,
      name: app.organizationName,
      appUserId: app.userId,
      access: tokens.access,
      refresh: tokens.refresh,
      expiresAt: tokens.expiresAt,
      installedBy: who.githubId,
    });
    const text = `Arakun is installed in ${app.organizationName.slice(0, MAX_NAME)}. You can close this page.`;
    return done(new Response(text, { headers: { "Content-Type": "text/plain; charset=utf-8" } }));
  } catch (e) {
    if (e instanceof LinearRefused) return done(error(502, "linear refused the install"));
    throw e;
  }
}
