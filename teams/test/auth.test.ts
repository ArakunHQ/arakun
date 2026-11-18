import { env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { session } from "../src/auth";
import { ADMIN, as, call, freshIp, github, nowSeconds, signIn, signInRequest } from "./helpers";

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

  it("asks GitHub whether our app issued the token, then revokes it, with the headers GitHub wants", async () => {
    await signIn(1001, "octocat", "gho_abc");
    expect(github.calls.map((req) => req.method)).toEqual(["POST", "DELETE"]);
    for (const [i, req] of github.calls.entries()) {
      expect(req.url).toBe("https://api.github.com/applications/Iv-test/token");
      expect(req.headers.get("Authorization")).toBe(`Basic ${btoa("Iv-test:client-secret")}`);
      expect(JSON.parse(github.bodies[i]!)).toEqual({ access_token: "gho_abc" });
      expect(req.headers.get("User-Agent")).toBe("arakun-teams");
      expect(req.headers.get("Accept")).toBe("application/vnd.github+json");
      expect(req.headers.get("X-GitHub-Api-Version")).toBe("2022-11-28");
      expect(req.redirect).toBe("manual");
    }
  });

  it("gives one session per GitHub token, since it revokes the token", async () => {
    github.users.set("gho_once", { id: 1001, login: "octocat" });
    expect((await signInRequest("gho_once")).status).toBe(200);
    expect(github.users.has("gho_once")).toBe(false);
    expect((await signInRequest("gho_once")).status).toBe(401);
  });

  it("gives no session when the token cannot be revoked", async () => {
    github.users.set("gho_ok", { id: 1001, login: "octocat" });
    for (const outage of ["throw", 503, 422, 401, 302] as const) {
      github.revokeOutage = outage;
      const res = await signInRequest("gho_ok");
      expect(res.status, String(outage)).toBe(502);
      expect(await res.json()).toEqual({ error: expect.any(String) });
    }
    // Revoked by another sign-in between the check and the revoke.
    github.revokeOutage = 404;
    expect((await signInRequest("gho_ok")).status).toBe(401);
    const sessions = await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions").first<{ n: number }>();
    expect(sessions?.n).toBe(0);
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
    // GitHub answers 422 when it is spammed, so it is not a clear no.
    github.outage = 422;
    expect((await signInRequest("gho_ok")).status).toBe(502);
  });

  it("does not follow a redirect with the client secret", async () => {
    github.users.set("gho_ok", { id: 1001, login: "octocat" });
    expect((await signInRequest("gho_ok")).status).toBe(200);
    expect(github.calls[0]!.redirect).toBe("manual");
    github.outage = 302;
    expect((await signInRequest("gho_ok")).status).toBe(502);
  });

  it("accepts the underscore in an Enterprise Managed User's login", async () => {
    const got = await signIn(1001, "mona-cat_octo");
    expect(got.login).toBe("mona-cat_octo");
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

  it("answers 413 for a big body, with or without a Content-Length", async () => {
    const big = JSON.stringify({ access_token: "gho_ok", pad: "x".repeat(8192) });
    const sized = await SELF.fetch("https://teams.test/auth/github", {
      method: "POST",
      headers: { "CF-Connecting-IP": freshIp() },
      body: big,
    });
    expect(sized.status).toBe(413);
    const bytes = new TextEncoder().encode(big);
    const streamed = await SELF.fetch("https://teams.test/auth/github", {
      method: "POST",
      headers: { "CF-Connecting-IP": freshIp() },
      body: new ReadableStream({
        start(controller) {
          for (let i = 0; i < bytes.length; i += 1024) controller.enqueue(bytes.slice(i, i + 1024));
          controller.close();
        },
      }),
    });
    expect(streamed.status).toBe(413);
    expect(github.calls).toHaveLength(0);
  });

  it("takes a token spelled like the size limit as a token, not as a big body", async () => {
    const res = await signInRequest("too big");
    expect(res.status).not.toBe(413);
    expect(github.calls).toHaveLength(1);
  });

  it("answers 400 when the body stream breaks off", async () => {
    const res = await SELF.fetch("https://teams.test/auth/github", {
      method: "POST",
      headers: { "CF-Connecting-IP": freshIp() },
      body: new ReadableStream({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('{"access_token":'));
          controller.error(new Error("connection lost"));
        },
      }),
    });
    expect(res.status).toBe(400);
    expect(github.calls).toHaveLength(0);
  });

  it("answers 429 after ten sign-ins a minute from one IP", async () => {
    // Each sign-in revokes its token, so each needs a new one.
    for (let i = 0; i <= 11; i++) github.users.set(`gho_ok_${i}`, { id: 1001, login: "octocat" });
    const ip = "203.0.113.7";
    for (let i = 0; i < 10; i++) expect((await signInRequest(`gho_ok_${i}`, ip)).status).toBe(200);
    const res = await signInRequest("gho_ok_10", ip);
    expect(res.status).toBe(429);
    expect(await res.json()).toEqual({ error: expect.any(String) });
    // A check and a revoke for each of the ten.
    expect(github.calls).toHaveLength(20);
    // Another IP is not held back.
    expect((await signInRequest("gho_ok_11", "203.0.113.8")).status).toBe(200);
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

  it("is never extended past 180 days after sign-in, and then ends", async () => {
    const got = await signIn(1001, "octocat");
    const t = nowSeconds();
    // Signed in 150 days ago, last used two days ago: renewal stops at day 180.
    await env.DB.prepare("UPDATE sessions SET created_at = ?, expires_at = ?, last_used_at = ?")
      .bind(t - 150 * DAY, t + DAY, t - 2 * DAY)
      .run();
    expect((await call("GET", "/me", as(got.session))).status).toBe(200);
    const row = await env.DB.prepare("SELECT expires_at, last_used_at FROM sessions").first<{ expires_at: number; last_used_at: number }>();
    expect(row!.expires_at).toBe(t - 150 * DAY + 180 * DAY);
    expect(row!.last_used_at).toBeGreaterThanOrEqual(t);

    // Day 180 has come: no renewal brings it back.
    await env.DB.prepare("UPDATE sessions SET created_at = ?, expires_at = ?, last_used_at = ?")
      .bind(t - 180 * DAY, t - 1, t - 2 * DAY)
      .run();
    expect((await call("GET", "/me", as(got.session))).status).toBe(401);
  });

  it("drops the user's expired sessions on the next sign-in", async () => {
    await signIn(1001, "octocat");
    await env.DB.prepare("UPDATE sessions SET expires_at = ?").bind(nowSeconds() - 1).run();
    await signIn(1001, "octocat");
    const left = await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions").first<{ n: number }>();
    expect(left?.n).toBe(1);
  });

  it("is not extended by logout", async () => {
    const got = await signIn(1001, "octocat");
    const t = nowSeconds();
    await env.DB.prepare("UPDATE sessions SET expires_at = ?, last_used_at = ?").bind(t + DAY, t - 2 * DAY).run();
    const req = new Request("https://teams.test/auth/logout", { method: "POST", headers: as(got.session) });
    expect(await session(req, env, { renew: false })).not.toBeNull();
    const row = await env.DB.prepare("SELECT expires_at, last_used_at FROM sessions").first();
    expect(row).toEqual({ expires_at: t + DAY, last_used_at: t - 2 * DAY });
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
