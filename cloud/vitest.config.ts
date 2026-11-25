import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-plugin";
import { configDefaults, defineConfig } from "vitest/config";

// Tests run inside workerd with a real D1, KV, Durable Objects and rate limiter, as the Worker will.
// The migrations are read here, in Node, and applied by test/setup.ts.
export default defineConfig(async () => {
  const migrations = await readD1Migrations("./migrations");
  return {
    plugins: [
      cloudflareTest({
        wrangler: { configPath: "./wrangler.toml" },
        miniflare: { bindings: { ADMIN_SECRET: "test-secret", GITHUB_CLIENT_ID: "Iv-test", GITHUB_CLIENT_SECRET: "client-secret", TEST_MIGRATIONS: migrations } },
      }),
    ],
    // Durable Object WebSockets do not work with per-file storage isolation
    // (Cloudflare's known issues), so the files share storage, one at a time.
    // setup.ts empties the tables before each test, so none sees another's.
    // admin/ is its own package with its own vitest config and bindings.
    test: { setupFiles: ["./test/setup.ts"], maxWorkers: 1, isolate: false, exclude: [...configDefaults.exclude, "admin/**"] },
  };
});
