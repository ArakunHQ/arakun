// Requests for cloud access: an account asks, the operator grants or declines.
import { createExecutionContext, env, waitOnExecutionContext } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { CLOUD_FLAG, DECLINE_COOLDOWN_DAYS } from "../src/cloud_access";
import worker from "../src/index";
import { ADMIN, as, call, github, nowSeconds, roomInWindow, signIn, webhook } from "./helpers";

const DAY = 24 * 60 * 60;
const CREATED = Date.parse("2020-01-01T00:00:00Z") / 1000;

/// A signed-in account that GitHub has a public profile for.
async function user(id: number, login: string) {
  github.profiles.set(id, { id, login, created_at: "2020-01-01T00:00:00Z", public_repos: 12, followers: 3, following: 4, email: "x@example.com" });
  return signIn(id, login);
}

function ask(session: string, reason?: unknown): Promise<Response> {
  const headers = { ...as(session), "Content-Type": "application/json" };
  return call("POST", "/cloud/access", headers, reason === undefined ? undefined : JSON.stringify({ reason }));
}

function decide(id: number, decision: "grant" | "decline", by?: string): Promise<Response> {
  return call("PUT", `/admin/cloud/requests/${id}/${decision}`, ADMIN, by === undefined ? undefined : JSON.stringify({ by }));
}

async function access(session: string): Promise<unknown> {
  const res = await call("GET", "/cloud/access", as(session));
  expect(res.status).toBe(200);
  return res.json();
}

describe("asking for cloud access", () => {
  it("starts at none, and a request is pending with what GitHub said", async () => {
    const me = await user(2001, "asker");
    expect(await access(me.session)).toEqual({ status: "none", requested_at: null, decided_at: null, retry_at: null });

    const before = nowSeconds();
    const res = await ask(me.session, "  I want to try agents from my phone  ");
    expect(res.status).toBe(201);
    const body = (await res.json()) as { status: string; requested_at: number };
    expect(body).toEqual({ status: "pending", requested_at: expect.any(Number), decided_at: null, retry_at: null });
    expect(body.requested_at).toBeGreaterThanOrEqual(before);
    expect(await access(me.session)).toEqual(body);

    // GitHub was asked by ID, with the app's credentials.
    expect(github.profileCalls.map((r) => r.url)).toEqual(["https://api.github.com/user/2001"]);
    expect(github.profileCalls[0]!.headers.get("Authorization")).toBe(`Basic ${btoa("Iv-test:client-secret")}`);
    const row = await env.DB.prepare("SELECT * FROM cloud_access_requests WHERE github_id = 2001").first();
    expect(row).toMatchObject({
      status: "pending",
      reason: "I want to try agents from my phone",
      gh_login: "asker",
      gh_created_at: CREATED,
      gh_public_repos: 12,
      gh_followers: 3,
      gh_following: 4,
    });
    // Nothing else GitHub said is kept: no email column, no other value.
    expect(JSON.stringify(row)).not.toContain("example.com");
  });

  it("is kept when GitHub does not answer, without signals", async () => {
    const me = await signIn(2002, "noprofile");
    github.outage = 503;
    expect((await ask(me.session)).status).toBe(201);
    const row = await env.DB.prepare("SELECT gh_login, gh_created_at, gh_public_repos, reason FROM cloud_access_requests WHERE github_id = 2002").first();
    expect(row).toEqual({ gh_login: "noprofile", gh_created_at: null, gh_public_repos: null, reason: null });
  });

  it("is refused while one is pending or after a grant", async () => {
    const me = await user(2003, "twice");
    expect((await ask(me.session)).status).toBe(201);
    const again = await ask(me.session);
    expect(again.status).toBe(409);
    expect(await again.json()).toMatchObject({ error: "already pending", status: "pending" });
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM cloud_access_requests").first()).toEqual({ n: 1 });

    expect((await decide(2003, "grant")).status).toBe(200);
    const granted = await ask(me.session);
    expect(granted.status).toBe(409);
    expect(await granted.json()).toMatchObject({ error: "already granted", status: "granted" });
  });

  it("checks the reason", async () => {
    const me = await user(2004, "wordy");
    expect((await ask(me.session, "x".repeat(501))).status).toBe(400);
    expect((await ask(me.session, 5)).status).toBe(400);
    expect((await call("POST", "/cloud/access", as(me.session), "[]")).status).toBe(400);
    expect((await ask(me.session, "x".repeat(500))).status).toBe(201);
  });

  it("needs a session", async () => {
    expect((await call("GET", "/cloud/access")).status).toBe(401);
    expect((await call("POST", "/cloud/access")).status).toBe(401);
    expect((await call("GET", "/cloud/access", as("A".repeat(43)))).status).toBe(401);
    // Under /cloud is the API, never the page.
    const other = await call("GET", "/cloud/other");
    expect(other.status).toBe(404);
    expect(await other.json()).toEqual({ error: "not found" });
  });

  it("is refused to a blocked account", async () => {
    const me = await user(2005, "blocked");
    expect((await call("PUT", "/admin/users/2005/block", ADMIN)).status).toBe(204);
    const res = await ask(me.session);
    expect(res.status).toBe(403);
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM cloud_access_requests").first()).toEqual({ n: 0 });
  });

  it("is rate limited per account", async () => {
    const me = await user(2006, "eager");
    await roomInWindow(60);
    const statuses: number[] = [];
    for (let i = 0; i < 8; i++) statuses.push((await ask(me.session)).status);
    expect(statuses).toContain(429);
  });
});

describe("a decline", () => {
  it("lets the account ask again only after the cooldown", async () => {
    const me = await user(2101, "declined");
    expect((await ask(me.session)).status).toBe(201);
    const res = await decide(2101, "decline", "karlo");
    expect(res.status).toBe(200);
    const decided = (await res.json()) as { decided_at: number; retry_at: number };
    expect(decided).toMatchObject({ status: "declined", decided_by: "karlo", has_flag: false });
    expect(decided.retry_at).toBe(decided.decided_at + DECLINE_COOLDOWN_DAYS * DAY);
    expect(await access(me.session)).toMatchObject({ status: "declined", retry_at: decided.retry_at });

    const early = await ask(me.session);
    expect(early.status).toBe(409);
    expect(await early.json()).toMatchObject({ error: "declined recently", status: "declined", retry_at: decided.retry_at });

    // The cooldown passes.
    await env.DB.prepare("UPDATE cloud_access_requests SET decided_at = ? WHERE github_id = 2101")
      .bind(nowSeconds() - DECLINE_COOLDOWN_DAYS * DAY - 1)
      .run();
    expect((await ask(me.session, "I have history now")).status).toBe(201);
    expect(await access(me.session)).toMatchObject({ status: "pending", decided_at: null, retry_at: null });
    const row = await env.DB.prepare("SELECT decided_by, reason FROM cloud_access_requests WHERE github_id = 2101").first();
    expect(row).toEqual({ decided_by: null, reason: "I have history now" });
  });

  it("takes only a pending request", async () => {
    await user(2102, "nobody");
    expect((await decide(2102, "decline")).status).toBe(404);
    const me = await user(2103, "granted");
    await ask(me.session);
    await decide(2103, "grant");
    const res = await decide(2103, "decline");
    expect(res.status).toBe(409);
    expect(await access(me.session)).toMatchObject({ status: "granted" });
  });
});

describe("a grant", () => {
  it("sets the cloud flag, and revoking the flag lets the account ask again", async () => {
    const me = await user(2201, "granted");
    await ask(me.session, "please");
    const res = await decide(2201, "grant", "karlo");
    expect(res.status).toBe(200);
    expect(await res.json()).toMatchObject({ github_id: 2201, status: "granted", decided_by: "karlo", has_flag: true, retry_at: null });
    const flags = (await (await call("GET", "/me", as(me.session))).json()) as { flags: string[] };
    expect(flags.flags).toEqual([CLOUD_FLAG]);
    expect(await access(me.session)).toMatchObject({ status: "granted" });

    // A second grant changes nothing.
    const first = (await env.DB.prepare("SELECT decided_at FROM cloud_access_requests WHERE github_id = 2201").first())!;
    expect((await decide(2201, "grant", "someone else")).status).toBe(200);
    expect(await env.DB.prepare("SELECT decided_at, decided_by FROM cloud_access_requests WHERE github_id = 2201").first()).toEqual({
      ...first,
      decided_by: "karlo",
    });

    expect((await call("DELETE", `/admin/users/2201/flags/${CLOUD_FLAG}`, ADMIN)).status).toBe(200);
    expect(await access(me.session)).toEqual({ status: "none", requested_at: null, decided_at: null, retry_at: null });
    expect((await ask(me.session)).status).toBe(201);
  });

  it("turns a decline around", async () => {
    const me = await user(2202, "changed");
    await ask(me.session);
    await decide(2202, "decline");
    expect((await decide(2202, "grant")).status).toBe(200);
    expect(await access(me.session)).toMatchObject({ status: "granted", retry_at: null });
  });

  it("needs a request", async () => {
    await user(2203, "unasked");
    expect((await decide(2203, "grant")).status).toBe(404);
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM user_flags").first()).toEqual({ n: 0 });
    expect((await decide(0, "grant")).status).toBe(400);
  });

  it("is seen as granted when the flag was given by hand", async () => {
    const me = await user(2204, "byhand");
    await call("PUT", `/admin/users/2204/flags/${CLOUD_FLAG}`, ADMIN);
    expect(await access(me.session)).toMatchObject({ status: "granted" });
    expect((await ask(me.session)).status).toBe(409);
  });
});

describe("the operator's list", () => {
  it("shows pending first, oldest first, with the signals", async () => {
    for (const [id, login] of [
      [2301, "first"],
      [2302, "second"],
      [2303, "third"],
    ] as const) {
      const me = await user(id, login);
      await ask(me.session, `from ${login}`);
    }
    await env.DB.prepare("UPDATE cloud_access_requests SET requested_at = requested_at - github_id").run();
    await decide(2301, "decline");

    const res = await call("GET", "/admin/cloud/requests", ADMIN);
    expect(res.status).toBe(200);
    const list = (await res.json()) as { github_id: number; status: string }[];
    expect(list.map((r) => [r.github_id, r.status])).toEqual([
      [2303, "pending"],
      [2302, "pending"],
      [2301, "declined"],
    ]);
    expect(list[0]).toEqual({
      github_id: 2303,
      login: "third",
      status: "pending",
      reason: "from third",
      requested_at: expect.any(Number),
      decided_at: null,
      decided_by: null,
      retry_at: null,
      has_flag: false,
      blocked: false,
      github: {
        login: "third",
        created_at: CREATED,
        age_days: expect.any(Number),
        public_repos: 12,
        followers: 3,
        following: 4,
      },
    });

    const pending = (await (await call("GET", "/admin/cloud/requests?status=pending", ADMIN)).json()) as unknown[];
    expect(pending).toHaveLength(2);
    expect((await call("GET", "/admin/cloud/requests?status=nope", ADMIN)).status).toBe(400);
  });

  it("is closed without the admin secret", async () => {
    const me = await user(2401, "sneaky");
    await ask(me.session);
    expect((await call("GET", "/admin/cloud/requests")).status).toBe(401);
    expect((await call("GET", "/admin/cloud/requests", as(me.session))).status).toBe(401);
    expect((await call("PUT", "/admin/cloud/requests/2401/grant", as(me.session))).status).toBe(401);
    expect((await call("PUT", "/admin/cloud/requests/2401/decline", as(me.session))).status).toBe(401);
    expect(await access(me.session)).toMatchObject({ status: "pending" });
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM user_flags").first()).toEqual({ n: 0 });
  });
});

describe("the webhook", () => {
  async function askWith(session: string, extra: Partial<Cloudflare.Env>): Promise<Response> {
    const ctx = createExecutionContext();
    const req = new Request("https://teams.test/cloud/access", { method: "POST", headers: as(session) });
    const res = await worker.fetch(req, { ...env, ...extra }, ctx);
    await waitOnExecutionContext(ctx);
    return res;
  }

  it("hears of a new request, with public facts only", async () => {
    const me = await user(2501, "hooked");
    expect((await askWith(me.session, { CLOUD_REQUESTS_WEBHOOK: webhook.url })).status).toBe(201);
    expect(webhook.bodies).toHaveLength(1);
    const sent = JSON.parse(webhook.bodies[0]!) as { content: string; allowed_mentions: unknown };
    expect(sent.allowed_mentions).toEqual({ parse: [] });
    expect(sent.content).toContain("`hooked`");
    expect(sent.content).toContain("2501");
    expect(sent.content).toContain("public repos: 12");
    expect(sent.content).not.toContain("example.com");
    expect(sent.content).not.toContain(me.session);

    // Not again for a refused request.
    expect((await askWith(me.session, { CLOUD_REQUESTS_WEBHOOK: webhook.url })).status).toBe(409);
    expect(webhook.bodies).toHaveLength(1);
  });

  it("is not called when unset", async () => {
    const me = await user(2502, "quiet");
    expect((await askWith(me.session, { CLOUD_REQUESTS_WEBHOOK: undefined })).status).toBe(201);
    expect(webhook.bodies).toHaveLength(0);
  });

  it("does not fail the request when it fails", async () => {
    const me = await user(2503, "broken");
    webhook.outage = "throw";
    expect((await askWith(me.session, { CLOUD_REQUESTS_WEBHOOK: webhook.url })).status).toBe(201);
    expect(webhook.bodies).toHaveLength(1);
    expect(await access(me.session)).toMatchObject({ status: "pending" });
  });
});
