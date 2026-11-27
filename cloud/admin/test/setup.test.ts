// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import worker from "../worker/index";
import { CODE_STEP_COOKIE, SESSION_COOKIE, signCodeStep, signSession } from "../worker/session";
import { codeAt, fromBase32 } from "../worker/totp";
import { cloud, codeNow, enroll, fakeGitHub, TEST_KEY, testEnv } from "./helpers";

const ORIGIN = "https://admin.test";
const SECRET = "test-session-secret";
const SETUP_COOKIE = "__Host-arakun_admin_setup";
const NOW = () => Math.floor(Date.now() / 1000);

beforeEach(() => {
  fakeGitHub();
  // Each test gives a few codes; the limit has its own test in auth.test.ts.
  vi.spyOn(testEnv.CODE_LIMIT, "limit").mockResolvedValue({ success: true });
});
afterEach(() => vi.restoreAllMocks());

function setValue(res: Response, name: string): string | null {
  for (const c of res.headers.getSetCookie()) if (c.startsWith(`${name}=`)) return c.slice(name.length + 1).split(";")[0]!;
  return null;
}

async function codeStep(githubId = 1002, login = "two"): Promise<string> {
  return `${CODE_STEP_COOKIE}=${await signCodeStep({ githubId, login }, SECRET, NOW())}`;
}

async function session(githubId = 1001, login = "octocat"): Promise<string> {
  return `${SESSION_COOKIE}=${await signSession({ githubId, login }, SECRET, NOW())}`;
}

function post(path: string, cookie: string, body?: unknown, origin: string | null = ORIGIN): Promise<Response> {
  const headers: Record<string, string> = { Cookie: cookie, "Content-Type": "application/json" };
  if (origin) headers["Origin"] = origin;
  return worker.fetch(new Request(`${ORIGIN}${path}`, { method: "POST", headers, body: body === undefined ? undefined : JSON.stringify(body) }), testEnv);
}

/// POST /auth/setup/start: the new key, and the cookies to finish with.
async function begin(cookie: string): Promise<{ key: Uint8Array<ArrayBuffer>; cookie: string; uri: string }> {
  const res = await post("/auth/setup/start", cookie);
  expect(res.status).toBe(200);
  const body = (await res.json()) as { key: string; uri: string };
  const setup = setValue(res, SETUP_COOKIE)!;
  expect(res.headers.getSetCookie().find((c) => c.startsWith(SETUP_COOKIE))).toContain("SameSite=Strict");
  // The cookie is sealed: the key is not in it as text.
  expect(setup).not.toContain(body.key);
  return { key: fromBase32(body.key)!, uri: body.uri, cookie: `${cookie}; ${SETUP_COOKIE}=${setup}` };
}

/// Whether the stored key for the operator is this one: its code signs in.
async function storedIs(githubId: number, key: Uint8Array<ArrayBuffer>): Promise<boolean> {
  const row = cloud.keys.get(githubId);
  if (!row) return false;
  const { unseal } = await import("../worker/seal");
  const plain = await unseal(row.sealed, `totp:${githubId}`, SECRET);
  return !!plain && plain.every((b, i) => b === key[i]) && plain.length === key.length;
}

describe("setup at the first sign-in", () => {
  it("stores the key once the new app's code is right, and signs in", async () => {
    const s = await begin(await codeStep());
    expect(s.key.length).toBe(20);
    expect(s.uri).toMatch(/^otpauth:\/\/totp\/Arakun%20admin%3Atwo\?secret=[A-Z2-7]{32}&issuer=Arakun%20admin$/);
    expect(cloud.keys.has(1002)).toBe(false);

    const wrong = await post("/auth/setup", s.cookie, { code: "000000" });
    expect(wrong.status).toBe(401);
    expect(await wrong.json()).toEqual({ error: "wrong code" });
    expect(cloud.keys.has(1002)).toBe(false);

    const code = await codeNow(s.key);
    const res = await post("/auth/setup", s.cookie, { code });
    expect(res.status).toBe(204);
    expect(setValue(res, SESSION_COOKIE)).toBeTruthy();
    expect(res.headers.getSetCookie().some((c) => c.startsWith(`${CODE_STEP_COOKIE}=;`))).toBe(true);
    expect(res.headers.getSetCookie().some((c) => c.startsWith(`${SETUP_COOKIE}=;`))).toBe(true);
    expect(await storedIs(1002, s.key)).toBe(true);
    // The code that confirmed the key does not sign in again.
    expect(cloud.keys.get(1002)!.last_step).toBe(Math.floor(NOW() / 30));
  });

  it("never replaces a key that exists", async () => {
    await enroll(1002);
    const start = await post("/auth/setup/start", await codeStep());
    expect(start.status).toBe(409);
    expect(await start.json()).toEqual({ error: "a key exists" });
    expect(setValue(start, SETUP_COOKIE)).toBeNull();

    // A key that appeared between start and finish wins too.
    cloud.keys.clear();
    const s = await begin(await codeStep());
    await enroll(1002);
    const res = await post("/auth/setup", s.cookie, { code: await codeNow(s.key) });
    expect(res.status).toBe(409);
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
    expect(await storedIs(1002, TEST_KEY)).toBe(true);
  });

  it("refuses without a sign-in, from another origin, and with another operator's setup", async () => {
    expect((await post("/auth/setup/start", "")).status).toBe(401);
    expect((await post("/auth/setup/start", await codeStep(), undefined, "https://evil.test")).status).toBe(403);
    expect((await post("/auth/setup/start", await codeStep(), undefined, null)).status).toBe(403);
    const s = await begin(await codeStep());
    const code = await codeNow(s.key);
    expect((await post("/auth/setup", s.cookie, { code }, "https://evil.test")).status).toBe(403);
    // 1002's setup cookie with 1001's code step.
    const setupPart = s.cookie.slice(s.cookie.indexOf(`${SETUP_COOKIE}=`));
    const other = `${await codeStep(1001, "octocat")}; ${setupPart}`;
    const res = await post("/auth/setup", other, { code });
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: "setup expired" });
    // A changed setup cookie.
    const changed = s.cookie.replace(/(__Host-arakun_admin_setup=)(.)/, (_, p: string, c: string) => p + (c === "A" ? "B" : "A"));
    expect(await (await post("/auth/setup", changed, { code })).json()).toEqual({ error: "setup expired" });
    expect(await (await post("/auth/setup", await codeStep(), { code })).json()).toEqual({ error: "setup expired" });
    expect(cloud.keys.size).toBe(0);
  });
});

describe("setup in Settings", () => {
  it("replaces the key with a code from the old app and the new one", async () => {
    await enroll(1001);
    const s = await begin(await session());
    const code = await codeNow(s.key);

    const none = await post("/auth/setup", s.cookie, { code });
    expect(none.status).toBe(400);
    expect(await none.json()).toEqual({ error: "no current code" });
    const wrong = await post("/auth/setup", s.cookie, { code, current_code: "000000" });
    expect(wrong.status).toBe(401);
    expect(await wrong.json()).toEqual({ error: "wrong current code" });
    expect(await storedIs(1001, TEST_KEY)).toBe(true);

    const res = await post("/auth/setup", s.cookie, { code, current_code: await codeNow() });
    expect(res.status).toBe(204);
    // Still signed in with the same session; no new session cookie.
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
    expect(await storedIs(1001, s.key)).toBe(true);
  });

  it("does not take a current code twice", async () => {
    await enroll(1001);
    const current = await codeNow();
    const first = await begin(await session());
    expect((await post("/auth/setup", first.cookie, { code: await codeNow(first.key), current_code: current })).status).toBe(204);
    // The old key is gone, and its code with it.
    const second = await begin(await session());
    const res = await post("/auth/setup", second.cookie, { code: await codeNow(second.key), current_code: current });
    expect(res.status).toBe(401);
    expect(await storedIs(1001, first.key)).toBe(true);
  });

  it("sets up a first key for a signed-in operator without one", async () => {
    const s = await begin(await session());
    expect((await post("/auth/setup", s.cookie, { code: await codeAt(s.key, Math.floor(NOW() / 30) - 1) })).status).toBe(204);
    expect(await storedIs(1001, s.key)).toBe(true);
  });
});
