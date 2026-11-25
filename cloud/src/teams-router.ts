// The account host: sign-in with GitHub, sessions, per-user feature flags, the
// account's machines and their relay sockets, and the teams of PROTOCOL.md.
// There is no CORS: the app and the page on this same host call it. Every
// error is JSON: { "error": "..." }. A GET outside the API is the phone page.

import { admin } from "./admin";
import { cloudAccess } from "./cloud_access";
import { authConfig, deleteSession, ForeignOrigin, listSessions, logout, me, signIn } from "./auth";
import { machines } from "./machines";
import { notices } from "./notices";
import { teams } from "./teams";
import { webAuthCallback, webAuthStart } from "./web_auth";
import { error, type Env } from "./util";

/// The API's paths: the first segment of each route, so an unknown path under
/// one stays a JSON 404 and never turns into the page.
const API = new Set(["auth", "me", "sessions", "machines", "notices", "admin", "cloud", "healthz", "devices", "users", "teams", "invites", "pending", "_relay"]);

export async function teamsFetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
  // The page and its files are the same for every account, so the assets'
  // own caching stands.
  if (isPage(request)) return withPagePolicy(await env.ASSETS.fetch(request), new URL(request.url).host);
  const res = await route(request, env, ctx);
  // Every answer is about one account (sessions, flags, teams, keys), so no
  // cache, a browser's included, may keep it for the next account. A 101
  // carries the socket and no body. A copy, since an answer from the channel
  // object has headers that cannot be changed.
  if (res.status === 101) return res;
  const copy = new Response(res.body, res);
  copy.headers.set("Cache-Control", "no-store");
  return copy;
}

/// What the page may load. It holds a session cookie and runs commands on
/// the account's computers, so script that slipped into it (through a
/// transcript, say) must not run, nor load or send anything elsewhere.
/// - Scripts, sockets and fetches: this host only. The socket is named with
///   `wss:` too, as some browsers do not count it under 'self'.
/// - Images: this host, and `data:` and `blob:` for pictures a session shows
///   (through the relay they arrive as blobs). `https:` too: a transcript may
///   show a picture from the web, as it did before; with no referrer it
///   carries nothing of the page.
/// - Styles: inline too, for the few `style` attributes the page writes.
/// - Forms only to here and GitHub; no framing.
export function pagePolicy(host: string): string {
  return [
    "default-src 'self'",
    "script-src 'self'",
    `connect-src 'self' wss://${host}`,
    "img-src 'self' data: blob: https:",
    "style-src 'self' 'unsafe-inline'",
    "frame-ancestors 'none'",
    "base-uri 'none'",
    "form-action 'self' https://github.com",
  ].join("; ");
}

/// The page's answer with its policy. A copy: the assets' headers cannot be
/// changed in place.
export function withPagePolicy(res: Response, host: string): Response {
  const copy = new Response(res.body, res);
  copy.headers.set("Content-Security-Policy", pagePolicy(host));
  copy.headers.set("X-Content-Type-Options", "nosniff");
  copy.headers.set("Referrer-Policy", "no-referrer");
  return copy;
}

function isPage(request: Request): boolean {
  if (request.method !== "GET" && request.method !== "HEAD") return false;
  const first = new URL(request.url).pathname.split("/")[1] ?? "";
  return !API.has(first);
}

async function route(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
  const url = new URL(request.url);
  const { pathname } = url;
  try {
    if (pathname.startsWith("/admin/")) return await admin(request, env, url);
    const cloud = await cloudAccess(request, env, url, ctx);
    if (cloud) return cloud;
    const machine = await machines(request, env, url);
    if (machine) return machine;
    const notice = await notices(request, env, url);
    if (notice) return notice;
    const answer = await teams(request, env, url);
    if (answer) return answer;
    switch (`${request.method} ${pathname}`) {
      case "GET /healthz":
        return Response.json({ ok: true });
      case "POST /auth/github":
        return await signIn(request, env);
      case "GET /auth/github/start":
        return await webAuthStart(request, env);
      case "GET /auth/github/callback":
        return await webAuthCallback(request, env);
      case "GET /auth/config":
        return authConfig(env);
      case "GET /sessions":
        return await listSessions(request, env);
      case "GET /me":
        return await me(request, env);
      case "POST /auth/logout":
        return await logout(request, env);
      default: {
        const id = request.method === "DELETE" ? /^\/sessions\/([0-9a-f]{32})$/.exec(pathname)?.[1] : undefined;
        if (id) return await deleteSession(request, env, id);
        return error(404, "not found");
      }
    }
  } catch (e) {
    if (e instanceof ForeignOrigin) return error(403, "the request comes from another origin");
    console.error(`${request.method} ${pathname}:`, e);
    return error(500, "internal error");
  }
}
