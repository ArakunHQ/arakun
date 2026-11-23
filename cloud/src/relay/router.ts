// The old relay hosts, `<label>.<BASE_DOMAIN>`. Computers and devices now
// reach the relay on the account host with a session, so an old app or an old
// page that still comes here is only told to update. The page is still served,
// so an old page can show why, under the same policy as on the account host:
// these hosts are the account host's siblings.

import { refuseSocket } from "../machines";
import { withPagePolicy } from "../teams-router";
import type { Env } from "../util";
import { isLabel } from "./label";

export async function oldHost(request: Request, env: Env, pathname: string): Promise<Response> {
  switch (pathname) {
    case "/_relay":
      // The page names the account host in its "update" sentence. It is a
      // setting of its own, so the page cannot always guess it from this host.
      return Response.json(env.ACCOUNT_HOST ? { update: true, account_host: env.ACCOUNT_HOST.toLowerCase() } : { update: true }, { status: 410 });
    case "/pc":
    case "/ws":
      if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return new Response("expected a websocket", { status: 426 });
      return refuseSocket(pathname === "/pc" ? "pc" : "ws", "update");
    default:
      return withPagePolicy(await env.ASSETS.fetch(request), new URL(request.url).host);
  }
}

/// The label an old host names: its first level, when it has the shape of one
/// and the rest is the base domain.
export function labelFrom(hostname: string, env: Pick<Env, "BASE_DOMAIN">): string | null {
  const dot = hostname.indexOf(".");
  if (dot < 0 || hostname.slice(dot + 1) !== env.BASE_DOMAIN) return null;
  const first = hostname.slice(0, dot);
  return isLabel(first) ? first : null;
}
