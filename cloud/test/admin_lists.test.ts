// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ADMIN, call, signIn } from "./helpers";

interface User {
  account_id: number;
  /// The linked GitHub user, or null for an account without one.
  github_id: number | null;
  login: string;
  plan: string;
  blocked_at: number | null;
  blocked_reason: string | null;
  created_at: number;
  last_seen_at: number;
  flags: string[];
  machines: number;
}
interface Page {
  users: User[];
  next: string | null;
}

/// An account with no identity, or linked to the GitHub user `githubId`.
async function add(id: number, login: string, lastSeen = 100, githubId?: number): Promise<void> {
  await env.DB.prepare("INSERT INTO accounts (id, login, created_at, last_seen_at) VALUES (?, ?, 1, ?)").bind(id, login, lastSeen).run();
  if (githubId !== undefined) {
    await env.DB.prepare("INSERT INTO identities (provider, subject, account_id, linked_at) VALUES ('github', ?, ?, 1)")
      .bind(String(githubId), id)
      .run();
  }
}

async function users(query = ""): Promise<Page> {
  const res = await call("GET", `/admin/users${query}`, ADMIN);
  expect(res.status).toBe(200);
  return res.json();
}

describe("GET /admin/users", () => {
  it("lists accounts, last seen first, with flags and machine counts", async () => {
    await add(1001, "old", 10);
    await add(1002, "new", 30, 77);
    await add(1003, "tie", 30);
    await env.DB.prepare("INSERT INTO user_flags (account_id, flag, granted_at) VALUES (1002, 'teams', 1), (1002, 'cloud', 1)").run();
    await env.DB.prepare(
      "INSERT INTO machines (label, account_id, pub, name, kind, created_at, last_seen_at) VALUES ('m1', 1002, 'p', 'n', 'app', 1, 1), ('m2', 1002, 'p', 'n', 'server', 1, 1)",
    ).run();
    const { users: list, next } = await users();
    expect(next).toBeNull();
    expect(list.map((u) => u.account_id)).toEqual([1003, 1002, 1001]);
    expect(list[1]).toEqual({
      account_id: 1002,
      github_id: 77,
      login: "new",
      plan: "free",
      blocked_at: null,
      blocked_reason: null,
      created_at: 1,
      last_seen_at: 30,
      flags: ["cloud", "teams"],
      machines: 2,
    });
    expect(list[2]!.flags).toEqual([]);
    expect(list[2]!.github_id).toBeNull();
  });

  it("pages with the cursor", async () => {
    await env.DB.batch(
      Array.from({ length: 120 }, (_, i) =>
        env.DB.prepare("INSERT INTO accounts (id, login, created_at, last_seen_at) VALUES (?, ?, 1, ?)").bind(1001 + i, `u${i}`, i % 7),
      ),
    );
    const seen: number[] = [];
    const sizes: number[] = [];
    let after = "";
    for (;;) {
      const page = await users(after);
      sizes.push(page.users.length);
      seen.push(...page.users.map((u) => u.account_id));
      if (page.next === null) break;
      after = `?after=${page.next}`;
    }
    expect(sizes).toEqual([50, 50, 20]);
    expect(new Set(seen).size).toBe(120);
  });

  it("finds by login prefix without case, and by exact ID", async () => {
    await add(1001, "octocat");
    await add(1002, "1001abc");
    await add(1003, "other");
    expect((await users("?q=Octo")).users.map((u) => u.login)).toEqual(["octocat"]);
    expect((await users("?q=1001")).users.map((u) => u.account_id).sort()).toEqual([1001, 1002]);
  });

  it("finds an account by the GitHub ID linked to it as well as by its own ID", async () => {
    const got = await signIn(4242, "octocat");
    await add(1001, "other");
    expect((await users("?q=4242")).users.map((u) => [u.account_id, u.github_id])).toEqual([[got.account_id, 4242]]);
    expect((await users(`?q=${got.account_id}`)).users.map((u) => [u.account_id, u.github_id])).toEqual([[got.account_id, 4242]]);
    expect((await users("?q=1001")).users.map((u) => [u.account_id, u.github_id])).toEqual([[1001, null]]);
  });

  it("does not treat _ as a wildcard", async () => {
    await add(1001, "a_b");
    await add(1002, "axb");
    expect((await users("?q=a_")).users.map((u) => u.login)).toEqual(["a_b"]);
    expect((await call("GET", "/admin/users?q=%25", ADMIN)).status).toBe(400);
    expect((await call("GET", "/admin/users?q=", ADMIN)).status).toBe(400);
    expect((await call("GET", `/admin/users?q=${"a".repeat(40)}`, ADMIN)).status).toBe(400);
  });

  it("refuses a bad limit or cursor", async () => {
    for (const query of ["?limit=0", "?limit=101", "?limit=x", "?after=x", "?after=1_0", "?after=1_9999999999999999"]) {
      expect((await call("GET", `/admin/users${query}`, ADMIN)).status, query).toBe(400);
    }
  });

  it("needs the secret", async () => {
    expect((await call("GET", "/admin/users")).status).toBe(401);
    expect((await call("GET", "/admin/users/1001")).status).toBe(401);
    expect((await call("GET", "/admin/users", { Authorization: "Bearer wrong" })).status).toBe(401);
  });
});

describe("GET /admin/users/<id>", () => {
  it("reads one account in full", async () => {
    const a = await signIn(1001, "octocat");
    const b = await signIn(1001, "octocat", "gho_second", { name: "laptop", kind: "phone" });
    const other = await signIn(1002, "other");
    const me = a.account_id;
    await env.DB.prepare("UPDATE accounts SET email = 'o@example.com' WHERE id = ?").bind(me).run();
    await env.DB.prepare(
      "INSERT INTO machines (label, account_id, pub, name, kind, created_at, last_seen_at) VALUES ('m1', ?1, 'p', 'desk', 'app', 1, 5), ('m2', ?2, 'p', 'x', 'app', 1, 5)",
    )
      .bind(me, other.account_id)
      .run();
    await env.DB.prepare("INSERT INTO cloud_access_requests (account_id, status, requested_at, gh_login) VALUES (?, 'pending', 1, 'octocat')")
      .bind(me)
      .run();
    await env.DB.prepare(
      `INSERT INTO notices (id, account_id, text, url, created_at, expires_at) VALUES
       ('mine', ?1, 'for me', NULL, 5, NULL), ('theirs', ?2, 'for them', NULL, 5, NULL), ('all', NULL, 'for all', NULL, 5, NULL)`,
    )
      .bind(me, other.account_id)
      .run();
    await env.DB.prepare("INSERT INTO notice_reads (account_id, notice_id) VALUES (?, 'mine')").bind(me).run();

    const res = await call("GET", `/admin/users/${me}`, ADMIN);
    expect(res.status).toBe(200);
    const text = await res.text();
    expect(text).not.toContain("token_hash");
    expect(text).not.toContain(a.session);
    expect(text).not.toContain(b.session);
    const body = JSON.parse(text) as {
      user: User & { email: string | null };
      sessions: { id: string; name: string; kind: string; machine_label: string | null }[];
      machines: { label: string; name: string }[];
      cloud_request: { account_id: number; github_id: number | null; status: string } | null;
      notices: Record<string, unknown>[];
    };
    expect(body.user).toMatchObject({ account_id: me, github_id: 1001, login: "octocat", email: "o@example.com", machines: 1 });
    expect(body.sessions.map((s) => s.id).sort()).toEqual([a.session_id, b.session_id].sort());
    expect(Object.keys(body.sessions[0]!).sort()).toEqual(
      ["created_at", "expires_at", "id", "kind", "last_used_at", "machine_label", "name"].sort(),
    );
    expect(body.machines).toEqual([{ label: "m1", name: "desk", kind: "app", created_at: 1, last_seen_at: 5 }]);
    expect(body.cloud_request).toMatchObject({ account_id: me, github_id: 1001, status: "pending" });
    expect(body.notices).toEqual([
      { id: "mine", account_id: me, login: "octocat", text: "for me", url: null, created_at: 5, expires_at: null, reads: 1 },
    ]);
  });

  it("has no cloud request for an account that never asked", async () => {
    await add(1001, "octocat");
    const body = (await (await call("GET", "/admin/users/1001", ADMIN)).json()) as { cloud_request: unknown; sessions: unknown[] };
    expect(body.cloud_request).toBeNull();
    expect(body.sessions).toEqual([]);
  });

  it("answers 404 for an account it does not have", async () => {
    const res = await call("GET", "/admin/users/9999", ADMIN);
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ error: "no such account" });
  });

  it("answers 400 for a bad ID", async () => {
    for (const id of ["abc", "0", "-1", "1e3"]) expect((await call("GET", `/admin/users/${id}`, ADMIN)).status, id).toBe(400);
  });
});

describe("GET /admin/notices", () => {
  it("lists notices newest first with read counts", async () => {
    await add(1001, "octocat");
    await add(1002, "other");
    await env.DB.prepare(
      `INSERT INTO notices (id, account_id, text, url, created_at, expires_at) VALUES
       ('all', NULL, 'for all', 'https://example.com', 20, NULL), ('mine', 1001, 'for me', NULL, 10, NULL), ('old', NULL, 'gone', NULL, 5, 6)`,
    ).run();
    await env.DB.prepare("INSERT INTO notice_reads (account_id, notice_id) VALUES (1001, 'all'), (1002, 'all')").run();
    const res = await call("GET", "/admin/notices", ADMIN);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual([
      { id: "all", account_id: null, login: null, text: "for all", url: "https://example.com", created_at: 20, expires_at: null, reads: 2 },
      { id: "mine", account_id: 1001, login: "octocat", text: "for me", url: null, created_at: 10, expires_at: null, reads: 0 },
      { id: "old", account_id: null, login: null, text: "gone", url: null, created_at: 5, expires_at: 6, reads: 0 },
    ]);
  });

  it("needs the secret", async () => {
    expect((await call("GET", "/admin/notices")).status).toBe(401);
  });
});

describe("GET /admin/plans", () => {
  it("lists plan names with free first", async () => {
    await env.CONFIG.put("plans", JSON.stringify({ pro: {}, free: {}, internal: {}, "Bad Name": {} }));
    const res = await call("GET", "/admin/plans", ADMIN);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ plans: ["free", "internal", "pro"] });
  });

  it("answers the built-in plans without the KV key", async () => {
    expect(await (await call("GET", "/admin/plans", ADMIN)).json()).toEqual({ plans: ["free", "internal"] });
  });

  it("needs the secret", async () => {
    expect((await call("GET", "/admin/plans")).status).toBe(401);
  });
});
