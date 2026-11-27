import { createExecutionContext, createScheduledController, env, waitOnExecutionContext } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker from "../src/index";
import { saveWorkspace } from "../src/linear_api";
import { KEEP_SECONDS, keepWorkspaces } from "../src/linear_keep";
import { open } from "../src/linear_seal";
import { linearFake, nowSeconds, signIn } from "./helpers";

const QUIET = "11111111-1111-4111-8111-111111111111";
const BUSY = "11111111-1111-4111-8111-222222222222";
const who = (org: string) => ({ userId: "55555555-5555-4555-8555-555555555555", organizationId: org, organizationName: "Acme", app: true });

/// A workspace whose pair the fake Linear takes, last refreshed `age` seconds ago.
async function workspace(org: string, age: number): Promise<void> {
  const ada = await signIn(1, "ada");
  const access = `lin_oauth_${org}`;
  const refresh = `lin_refresh_${org}`;
  linearFake.tokens.set(access, who(org));
  linearFake.refreshTokens.set(refresh, access);
  await saveWorkspace(env, {
    organizationId: org,
    name: "Acme",
    appUserId: "app-user",
    access,
    refresh,
    expiresAt: nowSeconds() + 86_400,
    installedBy: ada.account_id,
  });
  await env.DB.prepare("UPDATE linear_workspaces SET refreshed_at = ? WHERE organization_id = ?")
    .bind(nowSeconds() - age, org)
    .run();
}

async function row(org: string): Promise<{ refresh: string; refreshed_at: number; refused_at: number | null }> {
  const r = await env.DB.prepare("SELECT refresh_token, refreshed_at, refused_at FROM linear_workspaces WHERE organization_id = ?")
    .bind(org)
    .first<{ refresh_token: string; refreshed_at: number; refused_at: number | null }>();
  return { refresh: (await open(env, org, r!.refresh_token))!, refreshed_at: r!.refreshed_at, refused_at: r!.refused_at };
}

const refreshes = () => linearFake.exchanges.filter((f) => f.get("grant_type") === "refresh_token").map((f) => f.get("refresh_token"));

describe("keeping quiet Linear workspaces installed", () => {
  it("refreshes a workspace quiet for a week, and leaves a busy one", async () => {
    await workspace(QUIET, KEEP_SECONDS + 60);
    await workspace(BUSY, 3600);
    await keepWorkspaces(env);
    expect(refreshes()).toEqual([`lin_refresh_${QUIET}`]);
    const quiet = await row(QUIET);
    expect(quiet.refresh).toMatch(/^lin_refresh_issued/);
    expect(quiet.refreshed_at).toBeGreaterThanOrEqual(nowSeconds() - 5);
    expect(quiet.refused_at).toBeNull();
    expect((await row(BUSY)).refresh).toBe(`lin_refresh_${BUSY}`);
    // Refreshed now, so the next run leaves it.
    await keepWorkspaces(env);
    expect(refreshes()).toHaveLength(1);
  });

  it("a saved workspace counts as refreshed when it is saved", async () => {
    await workspace(QUIET, 0);
    await env.DB.prepare("UPDATE linear_workspaces SET refreshed_at = 0").run();
    await workspace(QUIET, 0);
    expect((await row(QUIET)).refreshed_at).toBeGreaterThanOrEqual(nowSeconds() - 5);
  });

  it("marks a workspace whose refresh token Linear refuses, and goes on to the next", async () => {
    await workspace(QUIET, KEEP_SECONDS + 120);
    await workspace(BUSY, KEEP_SECONDS + 60);
    // The app was removed from QUIET: Linear no longer takes its tokens.
    linearFake.refreshTokens.delete(`lin_refresh_${QUIET}`);
    await keepWorkspaces(env);
    expect((await row(QUIET)).refused_at).toBeGreaterThanOrEqual(nowSeconds() - 5);
    expect((await row(BUSY)).refused_at).toBeNull();
    expect((await row(BUSY)).refresh).toMatch(/^lin_refresh_issued/);
    // A refused workspace is not asked again; installing again clears the mark.
    await env.DB.prepare("UPDATE linear_workspaces SET refreshed_at = 0").run();
    await keepWorkspaces(env);
    expect(refreshes().filter((r) => r === `lin_refresh_${QUIET}`)).toHaveLength(1);
    await workspace(QUIET, 0);
    expect((await row(QUIET)).refused_at).toBeNull();
  });

  it("does not mark a workspace when Linear is down or fails", async () => {
    await workspace(QUIET, KEEP_SECONDS + 60);
    linearFake.down = true;
    await keepWorkspaces(env);
    linearFake.down = false;
    linearFake.refuse = 503;
    await keepWorkspaces(env);
    expect(await row(QUIET)).toMatchObject({ refresh: `lin_refresh_${QUIET}`, refused_at: null });
  });

  it("does nothing without Linear's secrets", async () => {
    await workspace(QUIET, KEEP_SECONDS + 60);
    await keepWorkspaces({ ...env, LINEAR_CLIENT_ID: undefined } as unknown as typeof env);
    expect(refreshes()).toEqual([]);
  });

  it("runs from the Worker's cron trigger", async () => {
    await workspace(QUIET, KEEP_SECONDS + 60);
    const ctx = createExecutionContext();
    await worker.scheduled(createScheduledController({ cron: "17 4 * * *" }), env, ctx);
    await waitOnExecutionContext(ctx);
    expect(refreshes()).toEqual([`lin_refresh_${QUIET}`]);
  });
});
