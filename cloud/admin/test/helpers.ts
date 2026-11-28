// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// A fake Cloudflare Access team. The Worker runs in the same isolate as the
// tests, so replacing the global fetch reaches it.
import { env } from "cloudflare:test";
import { vi } from "vitest";
import { forgetKeys } from "../worker/access";

export const TEAM = "arakun-test.cloudflareaccess.com";
export const AUD = "test-aud";
export const EMAIL = "ops@example.com";

/// The Worker's bindings with the fake team's Access application.
export const testEnv: Env = { ...env, ACCESS_TEAM_DOMAIN: TEAM, ACCESS_AUD: AUD, CLOUD_ORIGIN: "https://cloud.test" };

type KeyPair = CryptoKeyPair & { privateKey: CryptoKey; publicKey: CryptoKey };

function newKey(): Promise<KeyPair> {
  return crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  ) as Promise<KeyPair>;
}

/// The team's signing keys by ID, what the certs URL answers, and how often it was asked.
export const access = {
  keys: new Map<string, KeyPair>(),
  /// "throw": the network fails; a number: the certs URL answers with that status.
  outage: null as "throw" | number | null,
  fetches: 0,
};

/// A fresh team with one key, "k1", and no keys cached in the Worker.
export async function fakeAccess(): Promise<void> {
  forgetKeys();
  access.keys.clear();
  access.keys.set("k1", await newKey());
  access.outage = null;
  access.fetches = 0;
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const req = new Request(input, init);
    if (req.url !== `https://${TEAM}/cdn-cgi/access/certs` || req.method !== "GET") throw new Error(`unexpected fetch: ${req.method} ${req.url}`);
    access.fetches++;
    if (access.outage === "throw") throw new TypeError("Network connection lost.");
    if (access.outage !== null) return new Response("unavailable", { status: access.outage });
    const keys = await Promise.all(
      [...access.keys].map(async ([kid, pair]) => {
        const jwk = (await crypto.subtle.exportKey("jwk", pair.publicKey)) as JsonWebKey;
        return { kid, kty: "RSA", alg: "RS256", use: "sig", n: jwk.n, e: jwk.e };
      }),
    );
    return Response.json({ keys, public_cert: {}, public_certs: [] });
  });
}

/// The team starts signing with another key.
export async function rotate(kid: string): Promise<void> {
  access.keys.set(kid, await newKey());
}

function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function part(value: unknown): string {
  return b64url(new TextEncoder().encode(JSON.stringify(value)));
}

/// An Access token as the team would sign it for EMAIL, with `claims` over
/// the usual ones. `header` replaces fields of the header; `signWith` signs
/// with a key the team does not publish.
export async function token(
  claims: Record<string, unknown> = {},
  opts: { kid?: string; header?: Record<string, unknown>; signWith?: CryptoKey } = {},
): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  const kid = opts.kid ?? "k1";
  const head = part({ alg: "RS256", kid, typ: "JWT", ...opts.header });
  const body = part({ aud: [AUD], email: EMAIL, exp: now + 3600, iat: now, nbf: now, iss: `https://${TEAM}`, type: "app", sub: "u1", ...claims });
  const key = opts.signWith ?? access.keys.get(kid)!.privateKey;
  const sig = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", key, new TextEncoder().encode(`${head}.${body}`));
  return `${head}.${body}.${b64url(new Uint8Array(sig))}`;
}

/// A key pair the team never published.
export function strangerKey(): Promise<KeyPair> {
  return newKey();
}
