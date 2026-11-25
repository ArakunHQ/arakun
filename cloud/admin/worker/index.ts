import { error } from "./util";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);
    // The routes land in the next tasks. Until then the API and sign-in paths
    // are not the page, so they must not fall through to index.html.
    if (pathname.startsWith("/api/") || pathname.startsWith("/auth/")) return error(404, "not found");
    return env.ASSETS.fetch(request);
  },
} satisfies ExportedHandler<Env>;
