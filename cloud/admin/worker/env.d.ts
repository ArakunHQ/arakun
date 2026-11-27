// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The bindings wrangler.toml gives the Worker, in the shape the test runner's
// `env` is typed with.
declare namespace Cloudflare {
  interface Env {
    ASSETS: Fetcher;
    CLOUD: Fetcher;
    CLOUD_ORIGIN: string;
    GITHUB_CLIENT_ID: string;
    // Secrets: unset in a fresh deploy, and every route that needs one refuses.
    GITHUB_CLIENT_SECRET?: string;
    ADMIN_SECRET?: string;
    ADMIN_GITHUB_IDS?: string;
    SESSION_SECRET?: string;
    SIGN_IN_LIMIT: RateLimit;
    CODE_LIMIT: RateLimit;
  }
}

type Env = Cloudflare.Env;
