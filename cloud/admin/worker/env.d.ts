// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The bindings wrangler.toml gives the Worker, in the shape the test runner's
// `env` is typed with.
declare namespace Cloudflare {
  interface Env {
    ASSETS: Fetcher;
    CLOUD: Fetcher;
    CLOUD_ORIGIN: string;
    // The Cloudflare Access team and the panel's Access application. Unset or
    // PLACEHOLDER: nobody gets in (worker/access.ts).
    ACCESS_TEAM_DOMAIN?: string;
    ACCESS_AUD?: string;
    // Only for `npm run dev` on localhost, from .dev.vars.
    DEV_OPERATOR_EMAIL?: string;
    // A secret: unset in a fresh deploy, and /api/* refuses.
    ADMIN_SECRET?: string;
  }
}

type Env = Cloudflare.Env;
