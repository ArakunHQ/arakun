// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { as, freshIp, github, lastCode, mail, onGitHub, signIn, signInByEmail, verifyCode } from "./helpers";

function post(session: string, path: string, body: object, headers: Record<string, string> = {}): Promise<Response> {
  return SELF.fetch(`https://teams.test${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json", "CF-Connecting-IP": freshIp(), ...as(session), ...headers },
    body: JSON.stringify(body),
  });
}

function list(session: string): Promise<Response> {
  return SELF.fetch("https://teams.test/me/sign-in", { headers: as(session) });
}

function unlink(session: string, provider: string): Promise<Response> {
  return SELF.fetch(`https://teams.test/me/sign-in/${provider}`, { method: "DELETE", headers: as(session) });
}

/// A GitHub token the fake GitHub knows, for a user it has not seen sign in.
function githubToken(id: number, login: string): string {
  const token = `gho_link_${id}`;
  github.users.set(token, { id, login });
  onGitHub(id, login);
  return token;
}

async function linkAddress(session: string, address: string): Promise<Response> {
  const asked = await post(session, "/me/sign-in/email", { email: address });
  expect(asked.status).toBe(200);
  return post(session, "/me/sign-in/email/verify", { email: address, code: lastCode(address) });
}

async function changes(accountId: number) {
  const rows = await env.DB.prepare("SELECT action, provider, subject, session_id FROM sign_in_changes WHERE account_id = ? ORDER BY id").bind(accountId).all();
  return rows.results;
}

describe("listing sign-in methods", () => {
  it("lists GitHub by its login and an address by itself", async () => {
    const gh = await signIn(1001, "octocat");
    const res = await list(gh.session);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ methods: [{ provider: "github", name: "octocat", linked_at: expect.any(Number) }] });

    const em = await signInByEmail("ada@example.com");
    expect(await (await list(em.session)).json()).toEqual({ methods: [{ provider: "email", name: "ada@example.com", linked_at: expect.any(Number) }] });
  });

  it("needs a session", async () => {
    expect((await SELF.fetch("https://teams.test/me/sign-in")).status).toBe(401);
    expect((await SELF.fetch("https://teams.test/me/sign-in/github", { method: "DELETE" })).status).toBe(401);
  });

  it("answers 404 for what is not a route", async () => {
    const gh = await signIn(1001, "octocat");
    expect((await unlink(gh.session, "google")).status).toBe(404);
    expect((await SELF.fetch("https://teams.test/me/sign-in/other", { headers: as(gh.session) })).status).toBe(404);
  });
});

describe("adding an address", () => {
  it("mails a code that adds the address, records it and tells every address", async () => {
    github.emails = [{ email: "Octo@GitHub.example", primary: true, verified: true }];
    const gh = await signIn(1001, "octocat");
    const asked = await post(gh.session, "/me/sign-in/email", { email: "ada@example.com" });
    expect(asked.status).toBe(200);
    const sent = mail.sent.at(-1)!;
    expect(sent.subject).toMatch(/is your code to add this address to Arakun$/);
    expect(sent.html).toContain(lastCode("ada@example.com"));

    const res = await post(gh.session, "/me/sign-in/email/verify", { email: "ada@example.com", code: lastCode("ada@example.com") });
    expect(res.status).toBe(200);
    const { methods } = await res.json<{ methods: { provider: string; name: string }[] }>();
    expect(methods.map((m) => [m.provider, m.name])).toEqual([
      ["github", "octocat"],
      ["email", "ada@example.com"],
    ]);
    expect(await changes(gh.account_id)).toEqual([{ action: "link", provider: "email", subject: "ada@example.com", session_id: gh.session_id }]);
    const told = mail.sent.filter((m) => m.subject === "A sign-in method was added to your Arakun account").map((m) => m.to);
    expect(told.sort()).toEqual(["ada@example.com", "octo@github.example"]);

    // The address signs in to the same account now.
    const again = await signInByEmail("ada@example.com");
    expect(again.account_id).toBe(gh.account_id);
  });

  it("does not take a sign-in code, nor sign in with an adding code", async () => {
    const gh = await signIn(1001, "octocat");
    const asked = await SELF.fetch("https://teams.test/auth/email", {
      method: "POST",
      headers: { "Content-Type": "application/json", "CF-Connecting-IP": freshIp() },
      body: JSON.stringify({ email: "ada@example.com" }),
    });
    expect(asked.status).toBe(200);
    const res = await post(gh.session, "/me/sign-in/email/verify", { email: "ada@example.com", code: lastCode("ada@example.com") });
    expect(res.status).toBe(401);

    await post(gh.session, "/me/sign-in/email", { email: "ada@example.com" });
    expect((await verifyCode("ada@example.com", lastCode("ada@example.com"))).status).toBe(401);
  });

  it("does not take another account's code", async () => {
    const one = await signIn(1001, "octocat");
    const two = await signIn(1002, "hubot");
    await post(one.session, "/me/sign-in/email", { email: "ada@example.com" });
    const res = await post(two.session, "/me/sign-in/email/verify", { email: "ada@example.com", code: lastCode("ada@example.com") });
    expect(res.status).toBe(401);
  });

  it("never joins the account of the address, even with a right code", async () => {
    const em = await signInByEmail("ada@example.com");
    const gh = await signIn(1001, "octocat");
    const res = await linkAddress(gh.session, "ada@example.com");
    expect(res.status).toBe(409);
    expect((await res.json<{ error: string }>()).error).toMatch(/another Arakun account; accounts are never joined/);
    const owner = await env.DB.prepare("SELECT account_id FROM identities WHERE provider = 'email'").first("account_id");
    expect(owner).toBe(em.account_id);
    expect(await changes(gh.account_id)).toEqual([]);
  });

  it("refuses a second address", async () => {
    const em = await signInByEmail("ada@example.com");
    const res = await linkAddress(em.session, "bob@example.com");
    expect(res.status).toBe(409);
    expect((await res.json<{ error: string }>()).error).toMatch(/has an address already/);
  });

  it("refuses throwaway addresses", async () => {
    const gh = await signIn(1001, "octocat");
    const res = await post(gh.session, "/me/sign-in/email", { email: "x@mailinator.com" });
    expect(res.status).toBe(400);
  });
});

describe("adding GitHub", () => {
  it("links the GitHub account of the token, takes its login and revokes the token", async () => {
    const em = await signInByEmail("ada@example.com");
    const token = githubToken(2002, "ada-gh");
    const res = await post(em.session, "/me/sign-in/github", { access_token: token });
    expect(res.status).toBe(200);
    const { methods } = await res.json<{ methods: { provider: string; name: string }[] }>();
    expect(methods.map((m) => [m.provider, m.name])).toEqual([
      ["github", "ada-gh"],
      ["email", "ada@example.com"],
    ]);
    // Revoked: the fake forgets a token once it is.
    expect(github.users.has(token)).toBe(false);
    expect(await changes(em.account_id)).toEqual([{ action: "link", provider: "github", subject: "2002", session_id: em.session_id }]);
    expect(mail.sent.at(-1)).toMatchObject({ to: "ada@example.com", subject: "A sign-in method was added to your Arakun account" });
    expect(mail.sent.at(-1)!.text).toContain("The GitHub account ada-gh was added");

    const again = await signIn(2002, "ada-gh");
    expect(again.account_id).toBe(em.account_id);
  });

  it("never joins another account that GitHub signs in to", async () => {
    const gh = await signIn(1001, "octocat");
    const em = await signInByEmail("ada@example.com");
    const res = await post(em.session, "/me/sign-in/github", { access_token: githubToken(1001, "octocat") });
    expect(res.status).toBe(409);
    expect((await res.json<{ error: string }>()).error).toMatch(/another Arakun account/);
    const owner = await env.DB.prepare("SELECT account_id FROM identities WHERE provider = 'github'").first("account_id");
    expect(owner).toBe(gh.account_id);
  });

  it("answers with the list when the same GitHub account is linked already", async () => {
    const gh = await signIn(1001, "octocat");
    const res = await post(gh.session, "/me/sign-in/github", { access_token: githubToken(1001, "octocat") });
    expect(res.status).toBe(200);
    expect(await changes(gh.account_id)).toEqual([]);
  });

  it("refuses a second GitHub account", async () => {
    const gh = await signIn(1001, "octocat");
    const res = await post(gh.session, "/me/sign-in/github", { access_token: githubToken(1002, "hubot") });
    expect(res.status).toBe(409);
    expect((await res.json<{ error: string }>()).error).toMatch(/has a GitHub account already/);
  });

  it("passes on GitHub's refusal", async () => {
    const em = await signInByEmail("ada@example.com");
    expect((await post(em.session, "/me/sign-in/github", { access_token: "gho_unknown" })).status).toBe(401);
    expect((await post(em.session, "/me/sign-in/github", {})).status).toBe(400);
  });
});

describe("removing a sign-in method", () => {
  it("removes one of two, records it and tells the address removed too", async () => {
    const gh = await signIn(1001, "octocat");
    await linkAddress(gh.session, "ada@example.com");
    mail.sent = [];
    const res = await unlink(gh.session, "email");
    expect(res.status).toBe(200);
    expect((await res.json<{ methods: { provider: string }[] }>()).methods.map((m) => m.provider)).toEqual(["github"]);
    expect((await changes(gh.account_id)).at(-1)).toEqual({ action: "unlink", provider: "email", subject: "ada@example.com", session_id: gh.session_id });
    expect(mail.sent.map((m) => [m.to, m.subject])).toEqual([["ada@example.com", "A sign-in method was removed from your Arakun account"]]);
  });

  it("shows the address once GitHub is gone", async () => {
    const em = await signInByEmail("ada@example.com");
    await post(em.session, "/me/sign-in/github", { access_token: githubToken(2002, "ada-gh") });
    expect((await unlink(em.session, "github")).status).toBe(200);
    const login = await env.DB.prepare("SELECT login FROM accounts WHERE id = ?").bind(em.account_id).first("login");
    expect(login).toBe("ada@example.com");
    // GitHub signs in to a new account now, not this one.
    const again = await signIn(2002, "ada-gh");
    expect(again.account_id).not.toBe(em.account_id);
  });

  it("keeps the last one", async () => {
    const gh = await signIn(1001, "octocat");
    const res = await unlink(gh.session, "github");
    expect(res.status).toBe(409);
    expect((await res.json<{ error: string }>()).error).toMatch(/last sign-in method/);
    expect(await changes(gh.account_id)).toEqual([]);
    expect((await unlink(gh.session, "email")).status).toBe(404);
  });

  it("keeps one when two removals come at once", async () => {
    const gh = await signIn(1001, "octocat");
    await linkAddress(gh.session, "ada@example.com");
    const both = await Promise.all([unlink(gh.session, "github"), unlink(gh.session, "email")]);
    expect(both.map((r) => r.status).sort()).toEqual([200, 409]);
    const left = await env.DB.prepare("SELECT count(*) AS n FROM identities WHERE account_id = ?").bind(gh.account_id).first("n");
    expect(left).toBe(1);
  });

  it("is refused to a blocked account", async () => {
    const gh = await signIn(1001, "octocat");
    await linkAddress(gh.session, "ada@example.com");
    await env.DB.prepare("UPDATE accounts SET blocked_at = 1 WHERE id = ?").bind(gh.account_id).run();
    expect((await unlink(gh.session, "email")).status).toBe(403);
  });

  it("is refused to a cookie from another origin", async () => {
    const gh = await signIn(1001, "octocat");
    const res = await SELF.fetch("https://teams.test/me/sign-in/github", {
      method: "DELETE",
      headers: { Cookie: `__Host-arakun_session=${gh.session}`, Origin: "https://evil.test" },
    });
    expect(res.status).toBe(403);
  });
});
