// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// What vitest.config.ts adds to the Worker's bindings for the tests.
declare namespace Cloudflare {
  interface Env {
    TEST_MIGRATIONS: import("cloudflare:test").D1Migration[];
    /// The public halves of the push keys vitest.config.ts made, as JWKs.
    TEST_APNS_PUBLIC: string;
    TEST_FCM_PUBLIC: string;
  }
}
