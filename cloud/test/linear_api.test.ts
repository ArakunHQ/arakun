import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { LinearRefused, addLinks, asApp, exchangeCode, issuePlace, postActivity, saveWorkspace, viewer } from "../src/linear_api";
import { linearFake, nowSeconds, signIn } from "./helpers";

const ORG = "11111111-1111-4111-8111-111111111111";
const SESSION = "22222222-2222-4222-8222-222222222222";
const ISSUE = "33333333-3333-4333-8333-333333333333";
const TEAM = "44444444-4444-4444-8444-444444444444";
const who = { userId: "app-user", organizationId: ORG, organizationName: "Acme" };

type Workspace = Parameters<typeof saveWorkspace>[1];

/// Saves a workspace the fake Linear knows the tokens of. The installer is
/// signed in first: the row refers to the user.
async function workspace(overrides: Partial<Workspace>): Promise<Workspace> {
  await signIn(1, "ada");
  const w: Workspace = {
    organizationId: ORG,
    name: "Acme",
    appUserId: "app-user",
    access: "A1",
    refresh: "R1",
    expiresAt: nowSeconds() + 3600,
    installedBy: 1,
    ...overrides,
  };
  linearFake.tokens.set(w.access, who);
  linearFake.refreshTokens.add(w.refresh);
  return w;
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
    expect(await viewer("lin_oauth_t")).toEqual({ userId: "app-user", organizationId: ORG, organizationName: "Acme" });
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
    await save({ access: "A1", refresh: "R1" });
    const row = await env.DB.prepare("SELECT access_token, refresh_token FROM linear_workspaces").first<{ access_token: string; refresh_token: string }>();
    expect(row!.access_token.startsWith("v1.")).toBe(true);
    expect(JSON.stringify(row)).not.toMatch(/A1|R1/);
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
  });
  it("refreshes and retries after a 401", async () => {
    await save({});
    linearFake.expireNext = true;
    await postActivity(env, ORG, SESSION, "thought", "hi");
    expect(linearFake.activities()).toHaveLength(1);
    expect(linearFake.exchanges).toHaveLength(1);
  });
  it("gives up after a second 401", async () => {
    await save({});
    linearFake.expireAlways = true;
    await expect(postActivity(env, ORG, SESSION, "thought", "hi")).rejects.toMatchObject({ status: 401 });
    expect(linearFake.exchanges).toHaveLength(1);
  });
  it("two refreshes at once leave one valid pair", async () => {
    await save({ expiresAt: nowSeconds() + 60 });
    await Promise.all([postActivity(env, ORG, SESSION, "thought", "a"), postActivity(env, ORG, SESSION, "thought", "b")]);
    expect(linearFake.activities()).toHaveLength(2);
    await postActivity(env, ORG, SESSION, "thought", "c"); // the stored pair still works
    expect(linearFake.activities()).toHaveLength(3);
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
  it("adds links to an agent session", async () => {
    await save({});
    await addLinks(env, ORG, SESSION, [{ label: "Arakun", url: "https://example.com/x" }]);
    expect(linearFake.links()).toEqual([{ agentSessionId: SESSION, label: "Arakun", url: "https://example.com/x" }]);
  });
});
