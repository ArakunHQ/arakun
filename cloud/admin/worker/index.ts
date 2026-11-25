import { authCallback, authStart, logout } from "./auth";
import { page, proxy } from "./proxy";
import { readSession } from "./session";
import { error, json } from "./util";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);
    const route = `${request.method} ${pathname}`;
    if (route === "GET /auth/github/start") return authStart(request, env);
    if (route === "GET /auth/github/callback") return authCallback(request, env);
    if (route === "POST /auth/logout") return logout(request);
    // Answered here for every method, never forwarded.
    if (pathname === "/api/me") {
      if (request.method !== "GET") return error(405, "method not allowed");
      const op = await readSession(request, env, Math.floor(Date.now() / 1000));
      return op ? json({ github_id: op.githubId, login: op.login }) : error(401, "not signed in");
    }
    if (pathname.startsWith("/api/")) {
      const op = await readSession(request, env, Math.floor(Date.now() / 1000));
      return op ? proxy(request, env, op) : error(401, "not signed in");
    }
    // A sign-in path that is none of the routes above is not the page either.
    if (pathname.startsWith("/auth/")) return error(404, "not found");
    if (request.method !== "GET" && request.method !== "HEAD") return error(405, "method not allowed");
    return page(request, env);
  },
} satisfies ExportedHandler<Env>;
