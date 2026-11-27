// Runs before each test file: the tables from migrations/ and the relay's KV,
// emptied before each test, and GitHub replaced by the fake in helpers.ts.
import { applyD1Migrations, env } from "cloudflare:test";
import { afterEach, beforeEach, vi } from "vitest";
import { fakeGitHub } from "./helpers";

await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);

beforeEach(async () => {
  await env.DB.batch([
    env.DB.prepare("DELETE FROM linear_deliveries"),
    env.DB.prepare("DELETE FROM linear_routes"),
    env.DB.prepare("DELETE FROM linear_links"),
    env.DB.prepare("DELETE FROM linear_workspaces"),
    env.DB.prepare("DELETE FROM operator_totp"),
    env.DB.prepare("DELETE FROM admin_log"),
    env.DB.prepare("DELETE FROM beta_signups"),
    env.DB.prepare("DELETE FROM cloud_access_requests"),
    env.DB.prepare("DELETE FROM notice_reads"),
    env.DB.prepare("DELETE FROM notices"),
    env.DB.prepare("DELETE FROM cloud_usage"),
    env.DB.prepare("DELETE FROM cloud_holds"),
    env.DB.prepare("DELETE FROM cloud_machines"),
    env.DB.prepare("DELETE FROM machines"),
    env.DB.prepare("DELETE FROM envelopes"),
    env.DB.prepare("DELETE FROM channels"),
    env.DB.prepare("DELETE FROM invites"),
    env.DB.prepare("DELETE FROM pending"),
    env.DB.prepare("DELETE FROM members"),
    env.DB.prepare("DELETE FROM teams"),
    env.DB.prepare("DELETE FROM devices"),
    env.DB.prepare("DELETE FROM sessions"),
    env.DB.prepare("DELETE FROM user_flags"),
    env.DB.prepare("DELETE FROM identities"),
    env.DB.prepare("DELETE FROM accounts"),
  ]);
  // The relay's switches, plans and cards: files share storage, so a plan one
  // test set must not limit the phones of the next.
  const keys = await env.CONFIG.list();
  for (const key of keys.keys) await env.CONFIG.delete(key.name);
  fakeGitHub();
});

afterEach(() => {
  vi.restoreAllMocks();
});
