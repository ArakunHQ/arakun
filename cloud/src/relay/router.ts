// The old relay hosts, `<label>.<BASE_DOMAIN>`. Computers and devices now
// reach the relay on the account host with a session, so an old app or an old
// page that still comes here is only told to update. The page is still served,
// so an old page can show why.

import { refuseSocket } from "../machines";
import type { Env } from "../util";
import { isLabel } from "./label";

export async function oldHost(request: Request, env: Env, pathname: string): Promise<Response> {
  switch (pathname) {
    case "/_relay":
      return Response.json({ update: true }, { status: 410 });
    case "/pc":
    case "/ws":
      if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return new Response("expected a websocket", { status: 426 });
      return refuseSocket(pathname === "/pc" ? "pc" : "ws", "update");
    default:
      return env.ASSETS.fetch(request);
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
