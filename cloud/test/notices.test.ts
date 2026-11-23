import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ADMIN, as, call, nowSeconds, signIn } from "./helpers";

const JSON_HEADERS = { ...ADMIN, "Content-Type": "application/json" };

async function post(body: unknown): Promise<Response> {
  return call("POST", "/admin/notices", JSON_HEADERS, JSON.stringify(body));
}

async function notices(session: string): Promise<{ id: string; text: string; url?: string | null }[]> {
  const res = await call("GET", "/me", as(session));
  expect(res.status).toBe(200);
  return ((await res.json()) as { notices: { id: string; text: string; url?: string | null }[] }).notices;
}

async function make(body: unknown): Promise<string> {
  const res = await post(body);
  expect(res.status).toBe(200);
  return ((await res.json()) as { id: string }).id;
}

describe("notices", () => {
  it("one for everyone shows to two accounts, and a read hides it for the reader only", async () => {
    const a = await signIn(1001, "alice");
    const b = await signIn(1002, "bob");
    const id = await make({ text: "maintenance at 05:00 UTC", url: "https://arakun.dev/status" });
    expect(id).toMatch(/^[0-9a-f]{32}$/);
    for (const who of [a, b]) {
      expect(await notices(who.session)).toEqual([{ id, text: "maintenance at 05:00 UTC", url: "https://arakun.dev/status" }]);
    }
    const read = await call("POST", `/notices/${id}/read`, as(a.session));
    expect(read.status).toBe(204);
    expect(await notices(a.session)).toEqual([]);
    expect(await notices(b.session)).toHaveLength(1);
    // Reading twice is still 204.
    expect((await call("POST", `/notices/${id}/read`, as(a.session))).status).toBe(204);
  });

  it("one for account A is not in B's, and B cannot read it", async () => {
    const a = await signIn(1001, "alice");
    const b = await signIn(1002, "bob");
    const id = await make({ github_id: 1001, text: "your plan changed" });
    expect(await notices(a.session)).toEqual([{ id, text: "your plan changed", url: null }]);
    expect(await notices(b.session)).toEqual([]);
    expect((await call("POST", `/notices/${id}/read`, as(b.session))).status).toBe(404);
    expect(await notices(a.session)).toHaveLength(1);
  });

  it("an expired one is gone, and a read of it or of an unknown one is 404", async () => {
    const a = await signIn(1001, "alice");
    const t = nowSeconds();
    await env.DB.prepare("INSERT INTO notices (id, github_id, text, created_at, expires_at) VALUES ('old', NULL, 'past', ?1, ?2)")
      .bind(t - 100, t - 1)
      .run();
    const soon = await make({ text: "soon", expires_at: t + 1000 });
    expect(await notices(a.session)).toEqual([{ id: soon, text: "soon", url: null }]);
    expect((await call("POST", "/notices/old/read", as(a.session))).status).toBe(404);
    expect((await call("POST", "/notices/nothing/read", as(a.session))).status).toBe(404);
  });

  it("come oldest first, at most 5", async () => {
    const a = await signIn(1001, "alice");
    const ids: string[] = [];
    for (let i = 0; i < 7; i++) {
      ids.push(await make({ text: `n${i}` }));
      await env.DB.prepare("UPDATE notices SET created_at = ?1 WHERE id = ?2").bind(1000 + i, ids[i]).run();
    }
    expect((await notices(a.session)).map((n) => n.text)).toEqual(["n0", "n1", "n2", "n3", "n4"]);
    await call("POST", `/notices/${ids[0]}/read`, as(a.session));
    expect((await notices(a.session)).map((n) => n.text)).toEqual(["n1", "n2", "n3", "n4", "n5"]);
  });

  it("need a session to be read", async () => {
    const id = await make({ text: "hi" });
    expect((await call("POST", `/notices/${id}/read`)).status).toBe(401);
  });

  it("answer 400 for a bad text, url or expiry, and 404 for an account that does not exist", async () => {
    await signIn(1001, "alice");
    const bad: unknown[] = [
      { text: "" },
      { text: "x".repeat(501) },
      { text: 5 },
      {},
      { text: "hi", url: "http://arakun.dev" },
      { text: "hi", url: "javascript:alert(1)" },
      { text: "hi", url: "not a url" },
      { text: "hi", url: 5 },
      { text: "hi", expires_at: nowSeconds() - 5 },
      { text: "hi", expires_at: "tomorrow" },
      { text: "hi", github_id: "1001" },
      { text: "hi", github_id: -1 },
      "text",
    ];
    for (const body of bad) expect((await post(body)).status, JSON.stringify(body)).toBe(400);
    expect((await post({ text: "x".repeat(500) })).status).toBe(200);
    expect((await post({ text: "hi", github_id: 9999 })).status).toBe(404);
    expect((await call("POST", "/admin/notices", { "Content-Type": "application/json" }, "{}")).status).toBe(401);
    const n = await env.DB.prepare("SELECT COUNT(*) AS n FROM notices").first<{ n: number }>();
    expect(n?.n).toBe(1);
  });

  it("are deleted with their reads by the operator", async () => {
    const a = await signIn(1001, "alice");
    const id = await make({ text: "bye" });
    await call("POST", `/notices/${id}/read`, as(a.session));
    expect((await call("DELETE", `/admin/notices/${id}`)).status).toBe(401);
    expect((await call("DELETE", `/admin/notices/${id}`, ADMIN)).status).toBe(204);
    const left = await env.DB.prepare("SELECT (SELECT COUNT(*) FROM notices) AS n, (SELECT COUNT(*) FROM notice_reads) AS r").first();
    expect(left).toEqual({ n: 0, r: 0 });
    expect((await call("DELETE", `/admin/notices/${id}`, ADMIN)).status).toBe(404);
  });
});
