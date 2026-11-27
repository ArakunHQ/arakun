// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { LinearRefused, addLinks, asApp, exchangeCode, issuePlace, postActivity, saveWorkspace, viewer } from "../src/linear_api";
import { open } from "../src/linear_seal";
import { linearFake, nowSeconds, signIn } from "./helpers";

const ORG = "11111111-1111-4111-8111-111111111111";
const SESSION = "22222222-2222-4222-8222-222222222222";
const ISSUE = "33333333-3333-4333-8333-333333333333";
const TEAM = "44444444-4444-4444-8444-444444444444";
const APP_USER = "55555555-5555-4555-8555-555555555555";
const who = { userId: APP_USER, organizationId: ORG, organizationName: "Acme", app: true };

type Workspace = Parameters<typeof saveWorkspace>[1];

/// Saves a workspace the fake Linear knows the tokens of. The installer is
/// signed in first: the row refers to the user.
async function workspace(overrides: Partial<Workspace>): Promise<Workspace> {
  const ada = await signIn(1, "ada");
  const w: Workspace = {
    organizationId: ORG,
    name: "Acme",
    appUserId: "app-user",
    access: "A1",
    refresh: "R1",
    expiresAt: nowSeconds() + 3600,
    installedBy: ada.account_id,
    ...overrides,
  };
  linearFake.tokens.set(w.access, who);
  linearFake.refreshTokens.set(w.refresh, w.access);
  return w;
}

/// The pair stored for the workspace, opened, and when it expires.
async function stored(): Promise<{ access: string; refresh: string; expiresAt: number }> {
  const row = await env.DB.prepare("SELECT access_token, refresh_token, expires_at FROM linear_workspaces").first<{
    access_token: string;
    refresh_token: string;
    expires_at: number;
  }>();
  return { access: (await open(env, ORG, row!.access_token))!, refresh: (await open(env, ORG, row!.refresh_token))!, expiresAt: row!.expires_at };
}

/// Saves the default workspace, with the overrides.
async function save(overrides: Partial<Workspace>): Promise<void> {
  await saveWorkspace(env, await workspace(overrides));
}

function authHeaders(): string[] {
  return linearFake.auth;
}

describe("Linear's API", () => {
  it("sends an API key bare and an OAuth token as Bearer", async () => {
    linearFake.tokens.set("lin_api_k", who);
    linearFake.tokens.set("lin_oauth_t", who);
    await viewer("lin_api_k");
    await viewer("lin_oauth_t");
    expect(authHeaders()).toEqual(["lin_api_k", "Bearer lin_oauth_t"]);
  });
  it("reads who a token is", async () => {
    linearFake.tokens.set("lin_oauth_t", who);
    expect(await viewer("lin_oauth_t")).toEqual({ userId: APP_USER, app: true, organizationId: ORG, organizationName: "Acme" });
    linearFake.tokens.set("lin_api_person", { ...who, app: false });
    expect(await viewer("lin_api_person")).toMatchObject({ app: false });
  });
  it("refuses a viewer whose ids are not UUIDs", async () => {
    linearFake.tokens.set("lin_oauth_u", { ...who, userId: "app-user" });
    linearFake.tokens.set("lin_oauth_o", { ...who, organizationId: "org" });
    await expect(viewer("lin_oauth_u")).rejects.toMatchObject({ status: 502 });
    await expect(viewer("lin_oauth_o")).rejects.toMatchObject({ status: 502 });
  });
  it("exchanges a code", async () => {
    const got = await exchangeCode(env, "code-1", "https://teams.test/linear/callback");
    const form = linearFake.exchanges[0]!;
    expect(form.get("grant_type")).toBe("authorization_code");
    expect(form.get("code")).toBe("code-1");
    expect(form.get("redirect_uri")).toBe("https://teams.test/linear/callback");
    expect(got.access.startsWith("lin_oauth_")).toBe(true);
    expect(got.expiresAt).toBeGreaterThan(nowSeconds() + 86000);
  });
  it("stores a workspace's tokens sealed", async () => {
    // Long tokens: a short one like "A1" turns up in random ciphertext by chance.
    await save({ access: "plain-access-token", refresh: "plain-refresh-token" });
    const row = await env.DB.prepare("SELECT access_token, refresh_token FROM linear_workspaces").first<{ access_token: string; refresh_token: string }>();
    expect(row!.access_token.startsWith("v1.")).toBe(true);
    expect(JSON.stringify(row)).not.toMatch(/plain-(access|refresh)-token/);
  });
  it("saving a workspace again replaces its tokens", async () => {
    await save({});
    await save({ access: "A2", refresh: "R2" });
    const count = await env.DB.prepare("SELECT COUNT(*) AS n FROM linear_workspaces").first<{ n: number }>();
    expect(count!.n).toBe(1);
    await postActivity(env, ORG, SESSION, "thought", "hi");
    expect(linearFake.calls.map((c) => c.token)).toEqual(["A2"]);
  });
  it("refreshes a token that is about to expire, once", async () => {
    await save({ expiresAt: nowSeconds() + 60 });
    await postActivity(env, ORG, SESSION, "thought", "hi");
    expect(linearFake.exchanges.map((e) => e.get("grant_type"))).toEqual(["refresh_token"]);
    expect(linearFake.activities()).toEqual([{ agentSessionId: SESSION, type: "thought", body: "hi" }]);
    // The new pair is stored and used; the old one is revoked.
    const now = await stored();
    expect(now.access).toBe("lin_oauth_issued1");
    expect(now.refresh).toBe("lin_refresh_issued1");
    expect(now.expiresAt).toBeGreaterThan(nowSeconds() + 86000);
    expect(linearFake.calls.at(-1)!.token).toBe("lin_oauth_issued1");
    expect(linearFake.tokens.has("A1")).toBe(false);
  });
  it("refreshes and retries after a 401", async () => {
    await save({});
    linearFake.expireNext = true;
    await postActivity(env, ORG, SESSION, "thought", "hi");
    expect(linearFake.activities()).toHaveLength(1);
    expect(linearFake.exchanges).toHaveLength(1);
    expect((await stored()).access).toBe("lin_oauth_issued1");
    expect(linearFake.calls.at(-1)!.token).toBe("lin_oauth_issued1");
  });
  it("gives up after a second 401", async () => {
    await save({});
    linearFake.expireAlways = true;
    await expect(postActivity(env, ORG, SESSION, "thought", "hi")).rejects.toMatchObject({ status: 401 });
    expect(linearFake.exchanges).toHaveLength(1);
  });
  it("two refreshes at once leave one valid pair", async () => {
    await save({ expiresAt: nowSeconds() + 60 });
    linearFake.grace = true;
    await Promise.all([postActivity(env, ORG, SESSION, "thought", "a"), postActivity(env, ORG, SESSION, "thought", "b")]);
    expect(linearFake.activities()).toHaveLength(2);
    await postActivity(env, ORG, SESSION, "thought", "c"); // the stored pair still works
    expect(linearFake.activities()).toHaveLength(3);
    // Both asked Linear, it answered one pair twice, and that pair is stored.
    expect(linearFake.exchanges).toHaveLength(2);
    linearFake.grace = false;
    const now = await stored();
    expect(linearFake.refreshTokens.has(now.refresh)).toBe(true);
    expect(linearFake.tokens.has(now.access)).toBe(true);
    expect(linearFake.calls.at(-1)!.token).toBe(now.access);
  });
  it("does not overwrite a pair another request stored first", async () => {
    await save({ expiresAt: nowSeconds() + 60 });
    // While our refresh is out, another request refreshes again and stores its pair.
    linearFake.afterRefresh = async (first) => {
      linearFake.afterRefresh = null;
      const res = await fetch("https://api.linear.app/oauth/token", {
        method: "POST",
        body: new URLSearchParams({ grant_type: "refresh_token", refresh_token: first.refresh, client_id: "lin-test", client_secret: "lin-secret" }),
      });
      const next = (await res.json()) as { access_token: string; refresh_token: string };
      await saveWorkspace(env, { ...(await workspace({ access: next.access_token, refresh: next.refresh_token })) });
    };
    await postActivity(env, ORG, SESSION, "thought", "hi");
    expect(linearFake.calls[0]!.token).toBe("lin_oauth_issued2");
    expect((await stored()).access).toBe("lin_oauth_issued2");
  });
  it("a rotated refresh token is refused without grace", async () => {
    await save({ expiresAt: nowSeconds() + 60 });
    await postActivity(env, ORG, SESSION, "thought", "a");
    linearFake.grace = false;
    const res = await fetch("https://api.linear.app/oauth/token", {
      method: "POST",
      body: new URLSearchParams({ grant_type: "refresh_token", refresh_token: "R1", client_id: "lin-test", client_secret: "lin-secret" }),
    });
    expect(res.status).toBe(400);
  });
  it("a refusal is LinearRefused, not a crash", async () => {
    await save({});
    linearFake.refuse = 500;
    await expect(postActivity(env, ORG, SESSION, "thought", "hi")).rejects.toBeInstanceOf(LinearRefused);
    await expect(postActivity(env, "no-such-org", SESSION, "thought", "hi")).rejects.toMatchObject({ status: 404 });
  });
  it("a 200 with errors is LinearRefused, and says no token", async () => {
    await save({});
    const err = (await asApp(env, ORG, "query($id: String!) { issue(id: $id) { team { id } } }", { id: "x" }).catch((e: unknown) => e)) as LinearRefused;
    expect(err).toBeInstanceOf(LinearRefused);
    expect(err.status).toBe(502);
    expect(err.message).not.toMatch(/A1|R1/);
  });
  it("reads an issue's team and project", async () => {
    await save({});
    linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: null });
    expect(await issuePlace(env, ORG, ISSUE)).toEqual({ teamId: TEAM, projectId: null });
    linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: "p-1" });
    expect(await issuePlace(env, ORG, ISSUE)).toEqual({ teamId: TEAM, projectId: "p-1" });
  });
  it("a viewer answer without its fields is LinearRefused", async () => {
    linearFake.tokens.set("lin_oauth_t", who);
    linearFake.malformed = "viewer";
    await expect(viewer("lin_oauth_t")).rejects.toMatchObject({ status: 502 });
  });
  it("an issue that is null is a 404, one without a team a 502", async () => {
    await save({});
    linearFake.malformed = "issue-null";
    await expect(issuePlace(env, ORG, ISSUE)).rejects.toMatchObject({ status: 404 });
    linearFake.malformed = "no-team";
    await expect(issuePlace(env, ORG, ISSUE)).rejects.toMatchObject({ status: 502 });
  });
  it("adds links to an agent session", async () => {
    await save({});
    await addLinks(env, ORG, SESSION, [{ label: "Arakun", url: "https://example.com/x" }]);
    expect(linearFake.links()).toEqual([{ agentSessionId: SESSION, label: "Arakun", url: "https://example.com/x" }]);
  });
});
