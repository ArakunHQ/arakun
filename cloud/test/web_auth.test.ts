// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { as, call, freshIp, github, roomInWindow, signInRequest } from "./helpers";

const ORIGIN = "https://teams.test";
const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 Safari/604.1";

/// Starts a sign-in as a browser would: the state in the cookie and in the URL.
async function start(ip = freshIp()): Promise<{ state: string; cookie: string; res: Response }> {
  const res = await SELF.fetch(`${ORIGIN}/auth/github/start`, { redirect: "manual", headers: { "CF-Connecting-IP": ip } });
  const location = new URL(res.headers.get("Location") ?? "https://none.invalid/");
  const cookie = /^__Host-arakun_oauth=([^;]*)/.exec(res.headers.get("Set-Cookie") ?? "")?.[1] ?? "";
  return { state: location.searchParams.get("state") ?? "", cookie, res };
}

function callback(query: string, headers: Record<string, string> = {}): Promise<Response> {
  return SELF.fetch(`${ORIGIN}/auth/github/callback?${query}`, {
    redirect: "manual",
    headers: { "CF-Connecting-IP": freshIp(), ...headers },
  });
}

/// A code GitHub will exchange for a token of this user.
function codeFor(id: number, login: string, code = "abc123"): string {
  const token = `gho_${id}_${login}`;
  github.users.set(token, { id, login });
  github.codes.set(code, token);
  return code;
}

async function sessionCount(): Promise<number> {
  return (await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions").first<{ n: number }>())!.n;
}

const setCookies = (res: Response) => res.headers.getSetCookie();
const clearsOAuth = (res: Response) => setCookies(res).some((c) => /^__Host-arakun_oauth=;.*Max-Age=0/.test(c));

describe("GET /auth/github/start", () => {
  it("sends the browser to GitHub with a state that it also keeps in a cookie", async () => {
    const { res, state, cookie } = await start();
    expect(res.status).toBe(302);
    const url = new URL(res.headers.get("Location")!);
    expect(url.origin + url.pathname).toBe("https://github.com/login/oauth/authorize");
    expect(url.searchParams.get("client_id")).toBe("Iv-test");
    expect(url.searchParams.get("scope")).toBe("user:email");
    expect(url.searchParams.get("redirect_uri")).toBe(`${ORIGIN}/auth/github/callback`);
    expect(state).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(cookie).toBe(state);
    const set = res.headers.get("Set-Cookie")!;
    // `__Host-` with `Path=/` and no Domain: a sibling host cannot plant one.
    expect(set.startsWith("__Host-arakun_oauth=")).toBe(true);
    expect(set).not.toContain("Domain");
    for (const part of ["HttpOnly", "Secure", "SameSite=Lax", "Path=/;", "Max-Age=600"]) expect(set).toContain(part);
    expect((await start()).state).not.toBe(state);
  });

  it("does not count against the sign-in limit: the callback does, once", async () => {
    const ip = freshIp();
    for (let i = 0; i < 12; i++) expect((await start(ip)).res.status).toBe(302);
    expect((await signInRequest("gho_none", ip)).status).not.toBe(429);
  });

  it("sends the browser back to the page when sign-in is not set up", async () => {
    const { webAuthStart } = await import("../src/web_auth");
    const req = new Request(`${ORIGIN}/auth/github/start`, { headers: { "CF-Connecting-IP": freshIp() } });
    const res = await webAuthStart(req, { ...env, GITHUB_CLIENT_ID: undefined });
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
  });
});

describe("GET /auth/github/callback", () => {
  it("signs the browser in: a session cookie, and /me answers the login with it", async () => {
    const { state, cookie } = await start();
    const code = codeFor(1001, "octocat");
    const res = await callback(`code=${code}&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}`, "User-Agent": IPHONE });
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/");
    expect(await res.text()).toBe("");
    const set = setCookies(res).find((c) => c.startsWith("__Host-arakun_session="))!;
    for (const part of ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"]) expect(set).toContain(part);
    const row = await env.DB.prepare("SELECT name, kind, expires_at FROM sessions").first<{ name: string; kind: string; expires_at: number }>();
    expect(row).toMatchObject({ name: "iPhone", kind: "phone" });
    // The cookie outlives the row's 90 days, up to the most a renewed session
    // lasts: the row decides when the session ends, not the browser.
    expect(Number(/Max-Age=(\d+)/.exec(set)![1])).toBe(180 * 24 * 3600);
    expect(row!.expires_at - Math.floor(Date.now() / 1000)).toBeLessThanOrEqual(90 * 24 * 3600);
    expect(clearsOAuth(res)).toBe(true);

    const token = /^__Host-arakun_session=([^;]+)/.exec(set)![1]!;
    const me = await call("GET", "/me", { Cookie: `__Host-arakun_session=${token}` });
    expect(me.status).toBe(200);
    const body = await me.json<{ account_id: number; github_id: number }>();
    expect(body).toMatchObject({ login: "octocat" });
    // The account linked to the GitHub user who signed in; github_id is its ID too, for old clients.
    const linked = await env.DB.prepare("SELECT account_id FROM identities WHERE provider = 'github' AND subject = '1001'").first("account_id");
    expect(body.account_id).toBe(linked);
    expect(body.github_id).toBe(body.account_id);
    // The same steps as the app's sign-in: the GitHub token is revoked.
    expect(github.users.has("gho_1001_octocat")).toBe(false);
    expect(github.calls.map((r) => r.method)).toEqual(["POST", "DELETE"]);
  });

  it("ends the session a browser signs in again over, only when it is the same account's", async () => {
    const sessionOf = (res: Response) => /^__Host-arakun_session=([^;]+)/.exec(setCookies(res).find((c) => c.startsWith("__Host-arakun_session="))!)![1]!;
    const signInWith = async (id: number, login: string, extra: Record<string, string> = {}) => {
      const { state, cookie } = await start();
      const cookies = [`__Host-arakun_oauth=${cookie}`, ...(extra.session ? [`__Host-arakun_session=${extra.session}`] : [])].join("; ");
      return sessionOf(await callback(`code=${codeFor(id, login)}&state=${state}`, { Cookie: cookies }));
    };
    const count = async () => (await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions").first<{ n: number }>())!.n;
    const first = await signInWith(1001, "octocat");
    expect(await count()).toBe(1);
    // Signed in again over the cookie: the old row goes, the new one stays.
    const second = await signInWith(1001, "octocat", { session: first });
    expect(await count()).toBe(1);
    expect((await call("GET", "/me", { Cookie: `__Host-arakun_session=${first}` })).status).toBe(401);
    expect((await call("GET", "/me", { Cookie: `__Host-arakun_session=${second}` })).status).toBe(200);
    // As another account: the old account's session is left alone.
    await signInWith(2002, "hubot", { session: second });
    expect(await count()).toBe(2);
    expect((await call("GET", "/me", { Cookie: `__Host-arakun_session=${second}` })).status).toBe(200);
  });

  it("exchanges the code with the client secret and the same redirect_uri", async () => {
    const { state, cookie } = await start();
    await callback(`code=${codeFor(1001, "octocat")}&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}` });
    expect(JSON.parse(github.exchanges[0]!)).toEqual({
      client_id: "Iv-test",
      client_secret: "client-secret",
      code: "abc123",
      redirect_uri: `${ORIGIN}/auth/github/callback`,
    });
  });

  it("names the session after the browser's platform", async () => {
    const agents: [string, string][] = [
      [IPHONE, "iPhone"],
      ["Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) Safari/604.1", "iPad"],
      ["Mozilla/5.0 (Linux; Android 14; Pixel 8) Chrome/120 Mobile Safari/537.36", "Android"],
      ["Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Safari/605.1.15", "Mac"],
      ["Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/120", "Windows"],
      ["Mozilla/5.0 (X11; Linux x86_64) Firefox/120", "Linux"],
      ["curl/8.0", "Browser"],
    ];
    for (const [ua, name] of agents) {
      await env.DB.prepare("DELETE FROM sessions").run();
      const { state, cookie } = await start();
      await callback(`code=${codeFor(1001, "octocat")}&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}`, "User-Agent": ua });
      expect((await env.DB.prepare("SELECT name FROM sessions").first<{ name: string }>())?.name, ua).toBe(name);
    }
    await env.DB.prepare("DELETE FROM sessions").run();
    const { state, cookie } = await start();
    await callback(`code=${codeFor(1001, "octocat")}&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}` });
    expect((await env.DB.prepare("SELECT name FROM sessions").first<{ name: string }>())?.name).toBe("Browser");
  });

  it("sends the browser back and makes no session for a state that is not the cookie's", async () => {
    const { state, cookie } = await start();
    const other = (await start()).state;
    const code = codeFor(1001, "octocat");
    const cases: [string, Record<string, string>][] = [
      [`code=${code}&state=${other}`, { Cookie: `__Host-arakun_oauth=${cookie}` }],
      [`code=${code}&state=${state}`, {}],
      [`code=${code}`, { Cookie: `__Host-arakun_oauth=${cookie}` }],
      [`code=${code}&state=`, { Cookie: "__Host-arakun_oauth=" }],
      [`code=${code}&state=${state}x`, { Cookie: `__Host-arakun_oauth=${cookie}` }],
      [`code=${code}&state=short`, { Cookie: `__Host-arakun_oauth=${cookie}` }],
    ];
    for (const [query, headers] of cases) {
      const res = await callback(query, headers);
      expect(res.status, query).toBe(302);
      expect(res.headers.get("Location"), query).toBe("/?sign_in=failed");
      expect(setCookies(res).some((c) => c.startsWith("__Host-arakun_session=")), query).toBe(false);
      expect(clearsOAuth(res), query).toBe(true);
    }
    expect(await sessionCount()).toBe(0);
    // The code was never sent to GitHub, so it is still good.
    expect(github.exchanges).toEqual([]);
    expect(github.users.has("gho_1001_octocat")).toBe(true);
  });

  it("redirects to /?sign_in=failed, with no session, when GitHub refuses the code", async () => {
    for (const outage of ["error", "throw", 503] as const) {
      const { state, cookie } = await start();
      codeFor(1001, "octocat");
      github.codeOutage = outage;
      const res = await callback(`code=abc123&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}` });
      expect(res.status, String(outage)).toBe(302);
      expect(res.headers.get("Location")).toBe("/?sign_in=failed");
      expect(setCookies(res).some((c) => c.startsWith("__Host-arakun_session="))).toBe(false);
      expect(clearsOAuth(res)).toBe(true);
    }
    expect(await sessionCount()).toBe(0);
  });

  it("redirects to /?sign_in=failed for an unknown code, a refusal on GitHub's page or a token that cannot be revoked", async () => {
    for (const query of ["code=nope", "error=access_denied", "code="]) {
      const { state, cookie } = await start();
      const res = await callback(`${query}&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}` });
      expect(res.headers.get("Location"), query).toBe("/?sign_in=failed");
      expect(clearsOAuth(res)).toBe(true);
    }
    const { state, cookie } = await start();
    codeFor(1001, "octocat");
    github.revokeOutage = 503;
    const res = await callback(`code=abc123&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}` });
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setCookies(res).some((c) => c.startsWith("__Host-arakun_session="))).toBe(false);
    expect(await sessionCount()).toBe(0);
  });

  it("sends the browser back and still clears the state cookie when making the session throws", async () => {
    const { webAuthCallback } = await import("../src/web_auth");
    const { state, cookie } = await start();
    codeFor(1001, "octocat");
    const broken = { ...env, DB: new Proxy(env.DB, { get: (db, key) => (key === "batch" ? () => Promise.reject(new Error("db down")) : Reflect.get(db, key).bind(db)) }) };
    const req = new Request(`${ORIGIN}/auth/github/callback?code=abc123&state=${state}`, {
      headers: { "CF-Connecting-IP": freshIp(), Cookie: `__Host-arakun_oauth=${cookie}` },
    });
    const res = await webAuthCallback(req, broken as typeof env);
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(setCookies(res).some((c) => c.startsWith("__Host-arakun_session="))).toBe(false);
    expect(clearsOAuth(res)).toBe(true);
  });

  it("never puts the code, the state or an answer of GitHub in the response", async () => {
    const secret = "gho_secret_value";
    for (const good of [true, false]) {
      const { state, cookie } = await start();
      if (good) github.users.set(secret, { id: 1001, login: "octocat" });
      if (good) github.codes.set("c0de-reflect", secret);
      const res = await callback(`code=c0de-reflect&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}` });
      const everything = `${res.headers.get("Location")} ${await res.text()} ${setCookies(res).filter((c) => !c.startsWith("__Host-arakun_session=")).join(" ")}`;
      for (const value of ["c0de-reflect", state, secret, "bad_verification_code", "client-secret"]) expect(everything).not.toContain(value);
      github.users.clear();
      github.codes.clear();
    }
    // A wrong state answers with no echo either.
    const res = await callback("code=c0de-reflect&state=evil-state", { Cookie: "__Host-arakun_oauth=other" });
    const text = `${await res.text()} ${res.headers.get("Location")}`;
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    for (const value of ["c0de-reflect", "evil-state", "other"]) expect(text).not.toContain(value);
  });

  it("over the sign-in limit, shared with POST /auth/github, sends the browser back and asks GitHub nothing", async () => {
    const ip = freshIp();
    await roomInWindow(60);
    for (let i = 0; i < 10; i++) await signInRequest("gho_none", ip);
    const { state, cookie } = await start(ip);
    codeFor(1001, "octocat");
    const res = await SELF.fetch(`${ORIGIN}/auth/github/callback?code=abc123&state=${state}`, {
      redirect: "manual",
      headers: { "CF-Connecting-IP": ip, Cookie: `__Host-arakun_oauth=${cookie}` },
    });
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(clearsOAuth(res)).toBe(true);
    expect(github.exchanges).toEqual([]);
    expect(await sessionCount()).toBe(0);
  });

  it("does not take a state cookie without the __Host- prefix, which a sibling host could plant", async () => {
    codeFor(1001, "octocat");
    const res = await callback("code=abc123&state=planted", { Cookie: "arakun_oauth=planted" });
    expect(res.headers.get("Location")).toBe("/?sign_in=failed");
    expect(github.exchanges).toEqual([]);
    expect(await sessionCount()).toBe(0);
  });

  it("does not let a state be used twice", async () => {
    const { state, cookie } = await start();
    codeFor(1001, "octocat");
    expect((await callback(`code=abc123&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}` })).headers.get("Location")).toBe("/");
    // The browser dropped the cookie, so a replay of the URL has none.
    const again = await callback(`code=abc123&state=${state}`);
    expect(again.status).toBe(302);
    expect(again.headers.get("Location")).toBe("/?sign_in=failed");
  });

  it("gives the cookie session no more than a bearer one: it needs an Origin to change something", async () => {
    const { state, cookie } = await start();
    codeFor(1001, "octocat");
    const res = await callback(`code=abc123&state=${state}`, { Cookie: `__Host-arakun_oauth=${cookie}` });
    const token = /^__Host-arakun_session=([^;]+)/.exec(setCookies(res).find((c) => c.startsWith("__Host-arakun_session="))!)![1]!;
    expect((await call("POST", "/auth/logout", { Cookie: `__Host-arakun_session=${token}` })).status).toBe(403);
    expect((await call("GET", "/me", as(token))).status).toBe(200);
  });
});
