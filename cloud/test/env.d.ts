// What vitest.config.ts adds to the Worker's bindings for the tests.
declare namespace Cloudflare {
  interface Env {
    TEST_MIGRATIONS: import("cloudflare:test").D1Migration[];
  }
}
