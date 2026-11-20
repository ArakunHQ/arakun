// The bindings wrangler.toml gives the Worker, in the shape `wrangler types`
// would write and the test runner's `env` is typed with.
declare namespace Cloudflare {
  interface Env {
    DB: D1Database;
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
