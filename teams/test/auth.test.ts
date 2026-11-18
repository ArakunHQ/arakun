import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ADMIN, as, call, github, nowSeconds, signIn, signInRequest } from "./helpers";

const DAY = 24 * 60 * 60;

describe("sign-in with GitHub", () => {
  it("creates the user and a session from a valid token", async () => {
    const before = nowSeconds();
    const got = await signIn(1001, "octocat");
    expect(got.github_id).toBe(1001);
    expect(got.login).toBe("octocat");
    expect(got.flags).toEqual([]);
    expect(got.session).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(got.expires_at).toBeGreaterThanOrEqual(before + 90 * DAY);
    expect(got.expires_at).toBeLessThanOrEqual(nowSeconds() + 90 * DAY);

    const user = await env.DB.prepare("SELECT * FROM users WHERE github_id = 1001").first();
    expect(user).toMatchObject({ github_id: 1001, login: "octocat" });
    // Nothing else GitHub said is kept.
    expect(Object.keys(user!).sort()).toEqual(["created_at", "github_id", "last_seen_at", "login"]);
    const sessions = await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions WHERE github_id = 1001").first<{ n: number }>();
    expect(sessions?.n).toBe(1);
  });

  it("asks GitHub whether our app issued the token, with the headers GitHub wants", async () => {
    await signIn(1001, "octocat", "gho_abc");
    expect(github.calls).toHaveLength(1);
    const req = github.calls[0]!;
    expect(req.method).toBe("POST");
    expect(req.url).toBe("https://api.github.com/applications/Iv-test/token");
    expect(req.headers.get("Authorization")).toBe(`Basic ${btoa("Iv-test:client-secret")}`);
    expect(JSON.parse(github.bodies[0]!)).toEqual({ access_token: "gho_abc" });
    expect(req.headers.get("User-Agent")).toBe("arakun-teams");
    expect(req.headers.get("Accept")).toBe("application/vnd.github+json");
    expect(req.headers.get("X-GitHub-Api-Version")).toBe("2022-11-28");
  });

  it("gives a new session on each sign-in and keeps the login current", async () => {
    const first = await signIn(1001, "octocat");
    const second = await signIn(1001, "octo-renamed");
    expect(second.session).not.toBe(first.session);
    const me = await (await call("GET", "/me", as(first.session))).json();
    expect(me).toEqual({ github_id: 1001, login: "octo-renamed", flags: [] });
  });

  it("answers 401 when GitHub rejects the token", async () => {
    const res = await signInRequest("gho_unknown");
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: expect.any(String) });
    const users = await env.DB.prepare("SELECT COUNT(*) AS n FROM users").first<{ n: number }>();
    expect(users?.n).toBe(0);
  });

  it("answers 401 for a token another app issued, even one GitHub would accept elsewhere", async () => {
    // GitHub answers 404 when our app did not issue the token, the case /user
    // would have let through (a gh CLI or personal token of the user's).
    const res = await signInRequest("ghp_personal_token");
    expect(res.status).toBe(401);
  });

  it("answers 502, not 401, when GitHub refuses our own client credentials", async () => {
    github.users.set("gho_ok", { id: 1001, login: "octocat" });
    github.outage = 401;
    expect((await signInRequest("gho_ok")).status).toBe(502);
  });

  it("answers 502 when GitHub cannot be reached or fails", async () => {
    github.users.set("gho_ok", { id: 1001, login: "octocat" });
    github.outage = "throw";
    expect((await signInRequest("gho_ok")).status).toBe(502);
    github.outage = 503;
    expect((await signInRequest("gho_ok")).status).toBe(502);
    // GitHub's own rate limit is not the user's fault either.
    github.outage = 403;
    const res = await signInRequest("gho_ok");
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({ error: expect.any(String) });
  });

  it("answers 502 when GitHub's answer has no usable ID or login", async () => {
    github.users.set("gho_bad_id", { id: -5, login: "octocat" });
    github.users.set("gho_bad_login", { id: 1001, login: "not a login" });
    expect((await signInRequest("gho_bad_id")).status).toBe(502);
    expect((await signInRequest("gho_bad_login")).status).toBe(502);
  });

  it("refuses a body without an access token, before asking GitHub", async () => {
    for (const token of [undefined, "", 42, "x".repeat(1025)]) {
      const res = await signInRequest(token);
      expect(res.status).toBe(400);
      expect(await res.json()).toEqual({ error: expect.any(String) });
    }
    const notJson = await call("POST", "/auth/github");
    expect(notJson.status).toBe(400);
    expect(github.calls).toHaveLength(0);
  });

  it("answers 429 after ten sign-ins a minute from one IP", async () => {
    github.users.set("gho_ok", { id: 1001, login: "octocat" });
    const ip = "203.0.113.7";
    for (let i = 0; i < 10; i++) expect((await signInRequest("gho_ok", ip)).status).toBe(200);
    const res = await signInRequest("gho_ok", ip);
    expect(res.status).toBe(429);
    expect(await res.json()).toEqual({ error: expect.any(String) });
    expect(github.calls).toHaveLength(10);
    // Another IP is not held back.
    expect((await signInRequest("gho_ok", "203.0.113.8")).status).toBe(200);
  });

  it("never stores the GitHub token or the session token", async () => {
    const accessToken = "gho_secret_token_value_123";
    const got = await signIn(1001, "octocat", accessToken);
    await call("GET", "/me", as(got.session));
    const tables = await env.DB.prepare(
      "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'",
    ).all<{ name: string }>();
    const names = tables.results.map((t) => t.name);
    expect(names).toEqual(expect.arrayContaining(["users", "sessions", "user_flags"]));
    for (const name of names) {
      const rows = await env.DB.prepare(`SELECT * FROM "${name}"`).all();
      for (const row of rows.results) {
        for (const value of Object.values(row)) {
          expect(String(value)).not.toContain(accessToken);
          expect(String(value)).not.toContain(got.session);
        }
      }
    }
  });
});

describe("sessions", () => {
  it("lets /me answer with the user's flags", async () => {
    const got = await signIn(1001, "octocat");
    await call("PUT", "/admin/users/1001/flags/teams", ADMIN);
    await call("PUT", "/admin/users/1001/flags/beta_chat", ADMIN);
    const res = await call("GET", "/me", as(got.session));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ github_id: 1001, login: "octocat", flags: ["beta_chat", "teams"] });
  });

  it("answers 401 without a valid session", async () => {
    expect((await call("GET", "/me")).status).toBe(401);
    expect((await call("GET", "/me", as("not-a-session"))).status).toBe(401);
    expect((await call("GET", "/me", as("A".repeat(43)))).status).toBe(401);
    const got = await signIn(1001, "octocat");
    expect((await call("GET", "/me", { Authorization: got.session })).status).toBe(401);
  });

  it("answers 401 once the session has expired, and forgets it", async () => {
    const got = await signIn(1001, "octocat");
    await env.DB.prepare("UPDATE sessions SET expires_at = ?").bind(nowSeconds() - 1).run();
    const res = await call("GET", "/me", as(got.session));
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: expect.any(String) });
    const left = await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions").first<{ n: number }>();
    expect(left?.n).toBe(0);
  });

  it("is extended on use, at most once a day", async () => {
    const got = await signIn(1001, "octocat");
    const t = nowSeconds();
    const expiry = () => env.DB.prepare("SELECT expires_at, last_used_at FROM sessions").first<{ expires_at: number; last_used_at: number }>();

    // Used an hour ago: left alone.
    await env.DB.prepare("UPDATE sessions SET expires_at = ?, last_used_at = ?").bind(t + DAY, t - 3600).run();
    expect((await call("GET", "/me", as(got.session))).status).toBe(200);
    expect(await expiry()).toEqual({ expires_at: t + DAY, last_used_at: t - 3600 });

    // Used two days ago: another 90 days from now.
    await env.DB.prepare("UPDATE sessions SET last_used_at = ?").bind(t - 2 * DAY).run();
    expect((await call("GET", "/me", as(got.session))).status).toBe(200);
    const after = await expiry();
    expect(after!.expires_at).toBeGreaterThanOrEqual(t + 90 * DAY);
    expect(after!.last_used_at).toBeGreaterThanOrEqual(t);
  });

  it("drops the user's expired sessions on the next sign-in", async () => {
    await signIn(1001, "octocat");
    await env.DB.prepare("UPDATE sessions SET expires_at = ?").bind(nowSeconds() - 1).run();
    await signIn(1001, "octocat");
    const left = await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions").first<{ n: number }>();
    expect(left?.n).toBe(1);
  });

  it("ends with logout, and only that session", async () => {
    const one = await signIn(1001, "octocat");
    const two = await signIn(1001, "octocat");
    const res = await call("POST", "/auth/logout", as(one.session));
    expect(res.status).toBe(204);
    expect((await call("GET", "/me", as(one.session))).status).toBe(401);
    expect((await call("POST", "/auth/logout", as(one.session))).status).toBe(401);
    expect((await call("GET", "/me", as(two.session))).status).toBe(200);
  });
});

describe("routes", () => {
  it("answers /healthz", async () => {
    const res = await call("GET", "/healthz");
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });
  });

  it("answers 404 in JSON for anything else, without CORS", async () => {
    for (const [method, path] of [
      ["GET", "/"],
      ["GET", "/auth/github"],
      ["POST", "/me"],
      ["GET", "/auth/logout"],
      ["OPTIONS", "/me"],
      ["GET", "/healthz/"],
    ] as const) {
      const res = await call(method, path);
      expect(res.status, `${method} ${path}`).toBe(404);
      expect(await res.json()).toEqual({ error: "not found" });
      expect(res.headers.get("Access-Control-Allow-Origin")).toBeNull();
    }
  });
});
