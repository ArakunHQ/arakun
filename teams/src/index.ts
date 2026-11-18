// Arakun teams, first part: sign-in with GitHub and per-user feature flags.
// Only the desktop app calls this, so there is no CORS. Every error is JSON:
// { "error": "..." }.

import { admin } from "./admin";
import { logout, me, signIn } from "./auth";
import { error, type Env } from "./util";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);
    try {
      if (pathname.startsWith("/admin/")) return await admin(request, env, pathname);
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
  },
} satisfies ExportedHandler<Env>;
