// The bindings wrangler.toml gives the Worker, in the shape `wrangler types`
// would write and the test runner's `env` is typed with.
declare namespace Cloudflare {
  interface Env {
    DB: D1Database;
    /// The phone page (`../phone`), served on the old relay hosts.
    ASSETS: Fetcher;
    /// One object per computer, named by its label: the relay.
    RELAY: DurableObjectNamespace<import("./relay/relay").Relay>;
    /// Global switches and per-plan limits of the relay.
    CONFIG: KVNamespace;
    /// The old relay hosts are `<label>.<BASE_DOMAIN>`; they only tell old
    /// apps and pages to update.
    BASE_DOMAIN: string;
    /// The one host that serves the account, its API and the page, such as
    /// `app.arakun.dev`; unset, any host that is not an old relay host does.
    ACCOUNT_HOST?: string;
    /// Counts sign-ins per client IP.
    SIGN_IN_LIMIT: RateLimit;
    /// Counts the messages each device sends on channel sockets.
    SEND_LIMIT: RateLimit;
    /// Counts the invites each user makes, since each one asks GitHub.
    INVITE_LIMIT: RateLimit;
    /// One object per channel: its messages and live sockets.
    CHANNEL: DurableObjectNamespace<import("./channel").Channel>;
    /// Bearer for /admin, from `wrangler secret put ADMIN_SECRET`; unset = no admin.
    ADMIN_SECRET?: string;
    /// The GitHub OAuth App's client ID (public, in wrangler.toml).
    GITHUB_CLIENT_ID?: string;
    /// Its client secret, from `wrangler secret put GITHUB_CLIENT_SECRET`; it
    /// lets us ask GitHub whether our app issued a token. Unset = no sign-in.
    GITHUB_CLIENT_SECRET?: string;
  }
}
