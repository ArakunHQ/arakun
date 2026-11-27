// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { env, SELF } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { MIN_NEW_ACCOUNT_ID, session, signIn as signInHandler } from "../src/auth";
import { ADMIN, as, call, freshIp, github, nowSeconds, roomInWindow, signIn, signInRequest } from "./helpers";

const DAY = 24 * 60 * 60;

/// A session as a Worker from before sessions had ids made it.
async function oldSession(token: string, accountId: number): Promise<void> {
  const hash = [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token)))].map((b) => b.toString(16).padStart(2, "0")).join("");
  const t = nowSeconds();
  await env.DB.prepare("INSERT INTO sessions (token_hash, account_id, created_at, expires_at, last_used_at) VALUES (?, ?, ?, ?, ?)")
    .bind(hash, accountId, t, t + 90 * DAY, t)
    .run();
}

describe("sign-in with GitHub", () => {
  it("creates the user and a session from a valid token", async () => {
    const before = nowSeconds();
    const got = await signIn(1001, "octocat");
    expect(got.account_id).toBeGreaterThanOrEqual(MIN_NEW_ACCOUNT_ID);
    // Kept for clients of before: the account ID under its old name.
    expect(got.github_id).toBe(got.account_id);
    expect(got.login).toBe("octocat");
    expect(got.flags).toEqual([]);
    expect(got.session).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(got.expires_at).toBeGreaterThanOrEqual(before + 90 * DAY);
    expect(got.expires_at).toBeLessThanOrEqual(nowSeconds() + 90 * DAY);

    const user = await env.DB.prepare("SELECT * FROM accounts WHERE id = ?").bind(got.account_id).first();
    expect(user).toMatchObject({ id: got.account_id, login: "octocat" });
    // Nothing else GitHub said is kept, but the primary verified address.
    expect(Object.keys(user!).sort()).toEqual(
      ["blocked_at", "blocked_reason", "created_at", "email", "id", "last_seen_at", "login", "plan"],
    );
    expect(user).toMatchObject({ plan: "free", blocked_at: null, email: null });
    const sessions = await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions WHERE account_id = ?")
      .bind(got.account_id)
      .first<{ n: number }>();
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
    expect(second.account_id).toBe(first.account_id);
    const me = await (await call("GET", "/me", as(first.session))).json();
    expect(me).toEqual({
      account_id: first.account_id,
      github_id: first.account_id,
      login: "octo-renamed",
      flags: [],
      plan: "free",
      notices: [],
    });
  });

  it("gives a new account an ID of its own, linked to the GitHub user, and finds it again under a new login", async () => {
    const first = await signIn(1001, "octocat");
    expect(first.account_id).toBeGreaterThanOrEqual(MIN_NEW_ACCOUNT_ID);
    expect(first.account_id).toBeLessThanOrEqual(Number.MAX_SAFE_INTEGER);
    expect(first.account_id).not.toBe(1001);
    const identities = await env.DB.prepare("SELECT provider, subject, account_id FROM identities").all();
    expect(identities.results).toEqual([{ provider: "github", subject: "1001", account_id: first.account_id }]);

    const again = await signIn(1001, "octo-renamed");
    expect(again.account_id).toBe(first.account_id);
    expect(again.login).toBe("octo-renamed");
    const accounts = await env.DB.prepare("SELECT id, login FROM accounts").all();
    expect(accounts.results).toEqual([{ id: first.account_id, login: "octo-renamed" }]);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM identities").first<{ n: number }>())?.n).toBe(1);

    // Another GitHub user gets another account.
    const other = await signIn(2002, "other");
    expect(other.account_id).not.toBe(first.account_id);
  });

  it("signs an account from before accounts had IDs of their own in to it, and keeps its sessions working", async () => {
    const t = nowSeconds();
    // As the migration left it: the GitHub ID is the account ID.
    await env.DB.prepare("INSERT INTO accounts (id, login, created_at, last_seen_at) VALUES (1001, 'octocat', ?1, ?1)").bind(t).run();
    await env.DB.prepare(
      "INSERT INTO identities (provider, subject, account_id, linked_at) SELECT 'github', CAST(id AS TEXT), id, created_at FROM accounts",
    ).run();
    const token = "d".repeat(43);
    await oldSession(token, 1001);

    const got = await signIn(1001, "octo-renamed");
    expect(got.account_id).toBe(1001);
    expect(got.github_id).toBe(1001);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM accounts").first<{ n: number }>())?.n).toBe(1);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM identities").first<{ n: number }>())?.n).toBe(1);

    const res = await call("GET", "/me", as(token));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ account_id: 1001, github_id: 1001, login: "octo-renamed", flags: [], plan: "free", notices: [] });
  });

  it("answers 401 when GitHub rejects the token", async () => {
    const res = await signInRequest("gho_unknown");
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: expect.any(String) });
    const users = await env.DB.prepare("SELECT COUNT(*) AS n FROM accounts").first<{ n: number }>();
    expect(users?.n).toBe(0);
  });

  it("logs a rejected token without the token", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    expect((await signInRequest("gho_unknown_value")).status).toBe(401);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0])).toMatch(/404/);
    expect(String(warn.mock.calls[0])).not.toContain("gho_unknown_value");
  });

  it("logs a wrong client secret, and answers 502", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    github.users.set("gho_ok", { id: 1001, login: "octocat" });
    // The fake checks the credentials as GitHub does; a wrong secret gets 401.
    const wrong = { ...env, GITHUB_CLIENT_SECRET: "not-the-secret" };
    const req = new Request("https://teams.test/auth/github", {
      method: "POST",
      headers: { "CF-Connecting-IP": freshIp() },
      body: JSON.stringify({ access_token: "gho_ok" }),
    });
    expect((await signInHandler(req, wrong)).status).toBe(502);
    expect(error).toHaveBeenCalledWith("GitHub refused the app's client credentials");
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
    await roomInWindow(60);
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
    expect(names).toEqual(expect.arrayContaining(["accounts", "identities", "sessions", "user_flags"]));
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
    await call("PUT", `/admin/users/${got.account_id}/flags/teams`, ADMIN);
    await call("PUT", `/admin/users/${got.account_id}/flags/beta_chat`, ADMIN);
    const res = await call("GET", "/me", as(got.session));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({
      account_id: got.account_id,
      github_id: got.account_id,
      login: "octocat",
      flags: ["beta_chat", "teams"],
      plan: "free",
      notices: [],
    });
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
      // GET / is the phone page (router.test.ts).
      ["POST", "/"],
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

const ORIGIN = "https://teams.test";
const cookieOf = (session: string, extra: Record<string, string> = {}) => ({ Cookie: `__Host-arakun_session=${session}`, ...extra });

describe("named sessions", () => {
  it("lists a session by its name and kind, marked as the current one", async () => {
    const got = await signIn(1001, "octocat", undefined, { name: "Rik's Mac", kind: "app" });
    expect(got.session_id).toMatch(/^[0-9a-f]{32}$/);
    expect(got.plan).toBe("free");
    const res = await call("GET", "/sessions", as(got.session));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual([
      { id: got.session_id, name: "Rik's Mac", kind: "app", created_at: expect.any(Number), last_used_at: expect.any(Number), current: true },
    ]);
    const other = await signIn(1001, "octocat", "gho_other", { kind: "phone" });
    const seen = (await (await call("GET", "/sessions", as(other.session))).json()) as { id: string; name: string; current: boolean }[];
    expect(seen.map((r) => [r.id === other.session_id, r.name, r.current]).sort()).toEqual([
      [false, "Rik's Mac", false],
      [true, "octocat", true],
    ]);
  });

  it("answers 400 for an unknown kind or a bad name", async () => {
    for (const extra of [{ kind: "toaster" }, { kind: 3 }, { name: "" }, { name: "  " }, { name: "x".repeat(101) }, { name: 5 }]) {
      github.users.set("gho_bad", { id: 1001, login: "octocat" });
      const res = await signInRequest("gho_bad", undefined, extra);
      expect(res.status, JSON.stringify(extra)).toBe(400);
    }
    // Nothing was spent: the token still works.
    expect(github.users.has("gho_bad")).toBe(true);
  });

  it("signs another session out, and its token answers 401", async () => {
    const one = await signIn(1001, "octocat");
    const two = await signIn(1001, "octocat", "gho_two");
    expect((await call("DELETE", `/sessions/${two.session_id}`, as(one.session))).status).toBe(204);
    expect((await call("GET", "/me", as(two.session))).status).toBe(401);
    expect((await call("GET", "/me", as(one.session))).status).toBe(200);
    expect((await call("DELETE", `/sessions/${two.session_id}`, as(one.session))).status).toBe(404);
  });

  it("answers 404 for another account's session id", async () => {
    const mine = await signIn(1001, "octocat");
    const theirs = await signIn(2002, "other");
    expect((await call("DELETE", `/sessions/${theirs.session_id}`, as(mine.session))).status).toBe(404);
    expect((await call("GET", "/me", as(theirs.session))).status).toBe(200);
    expect((await call("DELETE", "/sessions/not-an-id", as(mine.session))).status).toBe(404);
    expect((await call("GET", "/sessions")).status).toBe(401);
    expect((await call("DELETE", `/sessions/${theirs.session_id}`)).status).toBe(401);
  });

  it("the migration gives old sessions without an id one and their login for a name, and ids stay unique", async () => {
    const kept = await signIn(1001, "octocat");
    await oldSession("a".repeat(43), kept.account_id);
    await oldSession("b".repeat(43), kept.account_id);
    // The migration's own statements, run again over rows as it found them.
    await env.DB.prepare("UPDATE sessions SET name = '' WHERE id IS NULL").run();
    const migration = env.TEST_MIGRATIONS.find((m) => m.name.startsWith("0008"))!;
    const updates = migration.queries.filter((q) => q.includes("UPDATE sessions"));
    expect(updates).toHaveLength(2);
    // Under the names 0016 gave the table and columns since.
    const renamed = (q: string) =>
      q
        .replace(/users\.github_id/g, "accounts.id")
        .replace(/\busers\b/g, "accounts")
        .replace(/sessions\.github_id/g, "sessions.account_id");
    for (const q of updates) await env.DB.prepare(renamed(q)).run();
    const rows = await env.DB.prepare("SELECT id, name FROM sessions").all<{ id: string | null; name: string }>();
    expect(rows.results).toHaveLength(3);
    for (const r of rows.results) {
      expect(r.id).toMatch(/^[0-9a-f]{32}$/);
      expect(r.name).toBe("octocat");
    }
    expect(new Set(rows.results.map((r) => r.id)).size).toBe(3);
    expect(rows.results.map((r) => r.id)).toContain(kept.session_id);
    await expect(env.DB.prepare("UPDATE sessions SET id = (SELECT MIN(id) FROM sessions)").run()).rejects.toThrow();
  });

  it("gives a session made after the migration by the Worker of before an id on its first use", async () => {
    const got = await signIn(1001, "octocat");
    const token = "c".repeat(43);
    await oldSession(token, got.account_id);
    const list = (await (await call("GET", "/sessions", as(token))).json()) as { id: string; name: string; current: boolean }[];
    expect(list).toHaveLength(2);
    const mine = list.filter((s) => s.current);
    expect(mine).toHaveLength(1);
    expect(mine[0]!.id).toMatch(/^[0-9a-f]{32}$/);
    expect(mine[0]!.name).toBe("octocat");
    // The same id from then on, and it signs the session out.
    const again = (await (await call("GET", "/sessions", as(token))).json()) as { id: string; current: boolean }[];
    expect(again.find((s) => s.current)!.id).toBe(mine[0]!.id);
    expect((await call("DELETE", `/sessions/${mine[0]!.id}`, as(token))).status).toBe(204);
    expect((await call("GET", "/me", as(token))).status).toBe(401);
  });

  it("keeps at most 100 sessions of an account: the one used longest ago makes room", async () => {
    const first = await signIn(1001, "octocat", "gho_0");
    for (let i = 1; i < 100; i++) await signIn(1001, "octocat", `gho_${i}`);
    const other = await signIn(2002, "other");
    await env.DB.prepare("UPDATE sessions SET last_used_at = last_used_at - 60 WHERE id = ?").bind(first.session_id).run();
    const newest = await signIn(1001, "octocat", "gho_100");
    const list = (await (await call("GET", "/sessions", as(newest.session))).json()) as { id: string }[];
    expect(list).toHaveLength(100);
    expect(list.map((s) => s.id)).not.toContain(first.session_id);
    expect((await call("GET", "/me", as(first.session))).status).toBe(401);
    expect((await call("GET", "/me", as(other.session))).status).toBe(200);
  });
});

describe("the session cookie", () => {
  it("is a __Host- cookie, and two of the name are no sign-in at all", async () => {
    const got = await signIn(1001, "octocat");
    expect((await call("GET", "/me", cookieOf(got.session))).status).toBe(200);
    // The name without the prefix is not ours.
    expect((await call("GET", "/me", { Cookie: `arakun_session=${got.session}` })).status).toBe(401);
    // A second cookie of the name, planted or not: neither is taken.
    const planted = { Cookie: `__Host-arakun_session=${got.session}; __Host-arakun_session=${"x".repeat(43)}` };
    expect((await call("GET", "/me", planted)).status).toBe(401);
    const reversed = { Cookie: `__Host-arakun_session=${"x".repeat(43)}; __Host-arakun_session=${got.session}` };
    expect((await call("GET", "/me", reversed)).status).toBe(401);
  });

  it("lets a cookie session read /me and /sessions", async () => {
    const got = await signIn(1001, "octocat");
    const res = await call("GET", "/me", cookieOf(got.session));
    expect(res.status).toBe(200);
    expect(((await res.json()) as { login: string }).login).toBe("octocat");
    expect((await call("GET", "/sessions", cookieOf(got.session))).status).toBe(200);
  });

  it("refuses a cookie session that changes something without our Origin", async () => {
    const one = await signIn(1001, "octocat");
    const two = await signIn(1001, "octocat", "gho_two");
    const path = `/sessions/${two.session_id}`;
    expect((await call("DELETE", path, cookieOf(one.session))).status).toBe(403);
    expect((await call("DELETE", path, cookieOf(one.session, { Origin: "https://evil.example" }))).status).toBe(403);
    expect((await call("GET", "/me", as(two.session))).status).toBe(200);
    expect((await call("DELETE", path, cookieOf(one.session, { Origin: ORIGIN }))).status).toBe(204);
  });

  it("does not ask a bearer session for an Origin", async () => {
    const one = await signIn(1001, "octocat");
    const two = await signIn(1001, "octocat", "gho_two");
    expect((await call("DELETE", `/sessions/${two.session_id}`, as(one.session))).status).toBe(204);
  });

  it("clears the cookie on logout only when the request came with it", async () => {
    const one = await signIn(1001, "octocat");
    const withCookie = await call("POST", "/auth/logout", cookieOf(one.session, { Origin: ORIGIN }));
    expect(withCookie.status).toBe(204);
    expect(withCookie.headers.get("Set-Cookie")).toMatch(/^__Host-arakun_session=;.*HttpOnly.*Secure.*SameSite=Lax.*Path=\/.*Max-Age=0/);
    expect((await call("GET", "/me", as(one.session))).status).toBe(401);
    const two = await signIn(1001, "octocat", "gho_two");
    const bearerOut = await call("POST", "/auth/logout", as(two.session));
    expect(bearerOut.status).toBe(204);
    expect(bearerOut.headers.get("Set-Cookie")).toBeNull();
  });

  it("clears a dead cookie when logout answers 401 to it", async () => {
    const one = await signIn(1001, "octocat");
    await call("POST", "/auth/logout", cookieOf(one.session, { Origin: ORIGIN }));
    const again = await call("POST", "/auth/logout", cookieOf(one.session, { Origin: ORIGIN }));
    expect(again.status).toBe(401);
    expect(again.headers.get("Set-Cookie")).toMatch(/^__Host-arakun_session=;.*Max-Age=0/);
    const noCookie = await call("POST", "/auth/logout", { Origin: ORIGIN });
    expect(noCookie.status).toBe(401);
    expect(noCookie.headers.get("Set-Cookie")).toBeNull();
    const dead = await call("POST", "/auth/logout", as(one.session));
    expect(dead.status).toBe(401);
    expect(dead.headers.get("Set-Cookie")).toBeNull();
  });

  it("clears the cookie when a cookie session deletes itself, and not for another session", async () => {
    const one = await signIn(1001, "octocat");
    const two = await signIn(1001, "octocat", "gho_two");
    const other = await call("DELETE", `/sessions/${two.session_id}`, cookieOf(one.session, { Origin: ORIGIN }));
    expect(other.status).toBe(204);
    expect(other.headers.get("Set-Cookie")).toBeNull();
    const own = await call("DELETE", `/sessions/${one.session_id}`, cookieOf(one.session, { Origin: ORIGIN }));
    expect(own.status).toBe(204);
    expect(own.headers.get("Set-Cookie")).toMatch(/^__Host-arakun_session=;.*Max-Age=0/);
    const three = await signIn(1001, "octocat", "gho_three");
    const byBearer = await call("DELETE", `/sessions/${three.session_id}`, as(three.session));
    expect(byBearer.status).toBe(204);
    expect(byBearer.headers.get("Set-Cookie")).toBeNull();
  });
});

describe("email and config", () => {
  it("stores the primary verified email", async () => {
    github.emails = [
      { email: "old@example.com", primary: false, verified: true },
      { email: "unverified@example.com", primary: true, verified: false },
    ];
    await signIn(1001, "octocat");
    expect((await env.DB.prepare("SELECT email FROM accounts").first<{ email: string | null }>())?.email).toBeNull();
    github.emails = [
      { email: "old@example.com", primary: false, verified: true },
      { email: "rik@example.com", primary: true, verified: true },
    ];
    await signIn(1001, "octocat", "gho_two");
    expect((await env.DB.prepare("SELECT email FROM accounts").first<{ email: string | null }>())?.email).toBe("rik@example.com");
    expect(github.emailCalls[0]!.headers.get("Authorization")).toBe("Bearer gho_1001_octocat");
  });

  it("still signs in when the emails call fails", async () => {
    for (const outage of ["throw", 403, 500] as const) {
      github.emails = outage;
      const got = await signIn(1001, "octocat", `gho_${outage}`);
      expect(got.session).toMatch(/^[A-Za-z0-9_-]{43}$/);
    }
    expect((await env.DB.prepare("SELECT email FROM accounts").first<{ email: string | null }>())?.email).toBeNull();
  });

  it("keeps the stored email when the emails call fails, and clears it when there is no primary verified one", async () => {
    const email = async () => (await env.DB.prepare("SELECT email FROM accounts").first<{ email: string | null }>())?.email;
    github.emails = [{ email: "rik@example.com", primary: true, verified: true }];
    await signIn(1001, "octocat", "gho_one");
    for (const outage of ["throw", 403, 500] as const) {
      github.emails = outage;
      await signIn(1001, "octocat", `gho_${outage}`);
      expect(await email(), String(outage)).toBe("rik@example.com");
    }
    github.emails = [{ email: "x@example.com", primary: true, verified: false }];
    await signIn(1001, "octocat", "gho_none");
    expect(await email()).toBeNull();
  });

  it("answers the client ID on /auth/config", async () => {
    const res = await call("GET", "/auth/config");
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ github_client_id: "Iv-test" });
  });
});
