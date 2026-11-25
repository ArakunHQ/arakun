import { authCallback, authStart, logout } from "./auth";
import { readSession } from "./session";
import { error } from "./util";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);
    const route = `${request.method} ${pathname}`;
    if (route === "GET /auth/github/start") return authStart(request, env);
    if (route === "GET /auth/github/callback") return authCallback(request, env);
    if (route === "POST /auth/logout") return logout(request);
    if (route === "GET /api/me") {
      const op = await readSession(request, env, Math.floor(Date.now() / 1000));
      return op ? Response.json({ github_id: op.githubId, login: op.login }) : error(401, "not signed in");
    }
    // The other routes land in the next tasks. Until then the API and sign-in
    // paths are not the page, so they must not fall through to index.html.
    if (pathname.startsWith("/api/") || pathname.startsWith("/auth/")) return error(404, "not found");
    return env.ASSETS.fetch(request);
  },
} satisfies ExportedHandler<Env>;
