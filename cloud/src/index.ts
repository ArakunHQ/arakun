// One Worker for Arakun's cloud. A host whose first level is a label under the
// base domain is a phone reaching its computer: the relay. The operator's
// /_admin comes in under any host. Everything else is the teams API, which
// the desktop app calls.

import { admin as relayAdmin } from "./relay/admin";
import { labelFrom, relayRoute } from "./relay/router";
import { teamsFetch } from "./teams-router";
import type { Env } from "./util";

export { Channel } from "./channel";
export { Relay } from "./relay/relay";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    // The operator comes in under any host; the secret is the door.
    if (url.pathname.startsWith("/_admin/")) return relayAdmin(request, env, url.pathname);
    const label = labelFrom(url.hostname, env);
    if (label) return relayRoute(request, env, url.pathname, label);
    return teamsFetch(request, env);
  },
} satisfies ExportedHandler<Env>;
