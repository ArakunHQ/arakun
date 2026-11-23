// The relay phones reach a computer through. Every host under the base domain
// names one computer by its first level (the label); this only routes: the
// computer's socket and the phones' sockets go to that label's Durable
// Object, the page comes from the static assets, anything else is 404.

import type { Env } from "../util";
import { isLabel } from "./label";
import { disabledReason, isBlocked } from "./limits";

export async function relayRoute(request: Request, env: Env, pathname: string, label: string): Promise<Response> {
  switch (pathname) {
    case "/_relay": {
      // The page asks here before opening its socket, so a closed relay can
      // be shown as such instead of looking like a dead network.
      const [disabled, blocked] = await door(env, label);
      if (disabled) return Response.json({ label, disabled }, { status: 503 });
      if (blocked) return Response.json({ label, blocked: true }, { status: 403 });
      return Response.json({ label });
    }
    case "/pc":
    case "/ws": {
      const [disabled, blocked] = await door(env, label);
      if (disabled) return new Response(disabled, { status: 503 });
      if (blocked) return new Response("blocked", { status: 403 });
      // The object cannot see its own name: the label travels in a header.
      const headers = new Headers(request.headers);
      headers.set("x-arakun-label", label);
      return env.RELAY.get(env.RELAY.idFromName(label)).fetch(new Request(request, { headers }));
    }
    default:
      return env.ASSETS.fetch(request);
  }
}

/// Whether the relay is closed, and whether this label is blocked. Both KV
/// reads go out together, so a socket waits for one round trip, not two.
function door(env: Env, label: string): Promise<[string | null, boolean]> {
  return Promise.all([disabledReason(env.CONFIG), isBlocked(env.CONFIG, label)]);
}

/// The label a host names: its first level, when it has the shape of one.
export function labelFrom(hostname: string, env: Pick<Env, "LABEL_OVERRIDE">): string | null {
  if (env.LABEL_OVERRIDE) return isLabel(env.LABEL_OVERRIDE) ? env.LABEL_OVERRIDE : null;
  const first = hostname.split(".")[0] ?? "";
  return isLabel(first) ? first : null;
}
