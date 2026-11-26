// The bindings wrangler.toml gives the Worker, in the shape `wrangler types`
// would write and the test runner's `env` is typed with.
declare namespace Cloudflare {
  interface Env {
    DB: D1Database;
    /// The phone page (`../phone/dist`), served on the account host and on the old
    /// relay hosts.
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
    /// Counts the requests for cloud access each user makes, since each one
    /// asks GitHub and may post to the webhook.
    CLOUD_ACCESS_LIMIT: RateLimit;
    /// Counts the beta sign-ups each user makes, since each one asks GitHub
    /// and may post to the webhook.
    BETA_LIMIT: RateLimit;
    /// One object per channel: its messages and live sockets.
    CHANNEL: DurableObjectNamespace<import("./channel").Channel>;
    /// Bearer for /admin, from `wrangler secret put ADMIN_SECRET`; unset = no admin.
    ADMIN_SECRET?: string;
    /// The GitHub OAuth App's client ID (public, in wrangler.toml).
    GITHUB_CLIENT_ID?: string;
    /// Its client secret, from `wrangler secret put GITHUB_CLIENT_SECRET`; it
    /// lets us ask GitHub whether our app issued a token. Unset = no sign-in.
    GITHUB_CLIENT_SECRET?: string;
    /// A Discord webhook URL, from `wrangler secret put CLOUD_REQUESTS_WEBHOOK`:
    /// each new request for cloud access is posted there. Unset = no post.
    CLOUD_REQUESTS_WEBHOOK?: string;
    /// A Discord webhook URL, from `wrangler secret put BETA_REQUESTS_WEBHOOK`:
    /// each new sign-up for a phone app beta is posted there. Unset = no post.
    BETA_REQUESTS_WEBHOOK?: string;
    /// Counts the feedback each client IP sends (src/feedback.ts).
    FEEDBACK_LIMIT: RateLimit;
    /// A Linear API key that may create issues and customer requests, from
    /// `wrangler secret put LINEAR_API_KEY`. Unset = no feedback to Linear.
    LINEAR_API_KEY?: string;
    /// The Linear team feedback is filed in (public, in wrangler.toml).
    LINEAR_TEAM_ID?: string;
    /// Counts the messages pushed to each native app's token (src/push.ts).
    PUSH_LIMIT: RateLimit;
    /// Apple's push key for the iOS app: the .p8 file's text, from
    /// `wrangler secret put APNS_KEY`. Unset (or any APNS_* unset) = no push
    /// to iOS.
    APNS_KEY?: string;
    /// That key's ID, as App Store Connect shows it.
    APNS_KEY_ID?: string;
    /// The Apple developer team the key belongs to.
    APNS_TEAM_ID?: string;
    /// The iOS app's bundle ID, which Apple delivers to.
    APNS_TOPIC?: string;
    /// A Google service account's JSON key, allowed to send with Firebase
    /// Cloud Messaging, from `wrangler secret put FCM_SERVICE_ACCOUNT`. Unset =
    /// no push to Android.
    FCM_SERVICE_ACCOUNT?: string;
    /// The Linear OAuth app's client ID, from `wrangler secret put
    /// LINEAR_CLIENT_ID`. Without all four LINEAR_* values, /linear is a 404.
    LINEAR_CLIENT_ID?: string;
    /// Its client secret.
    LINEAR_CLIENT_SECRET?: string;
    /// The secret that signs Linear's webhooks.
    LINEAR_WEBHOOK_SECRET?: string;
    /// 32 bytes in base64url: the key Linear's tokens are sealed with.
    LINEAR_TOKEN_KEY?: string;
    /// Counts the /linear calls each user makes, since some ask Linear.
    LINEAR_LIMIT: RateLimit;
  }
}
