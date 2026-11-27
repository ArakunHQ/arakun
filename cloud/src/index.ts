// One Worker for Arakun's cloud. A host `<label>.<BASE_DOMAIN>` is an old
// relay host: an old app or page is told to update there. The account host
// has the rest: sign-in, the account's machines and their sockets, the
// operator's /admin, the teams API and the phone page. With ACCOUNT_HOST set
// (production), that is the one host; any other answers 404, so the session
// cookie and the page live on one name only. Unset (a Worker of one's own on
// workers.dev, the tests), every other host is the account host.
//
// The cron triggers of wrangler.toml run `scheduled`: once a day, it keeps
// the Linear installs of quiet workspaces working.

import { keepWorkspaces } from "./linear_keep";
import { labelFrom, oldHost } from "./relay/router";
import { teamsFetch } from "./teams-router";
import { error, type Env } from "./util";

export { Channel } from "./channel";
export { CloudMachine } from "./cloud_machine";
export { Relay } from "./relay/relay";

export default {
  async fetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
    const url = new URL(request.url);
    if (labelFrom(url.hostname, env)) return oldHost(request, env, url.pathname);
    if (env.ACCOUNT_HOST && url.hostname !== env.ACCOUNT_HOST.toLowerCase()) return error(404, "not found");
    return teamsFetch(request, env, ctx);
  },
  async scheduled(_controller: ScheduledController, env: Env, ctx: ExecutionContext): Promise<void> {
    ctx.waitUntil(keepWorkspaces(env));
  },
} satisfies ExportedHandler<Env>;
