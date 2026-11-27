// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-plugin";
import { configDefaults, defineConfig } from "vitest/config";

/// Keys of our own in place of Apple's and Google's, made per run: the Worker
/// signs with the private halves, the tests check with the public ones (JWKs).
async function pushKeys() {
  const subtle = crypto.subtle;
  const pem = async (key: CryptoKey) => {
    const der = new Uint8Array((await subtle.exportKey("pkcs8", key)) as ArrayBuffer);
    const b64 = btoa(String.fromCharCode(...der)).replace(/.{64}/g, "$&\n");
    return `-----BEGIN PRIVATE KEY-----\n${b64}\n-----END PRIVATE KEY-----\n`;
  };
  const jwk = async (key: CryptoKey) => JSON.stringify(await subtle.exportKey("jwk", key));
  const apns = (await subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const fcm = (await subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  return {
    APNS_KEY: await pem(apns.privateKey),
    APNS_KEY_ID: "KEY123ABCD",
    TEST_APNS_PUBLIC: await jwk(apns.publicKey),
    FCM_SERVICE_ACCOUNT: JSON.stringify({
      project_id: "arakun-test",
      client_email: "push@arakun-test.iam.gserviceaccount.com",
      private_key: await pem(fcm.privateKey),
      token_uri: "https://oauth2.googleapis.com/token",
    }),
    TEST_FCM_PUBLIC: await jwk(fcm.publicKey),
  };
}

// Tests run inside workerd with a real D1, KV, Durable Objects and rate limiter, as the Worker will.
// The migrations are read here, in Node, and applied by test/setup.ts.
export default defineConfig(async () => {
  const migrations = await readD1Migrations("./migrations");
  return {
    plugins: [
      cloudflareTest({
        wrangler: { configPath: "./wrangler.toml" },
        miniflare: { bindings: { ADMIN_SECRET: "test-secret", GITHUB_CLIENT_ID: "Iv-test", GITHUB_CLIENT_SECRET: "client-secret", LINEAR_API_KEY: "lin_api_test", LINEAR_TEAM_ID: "team-arakun", LINEAR_CLIENT_ID: "lin-test", LINEAR_CLIENT_SECRET: "lin-secret", LINEAR_WEBHOOK_SECRET: "lin-hook", LINEAR_TOKEN_KEY: "A".repeat(43), CLOUD_MODEL_KEY: "B".repeat(42) + "A", TEST_MIGRATIONS: migrations, ...(await pushKeys()) } },
      }),
    ],
    // Durable Object WebSockets do not work with per-file storage isolation
    // (Cloudflare's known issues), so the files share storage, one at a time.
    // setup.ts empties the tables before each test, so none sees another's.
    // admin/ is its own package with its own vitest config and bindings.
    test: { setupFiles: ["./test/setup.ts"], maxWorkers: 1, isolate: false, exclude: [...configDefaults.exclude, "admin/**"] },
  };
});
