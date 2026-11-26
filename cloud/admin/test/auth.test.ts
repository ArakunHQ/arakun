import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import worker from "../worker/index";
import { CODE_STEP_COOKIE, SESSION_COOKIE } from "../worker/session";
import { seal } from "../worker/seal";
import { codeAt } from "../worker/totp";
import { env } from "cloudflare:test";
import { cloud, enroll, fakeGitHub, freshIp, github, issue, TEST_KEY, testEnv } from "./helpers";

const ORIGIN = "https://admin.test";

beforeEach(async () => {
  fakeGitHub();
  // 1001 has set up an authenticator app; 1002 has not.
  await enroll(1001);
});
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

function stateCleared(res: Response): boolean {
  return setCookies(res).some((c) => c.startsWith("__Host-arakun_admin_oauth=;") && c.includes("Max-Age=0"));
}

/// The code operator 1001's app shows at `now` (vitest.config.ts has the key).
async function codeFor(now = Date.now() / 1000): Promise<string> {
  return codeAt(TEST_KEY, Math.floor(now / 30));
}

/// Through GitHub to the code step: the code step's cookie, as the browser sends it.
async function toCodeStep(user = { id: 1001, login: "octocat" }, env: Env = testEnv): Promise<string> {
  const n = freshIp();
  issue(`code-${n}`, `gho_${n}`, user);
  const res = await callback(await start(), `code-${n}`, {}, env);
  expect(res.headers.get("Location")).toBe("/sign-in?step=code");
  return `${CODE_STEP_COOKIE}=${setValue(res, CODE_STEP_COOKIE)!}`;
}

function sendCode(cookie: string, body: unknown, headers: Record<string, string> = { Origin: ORIGIN }, env: Env = testEnv): Promise<Response> {
  return call(
    "/auth/code",
    { method: "POST", headers: { Cookie: cookie, "Content-Type": "application/json", ...headers }, body: typeof body === "string" ? body : JSON.stringify(body) },
    env,
  );
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

  it("start and callback treat the placeholder client id as unset", async () => {
    // The env as wrangler.toml gives it, before the OAuth App exists.
    const res = await call("/auth/github/start", {}, env);
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setCookies(res)).toEqual([]);
    const s = await start();
    issue("codeP", "gho_tokenP", { id: 1001, login: "octocat" });
    const cb = await callback(s, "codeP", {}, env);
    expect(cb.status).toBe(302);
    expect(cb.headers.get("Location")).toBe("/?sign_in=failed");
    expect(stateCleared(cb)).toBe(true);
    expect(github.exchanges).toEqual([]);
    expect(github.urls).toEqual([]);
  });

  it("signs a listed account in after GitHub and the code", async () => {
    issue("code1", "gho_token1", { id: 1001, login: "octocat" });
    const res = await callback(await start(), "code1");
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/sign-in?step=code");
    // GitHub alone gives no session, only the code step.
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
    const step = setValue(res, CODE_STEP_COOKIE)!;
    expect(step).toBeTruthy();
    expect(setCookies(res).find((c) => c.startsWith(`${CODE_STEP_COOKIE}=`))).toContain("HttpOnly");
    // The state cookie is single use.
    expect(stateCleared(res)).toBe(true);
    // The code step's cookie is not a session.
    expect((await call("/api/me", { headers: { Cookie: `${SESSION_COOKIE}=${step}` } })).status).toBe(401);

    const done = await sendCode(`${CODE_STEP_COOKIE}=${step}`, { code: await codeFor() });
    expect(done.status).toBe(204);
    const session = setValue(done, SESSION_COOKIE)!;
    expect(session).toBeTruthy();
    expect(setCookies(done).find((c) => c.startsWith(`${SESSION_COOKIE}=`))).toContain("HttpOnly");
    expect(setCookies(done).some((c) => c.startsWith(`${CODE_STEP_COOKIE}=;`) && c.includes("Max-Age=0"))).toBe(true);
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
    expect(stateCleared(res)).toBe(true);
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
    expect((await callback(s, "code4")).headers.get("Location")).toBe("/sign-in?step=code");
    // The browser dropped the cookie, so the second callback carries none.
    const again = await callback({ ...s, cookie: "" }, "code4");
    expect(again.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setValue(again, SESSION_COOKIE)).toBeNull();
  });

  it("fails when GitHub refuses the code", async () => {
    const res = await callback(await start(), "unknown");
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
    expect(stateCleared(res)).toBe(true);
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
    expect(stateCleared(res)).toBe(true);
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
    expect(stateCleared(last!)).toBe(true);
    // The limit is 10 a minute: exactly ten callbacks reached GitHub.
    expect(github.exchanges.length).toBe(10);
  });

  it("fails, clearing the state, when the rate limiter throws", async () => {
    issue("code11", "gho_token11", { id: 1001, login: "octocat" });
    const s = await start();
    const limit = vi.spyOn(testEnv.SIGN_IN_LIMIT, "limit").mockRejectedValue(new Error("limiter down"));
    const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
    const res = await callback(s, "code11");
    expect(limit).toHaveBeenCalled();
    expect(quiet).toHaveBeenCalled();
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(stateCleared(res)).toBe(true);
    expect(github.exchanges).toEqual([]);
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

describe("the code step", () => {
  // Every test signs in as 1001, and five codes a minute would not last.
  // The last test counts with the real limiter.
  beforeEach(() => {
    vi.spyOn(testEnv.CODE_LIMIT, "limit").mockResolvedValue({ success: true });
  });

  it("sends an operator with no key yet to set one up", async () => {
    issue("codeK", "gho_tokenK", { id: 1002, login: "two" });
    const res = await callback(await start(), "codeK");
    expect(res.headers.get("Location")).toBe("/sign-in?step=setup");
    expect(setValue(res, CODE_STEP_COOKIE)).toBeTruthy();
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
    expect(revoked()).toBe(true);
    // The code step alone gives no session without a key.
    const code = await sendCode(`${CODE_STEP_COOKIE}=${setValue(res, CODE_STEP_COOKIE)!}`, { code: "000000" });
    expect(code.status).toBe(409);
    expect(await code.json()).toEqual({ error: "no key" });
  });

  it("asks for a reset when the stored key does not open, and fails when the cloud is down", async () => {
    // Sealed with another SESSION_SECRET.
    cloud.keys.set(1001, { sealed: await seal(TEST_KEY, "totp:1001", "old-secret"), last_step: 0 });
    issue("codeR", "gho_tokenR", { id: 1001, login: "octocat" });
    const reset = await callback(await start(), "codeR");
    expect(reset.headers.get("Location")).toBe("/?sign_in=reset");
    expect(setValue(reset, CODE_STEP_COOKIE)).toBeNull();
    // Another operator's sealed key does not open as this one's either.
    cloud.keys.set(1001, { sealed: await seal(TEST_KEY, "totp:1002", "test-session-secret"), last_step: 0 });
    issue("codeR2", "gho_tokenR2", { id: 1001, login: "octocat" });
    expect((await callback(await start(), "codeR2")).headers.get("Location")).toBe("/?sign_in=reset");

    cloud.down = true;
    const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
    issue("codeD", "gho_tokenD", { id: 1001, login: "octocat" });
    const down = await callback(await start(), "codeD");
    expect(down.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setValue(down, CODE_STEP_COOKIE)).toBeNull();
    expect(quiet).toHaveBeenCalled();
  });

  it("takes the code of the step before and after, and gives nothing for a wrong one", async () => {
    const cookie = await toCodeStep();
    const now = Date.now() / 1000;
    for (const code of ["000000", "12345", "abcdef", await codeFor(now + 120)]) {
      const res = await sendCode(cookie, { code });
      expect(res.status, code).toBe(401);
      expect(await res.json()).toEqual({ error: "wrong code" });
      expect(setValue(res, SESSION_COOKIE)).toBeNull();
    }
    // The app may show the code with a space in the middle.
    const late = await codeFor(now - 30);
    const res = await sendCode(cookie, { code: `${late.slice(0, 3)} ${late.slice(3)}` });
    expect(res.status).toBe(204);
    expect(setValue(res, SESSION_COOKIE)).toBeTruthy();
  });

  it("takes each code once, and none older than the last", async () => {
    const now = Date.now() / 1000;
    const code = await codeFor(now);
    expect((await sendCode(await toCodeStep(), { code })).status).toBe(204);
    for (const used of [code, await codeFor(now - 30)]) {
      const again = await sendCode(await toCodeStep(), { code: used });
      expect(again.status).toBe(409);
      expect(await again.json()).toEqual({ error: "code used" });
      expect(setValue(again, SESSION_COOKIE)).toBeNull();
    }
  });

  it("refuses a code without the code step, from another origin, or with a bad body", async () => {
    const code = await codeFor();
    expect((await sendCode("", { code })).status).toBe(401);
    expect(await (await sendCode(`${CODE_STEP_COOKIE}=abc.def`, { code })).json()).toEqual({ error: "sign-in expired" });
    const cookie = await toCodeStep();
    for (const headers of [{ Origin: "https://evil.test" }, {} as Record<string, string>]) {
      const res = await sendCode(cookie, { code }, headers);
      expect(res.status).toBe(403);
      expect(setValue(res, SESSION_COOKIE)).toBeNull();
    }
    for (const body of ["not json", "null", "[]", JSON.stringify({ code: 123456 }), JSON.stringify({ code: "1".repeat(300) })]) {
      const res = await sendCode(cookie, body);
      expect(res.status, body).toBe(400);
      expect(setValue(res, SESSION_COOKIE)).toBeNull();
    }
  });

  it("refuses the code step of an operator whose key or place on the list was taken away", async () => {
    const cookie = await toCodeStep();
    const code = await codeFor();
    expect((await sendCode(cookie, { code }, undefined, { ...testEnv, ADMIN_GITHUB_IDS: "1002" })).status).toBe(401);
    expect((await sendCode(cookie, { code }, undefined, { ...testEnv, SESSION_SECRET: "another" })).status).toBe(401);
    cloud.keys.delete(1001);
    expect((await sendCode(cookie, { code })).status).toBe(409);
  });

  it("takes five codes a minute per operator, right ones too", async () => {
    vi.mocked(testEnv.CODE_LIMIT.limit).mockRestore();
    const env = { ...testEnv, ADMIN_GITHUB_IDS: "1001,1003" };
    await enroll(1003);
    const cookie = await toCodeStep({ id: 1003, login: "three" }, env);
    for (let i = 0; i < 5; i++) expect((await sendCode(cookie, { code: "000000" }, undefined, env)).status).toBe(401);
    // Another operator still has their own five.
    const other = await toCodeStep();
    expect((await sendCode(other, { code: await codeFor() })).status).toBe(204);
    const res = await sendCode(cookie, { code: await codeFor() }, undefined, env);
    expect(res.status).toBe(429);
    expect(await res.json()).toEqual({ error: "too many codes; wait a minute" });
    expect(setValue(res, SESSION_COOKIE)).toBeNull();
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
    const cleared = setCookies(res);
    expect(cleared.some((c) => c.startsWith(`${SESSION_COOKIE}=;`) && c.includes("Max-Age=0"))).toBe(true);
    expect(cleared.some((c) => c.startsWith(`${CODE_STEP_COOKIE}=;`) && c.includes("Max-Age=0"))).toBe(true);
  });

  it("logout refuses another origin, and none", async () => {
    for (const headers of [{ Origin: "https://evil.test" }, {} as Record<string, string>]) {
      const res = await call("/auth/logout", { method: "POST", headers });
      expect(res.status).toBe(403);
      expect(setCookies(res)).toEqual([]);
    }
  });
});
