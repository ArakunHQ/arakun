import { describe, expect, it } from "vitest";
import { env } from "cloudflare:test";
import { allowed, clearedCookie, readSession, SESSION_COOKIE, sessionCookie, signSession } from "../worker/session";

const T = 1_800_000_000;
const op = { githubId: 1001, login: "octo" };

function withCookie(value: string): Request {
  return new Request("https://admin.test/", { headers: { Cookie: `${SESSION_COOKIE}=${value}` } });
}

function flip(text: string): string {
  return text.slice(0, -1) + (text.endsWith("A") ? "B" : "A");
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
  });

  it("sets the cookie HttpOnly, Secure, SameSite=Lax, Path=/ and Max-Age=43200", () => {
    const set = sessionCookie("v");
    expect(set).toBe(`${SESSION_COOKIE}=v; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=43200`);
    expect(clearedCookie()).toContain("Max-Age=0");
  });
});
