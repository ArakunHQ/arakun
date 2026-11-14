import { cloudflareTest } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

// Tests run inside workerd with the real Durable Object and KV, as the Worker will.
export default defineConfig({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: "./wrangler.toml" },
      // Tests put the label in the Host themselves; the override stays off.
      miniflare: { bindings: { LABEL_OVERRIDE: "", ADMIN_SECRET: "test-secret" } },
    }),
  ],
});
