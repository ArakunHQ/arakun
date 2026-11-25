import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import worker from "../worker/index";
import { SESSION_COOKIE } from "../worker/session";
import { fakeGitHub, freshIp, github, issue, testEnv } from "./helpers";

const ORIGIN = "https://admin.test";

beforeEach(fakeGitHub);
afterEach(() => vi.restoreAllMocks());

function call(path: string, init: RequestInit = {}, env: Env = testEnv): Promise<Response> {
  return worker.fetch(new Request(`${ORIGIN}${path}`, init), env);
}

function setCookies(res: Response): string[] {
  return res.headers.getSetCookie();
}

/// The value of a cookie the response sets, or null.
function setValue(res: Response, name: string): string | null {
  for (const c of setCookies(res)) if (c.startsWith(`${name}=`)) return c.slice(name.length + 1).split(";")[0]!;
  return null;
}

/// Starts a sign-in: the state and the cookie the browser would send back.
async function start(ip = freshIp()): Promise<{ state: string; cookie: string; ip: string }> {
  const res = await call("/auth/github/start", { headers: { "CF-Connecting-IP": ip } });
  expect(res.status).toBe(302);
  const state = new URL(res.headers.get("Location")!).searchParams.get("state")!;
  const value = setValue(res, "__Host-arakun_admin_oauth")!;
  expect(value).toBe(state);
  return { state, cookie: `__Host-arakun_admin_oauth=${value}`, ip };
}

function callback(s: { state: string; cookie: string; ip: string }, code: string, extra: Record<string, string> = {}, env: Env = testEnv): Promise<Response> {
  const query = new URLSearchParams({ state: s.state, code });
  return call(`/auth/github/callback?${query}`, { headers: { Cookie: s.cookie, "CF-Connecting-IP": s.ip, ...extra } }, env);
}

function revoked(): boolean {
  return github.calls.some((r) => r.method === "DELETE");
}

describe("sign-in", () => {
  it("start sends the browser to GitHub with a state and no scope", async () => {
    const res = await call("/auth/github/start");
    expect(res.status).toBe(302);
    const url = new URL(res.headers.get("Location")!);
    expect(url.origin + url.pathname).toBe("https://github.com/login/oauth/authorize");
    expect(url.searchParams.get("client_id")).toBe("Iv-test");
    expect(url.searchParams.get("redirect_uri")).toBe(`${ORIGIN}/auth/github/callback`);
    expect(url.searchParams.get("state")).toMatch(/^[A-Za-z0-9_-]{40,}$/);
    expect(url.searchParams.has("scope")).toBe(false);
    const cookie = setCookies(res)[0]!;
    expect(cookie).toContain("__Host-arakun_admin_oauth=");
    for (const part of ["HttpOnly", "Secure", "SameSite=Lax", "Path=/", "Max-Age=600"]) expect(cookie).toContain(part);
  });

  it("start fails without a client secret", async () => {
    const res = await call("/auth/github/start", {}, { ...testEnv, GITHUB_CLIENT_SECRET: undefined });
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
  });

  it("signs a listed account in", async () => {
    issue("code1", "gho_token1", { id: 1001, login: "octocat" });
    const res = await callback(await start(), "code1");
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/");
    const session = setValue(res, SESSION_COOKIE)!;
    expect(session).toBeTruthy();
    // The state cookie is single use.
    expect(setCookies(res).some((c) => c.startsWith("__Host-arakun_admin_oauth=;") && c.includes("Max-Age=0"))).toBe(true);
    expect(setCookies(res).find((c) => c.startsWith(`${SESSION_COOKIE}=`))).toContain("HttpOnly");
    const me = await call("/api/me", { headers: { Cookie: `${SESSION_COOKIE}=${session}` } });
    expect(me.status).toBe(200);
    expect(await me.json()).toEqual({ github_id: 1001, login: "octocat" });
    expect(revoked()).toBe(true);
    expect(github.users.has("gho_token1")).toBe(false);
    // Neither call asks for more than who the token belongs to.
    expect(github.urls).not.toContain("https://api.github.com/user/emails");
  });

  it("turns away an account not on the list", async () => {
    issue("code2", "gho_token2", { id: 4242, login: "stranger" });
    const res = await callback(await start(), "code2");
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/?sign_in=denied");
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
    expect(revoked()).toBe(true);
    expect(github.users.has("gho_token2")).toBe(false);
  });

  it("fails without a matching state", async () => {
    issue("code3", "gho_token3", { id: 1001, login: "octocat" });
    const s = await start();
    for (const res of [
      await callback({ ...s, state: "other" }, "code3"),
      await callback({ ...s, cookie: "" }, "code3"),
      await callback({ ...s, cookie: "__Host-arakun_admin_oauth=other" }, "code3"),
    ]) {
      expect(res.headers.get("Location")).toBe("/?sign_in=failed");
      expect(setValue(res, SESSION_COOKIE)).toBeNull();
    }
    // GitHub was never asked: the code is still good.
    expect(github.exchanges).toEqual([]);
  });

  it("fails with a state used twice", async () => {
    issue("code4", "gho_token4", { id: 1001, login: "octocat" });
    const s = await start();
    expect((await callback(s, "code4")).headers.get("Location")).toBe("/");
    // The browser dropped the cookie, so the second callback carries none.
    const again = await callback({ ...s, cookie: "" }, "code4");
    expect(again.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setValue(again, SESSION_COOKIE)).toBeNull();
  });

  it("fails when GitHub refuses the code", async () => {
    const res = await callback(await start(), "unknown");
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
    github.codeOutage = "error";
    issue("code5", "gho_token5", { id: 1001, login: "octocat" });
    expect((await callback(await start(), "code5")).headers.get("Location")).toBe("/?sign_in=failed");
  });

  it("fails without a code", async () => {
    const s = await start();
    const res = await call(`/auth/github/callback?state=${s.state}`, { headers: { Cookie: s.cookie } });
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
  });

  it("fails when GitHub is down", async () => {
    issue("code6", "gho_token6", { id: 1001, login: "octocat" });
    for (const outage of ["throw", 503] as const) {
      github.codeOutage = outage;
      const res = await callback(await start(), "code6");
      expect(res.headers.get("Location")).toBe("/?sign_in=failed");
      expect(setValue(res, SESSION_COOKIE)).toBeNull();
    }
    github.codeOutage = null;
    for (const outage of ["throw", 503] as const) {
      github.outage = outage;
      const res = await callback(await start(), "code6");
      expect(res.headers.get("Location")).toBe("/?sign_in=failed");
      expect(setValue(res, SESSION_COOKIE)).toBeNull();
    }
  });

  it("fails, and signs nobody in, when the token cannot be revoked", async () => {
    issue("code7", "gho_token7", { id: 1001, login: "octocat" });
    github.revokeOutage = 503;
    const res = await callback(await start(), "code7");
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
  });

  it("still revokes a token whose owner could not be read", async () => {
    issue("code8", "gho_token8", { id: 1001, login: "octocat" });
    github.users.set("gho_token8", { id: 1001, login: "bad login" });
    const res = await callback(await start(), "code8");
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(revoked()).toBe(true);
  });

  it("fails without SESSION_SECRET or a client secret, and asks GitHub for nothing", async () => {
    issue("code9", "gho_token9", { id: 1001, login: "octocat" });
    for (const missing of [{ SESSION_SECRET: undefined }, { GITHUB_CLIENT_SECRET: undefined }]) {
      const res = await callback(await start(), "code9", {}, { ...testEnv, ...missing });
      expect(res.headers.get("Location")).toBe("/?sign_in=failed");
      expect(setValue(res, SESSION_COOKIE)).toBeNull();
    }
    expect(github.exchanges).toEqual([]);
  });

  it("fails when the client IP is over the limit, counting only the callback", async () => {
    const ip = freshIp();
    // Starting is not counted.
    for (let i = 0; i < 12; i++) await start(ip);
    issue("code10", "gho_token10", { id: 1001, login: "octocat" });
    let last: Response | null = null;
    for (let i = 0; i < 12; i++) last = await callback(await start(ip), i === 0 ? "code10" : "none");
    expect(last!.headers.get("Location")).toBe("/?sign_in=failed");
    // The first callbacks went through to GitHub; the later ones did not.
    expect(github.exchanges.length).toBeLessThan(12);
    expect(github.exchanges.length).toBeGreaterThan(0);
  });

  it("never puts GitHub's token in a cookie, a redirect or a body", async () => {
    for (const [code, token, user] of [
      ["codeA", "gho_secretA", { id: 1001, login: "octocat" }],
      ["codeB", "gho_secretB", { id: 4242, login: "stranger" }],
    ] as const) {
      issue(code, token, user);
      const res = await callback(await start(), code);
      const seen = [res.headers.get("Location"), ...setCookies(res), await res.text()].join("\n");
      expect(seen).not.toContain(token);
    }
    // And GitHub alone received it.
    for (const sent of github.calls) expect(new URL(sent.url).host).toBe("api.github.com");
    expect(github.urls.every((u) => u.startsWith("https://github.com/") || u.startsWith("https://api.github.com/"))).toBe(true);
  });
});

describe("me and logout", () => {
  it("me answers 401 without a session", async () => {
    const res = await call("/api/me");
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: "not signed in" });
    const forged = await call("/api/me", { headers: { Cookie: `${SESSION_COOKIE}=abc.def` } });
    expect(forged.status).toBe(401);
  });

  it("logout clears the cookie", async () => {
    const res = await call("/auth/logout", { method: "POST", headers: { Origin: ORIGIN } });
    expect(res.status).toBe(204);
    const cleared = setCookies(res)[0]!;
    expect(cleared).toContain(`${SESSION_COOKIE}=;`);
    expect(cleared).toContain("Max-Age=0");
  });

  it("logout refuses another origin, and none", async () => {
    for (const headers of [{ Origin: "https://evil.test" }, {} as Record<string, string>]) {
      const res = await call("/auth/logout", { method: "POST", headers });
      expect(res.status).toBe(403);
      expect(setCookies(res)).toEqual([]);
    }
  });
});
