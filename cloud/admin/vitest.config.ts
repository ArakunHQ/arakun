// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
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
              bindings: { ADMIN_SECRET: "test-secret" },
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
