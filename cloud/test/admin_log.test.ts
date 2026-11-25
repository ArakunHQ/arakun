import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ADMIN, call } from "./helpers";

const OP = { ...ADMIN, "X-Arakun-Operator-Id": "1001", "X-Arakun-Operator-Login": "octocat" };

interface Entry {
  id: number;
  at: number;
  operator_id: number | null;
  operator_login: string | null;
  action: string;
  github_id: number | null;
  login: string | null;
  detail: Record<string, unknown> | null;
}
interface Page {
  entries: Entry[];
  next: number | null;
}

async function log(query = ""): Promise<Page> {
  const res = await call("GET", `/admin/log${query}`, ADMIN);
  expect(res.status).toBe(200);
  return res.json();
}

async function ask(id: number, status = "pending"): Promise<void> {
  await env.DB.prepare("INSERT OR IGNORE INTO users (github_id, login, created_at, last_seen_at) VALUES (?1, 'two', 1, 0)").bind(id).run();
  await env.DB.prepare(
    `INSERT INTO cloud_access_requests (github_id, status, requested_at, decided_at, gh_login) VALUES (?1, ?2, 1, ?3, 'two')
     ON CONFLICT (github_id) DO UPDATE SET status = excluded.status, decided_at = excluded.decided_at`,
  )
    .bind(id, status, status === "pending" ? null : 2)
    .run();
}

describe("the admin log", () => {
  it("records each change with the operator", async () => {
    const base = "/admin/users/2002";
    expect((await call("PUT", `${base}/flags/teams`, OP)).status).toBe(200);
    expect((await call("DELETE", `${base}/flags/teams`, OP)).status).toBe(200);
    expect((await call("DELETE", `${base}/sessions`, OP)).status).toBe(200);
    expect((await call("PUT", `${base}/block`, OP, JSON.stringify({ reason: "spam" }))).status).toBe(204);
    expect((await call("DELETE", `${base}/block`, OP)).status).toBe(204);
    expect((await call("PUT", `${base}/plan`, OP, JSON.stringify({ plan: "pro" }))).status).toBe(204);
    const made = await call("POST", "/admin/notices", OP, JSON.stringify({ github_id: 2002, text: "hello" }));
    const { id: noticeId } = (await made.json()) as { id: string };
    expect((await call("DELETE", `/admin/notices/${noticeId}`, OP)).status).toBe(204);
    await ask(2002);
    expect((await call("PUT", "/admin/cloud/requests/2002/grant", OP)).status).toBe(200);
    await ask(2002);
    expect((await call("PUT", "/admin/cloud/requests/2002/decline", OP)).status).toBe(200);

    const { entries, next } = await log();
    expect(next).toBeNull();
    expect(entries.map((e) => e.action)).toEqual([
      "cloud.decline",
      "cloud.grant",
      "notice.delete",
      "notice.create",
      "plan.set",
      "unblock",
      "block",
      "sessions.close",
      "flag.revoke",
      "flag.grant",
    ]);
    for (const e of entries) {
      expect(e.operator_id).toBe(1001);
      expect(e.operator_login).toBe("octocat");
      expect(e.github_id).toBe(2002);
      // The flag route made the account before it signed in, so it has no login yet.
      expect(e.login).toBe("");
      expect(e.at).toBeGreaterThan(0);
    }
    const detail = Object.fromEntries(entries.map((e) => [e.action, e.detail]));
    expect(detail).toEqual({
      "flag.grant": { flag: "teams" },
      "flag.revoke": { flag: "teams" },
      "sessions.close": { closed: 0 },
      block: { reason: "spam" },
      unblock: null,
      "plan.set": { plan: "pro" },
      "notice.create": { notice_id: noticeId },
      "notice.delete": { notice_id: noticeId },
      "cloud.grant": null,
      "cloud.decline": null,
    });
  });

  it("records a call without operator headers with a null operator", async () => {
    await call("PUT", "/admin/users/2002/flags/teams", ADMIN);
    const { entries } = await log();
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({ operator_id: null, operator_login: null, action: "flag.grant" });
  });

  it("does not take one operator header without the other", async () => {
    const cases = [
      { ...ADMIN, "X-Arakun-Operator-Id": "1001" },
      { ...ADMIN, "X-Arakun-Operator-Login": "octocat" },
      { ...ADMIN, "X-Arakun-Operator-Id": "abc", "X-Arakun-Operator-Login": "octocat" },
    ];
    for (const headers of cases) await call("PUT", "/admin/users/2002/flags/teams", headers);
    const { entries } = await log();
    expect(entries).toHaveLength(3);
    for (const e of entries) expect([e.operator_id, e.operator_login]).toEqual([null, null]);
  });

  it("records nothing for a refused change", async () => {
    await ask(2002, "granted");
    expect((await call("PUT", "/admin/cloud/requests/2002/decline", OP)).status).toBe(409);
    expect((await call("PUT", "/admin/users/2002/plan", OP, JSON.stringify({ plan: "Bad Plan" }))).status).toBe(400);
    expect((await call("DELETE", "/admin/notices/nope", OP)).status).toBe(404);
    expect((await log()).entries).toEqual([]);
  });

  it("a second grant records nothing new", async () => {
    await ask(2002);
    expect((await call("PUT", "/admin/cloud/requests/2002/grant", OP)).status).toBe(200);
    expect((await call("PUT", "/admin/cloud/requests/2002/grant", OP)).status).toBe(200);
    expect((await log()).entries.filter((e) => e.action === "cloud.grant")).toHaveLength(1);
  });

  it("records a grant that only restores the flag", async () => {
    await ask(2002);
    await call("PUT", "/admin/cloud/requests/2002/grant", OP);
    expect((await call("DELETE", "/admin/users/2002/flags/cloud", OP)).status).toBe(200);
    expect((await call("PUT", "/admin/cloud/requests/2002/grant", OP)).status).toBe(200);
    expect((await log()).entries.filter((e) => e.action === "cloud.grant")).toHaveLength(2);
  });

  it("records a notice for everyone with no account", async () => {
    await call("POST", "/admin/notices", OP, JSON.stringify({ text: "hello" }));
    expect((await log()).entries[0]).toMatchObject({ action: "notice.create", github_id: null });
  });

  it("counts the sessions it closed", async () => {
    const now = Math.floor(Date.now() / 1000);
    await env.DB.prepare("INSERT INTO users (github_id, login, created_at, last_seen_at) VALUES (2002, 'two', 1, 0)").run();
    await env.DB.batch(
      ["a", "b"].map((id) =>
        env.DB.prepare("INSERT INTO sessions (token_hash, github_id, created_at, expires_at, last_used_at) VALUES (?, 2002, ?, ?, ?)").bind(id, now, now + 1000, now),
      ),
    );
    await call("DELETE", "/admin/users/2002/sessions", OP);
    expect((await log()).entries[0]).toMatchObject({ action: "sessions.close", detail: { closed: 2 } });
  });

  it("filters by account and pages", async () => {
    const rows = Array.from({ length: 60 }, (_, i) => env.DB.prepare("INSERT INTO admin_log (at, action, github_id) VALUES (?, 'plan.set', ?)").bind(i + 1, i < 5 ? 2002 : 3003));
    await env.DB.batch(rows);
    const first = await log();
    expect(first.entries).toHaveLength(50);
    expect(first.next).toBe(first.entries[49]!.id);
    const rest = await log(`?before=${first.next}`);
    expect(rest.entries).toHaveLength(10);
    expect(rest.next).toBeNull();
    const mine = await log("?github_id=2002");
    expect(mine.entries).toHaveLength(5);
    expect(mine.entries.every((e) => e.github_id === 2002)).toBe(true);
    expect((await log("?limit=10")).entries).toHaveLength(10);
    expect((await call("GET", "/admin/log?limit=101", ADMIN)).status).toBe(400);
    expect((await call("GET", "/admin/log?before=x", ADMIN)).status).toBe(400);
    expect((await call("GET", "/admin/log?github_id=x", ADMIN)).status).toBe(400);
  });

  it("needs the secret", async () => {
    const res = await call("GET", "/admin/log", { "X-Arakun-Operator-Id": "1001", "X-Arakun-Operator-Login": "octocat" });
    expect(res.status).toBe(401);
  });

  it("has no route that changes it", async () => {
    expect((await call("DELETE", "/admin/log", ADMIN)).status).toBe(404);
    expect((await call("PUT", "/admin/log/1", ADMIN)).status).toBe(404);
  });
});
