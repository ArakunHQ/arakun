import { describe, expect, it } from "vitest";
import { env } from "cloudflare:test";
import {
  allowed,
  CODE_STEP_COOKIE,
  clearedCodeStepCookie,
  clearedCookie,
  codeStepCookie,
  readCodeStep,
  readSession,
  SESSION_COOKIE,
  sessionCookie,
  signCodeStep,
  signSession,
} from "../worker/session";

const T = 1_800_000_000;
const op = { githubId: 1001, login: "octo" };

function withCookie(value: string): Request {
  return new Request("https://admin.test/", { headers: { Cookie: `${SESSION_COOKIE}=${value}` } });
}

// A character in the middle: the last one of a MAC has padding bits, and
// changing those may decode to the same bytes.
function flip(text: string): string {
  const i = Math.floor(text.length / 2);
  return text.slice(0, i) + (text[i] === "A" ? "B" : "A") + text.slice(i + 1);
}

/// A cookie value with a valid signature over any first part, as a bug in a
/// signer would make one.
async function signed(payload: string, secret = env.SESSION_SECRET!, purpose: string | null = "session"): Promise<string> {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(purpose === null ? payload : `${purpose}.${payload}`)));
  return `${payload}.${b64url(mac)}`;
}

function b64url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function claims(value: unknown): string {
  return b64url(new TextEncoder().encode(JSON.stringify(value)));
}

describe("session", () => {
  it("reads back what it signed", async () => {
    const value = await signSession(op, env.SESSION_SECRET!, T);
    expect(await readSession(withCookie(value), env, T)).toEqual(op);
  });

  it("refuses a changed payload, a changed signature, another secret, and a value without a dot", async () => {
    const value = await signSession(op, env.SESSION_SECRET!, T);
    const [payload, mac] = value.split(".") as [string, string];
    const forged = btoa(JSON.stringify({ github_id: 1002, login: "octo", exp: T + 100 })).replace(/=+$/, "");
    expect(await readSession(withCookie(`${forged}.${mac}`), env, T)).toBeNull();
    expect(await readSession(withCookie(`${flip(payload)}.${mac}`), env, T)).toBeNull();
    expect(await readSession(withCookie(`${payload}.${flip(mac)}`), env, T)).toBeNull();
    expect(await readSession(withCookie(await signSession(op, "another", T)), env, T)).toBeNull();
    expect(await readSession(withCookie(payload), env, T)).toBeNull();
    expect(await readSession(new Request("https://admin.test/"), env, T)).toBeNull();
    expect(await readSession(withCookie(value), { ...env, SESSION_SECRET: undefined }, T)).toBeNull();
  });

  it("refuses a validly signed payload of the wrong shape", async () => {
    const good = { github_id: 1001, login: "octo", exp: T + 100 };
    expect(await readSession(withCookie(await signed(claims(good))), env, T)).toEqual(op);
    const bad: unknown[] = [
      "text",
      null,
      [1001],
      { ...good, github_id: "1001" },
      { ...good, github_id: 0 },
      { ...good, github_id: -5 },
      { ...good, github_id: 1001.5 },
      { ...good, github_id: 2 ** 60 },
      { github_id: 1001, login: "octo" },
      { ...good, exp: "999999999999" },
      { ...good, login: 7 },
      { ...good, login: "" },
      { ...good, login: "bad\r\nX-Evil: 1" },
      { ...good, login: "a".repeat(40) },
      { ...good, login: "octo cat" },
    ];
    for (const value of bad) {
      expect(await readSession(withCookie(await signed(claims(value))), env, T), JSON.stringify(value)).toBeNull();
    }
    expect(await readSession(withCookie(await signed("not base64url!")), env, T)).toBeNull();
    expect(await readSession(withCookie(await signed("a")), env, T)).toBeNull();
  });

  it("does not sign an operator it would refuse to read", async () => {
    const secret = env.SESSION_SECRET!;
    await expect(signSession({ githubId: 0, login: "octo" }, secret, T)).rejects.toThrow();
    await expect(signSession({ githubId: 1.5, login: "octo" }, secret, T)).rejects.toThrow();
    await expect(signSession({ githubId: 2 ** 60, login: "octo" }, secret, T)).rejects.toThrow();
    await expect(signSession({ githubId: 1001, login: "bad\nlogin" }, secret, T)).rejects.toThrow();
    await expect(signSession({ githubId: 1001, login: "" }, secret, T)).rejects.toThrow();
  });

  it("never reads a code step's value as a session, or the other way round", async () => {
    const step = await signCodeStep(op, env.SESSION_SECRET!, T);
    const session = await signSession(op, env.SESSION_SECRET!, T);
    const both = (value: string) =>
      new Request("https://admin.test/", { headers: { Cookie: `${SESSION_COOKIE}=${value}; ${CODE_STEP_COOKIE}=${value}` } });
    expect(await readCodeStep(both(step), env, T)).toEqual(op);
    expect(await readSession(both(step), env, T)).toBeNull();
    expect(await readSession(both(session), env, T)).toEqual(op);
    expect(await readCodeStep(both(session), env, T)).toBeNull();
    // A MAC over the bare payload, as sessions were signed before the code step.
    const bare = await signed(claims({ github_id: 1001, login: "octo", exp: T + 100 }), env.SESSION_SECRET!, null);
    expect(await readSession(withCookie(bare), env, T)).toBeNull();
  });

  it("ends the code step after five minutes", async () => {
    const step = await signCodeStep(op, env.SESSION_SECRET!, T);
    const request = new Request("https://admin.test/", { headers: { Cookie: `${CODE_STEP_COOKIE}=${step}` } });
    expect(await readCodeStep(request, env, T + 299)).toEqual(op);
    expect(await readCodeStep(request, env, T + 300)).toBeNull();
    expect(codeStepCookie("v")).toBe(`${CODE_STEP_COOKIE}=v; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=300`);
    expect(clearedCodeStepCookie()).toContain("Max-Age=0");
  });

  it("refuses at and after exp", async () => {
    const request = withCookie(await signSession(op, env.SESSION_SECRET!, T));
    expect(await readSession(request, env, T + 43199)).toEqual(op);
    expect(await readSession(request, env, T + 43200)).toBeNull();
  });

  it("refuses an operator taken off the list", async () => {
    const request = withCookie(await signSession({ githubId: 1002, login: "two" }, env.SESSION_SECRET!, T));
    expect(await readSession(request, { ...env, ADMIN_GITHUB_IDS: "1001" }, T)).toBeNull();
  });

  it("lets nobody in without a list or a secret", () => {
    expect(allowed(undefined, 1001)).toBe(false);
    expect(allowed("", 1001)).toBe(false);
    expect(allowed("abc, ,", 1001)).toBe(false);
    expect(allowed(" 1001 ,1002", 1002)).toBe(true);
    expect(allowed("10011", 1001)).toBe(false);
    // Past 2^53 two entries spell one number: neither may match.
    expect(allowed("9007199254740993", 9007199254740992)).toBe(false);
    expect(allowed("0", 0)).toBe(false);
  });

  it("sets the cookie HttpOnly, Secure, SameSite=Lax, Path=/ and Max-Age=43200", () => {
    const set = sessionCookie("v");
    expect(set).toBe(`${SESSION_COOKIE}=v; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=43200`);
    expect(clearedCookie()).toContain("Max-Age=0");
  });
});
