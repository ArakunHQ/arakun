import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

// Tests run inside workerd with a real D1 and rate limiter, as the Worker will.
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
    test: { setupFiles: ["./test/setup.ts"] },
  };
});
