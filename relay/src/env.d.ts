// The bindings wrangler.toml gives the Worker, in the shape `wrangler types`
// would write and the test runner's `env` is typed with.
declare namespace Cloudflare {
  interface Env {
    /// Hosts under this domain carry the label as their first level.
    BASE_DOMAIN: string;
    /// `wrangler dev` has no subdomains: this names the one label it serves.
    LABEL_OVERRIDE?: string;
    /// Bearer for /_admin, from `wrangler secret put ADMIN_SECRET`; unset = no admin.
    ADMIN_SECRET?: string;
    ASSETS: Fetcher;
    RELAY: DurableObjectNamespace;
    CONFIG: KVNamespace;
  }
}
