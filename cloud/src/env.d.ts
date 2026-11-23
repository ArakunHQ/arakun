// The bindings wrangler.toml gives the Worker, in the shape `wrangler types`
// would write and the test runner's `env` is typed with.
declare namespace Cloudflare {
  interface Env {
    DB: D1Database;
    /// The phone page (`../phone`), served by the relay router.
    ASSETS: Fetcher;
    /// One object per computer, named by its label: the relay.
    RELAY: DurableObjectNamespace<import("./relay/relay").Relay>;
    /// Global switches and per-plan limits of the relay.
    CONFIG: KVNamespace;
    /// Hosts under this domain carry the relay's label as their first level.
    BASE_DOMAIN: string;
    /// `wrangler dev` has no subdomains: this names the one label it serves.
    LABEL_OVERRIDE?: string;
    /// Counts sign-ins per client IP.
    SIGN_IN_LIMIT: RateLimit;
    /// Counts the messages each device sends on channel sockets.
    SEND_LIMIT: RateLimit;
    /// Counts the invites each user makes, since each one asks GitHub.
    INVITE_LIMIT: RateLimit;
    /// One object per channel: its messages and live sockets.
    CHANNEL: DurableObjectNamespace<import("./channel").Channel>;
    /// Bearer for /admin and /_admin, from `wrangler secret put ADMIN_SECRET`; unset = no admin.
    ADMIN_SECRET?: string;
    /// The GitHub OAuth App's client ID (public, in wrangler.toml).
    GITHUB_CLIENT_ID?: string;
    /// Its client secret, from `wrangler secret put GITHUB_CLIENT_SECRET`; it
    /// lets us ask GitHub whether our app issued a token. Unset = no sign-in.
    GITHUB_CLIENT_SECRET?: string;
  }
}
