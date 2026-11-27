// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The push service for the native apps (src/push.ts): what a computer's Web
// Push request turns into at Apple and Google, and what comes back.
import { env } from "cloudflare:test";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FALLBACK, forgetTokens, MAX_BODY, push } from "../src/push";
import { b64url, fromB64url, utf8 } from "../src/util";
import { call, roomInWindow } from "./helpers";

const ORIGIN = "https://teams.test";
const APNS_TOKEN = "a1".repeat(32);
const FCM_TOKEN = "fcm-token_ABCDEFGHIJ:0123456789-xyz";

/// What Apple and Google were sent, and how they answer next.
let sent: Request[] = [];
let bodies: string[] = [];
let answer: () => Response = () => new Response(null, { status: 200 });

beforeEach(() => {
  forgetTokens();
  sent = [];
  bodies = [];
  answer = () => new Response(null, { status: 200 });
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const req = new Request(input, init);
    sent.push(req);
    bodies.push(new TextDecoder().decode(await req.arrayBuffer()));
    if (req.url === "https://oauth2.googleapis.com/token") return Response.json({ access_token: "ya29.test", expires_in: 3599 });
    return answer();
  });
});

/// A VAPID key of a computer's own.
async function vapidKey(): Promise<CryptoKeyPair> {
  return (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
}

/// `Authorization` as core/src/phone/push.rs writes it.
async function vapid(claims: Record<string, unknown> = {}, pair?: CryptoKeyPair, signer?: CryptoKeyPair): Promise<string> {
  const keys = pair ?? (await vapidKey());
  const head = b64url(utf8(JSON.stringify({ typ: "JWT", alg: "ES256" })));
  const body = b64url(utf8(JSON.stringify({ aud: ORIGIN, exp: Math.floor(Date.now() / 1000) + 12 * 3600, sub: "https://arakun.dev", ...claims })));
  const sig = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, (signer ?? keys).privateKey, utf8(`${head}.${body}`)));
  const k = new Uint8Array((await crypto.subtle.exportKey("raw", keys.publicKey)) as ArrayBuffer);
  return `vapid t=${head}.${body}.${b64url(sig)}, k=${b64url(k)}`;
}

/// A sealed notice: the Worker never opens it, so any bytes of the right size do.
function sealed(size = 300): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(size));
}

async function deliver(path: string, opts: { auth?: string; body?: Uint8Array; headers?: Record<string, string> } = {}): Promise<Response> {
  return call(
    "POST",
    path,
    { "Content-Encoding": "aes128gcm", TTL: "3600", Urgency: "high", Authorization: opts.auth ?? (await vapid()), ...opts.headers },
    opts.body ?? sealed(),
  );
}

async function verifyJwt(jwt: string, jwk: string, algorithm: SubtleCryptoImportKeyAlgorithm, verify: SubtleCryptoSignAlgorithm) {
  const [head, claims, sig] = jwt.split(".") as [string, string, string];
  const key = await crypto.subtle.importKey("jwk", JSON.parse(jwk), algorithm, false, ["verify"]);
  const ok = await crypto.subtle.verify(verify, key, fromB64url(sig)!, utf8(`${head}.${claims}`));
  const decode = (part: string) => JSON.parse(new TextDecoder().decode(fromB64url(part)!));
  return { ok, header: decode(head), claims: decode(claims) };
}

describe("push to iOS", () => {
  it("hands Apple the ciphertext in a mutable alert, signed with our key", async () => {
    const body = sealed(500);
    const res = await deliver(`/push/v1/apns/${APNS_TOKEN}`, { body });
    expect(res.status).toBe(201);
    expect(sent).toHaveLength(1);
    const req = sent[0]!;
    expect(req.url).toBe(`https://api.push.apple.com/3/device/${APNS_TOKEN}`);
    expect(req.headers.get("apns-topic")).toBe("com.arakun.ios");
    expect(req.headers.get("apns-push-type")).toBe("alert");
    expect(req.headers.get("apns-priority")).toBe("10");
    const expires = Number(req.headers.get("apns-expiration"));
    expect(Math.abs(expires - (Math.floor(Date.now() / 1000) + 3600))).toBeLessThan(5);
    expect(JSON.parse(bodies[0]!)).toEqual({ aps: { alert: FALLBACK, "mutable-content": 1, sound: "default" }, e: b64url(body) });
    const jwt = /^bearer (.+)$/.exec(req.headers.get("authorization") ?? "")![1]!;
    const checked = await verifyJwt(jwt, env.TEST_APNS_PUBLIC, { name: "ECDSA", namedCurve: "P-256" }, { name: "ECDSA", hash: "SHA-256" });
    expect(checked.ok).toBe(true);
    expect(checked.header).toEqual({ alg: "ES256", kid: "KEY123ABCD" });
    expect(checked.claims.iss).toBe("V9JGU28UV7");
  });

  it("sends a development build's token to Apple's sandbox, and a normal one at low priority", async () => {
    const res = await deliver(`/push/v1/apns-sandbox/${APNS_TOKEN}`, { headers: { Urgency: "normal" } });
    expect(res.status).toBe(201);
    expect(sent[0]!.url).toBe(`https://api.sandbox.push.apple.com/3/device/${APNS_TOKEN}`);
    expect(sent[0]!.headers.get("apns-priority")).toBe("5");
  });

  it("reuses its provider token rather than signing one per message", async () => {
    await deliver(`/push/v1/apns/${APNS_TOKEN}`);
    await deliver(`/push/v1/apns/${APNS_TOKEN}`);
    expect(sent[0]!.headers.get("authorization")).toBe(sent[1]!.headers.get("authorization"));
  });

  it("answers 410 for a token Apple says is gone or not ours, and 502 for other refusals", async () => {
    answer = () => Response.json({ reason: "Unregistered" }, { status: 410 });
    expect((await deliver(`/push/v1/apns/${APNS_TOKEN}`)).status).toBe(410);
    answer = () => Response.json({ reason: "BadDeviceToken" }, { status: 400 });
    expect((await deliver(`/push/v1/apns/${APNS_TOKEN}`)).status).toBe(410);
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});
    answer = () => Response.json({ reason: "PayloadTooLarge" }, { status: 413 });
    expect((await deliver(`/push/v1/apns/${APNS_TOKEN}`)).status).toBe(502);
    answer = () => {
      throw new TypeError("Network connection lost.");
    };
    expect((await deliver(`/push/v1/apns/${APNS_TOKEN}`)).status).toBe(502);
    // The log says what failed, never the token or the message.
    for (const args of logged.mock.calls) expect(String(args)).not.toContain(APNS_TOKEN);
  });

  it("answers 429 when Apple is busy", async () => {
    answer = () => Response.json({ reason: "TooManyRequests" }, { status: 429 });
    expect((await deliver(`/push/v1/apns/${APNS_TOKEN}`)).status).toBe(429);
  });
});

describe("push to Android", () => {
  it("trades the service account for a token once, and sends the ciphertext as data", async () => {
    const body = sealed(400);
    expect((await deliver(`/push/v1/fcm/${encodeURIComponent(FCM_TOKEN)}`, { body })).status).toBe(201);
    expect((await deliver(`/push/v1/fcm/${encodeURIComponent(FCM_TOKEN)}`, { headers: { TTL: "0", Urgency: "low" } })).status).toBe(201);
    expect(sent.map((r) => r.url)).toEqual([
      "https://oauth2.googleapis.com/token",
      "https://fcm.googleapis.com/v1/projects/arakun-test/messages:send",
      "https://fcm.googleapis.com/v1/projects/arakun-test/messages:send",
    ]);
    const form = new URLSearchParams(bodies[0]!);
    expect(form.get("grant_type")).toBe("urn:ietf:params:oauth:grant-type:jwt-bearer");
    const checked = await verifyJwt(form.get("assertion")!, env.TEST_FCM_PUBLIC, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, { name: "RSASSA-PKCS1-v1_5" });
    expect(checked.ok).toBe(true);
    expect(checked.claims).toMatchObject({
      iss: "push@arakun-test.iam.gserviceaccount.com",
      scope: "https://www.googleapis.com/auth/firebase.messaging",
      aud: "https://oauth2.googleapis.com/token",
    });
    expect(sent[1]!.headers.get("authorization")).toBe("Bearer ya29.test");
    expect(JSON.parse(bodies[1]!)).toEqual({ message: { token: FCM_TOKEN, data: { e: b64url(body) }, android: { priority: "HIGH", ttl: "3600s" } } });
    expect(JSON.parse(bodies[2]!).message.android).toEqual({ priority: "NORMAL", ttl: "0s" });
  });

  it("answers 410 for a token Google no longer knows", async () => {
    answer = () => Response.json({ error: { status: "NOT_FOUND", details: [{ errorCode: "UNREGISTERED" }] } }, { status: 404 });
    expect((await deliver(`/push/v1/fcm/${FCM_TOKEN}`)).status).toBe(410);
  });
});

describe("what the push service refuses", () => {
  it("a missing, foreign, expired or forged VAPID token", async () => {
    const pair = await vapidKey();
    const now = Math.floor(Date.now() / 1000);
    const path = `/push/v1/apns/${APNS_TOKEN}`;
    expect((await deliver(path, { auth: "" })).status).toBe(401);
    expect((await deliver(path, { auth: "WebPush abc" })).status).toBe(401);
    expect((await deliver(path, { auth: await vapid({ aud: "https://fcm.googleapis.com" }) })).status).toBe(401);
    expect((await deliver(path, { auth: await vapid({ exp: now - 1 }) })).status).toBe(401);
    expect((await deliver(path, { auth: await vapid({ exp: now + 3 * 24 * 3600 }) })).status).toBe(401);
    expect((await deliver(path, { auth: await vapid({}, pair, await vapidKey()) })).status).toBe(401);
    expect(sent).toHaveLength(0);
    // The same key, signing for itself, is fine.
    expect((await deliver(path, { auth: await vapid({}, pair) })).status).toBe(201);
  });

  it("a body that is not aes128gcm, too big or too short", async () => {
    const path = `/push/v1/apns/${APNS_TOKEN}`;
    expect((await deliver(path, { headers: { "Content-Encoding": "aesgcm" } })).status).toBe(415);
    expect((await deliver(path, { body: sealed(MAX_BODY + 1) })).status).toBe(413);
    expect((await deliver(path, { body: sealed(20) })).status).toBe(400);
    expect((await deliver(path, { body: sealed(MAX_BODY) })).status).toBe(201);
    expect(sent).toHaveLength(1);
  });

  it("a token that cannot be one, another service, or another method", async () => {
    expect((await deliver("/push/v1/apns/not-hex")).status).toBe(404);
    expect((await deliver("/push/v1/fcm/short")).status).toBe(404);
    expect((await deliver(`/push/v1/webpush/${APNS_TOKEN}`)).status).toBe(404);
    expect((await call("GET", `/push/v1/apns/${APNS_TOKEN}`)).status).toBe(405);
    expect(sent).toHaveLength(0);
  });

  it("a 31st message to one token in a minute, while another token still gets through", async () => {
    await roomInWindow(60);
    const path = `/push/v1/apns/${"b2".repeat(32)}`;
    const auth = await vapid();
    for (let i = 0; i < 30; i++) expect((await deliver(path, { auth })).status).toBe(201);
    expect((await deliver(path, { auth })).status).toBe(429);
    expect((await deliver(`/push/v1/apns/${"c3".repeat(32)}`, { auth })).status).toBe(201);
    // The wait for a window can take 3 seconds; 32 calls need room after it.
  }, 10_000);

  it("anything while Apple's or Google's keys are unset", async () => {
    const url = new URL(`${ORIGIN}/push/v1/apns/${APNS_TOKEN}`);
    const request = async (u: URL) =>
      new Request(u, { method: "POST", headers: { "Content-Encoding": "aes128gcm", Authorization: await vapid() }, body: sealed() });
    const noApple = { ...env, APNS_KEY: undefined };
    expect((await push(await request(url), noApple, url))!.status).toBe(503);
    const fcm = new URL(`${ORIGIN}/push/v1/fcm/${FCM_TOKEN}`);
    const noGoogle = { ...env, FCM_SERVICE_ACCOUNT: undefined };
    expect((await push(await request(fcm), noGoogle, fcm))!.status).toBe(503);
    expect(sent).toHaveLength(0);
  });
});
