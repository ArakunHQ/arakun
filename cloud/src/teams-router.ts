// Arakun teams: sign-in with GitHub, per-user feature flags, and the teams of
// PROTOCOL.md. Only the desktop app calls this, so there is no CORS. Every
// error is JSON: { "error": "..." }.

import { admin } from "./admin";
import { logout, me, signIn } from "./auth";
import { teams } from "./teams";
import { error, type Env } from "./util";

export async function teamsFetch(request: Request, env: Env): Promise<Response> {
  const res = await route(request, env);
  // Every answer is about one account (sessions, flags, teams, keys), so no
  // cache, a browser's included, may keep it for the next account. A 101
  // carries the socket and no body. A copy, since an answer from the channel
  // object has headers that cannot be changed.
  if (res.status === 101) return res;
  const copy = new Response(res.body, res);
  copy.headers.set("Cache-Control", "no-store");
  return copy;
}

async function route(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  const { pathname } = url;
  try {
    if (pathname.startsWith("/admin/")) return await admin(request, env, pathname);
    const answer = await teams(request, env, url);
    if (answer) return answer;
    switch (`${request.method} ${pathname}`) {
      case "GET /healthz":
        return Response.json({ ok: true });
      case "POST /auth/github":
        return await signIn(request, env);
      case "GET /me":
        return await me(request, env);
      case "POST /auth/logout":
        return await logout(request, env);
      default:
        return error(404, "not found");
    }
  } catch (e) {
    console.error(`${request.method} ${pathname}:`, e);
    return error(500, "internal error");
  }
}
