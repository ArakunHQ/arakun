// Runs before each test file: the tables from migrations/, emptied before each
// test, and GitHub replaced by the fake in helpers.ts.
import { applyD1Migrations, env } from "cloudflare:test";
import { afterEach, beforeEach, vi } from "vitest";
import { fakeGitHub } from "./helpers";

await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);

beforeEach(async () => {
  await env.DB.batch([
    env.DB.prepare("DELETE FROM envelopes"),
    env.DB.prepare("DELETE FROM channels"),
    env.DB.prepare("DELETE FROM invites"),
    env.DB.prepare("DELETE FROM pending"),
    env.DB.prepare("DELETE FROM members"),
    env.DB.prepare("DELETE FROM teams"),
    env.DB.prepare("DELETE FROM devices"),
    env.DB.prepare("DELETE FROM sessions"),
    env.DB.prepare("DELETE FROM user_flags"),
    env.DB.prepare("DELETE FROM users"),
  ]);
  fakeGitHub();
});

afterEach(() => {
  vi.restoreAllMocks();
});
