import { cloudflareTest } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

// Two projects: the Worker's tests run inside workerd as the Worker will, the
// panel's in jsdom.
export default defineConfig({
  test: {
    projects: [
      {
        plugins: [
          cloudflareTest({
            wrangler: { configPath: "./wrangler.toml" },
            miniflare: {
              bindings: {
                ADMIN_SECRET: "test-secret",
                SESSION_SECRET: "test-session-secret",
                ADMIN_GITHUB_IDS: "1001,1002",
                GITHUB_CLIENT_SECRET: "client-secret",
                // 1001 has an authenticator key (RFC 6238's test key), 1002 none.
                ADMIN_TOTP_SECRETS: "1001:GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
              },
            },
          }),
        ],
        test: { name: "worker", include: ["test/**/*.test.ts"] },
      },
      {
        test: { name: "panel", include: ["src/**/*.test.{ts,tsx}"], environment: "jsdom" },
      },
    ],
  },
});
