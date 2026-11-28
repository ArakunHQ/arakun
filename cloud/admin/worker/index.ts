// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Cloudflare Access signs operators in before a request gets here; the
// Worker checks its token on every request (worker/access.ts).
import { readOperator } from "./access";
import { page, proxy } from "./proxy";
import { error, json } from "./util";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);
    // Answered here for every method, never forwarded.
    if (pathname === "/api/me") {
      if (request.method !== "GET") return error(405, "method not allowed");
      const op = await readOperator(request, env, Math.floor(Date.now() / 1000));
      return op ? json({ email: op.email }) : error(401, "not signed in");
    }
    if (pathname.startsWith("/api/")) {
      const op = await readOperator(request, env, Math.floor(Date.now() / 1000));
      return op ? proxy(request, env, op) : error(401, "not signed in");
    }
    if (request.method !== "GET" && request.method !== "HEAD") return error(405, "method not allowed");
    return page(request, env);
  },
} satisfies ExportedHandler<Env>;
