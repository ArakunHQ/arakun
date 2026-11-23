import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { admin } from "../src/admin";
import { ADMIN, as, call, signIn } from "./helpers";

describe("the admin door", () => {
  it("opens only to the exact secret", async () => {
    const path = "/admin/users/1001/flags/teams";
    for (const headers of [
      {} as Record<string, string>,
      { Authorization: "Bearer test-secre" },
      { Authorization: "Bearer test-secret!" },
      { Authorization: "test-secret" },
      { Authorization: "Basic test-secret" },
    ]) {
      const res = await call("PUT", path, headers);
      expect(res.status, JSON.stringify(headers)).toBe(401);
      expect(await res.json()).toEqual({ error: "unauthorized" });
    }
    const flags = await env.DB.prepare("SELECT COUNT(*) AS n FROM user_flags").first<{ n: number }>();
    expect(flags?.n).toBe(0);
    expect((await call("PUT", path, ADMIN)).status).toBe(200);
  });

  it("does not take a user's session as the secret", async () => {
    const got = await signIn(1001, "octocat");
    expect((await call("PUT", "/admin/users/1001/flags/teams", as(got.session))).status).toBe(401);
  });

  it("knows only its own paths and methods", async () => {
    expect((await call("GET", "/admin/users/1001/flags/teams", ADMIN)).status).toBe(404);
    expect((await call("POST", "/admin/users/1001/flags/teams", ADMIN)).status).toBe(404);
    expect((await call("PUT", "/admin/users/1001/flags", ADMIN)).status).toBe(404);
    expect((await call("PUT", "/admin/users/1001/flags/teams/x", ADMIN)).status).toBe(404);
    expect((await call("GET", "/admin/users/1001/sessions", ADMIN)).status).toBe(404);
    expect((await call("PUT", "/admin/users/1001/sessions", ADMIN)).status).toBe(404);
    expect((await call("DELETE", "/admin/users/1001/sessions/x", ADMIN)).status).toBe(404);
    // Without the secret, the paths that exist are not told apart from the rest.
    expect((await call("GET", "/admin/nothing")).status).toBe(401);
  });
});

describe("flags", () => {
  it("are granted and revoked, and /me follows", async () => {
    const got = await signIn(1001, "octocat");
    const grant = await call("PUT", "/admin/users/1001/flags/teams", ADMIN);
    expect(grant.status).toBe(200);
    expect(await grant.json()).toEqual({ github_id: 1001, flags: ["teams"] });
    // Granting twice changes nothing.
    expect(await (await call("PUT", "/admin/users/1001/flags/teams", ADMIN)).json()).toEqual({ github_id: 1001, flags: ["teams"] });
    expect(await (await call("GET", "/me", as(got.session))).json()).toEqual({ github_id: 1001, login: "octocat", flags: ["teams"], plan: "free", notices: [] });

    const revoke = await call("DELETE", "/admin/users/1001/flags/teams", ADMIN);
    expect(revoke.status).toBe(200);
    expect(await revoke.json()).toEqual({ github_id: 1001, flags: [] });
    expect(await (await call("GET", "/me", as(got.session))).json()).toEqual({ github_id: 1001, login: "octocat", flags: [], plan: "free", notices: [] });
    // Revoking one the user does not have is fine too.
    expect((await call("DELETE", "/admin/users/1001/flags/teams", ADMIN)).status).toBe(200);
  });

  it("only take names that are a lower-case letter, then up to 31 of a-z, 0-9 and _", async () => {
    for (const flag of ["teams", "t", "beta_2", "a" + "b".repeat(31)]) {
      expect((await call("PUT", `/admin/users/1001/flags/${flag}`, ADMIN)).status, flag).toBe(200);
    }
    for (const flag of ["Teams", "2fa", "_x", "a-b", "a.b", "a%20b", "é", "a" + "b".repeat(32)]) {
      const res = await call("PUT", `/admin/users/1001/flags/${flag}`, ADMIN);
      expect(res.status, flag).toBe(400);
      expect(await res.json()).toEqual({ error: expect.any(String) });
    }
  });

  it("only take a positive integer GitHub ID", async () => {
    for (const id of ["0", "-1", "01", "1.5", "abc", "1e3", "99999999999999999"]) {
      expect((await call("PUT", `/admin/users/${id}/flags/teams`, ADMIN)).status, id).toBe(400);
    }
  });

  it("can be granted before the first sign-in, which then fills in the user", async () => {
    const grant = await call("PUT", "/admin/users/2002/flags/teams", ADMIN);
    expect(await grant.json()).toEqual({ github_id: 2002, flags: ["teams"] });
    const placeholder = await env.DB.prepare("SELECT login, last_seen_at FROM users WHERE github_id = 2002").first();
    expect(placeholder).toEqual({ login: "", last_seen_at: 0 });

    const got = await signIn(2002, "newcomer");
    expect(got.flags).toEqual(["teams"]);
    expect(got.login).toBe("newcomer");
    const user = await env.DB.prepare("SELECT login, created_at, last_seen_at FROM users WHERE github_id = 2002").first<{
      login: string;
      created_at: number;
      last_seen_at: number;
    }>();
    expect(user?.login).toBe("newcomer");
    expect(user?.last_seen_at).toBeGreaterThan(0);
    expect(await (await call("GET", "/me", as(got.session))).json()).toEqual({ github_id: 2002, login: "newcomer", flags: ["teams"], plan: "free", notices: [] });
  });

  it("belong to one user only", async () => {
    const grant = await call("PUT", "/admin/users/1001/flags/teams", ADMIN);
    expect(grant.status).toBe(200);
    expect(await grant.json()).toEqual({ github_id: 1001, flags: ["teams"] });
    const other = await signIn(3003, "someone-else");
    expect(other.flags).toEqual([]);
    expect(await (await call("GET", "/me", as(other.session))).json()).toEqual({ github_id: 3003, login: "someone-else", flags: [], plan: "free", notices: [] });
  });
});

describe("closing sessions", () => {
  it("ends all of one user's sessions, and no one else's", async () => {
    const one = await signIn(1001, "octocat");
    const two = await signIn(1001, "octocat");
    const other = await signIn(3003, "someone-else");
    const res = await call("DELETE", "/admin/users/1001/sessions", ADMIN);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ github_id: 1001, closed: 2 });
    expect((await call("GET", "/me", as(one.session))).status).toBe(401);
    expect((await call("GET", "/me", as(two.session))).status).toBe(401);
    expect((await call("GET", "/me", as(other.session))).status).toBe(200);
    // Nothing left to close is fine too, and the user can sign in again.
    expect(await (await call("DELETE", "/admin/users/1001/sessions", ADMIN)).json()).toEqual({ github_id: 1001, closed: 0 });
    const again = await signIn(1001, "octocat");
    expect((await call("GET", "/me", as(again.session))).status).toBe(200);
  });

  it("needs the secret and a positive integer GitHub ID", async () => {
    const got = await signIn(1001, "octocat");
    expect((await call("DELETE", "/admin/users/1001/sessions")).status).toBe(401);
    expect((await call("DELETE", "/admin/users/1001/sessions", as(got.session))).status).toBe(401);
    for (const id of ["0", "-1", "01", "abc", "99999999999999999"]) {
      expect((await call("DELETE", `/admin/users/${id}/sessions`, ADMIN)).status, id).toBe(400);
    }
    expect((await call("GET", "/me", as(got.session))).status).toBe(200);
  });
});

describe("without ADMIN_SECRET", () => {
  it("there is no admin at all", async () => {
    const unset = { ...env, ADMIN_SECRET: undefined };
    for (const authorization of ["Bearer test-secret", "Bearer ", "Bearer undefined"]) {
      const req = new Request("https://teams.test/admin/users/1001/flags/teams", { method: "PUT", headers: { Authorization: authorization } });
      expect((await admin(req, unset, "/admin/users/1001/flags/teams")).status, authorization).toBe(401);
    }
  });
});
