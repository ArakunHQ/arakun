// Sign-ups for the phone app betas: an account signs up, the operator admits by hand.
import { createExecutionContext, env, waitOnExecutionContext } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { DECLINE_COOLDOWN_DAYS } from "../src/cloud_access";
import worker from "../src/index";
import { ADMIN, as, call, github, nowSeconds, roomInWindow, signIn, webhook } from "./helpers";

const DAY = 24 * 60 * 60;

/// A signed-in account with a public profile and, unless `email` is null, a
/// verified primary address.
async function user(id: number, login: string, email: string | null = `${login}@example.com`) {
  github.profiles.set(id, { id, login, created_at: "2020-01-01T00:00:00Z", public_repos: 12, followers: 3, following: 4 });
  github.emails = email === null ? [] : [{ email, primary: true, verified: true }];
  return signIn(id, login);
}

function signUp(session: string, platform: string, email?: unknown): Promise<Response> {
  const headers = { ...as(session), "Content-Type": "application/json" };
  return call("POST", `/beta/${platform}`, headers, email === undefined ? undefined : JSON.stringify({ email }));
}

/// Decides on the sign-up of the account with that ID.
function decide(id: number, platform: string, decision: "admit" | "decline", by?: string): Promise<Response> {
  return call("PUT", `/admin/beta/signups/${id}/${platform}/${decision}`, ADMIN, by === undefined ? undefined : JSON.stringify({ by }));
}

async function signups(session: string): Promise<Record<string, unknown>> {
  const res = await call("GET", "/beta", as(session));
  expect(res.status).toBe(200);
  return res.json();
}

const NONE = { status: "none", email: null, requested_at: null, decided_at: null, retry_at: null };

describe("signing up for a beta", () => {
  it("starts at none, offers the GitHub email, and a sign-up is pending", async () => {
    const me = await user(3001, "tester");
    expect(await signups(me.session)).toEqual({ email: "tester@example.com", ios: NONE, android: NONE });

    const res = await signUp(me.session, "ios");
    expect(res.status).toBe(201);
    const body = await res.json();
    expect(body).toEqual({ status: "pending", email: "tester@example.com", requested_at: expect.any(Number), decided_at: null, retry_at: null });
    expect(await signups(me.session)).toEqual({ email: "tester@example.com", ios: body, android: NONE });
    const row = await env.DB.prepare("SELECT gh_login, gh_public_repos FROM beta_signups WHERE account_id = ?").bind(me.account_id).first();
    expect(row).toEqual({ gh_login: "tester", gh_public_repos: 12 });
  });

  it("takes another email, trimmed, and checks it", async () => {
    const me = await user(3002, "apple");
    expect((await signUp(me.session, "ios", "not an email")).status).toBe(400);
    expect((await signUp(me.session, "ios", "a@b")).status).toBe(400);
    expect((await signUp(me.session, "ios", 5)).status).toBe(400);
    expect((await signUp(me.session, "ios", `${"x".repeat(250)}@b.co`)).status).toBe(400);
    const res = await signUp(me.session, "ios", "  me@icloud.com ");
    expect(res.status).toBe(201);
    expect(await res.json()).toMatchObject({ email: "me@icloud.com" });
  });

  it("needs an email when GitHub gave none", async () => {
    const me = await user(3003, "private", null);
    expect((await signups(me.session))["email"]).toBeNull();
    const res = await signUp(me.session, "android");
    expect(res.status).toBe(400);
    expect(((await res.json()) as { error: string }).error).toContain("no verified email");
    expect((await signUp(me.session, "android", "me@gmail.com")).status).toBe(201);
  });

  it("keeps the platforms apart", async () => {
    const me = await user(3004, "both");
    expect((await signUp(me.session, "ios")).status).toBe(201);
    // An empty object is no email, as no body is.
    expect((await call("POST", "/beta/android", { ...as(me.session), "Content-Type": "application/json" }, "{}")).status).toBe(201);
    expect((await signUp(me.session, "android", "me@gmail.com")).status).toBe(200);
    const all = await signups(me.session);
    expect(all["ios"]).toMatchObject({ status: "pending", email: "both@example.com" });
    expect(all["android"]).toMatchObject({ status: "pending", email: "me@gmail.com" });
  });

  it("changes only the email of a pending sign-up, with no new post", async () => {
    const me = await user(3005, "typo");
    const first = (await (await signUp(me.session, "ios", "me@icloud.con")).json()) as { requested_at: number };
    const again = await signUp(me.session, "ios", "me@icloud.com");
    expect(again.status).toBe(200);
    expect(await again.json()).toEqual({ ...first, email: "me@icloud.com" });
    expect(github.profileCalls).toHaveLength(1);
  });

  it("is refused once admitted", async () => {
    const me = await user(3006, "inside");
    await signUp(me.session, "ios");
    expect((await decide(me.account_id, "ios", "admit")).status).toBe(200);
    const res = await signUp(me.session, "ios", "other@example.com");
    expect(res.status).toBe(409);
    expect(await res.json()).toMatchObject({ error: "already admitted", status: "admitted", email: "inside@example.com" });
  });

  it("needs a session and a known platform", async () => {
    expect((await call("GET", "/beta")).status).toBe(401);
    expect((await call("POST", "/beta/ios")).status).toBe(401);
    const me = await user(3007, "lost");
    expect((await signUp(me.session, "windows")).status).toBe(404);
    // Under /beta is the API, never the page.
    const other = await call("GET", "/beta/other");
    expect(other.status).toBe(404);
    expect(await other.json()).toEqual({ error: "not found" });
  });

  it("is refused to a blocked account", async () => {
    const me = await user(3008, "blocked");
    expect((await call("PUT", `/admin/users/${me.account_id}/block`, ADMIN)).status).toBe(204);
    expect((await signUp(me.session, "ios")).status).toBe(403);
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM beta_signups").first()).toEqual({ n: 0 });
  });

  it("is rate limited per account", async () => {
    const me = await user(3009, "eager");
    await roomInWindow(60);
    const statuses: number[] = [];
    for (let i = 0; i < 8; i++) statuses.push((await signUp(me.session, "android")).status);
    expect(statuses).toContain(429);
  });
});

describe("the operator", () => {
  it("lists pending first with the email, and filters", async () => {
    const a = await user(3101, "first");
    await signUp(a.session, "ios");
    const b = await user(3102, "second");
    await signUp(b.session, "android", "second@gmail.com");
    await decide(a.account_id, "ios", "admit", "rik");

    const res = await call("GET", "/admin/beta/signups", ADMIN);
    expect(res.status).toBe(200);
    const list = (await res.json()) as Record<string, unknown>[];
    expect(list.map((r) => [r["account_id"], r["github_id"], r["platform"], r["status"]])).toEqual([
      [b.account_id, 3102, "android", "pending"],
      [a.account_id, 3101, "ios", "admitted"],
    ]);
    expect(list[0]).toMatchObject({ email: "second@gmail.com", login: "second", github: { public_repos: 12 } });
    expect(list[1]).toMatchObject({ decided_by: "rik" });

    const ios = (await (await call("GET", "/admin/beta/signups?platform=ios&status=admitted", ADMIN)).json()) as unknown[];
    expect(ios).toHaveLength(1);
    expect((await call("GET", "/admin/beta/signups?platform=web", ADMIN)).status).toBe(400);
    expect((await call("GET", "/admin/beta/signups?status=granted", ADMIN)).status).toBe(400);
  });

  it("declines, and the account may sign up again after the cooldown", async () => {
    const me = await user(3103, "later");
    await signUp(me.session, "android");
    const res = await decide(me.account_id, "android", "decline");
    expect(res.status).toBe(200);
    const decided = (await res.json()) as { decided_at: number; retry_at: number };
    expect(decided.retry_at).toBe(decided.decided_at + DECLINE_COOLDOWN_DAYS * DAY);
    const early = await signUp(me.session, "android");
    expect(early.status).toBe(409);
    expect(await early.json()).toMatchObject({ error: "declined recently" });

    await env.DB.prepare("UPDATE beta_signups SET decided_at = ? WHERE account_id = ?")
      .bind(nowSeconds() - DECLINE_COOLDOWN_DAYS * DAY - 1, me.account_id)
      .run();
    expect((await signUp(me.session, "android")).status).toBe(201);
    expect((await signups(me.session))["android"]).toMatchObject({ status: "pending", decided_at: null });
  });

  it("can change their mind, and a second decision keeps the first's time", async () => {
    const me = await user(3104, "mind");
    await signUp(me.session, "ios");
    expect(await (await decide(me.account_id, "ios", "decline")).json()).toMatchObject({ status: "declined" });
    const admitted = (await (await decide(me.account_id, "ios", "admit", "rik")).json()) as { decided_at: number };
    expect(admitted).toMatchObject({ status: "admitted", decided_by: "rik", retry_at: null });
    await env.DB.prepare("UPDATE beta_signups SET decided_at = 1 WHERE account_id = ?").bind(me.account_id).run();
    expect(await (await decide(me.account_id, "ios", "admit", "other")).json()).toMatchObject({ decided_at: 1, decided_by: "rik" });
  });

  it("logs each change with the operator and the platform, and not a repeat", async () => {
    const me = await user(3106, "logged");
    await signUp(me.session, "android");
    const op = { ...ADMIN, "X-Arakun-Operator-Id": "1001", "X-Arakun-Operator-Login": "operator" };
    expect((await call("PUT", `/admin/beta/signups/${me.account_id}/android/admit`, op)).status).toBe(200);
    expect((await call("PUT", `/admin/beta/signups/${me.account_id}/android/admit`, op)).status).toBe(200);
    expect((await decide(me.account_id, "android", "decline")).status).toBe(200);
    const rows = await env.DB.prepare("SELECT operator_login, action, account_id, detail FROM admin_log ORDER BY id").all();
    expect(rows.results).toEqual([
      { operator_login: "operator", action: "beta.admit", account_id: me.account_id, detail: '{"platform":"android"}' },
      { operator_login: null, action: "beta.decline", account_id: me.account_id, detail: '{"platform":"android"}' },
    ]);
  });

  it("checks the path and needs the admin secret", async () => {
    const me = await user(3105, "door");
    await signUp(me.session, "ios");
    expect((await decide(me.account_id, "android", "admit")).status).toBe(404);
    expect((await decide(me.account_id, "web", "admit")).status).toBe(400);
    expect((await call("PUT", "/admin/beta/signups/0/ios/admit", ADMIN)).status).toBe(400);
    expect((await call("GET", "/admin/beta/signups", as(me.session))).status).toBe(401);
    expect((await call("PUT", `/admin/beta/signups/${me.account_id}/ios/admit`, as(me.session))).status).toBe(401);
    expect((await signups(me.session))["ios"]).toMatchObject({ status: "pending" });
  });
});

describe("the webhook", () => {
  async function signUpWith(session: string, extra: Partial<Cloudflare.Env>): Promise<Response> {
    const ctx = createExecutionContext();
    const req = new Request("https://teams.test/beta/ios", { method: "POST", headers: as(session) });
    const res = await worker.fetch(req, { ...env, ...extra }, ctx);
    await waitOnExecutionContext(ctx);
    return res;
  }

  it("hears of a new sign-up, without the email", async () => {
    const me = await user(3201, "hooked");
    expect((await signUpWith(me.session, { BETA_REQUESTS_WEBHOOK: webhook.url })).status).toBe(201);
    expect(webhook.bodies).toHaveLength(1);
    const sent = JSON.parse(webhook.bodies[0]!) as { content: string; allowed_mentions: unknown };
    expect(sent.allowed_mentions).toEqual({ parse: [] });
    expect(sent.content).toContain("iOS (TestFlight)");
    expect(sent.content).toContain("`hooked`");
    expect(sent.content).not.toContain("example.com");
    expect(sent.content).not.toContain(me.session);
  });

  it("is not called when unset", async () => {
    const me = await user(3202, "quiet");
    expect((await signUpWith(me.session, { BETA_REQUESTS_WEBHOOK: undefined })).status).toBe(201);
    expect(webhook.bodies).toHaveLength(0);
  });
});
