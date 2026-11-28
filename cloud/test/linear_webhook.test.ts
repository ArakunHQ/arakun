// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { linear } from "../src/linear";
import { saveWorkspace } from "../src/linear_api";
import { expired } from "../src/linear_expire";
import { KEEP_SECONDS, keepWorkspaces } from "../src/linear_keep";
import { deliver, dropDelivery } from "../src/machines";
import { guidanceOf, MAX_GUIDANCE, verified, webhook } from "../src/linear_webhook";
import { ADMIN, HOOK_ORG, HOOK_SESSION, as, call, created, dump, linearFake, notification, nowSeconds, prompted, signedWebhook } from "./helpers";
import { account, away, claim, connectedComputer, openPc, stored, type Computer, type PcSocket } from "./relay/helpers";

const ORG = HOOK_ORG;
const ORG2 = "11111111-1111-4111-8111-222222222222";
const SESSION = HOOK_SESSION;
const LU = "22222222-2222-4222-8222-222222222222";
const OTHER = "22222222-2222-4222-8222-999999999999";
const ISSUE = "66666666-6666-4666-8666-666666666666";
const UNMAPPED = "66666666-6666-4666-8666-777777777777";
const TEAM = "33333333-3333-4333-8333-333333333333";
const TEAM2 = "33333333-3333-4333-8333-444444444444";
const PROJECT = "44444444-4444-4444-8444-444444444444";
const ACCESS = "lin_oauth_ws";
const COMMENT = "88888888-8888-4888-8888-888888888888";
const SESSION2 = "55555555-5555-4555-8555-111111111111";

type Hook = { text: string; headers: Record<string, string> };

const body = created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION });

function send(hook: Hook | Promise<Hook>): Promise<Response> {
  return Promise.resolve(hook).then((h) => call("POST", "/linear/webhook", h.headers, h.text));
}

/// The text without a signature.
async function unsigned(b: Record<string, unknown>): Promise<Hook> {
  const { text, headers } = await signedWebhook(b);
  const { "Linear-Signature": _gone, ...rest } = headers;
  return { text, headers: rest };
}

/// One character of the text changed after signing.
async function altered(hook: Promise<Hook>): Promise<Hook> {
  const h = await hook;
  return { ...h, text: h.text.replace("Make it blue", "Make it blux") };
}

async function count(table: string): Promise<number> {
  return (await env.DB.prepare(`SELECT COUNT(*) AS n FROM ${table}`).first<{ n: number }>())!.n;
}

/// The installed workspace, with a token the fake Linear takes.
async function workspace(installer: number): Promise<void> {
  linearFake.tokens.set(ACCESS, { userId: "app-user", organizationId: ORG, organizationName: "Acme" });
  await saveWorkspace(env, {
    organizationId: ORG,
    name: "Acme",
    appUserId: "app-user",
    access: ACCESS,
    refresh: "lin_refresh_ws",
    expiresAt: nowSeconds() + 86_400,
    installedBy: installer,
  });
}

/// An account with the flag, a link from Linear user `linearUser`, and a
/// computer named "Desk" (the name `openPc` registers) that takes `team` or
/// `project`. Also sets where ISSUE and UNMAPPED live.
async function linkedComputerWithRoute({
  team = TEAM,
  project,
  linearUser = LU,
}: { team?: string; project?: string; linearUser?: string } = {}) {
  const owner = await account();
  await call("PUT", `/admin/users/${owner.account_id}/flags/linear`, ADMIN);
  if ((await count("linear_workspaces")) === 0) await workspace(owner.account_id);
  await env.DB.prepare("INSERT INTO linear_links (organization_id, linear_user_id, account_id, linked_at) VALUES (?, ?, ?, ?)")
    .bind(ORG, linearUser, owner.account_id, nowSeconds())
    .run();
  const { me, pc } = await connectedComputer(owner);
  const routes = { label: me.label, organization_id: ORG, teams: project ? [] : [team], projects: project ? [project] : [] };
  const res = await call("PUT", "/linear/routes", as(owner.session), JSON.stringify(routes));
  if (res.status !== 204) throw new Error(`routes refused: ${res.status}`);
  linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: PROJECT });
  linearFake.issues.set(UNMAPPED, { teamId: TEAM2, projectId: null });
  return { me, pc, owner };
}

function deliveries() {
  return env.DB.prepare("SELECT * FROM linear_deliveries").all<Record<string, unknown>>().then((r) => r.results);
}

describe("verifying Linear's signature", () => {
  it("takes the HMAC of the exact text and nothing else", async () => {
    const { text, headers } = await signedWebhook(body);
    const sig = headers["Linear-Signature"]!;
    expect(await verified(text, sig, "lin-hook")).toBe(true);
    expect(await verified(text, sig.toUpperCase(), "lin-hook")).toBe(true);
    expect(await verified(text, sig, "other")).toBe(false);
    expect(await verified(text + " ", sig, "lin-hook")).toBe(false);
    expect(await verified(text, null, "lin-hook")).toBe(false);
    expect(await verified(text, sig.slice(1), "lin-hook")).toBe(false);
    expect(await verified(text, `${sig.slice(0, 63)}g`, "lin-hook")).toBe(false);
  });
});

describe("Linear's webhook", () => {
  it("refuses a webhook that is unsigned, wrongly signed, altered or old", async () => {
    // Everything a forged webhook would need is in place, so the checks below can fail.
    const { pc } = await linkedComputerWithRoute();
    for (const bad of [
      unsigned(body),
      signedWebhook(body, { secret: "other" }),
      altered(signedWebhook(body)),
      signedWebhook(body, { at: Date.now() - 61_000 }),
      signedWebhook(body, { at: Date.now() + 61_000 }),
    ])
      expect((await send(bad)).status).toBe(401);
    expect(await count("linear_deliveries")).toBe(0);
    expect(linearFake.calls).toEqual([]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("refuses a body over 1 MB", async () => {
    const res = await send(signedWebhook({ ...body, pad: "x".repeat(1024 * 1024) }));
    expect(res.status).toBe(413);
    expect(linearFake.calls).toEqual([]);
  });

  it("needs no session, but is 404 without all four secrets", async () => {
    const hook = await signedWebhook(body);
    const req = new Request("https://teams.test/linear/webhook", { method: "POST", headers: hook.headers, body: hook.text });
    const bare = { ...env, LINEAR_CLIENT_ID: undefined } as unknown as typeof env;
    expect((await linear(req, bare, new URL(req.url)))?.status).toBe(404);
  });

  it("sends a delegated issue to the delegator's computer and says so", async () => {
    const { pc, me, owner } = await linkedComputerWithRoute({ team: TEAM });
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ type: "linear", kind: "created", agent_session: SESSION, issue: ISSUE, organization: ORG });
    expect(linearFake.activities()).toEqual([{ agentSessionId: SESSION, type: "thought", body: "Sent to Desk. A session starts there now." }]);
    const rows = await deliveries();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      agent_session_id: SESSION,
      organization_id: ORG,
      issue_id: ISSUE,
      kind: "created",
      activity_id: null,
      account_id: owner.account_id,
      label: me.label,
      accepted_at: null,
      expired_at: null,
    });
    expect(rows[0]!["id"]).toMatch(/^[0-9a-f]{32}$/);
    expect(rows[0]!["announced_at"]).toBeTypeOf("number");
  });

  it("passes the id of the comment that mentioned Arakun, never its text", async () => {
    const { me, pc } = await linkedComputerWithRoute({ team: TEAM });
    const comment = { id: COMMENT, body: "SECRET-MENTION only do the migration", userId: LU };
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION, comment })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ type: "linear", kind: "created", comment: COMMENT, body: null });
    const everything = (await dump(env.DB)) + (await storedAll(me.label));
    expect(everything).not.toMatch(/SECRET-/);
  });

  it("a delegation, or a comment id that is not one, passes no comment", async () => {
    const { pc } = await linkedComputerWithRoute({ team: TEAM });
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "created", comment: null });
    const odd = created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION2, comment: { id: "x".repeat(80) } });
    expect((await send(signedWebhook(odd))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "created", agent_session: SESSION2, comment: null });
  });

  it("takes a project route too", async () => {
    const { pc } = await linkedComputerWithRoute({ project: PROJECT });
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ type: "linear", issue: ISSUE });
  });

  it("says the computer is offline and keeps the delivery", async () => {
    const { me, pc, owner } = await linkedComputerWithRoute();
    await away(me.label, pc);
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(200);
    expect(linearFake.activities()).toEqual([
      { agentSessionId: SESSION, type: "thought", body: "Desk is offline. The session starts when it is back, for up to 7 days." },
    ]);
    expect(await count("linear_deliveries")).toBe(1);
    const back = await connectedComputerAgain(me, owner.session);
    expect(await back.inbox.control()).toMatchObject({ type: "linear", kind: "created", issue: ISSUE });
  });

  it("passes the guidance in the live frame and stores none of it", async () => {
    const { me, pc } = await linkedComputerWithRoute({ team: TEAM });
    const guidance = [
      { body: "SECRET-WORKSPACE-RULE", origin: { type: "Organization" } },
      { body: "SECRET-TEAM-RULE", origin: { type: "Team", team: { id: TEAM, key: "ENG", name: "Web", displayName: "Eng / Web", parentId: null } } },
    ];
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION, guidance })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({
      kind: "created",
      body: null,
      guidance: [
        { team: null, body: "SECRET-WORKSPACE-RULE" },
        { team: "Eng / Web", body: "SECRET-TEAM-RULE" },
      ],
    });
    const everything = (await dump(env.DB)) + (await storedAll(me.label));
    expect(everything).toContain(SESSION);
    expect(everything).not.toMatch(/SECRET-/);
  });

  it("an offline computer gets the delegation without its guidance", async () => {
    const { me, pc, owner } = await linkedComputerWithRoute();
    await away(me.label, pc);
    const guidance = [{ body: "Branch from develop.", origin: { type: "Organization" } }];
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION, guidance })))).status).toBe(200);
    const back = await connectedComputerAgain(me, owner.session);
    expect(await back.inbox.control()).toMatchObject({ kind: "created", issue: ISSUE, guidance: null });
  });

  it("the same delegation twice is one delivery and one activity", async () => {
    const { pc } = await linkedComputerWithRoute();
    const b = created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION });
    expect((await send(signedWebhook(b))).status).toBe(200);
    // Signed again, as a retry or a second delegation of the same session would be.
    expect((await send(signedWebhook(b))).status).toBe(200);
    expect(await count("linear_deliveries")).toBe(1);
    expect(linearFake.activities()).toHaveLength(1);
    await pc.inbox.control();
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("two delegations from the same webhook are two deliveries", async () => {
    // Linear's webhookId names the webhook, so every event carries the same one.
    const { pc } = await linkedComputerWithRoute();
    const [S1, S2] = [crypto.randomUUID(), crypto.randomUUID()];
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: S1 })))).status).toBe(200);
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: S2 })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "created", agent_session: S1 });
    expect(await pc.inbox.control()).toMatchObject({ kind: "created", agent_session: S2 });
    expect(await count("linear_deliveries")).toBe(2);
    expect(linearFake.activities().map((a) => a.agentSessionId)).toEqual([S1, S2]);
  });

  it("a retry after Linear refused the issue lookup finishes the same delivery", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    await linkedComputerWithRoute();
    const hook = signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION }));
    linearFake.refuse = 500;
    expect((await send(hook)).status).toBe(500);
    linearFake.refuse = null;
    expect((await send(hook)).status).toBe(200);
    expect(await count("linear_deliveries")).toBe(1);
    expect(linearFake.activities()).toHaveLength(1);
  });

  it("a retry after only the activity failed sends the same delivery once", async () => {
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    const { pc } = await linkedComputerWithRoute();
    const hook = signedWebhook(created({ creatorId: LU, issueId: ISSUE, promptContext: "SECRET-CONTEXT", agentSessionId: SESSION }));
    linearFake.refuse = 500;
    linearFake.refuseOnly = "agentActivityCreate";
    const first = await send(hook);
    expect(first.status).toBe(500);
    expect(await first.json()).toEqual({ error: "internal error" });
    const [row] = await deliveries();
    expect(row!["announced_at"]).toBeNull();
    const sent = await pc.inbox.control();
    expect(sent["id"]).toBe(row!["id"]);

    linearFake.refuse = null;
    linearFake.refuseOnly = null;
    expect((await send(hook)).status).toBe(200);
    expect(await count("linear_deliveries")).toBe(1);
    expect(linearFake.activities()).toHaveLength(1);
    // The relay already has that id: no second frame.
    await expect(pc.inbox.next(200)).rejects.toThrow();
    // One line, with the route and the status only.
    expect(log.mock.calls).toEqual([["POST /linear/webhook: 500"]]);
    expect(await dump(env.DB)).not.toContain("SECRET-CONTEXT");
    expect(await dump(env.DB)).not.toContain("Make it blue");
  });

  it.each([
    ["accepted", "accepted_at", [{ agentSessionId: SESSION, type: "thought", body: "Sent to Desk. A session starts there now." }]],
    ["expired", "expired_at", []],
  ])("a retry after the computer's delivery was %s does not send it again", async (_n, column, said) => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const { me, pc } = await linkedComputerWithRoute();
    const hook = signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION }));
    linearFake.refuse = 500;
    linearFake.refuseOnly = "agentActivityCreate";
    expect((await send(hook)).status).toBe(500);
    const first = await pc.inbox.control();
    // What accepting (or the week running out) does: the row says so and the relay forgets it.
    await env.DB.prepare(`UPDATE linear_deliveries SET ${column} = ? WHERE id = ?`).bind(nowSeconds(), first["id"]).run();
    await dropDelivery(env, me.label, first["id"] as string);

    linearFake.refuse = null;
    linearFake.refuseOnly = null;
    expect((await send(hook)).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(linearFake.activities()).toEqual(said);
    const [row] = await deliveries();
    expect(row!["announced_at"]).toBeTypeOf("number");
    // The same webhook again changes nothing.
    expect((await send(hook)).status).toBe(200);
    expect(linearFake.activities()).toEqual(said);
  });

  it.each([
    ["Linear is down", () => (linearFake.down = true), "502"],
    ["Linear answers 500", () => (linearFake.refuse = 500), "500"],
    ["Linear answers 503", () => (linearFake.refuse = 503), "503"],
    ["Linear limits the app", () => (linearFake.refuse = 429), "429"],
  ])("answers 500 so Linear retries when %s", async (_n, fail, why) => {
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    const { pc } = await linkedComputerWithRoute();
    fail();
    const res = await send(signedWebhook(body));
    expect(res.status).toBe(500);
    expect(await res.json()).toEqual({ error: "internal error" });
    expect(log.mock.calls).toEqual([[`POST /linear/webhook: ${why}`]]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("answers 500 so Linear retries when the database fails", async () => {
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    const broken = { ...env, DB: { prepare: () => { throw new TypeError("D1 is down"); }, batch: () => { throw new TypeError("D1 is down"); } } } as unknown as typeof env;
    const hook = await signedWebhook(body);
    const res = await webhook(new Request("https://teams.test/linear/webhook", { method: "POST", headers: hook.headers, body: hook.text }), broken);
    expect(res.status).toBe(500);
    expect(log.mock.calls).toEqual([["POST /linear/webhook: TypeError"]]);
  });

  it.each([
    ["the workspace never installed the app", async () => void (await env.DB.prepare("DELETE FROM linear_workspaces").run()), "404"],
    ["Linear refuses the token even after a refresh", async () => void (linearFake.expireAlways = true), "401"],
    [
      "the app was removed, so the refresh is refused too",
      async () => {
        linearFake.expireAlways = true;
        linearFake.refreshTokens.clear();
      },
      "401",
    ],
    ["Linear forbids the call", async () => void (linearFake.refuse = 403), "403"],
    [
      "the stored token will not open",
      async () => void (await env.DB.prepare("UPDATE linear_workspaces SET access_token = 'v1.AAAA.AAAA'").run()),
      "TokenUnopened",
    ],
  ])("answers 200 when a retry cannot help: %s", async (_n, fail, why) => {
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    const { pc } = await linkedComputerWithRoute();
    await fail();
    const res = await send(signedWebhook(body));
    expect(res.status).toBe(200);
    expect(log.mock.calls).toEqual([[`POST /linear/webhook: ${why}`]]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(0);
  });

  it("answers 200 when a follow-up's workspace is gone", async () => {
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    const { pc } = await delegatedIssue();
    await env.DB.prepare("DELETE FROM linear_workspaces").run();
    // A stranger's words: the refusal is said in the session, which needs the workspace.
    expect((await send(signedWebhook(prompted({ userId: OTHER, body: "x", agentSessionId: SESSION })))).status).toBe(200);
    expect(log.mock.calls).toEqual([["POST /linear/webhook: 404"]]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it.each([
    ["no creator", created({ creatorId: null, issueId: ISSUE, agentSessionId: SESSION }), "Arakun starts work only for a person. Delegate this issue yourself."],
    ["no issue", created({ creatorId: LU, issueId: null, agentSessionId: SESSION }), "Arakun works on issues. Mention it on an issue instead."],
    [
      "an unlinked creator",
      created({ creatorId: OTHER, issueId: ISSUE, agentSessionId: SESSION }),
      'The person who delegated this has not connected Linear in Arakun. In Arakun, open the Linear tab and turn on "Let Linear start sessions on this computer".',
    ],
    [
      "no route",
      created({ creatorId: LU, issueId: UNMAPPED, agentSessionId: SESSION }),
      "No computer is set up for this team. In Arakun's Linear tab, map the team or its project to a project on your computer.",
    ],
  ])("answers %s with an error in the issue and stores nothing", async (_n, b, text) => {
    await linkedComputerWithRoute();
    expect((await send(signedWebhook(b))).status).toBe(200);
    expect(linearFake.activities()).toEqual([{ agentSessionId: SESSION, type: "error", body: text }]);
    expect(await count("linear_deliveries")).toBe(0);
  });

  it("answers no route for an issue the app cannot see", async () => {
    await linkedComputerWithRoute();
    linearFake.malformed = "issue-null";
    expect((await send(signedWebhook(body))).status).toBe(200);
    expect(linearFake.activities()).toEqual([
      { agentSessionId: SESSION, type: "error", body: "No computer is set up for this team. In Arakun's Linear tab, map the team or its project to a project on your computer." },
    ]);
    expect(await count("linear_deliveries")).toBe(0);
  });

  it.each([
    ["blocked", "UPDATE accounts SET blocked_at = 1 WHERE id = ?"],
    ["without the flag", "DELETE FROM user_flags WHERE account_id = ?"],
  ])("treats a creator whose account is %s as not linked", async (_n, sql) => {
    const { owner } = await linkedComputerWithRoute();
    await env.DB.prepare(sql).bind(owner.account_id).run();
    expect((await send(signedWebhook(body))).status).toBe(200);
    expect(linearFake.activities()).toEqual([
      {
        agentSessionId: SESSION,
        type: "error",
        body: 'The person who delegated this has not connected Linear in Arakun. In Arakun, open the Linear tab and turn on "Let Linear start sessions on this computer".',
      },
    ]);
    expect(await count("linear_deliveries")).toBe(0);
  });

  it("refuses a creator without a linked GitHub account", async () => {
    const { owner } = await linkedComputerWithRoute();
    // As if the account were made by email: no GitHub identity.
    await env.DB.prepare("DELETE FROM identities WHERE account_id = ? AND provider = 'github'").bind(owner.account_id).run();
    expect((await send(signedWebhook(body))).status).toBe(200);
    expect(linearFake.activities()).toEqual([
      {
        agentSessionId: SESSION,
        type: "error",
        body: "The person who delegated this has no GitHub account linked to their Arakun account, which Linear delegation needs. In Arakun, open Settings › Account and link GitHub.",
      },
    ]);
    expect(await count("linear_deliveries")).toBe(0);
  });

  it("says the queue is full and keeps nothing", async () => {
    const { me, pc } = await linkedComputerWithRoute();
    await away(me.label, pc);
    const waiting = () => ({ id: crypto.randomUUID().replaceAll("-", ""), kind: "created" as const, agent_session: SESSION, issue: ISSUE, organization: ORG, activity: null, created: nowSeconds() });
    for (let i = 0; i < 50; i++) await deliver(env, me.label, waiting(), null);
    expect((await send(signedWebhook(body))).status).toBe(200);
    expect(linearFake.activities()).toEqual([
      { agentSessionId: SESSION, type: "error", body: "Too many requests are waiting for Desk. Try again when it has caught up." },
    ]);
    expect(await count("linear_deliveries")).toBe(0);
  });

  it("ignores other events and actions", async () => {
    await linkedComputerWithRoute();
    const other = await signedWebhook(body);
    other.headers["Linear-Event"] = "Issue";
    expect((await send(other)).status).toBe(200);
    expect((await send(signedWebhook({ ...body, action: "updated" }))).status).toBe(200);
    expect(linearFake.calls).toEqual([]);
    expect(await count("linear_deliveries")).toBe(0);
  });

  it.each([
    ["organizationId", { ...body, organizationId: "org" }],
    ["agentSession.id", { ...body, agentSession: { ...(body["agentSession"] as object), id: "s" } }],
    ["agentSession", { ...body, agentSession: null }],
  ])("refuses a bad %s", async (_n, b) => {
    const res = await send(signedWebhook(b));
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "bad webhook" });
  });

  it("does not read webhookId: it names the webhook, not the event", async () => {
    const { pc } = await linkedComputerWithRoute();
    const { webhookId: _gone, ...without } = created({ creatorId: LU, issueId: ISSUE });
    expect((await send(signedWebhook(without))).status).toBe(200);
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, webhookId: "7" })))).status).toBe(200);
    expect((await pc.inbox.control())["kind"]).toBe("created");
    expect((await pc.inbox.control())["kind"]).toBe("created");
  });

  it("refuses a signed body that is not JSON", async () => {
    const hook = await signedWebhook(body);
    const key = await crypto.subtle.importKey("raw", new TextEncoder().encode("lin-hook"), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    const text = "{not json";
    const mac = new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(text)));
    const sig = Array.from(mac, (b) => b.toString(16).padStart(2, "0")).join("");
    const res = await send({ text, headers: { ...hook.headers, "Linear-Signature": sig } });
    expect(res.status).toBe(400);
  });

  it("never delivers to another account's computer", async () => {
    const mine = await linkedComputerWithRoute({ team: TEAM });
    const theirs = await linkedComputerWithRoute({ team: TEAM, linearUser: OTHER });
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(200);
    expect(await mine.pc.inbox.control()).toMatchObject({ type: "linear", issue: ISSUE });
    await expect(theirs.pc.inbox.next(200)).rejects.toThrow();
    const [row] = await deliveries();
    expect(row).toMatchObject({ account_id: mine.owner.account_id, label: mine.me.label });
  });
});

const ACT = "77777777-7777-4777-8777-777777777777";
const ACT2 = "77777777-7777-4777-8777-888888888888";
const SENT = { agentSessionId: SESSION, type: "thought", body: "Sent to Desk. A session starts there now." };
const SOMEONE_ELSE = { agentSessionId: SESSION, type: "thought", body: "Only the person who delegated this can steer it." };

/// A delegated issue the computer has heard of: the created delivery's id with it.
async function delegatedIssue(opts: Parameters<typeof linkedComputerWithRoute>[0] = {}) {
  const got = await linkedComputerWithRoute(opts);
  expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, promptContext: "SECRET-CONTEXT", agentSessionId: SESSION })))).status).toBe(200);
  const frame = await got.pc.inbox.control();
  return { ...got, id: frame["id"] as string };
}

/// Everything the label's object keeps, as JSON.
async function storedAll(label: string): Promise<string> {
  const stub = env.RELAY.get(env.RELAY.idFromName(label));
  return runInDurableObject(stub, async (_i, state) => JSON.stringify(Object.fromEntries(await state.storage.list())));
}

/// Another account with the flag: a stranger to the delegated issue.
async function stranger() {
  const s = await account();
  await call("PUT", `/admin/users/${s.account_id}/flags/linear`, ADMIN);
  return s;
}

describe("follow-ups from Linear", () => {
  it("sends the delegator's follow-up to the same computer with its text, and stores no text", async () => {
    const { me, pc, owner, id } = await delegatedIssue();
    expect((await send(signedWebhook(prompted({ userId: LU, body: "SECRET-FOLLOW-UP", activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    const frame = await pc.inbox.control();
    expect(frame).toMatchObject({ type: "linear", kind: "prompt", agent_session: SESSION, issue: ISSUE, organization: ORG, activity: ACT, body: "SECRET-FOLLOW-UP" });
    expect(frame["id"]).not.toBe(id);
    // A follow-up says nothing in the session itself.
    expect(linearFake.activities()).toEqual([SENT]);
    const rows = await env.DB.prepare("SELECT * FROM linear_deliveries WHERE kind = 'prompt'").all<Record<string, unknown>>();
    expect(rows.results).toEqual([
      expect.objectContaining({ id: frame["id"], agent_session_id: SESSION, organization_id: ORG, issue_id: ISSUE, activity_id: ACT, account_id: owner.account_id, label: me.label, accepted_at: null, expired_at: null }),
    ]);
    expect(rows.results[0]!["announced_at"]).toBeTypeOf("number");
    const everything = (await dump(env.DB)) + (await storedAll(me.label));
    expect(everything).toContain(ACT);
    expect(everything).not.toMatch(/SECRET-CONTEXT|SECRET-FOLLOW-UP/);
  });

  it("reads the text where Linear's schema puts it, in the activity's content", async () => {
    const { pc } = await delegatedIssue();
    const b = prompted({ userId: LU, body: "make it red", activityId: ACT, agentSessionId: SESSION });
    delete (b["agentActivity"] as Record<string, unknown>)["body"];
    expect((await send(signedWebhook(b))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "prompt", body: "make it red" });
  });

  it("passes a text over 16,000 characters as null, never cut", async () => {
    const { pc } = await delegatedIssue();
    expect((await send(signedWebhook(prompted({ userId: LU, body: "x".repeat(16_001), activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "prompt", activity: ACT, body: null });
    expect((await send(signedWebhook(prompted({ userId: LU, body: "y".repeat(16_000), agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "prompt", body: "y".repeat(16_000) });
  });

  it("a stop arrives as a stop with no text", async () => {
    const { pc, me } = await delegatedIssue();
    expect((await send(signedWebhook(prompted({ userId: LU, body: "Stop", activityId: ACT, signal: "stop", agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ type: "linear", kind: "stop", agent_session: SESSION, activity: ACT, body: null });
    expect(linearFake.activities()).toEqual([SENT]);
    const rows = await env.DB.prepare("SELECT kind, label, activity_id FROM linear_deliveries ORDER BY kind").all();
    expect(rows.results).toEqual([
      { kind: "created", label: me.label, activity_id: null },
      { kind: "stop", label: me.label, activity_id: ACT },
    ]);
  });

  it.each([
    ["linked to another account", OTHER, true],
    ["not linked", OTHER, false],
  ])("someone else's follow-up never reaches the computer (%s)", async (_n, author, linkIt) => {
    const { pc } = await delegatedIssue();
    const theirs = linkIt ? await linkedComputerWithRoute({ linearUser: author }) : null;
    expect((await send(signedWebhook(prompted({ userId: author, body: "do this instead", activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, SOMEONE_ELSE]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    if (theirs) await expect(theirs.pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("a stop from someone else is refused", async () => {
    const { pc } = await delegatedIssue();
    await linkedComputerWithRoute({ linearUser: OTHER });
    expect((await send(signedWebhook(prompted({ userId: OTHER, body: "Stop", signal: "stop", agentSessionId: SESSION })))).status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, SOMEONE_ELSE]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("a Linear user linked to the delegator's account in another workspace cannot steer", async () => {
    const { pc, owner } = await delegatedIssue();
    // OTHER is the delegator's account too, but only in another workspace.
    await env.DB.prepare("INSERT INTO linear_links (organization_id, linear_user_id, account_id, linked_at) VALUES (?, ?, ?, ?)")
      .bind(ORG2, OTHER, owner.account_id, nowSeconds())
      .run();
    expect((await send(signedWebhook(prompted({ userId: OTHER, body: "do this instead", agentSessionId: SESSION })))).status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, SOMEONE_ELSE]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("a follow-up from another workspace finds no session", async () => {
    const { pc, owner } = await delegatedIssue();
    // The other workspace has the app too, so the answer can be said there.
    linearFake.tokens.set("lin_oauth_ws2", { userId: "app-user", organizationId: ORG2, organizationName: "Other" });
    await saveWorkspace(env, { organizationId: ORG2, name: "Other", appUserId: "app-user", access: "lin_oauth_ws2", refresh: "lin_refresh_ws2", expiresAt: nowSeconds() + 86_400, installedBy: owner.account_id });
    await env.DB.prepare("INSERT INTO linear_links (organization_id, linear_user_id, account_id, linked_at) VALUES (?, ?, ?, ?)")
      .bind(ORG2, LU, owner.account_id, nowSeconds())
      .run();
    const res = await send(signedWebhook(prompted({ userId: LU, body: "do this instead", organizationId: ORG2, agentSessionId: SESSION })));
    expect(res.status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, { agentSessionId: SESSION, type: "error", body: "Arakun has no session for this. Delegate the issue again." }]);
    expect(linearFake.accepted.at(-1)!.token).toBe("lin_oauth_ws2");
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("a follow-up with no readable author is treated as someone else's", async () => {
    const { pc } = await delegatedIssue();
    expect((await send(signedWebhook(prompted({ userId: null, body: "do this instead", activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    const odd = prompted({ userId: null, body: "do this instead", activityId: ACT, agentSessionId: SESSION });
    (odd["agentActivity"] as Record<string, unknown>)["userId"] = 7;
    expect((await send(signedWebhook(odd))).status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, SOMEONE_ELSE, SOMEONE_ELSE]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("a follow-up for a session we never saw gets an error", async () => {
    const { pc, id } = await delegatedIssue();
    const unknown = { agentSessionId: SESSION, type: "error", body: "Arakun has no session for this. Delegate the issue again." };
    const OTHER_SESSION = "55555555-5555-4555-8555-000000000000";
    const res = await send(signedWebhook(prompted({ userId: LU, body: "hello", activityId: ACT, agentSessionId: OTHER_SESSION })));
    expect(res.status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, { ...unknown, agentSessionId: OTHER_SESSION }]);
    // A delivery that ran out of time is no session either.
    await env.DB.prepare("UPDATE linear_deliveries SET expired_at = ? WHERE id = ?").bind(nowSeconds(), id).run();
    expect((await send(signedWebhook(prompted({ userId: LU, body: "hello", activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, { ...unknown, agentSessionId: OTHER_SESSION }, unknown]);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("goes to the delegator's computer even before it took the issue", async () => {
    const { me, pc, owner } = await linkedComputerWithRoute();
    await away(me.label, pc);
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(200);
    expect((await send(signedWebhook(prompted({ userId: LU, body: "SECRET-FOLLOW-UP", activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    expect(await count("linear_deliveries")).toBe(2);
    const back = await connectedComputerAgain(me, owner.session);
    const kinds = [(await back.inbox.control())["kind"], (await back.inbox.control())["kind"]].sort();
    expect(kinds).toEqual(["created", "prompt"]);
    expect(await storedAll(me.label)).not.toContain("SECRET-FOLLOW-UP");
  });

  it("says so when a reply or a stop waits for an offline computer", async () => {
    const { me, pc, owner } = await delegatedIssue();
    await away(me.label, pc);
    const reply = signedWebhook(prompted({ userId: LU, body: "SECRET-FOLLOW-UP", activityId: ACT, agentSessionId: SESSION }));
    expect((await send(reply)).status).toBe(200);
    const waits = { agentSessionId: SESSION, type: "thought", body: "Desk is offline. Your message reaches the session when it is back." };
    expect(linearFake.activities()).toEqual([SENT, waits]);
    // Linear's retry of the same reply says it once.
    expect((await send(reply)).status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, waits]);
    expect((await send(signedWebhook(prompted({ userId: LU, body: "Stop", activityId: ACT2, signal: "stop", agentSessionId: SESSION })))).status).toBe(200);
    const stops = { agentSessionId: SESSION, type: "response", body: "Desk is offline. The session there is stopped as soon as it is back." };
    expect(linearFake.activities()).toEqual([SENT, waits, stops]);
    // Both still reach the computer.
    const back = await connectedComputerAgain(me, owner.session);
    const kinds = [(await back.inbox.control())["kind"], (await back.inbox.control())["kind"], (await back.inbox.control())["kind"]].sort();
    expect(kinds).toEqual(["created", "prompt", "stop"]);
  });

  it("the same follow-up twice is one delivery and one frame", async () => {
    const { pc } = await delegatedIssue();
    const hook = signedWebhook(prompted({ userId: LU, body: "make it red", activityId: ACT, agentSessionId: SESSION }));
    expect((await send(hook)).status).toBe(200);
    expect((await send(hook)).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "prompt" });
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(2);
    expect(linearFake.activities()).toEqual([SENT]);
  });

  it("two follow-ups in one session are two deliveries", async () => {
    const { pc } = await delegatedIssue();
    expect((await send(signedWebhook(prompted({ userId: LU, body: "make it red", agentSessionId: SESSION })))).status).toBe(200);
    expect((await send(signedWebhook(prompted({ userId: LU, body: "and round", agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "prompt", body: "make it red" });
    expect(await pc.inbox.control()).toMatchObject({ kind: "prompt", body: "and round" });
    expect(await count("linear_deliveries")).toBe(3);
  });

  it("a follow-up by another person never finishes or reuses someone else's delivery", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const mine = await linkedComputerWithRoute();
    const theirs = await linkedComputerWithRoute({ linearUser: OTHER });
    // My delegation reaches my computer, but the activity fails: the row stays
    // unannounced, and the relay then loses it.
    linearFake.refuse = 500;
    linearFake.refuseOnly = "agentActivityCreate";
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(500);
    const first = await mine.pc.inbox.control();
    await dropDelivery(env, mine.me.label, first["id"] as string);
    linearFake.refuse = null;
    linearFake.refuseOnly = null;
    // Someone else delegates and follows up, through the same webhook.
    const THEIRS = crypto.randomUUID();
    expect((await send(signedWebhook(created({ creatorId: OTHER, issueId: ISSUE, agentSessionId: THEIRS })))).status).toBe(200);
    expect((await send(signedWebhook(prompted({ userId: OTHER, body: "THEIR-WORDS", agentSessionId: THEIRS })))).status).toBe(200);
    expect(await theirs.pc.inbox.control()).toMatchObject({ kind: "created", agent_session: THEIRS });
    expect(await theirs.pc.inbox.control()).toMatchObject({ kind: "prompt", agent_session: THEIRS, body: "THEIR-WORDS" });
    await expect(mine.pc.inbox.next(200)).rejects.toThrow();
    const row = await env.DB.prepare("SELECT announced_at FROM linear_deliveries WHERE id = ?").bind(first["id"]).first<{ announced_at: number | null }>();
    expect(row!.announced_at).toBeNull();
  });

  it("refuses to finish a row the event key finds for another session", async () => {
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    const { pc, me, owner } = await delegatedIssue();
    // Unreachable with keys made of the event's own ids: a guard.
    await env.DB.prepare(
      `INSERT INTO linear_deliveries (id, event_key, agent_session_id, organization_id, issue_id, kind, activity_id, account_id, label, created_at)
       VALUES (?, ?, ?, ?, ?, 'prompt', ?, ?, ?, ?)`,
    )
      .bind("e".repeat(32), `activity:${ACT}`, crypto.randomUUID(), ORG, ISSUE, ACT, owner.account_id, me.label, nowSeconds())
      .run();
    expect((await send(signedWebhook(prompted({ userId: LU, body: "make it red", activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(log.mock.calls).toEqual([["POST /linear/webhook: delivery mismatch"]]);
    expect(linearFake.activities()).toEqual([SENT]);
  });

  it("a retry of a follow-up the computer already took does not send it again", async () => {
    const { me, pc } = await delegatedIssue();
    const b = prompted({ userId: LU, body: "make it red", activityId: ACT, agentSessionId: SESSION });
    expect((await send(signedWebhook(b))).status).toBe(200);
    const first = await pc.inbox.control();
    // As if the first try died before it marked the row announced.
    await env.DB.prepare("UPDATE linear_deliveries SET announced_at = NULL, accepted_at = ? WHERE id = ?").bind(nowSeconds(), first["id"]).run();
    await dropDelivery(env, me.label, first["id"] as string);
    expect((await send(signedWebhook(b))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    const row = await env.DB.prepare("SELECT announced_at FROM linear_deliveries WHERE id = ?").bind(first["id"]).first<{ announced_at: number | null }>();
    expect(row!.announced_at).toBeTypeOf("number");
  });

  it("says the queue is full and keeps no follow-up", async () => {
    const { me, pc } = await delegatedIssue();
    await away(me.label, pc);
    // The created delivery is the first of the 50 the relay keeps.
    const waiting = () => ({ id: crypto.randomUUID().replaceAll("-", ""), kind: "created" as const, agent_session: SESSION, issue: ISSUE, organization: ORG, activity: null, created: nowSeconds() });
    for (let i = 0; i < 49; i++) await deliver(env, me.label, waiting(), null);
    expect((await send(signedWebhook(prompted({ userId: LU, body: "make it red", activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT, { agentSessionId: SESSION, type: "error", body: "Too many requests are waiting for Desk. Try again when it has caught up." }]);
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("refuses a follow-up without an activity id", async () => {
    await delegatedIssue();
    const res = await send(signedWebhook(prompted({ userId: LU, body: "make it red", activityId: "not-a-uuid", agentSessionId: SESSION })));
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "bad webhook" });
    expect(await count("linear_deliveries")).toBe(1);
  });
});

describe("stops Linear did not ask for", () => {
  const unassigned = (n: Partial<Parameters<typeof notification>[0]> = {}) => notification({ action: "issueUnassignedFromYou", issueId: ISSUE, ...n });
  const statusChanged = (n: Partial<Parameters<typeof notification>[0]> = {}) => notification({ action: "issueStatusChanged", issueId: ISSUE, ...n });
  const setState = (stateType: string) => linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: PROJECT, stateType });

  it("stops the session when Arakun is unassigned, from anyone, and says nothing in Linear", async () => {
    const { pc, me } = await delegatedIssue();
    expect((await send(signedWebhook(unassigned({ actorId: OTHER })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({
      type: "linear",
      kind: "stop",
      reason: "unassigned",
      agent_session: SESSION,
      issue: ISSUE,
      organization: ORG,
      activity: null,
      body: null,
    });
    expect(linearFake.activities()).toEqual([SENT]);
    const rows = await env.DB.prepare("SELECT kind, reason, label, team_id FROM linear_deliveries ORDER BY kind").all();
    expect(rows.results).toEqual([
      { kind: "created", reason: null, label: me.label, team_id: TEAM },
      { kind: "stop", reason: "unassigned", label: me.label, team_id: null },
    ]);
  });

  it("the same notification twice is one stop", async () => {
    const { pc } = await delegatedIssue();
    const n = unassigned();
    expect((await send(signedWebhook(n))).status).toBe(200);
    expect((await send(signedWebhook(n))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "stop" });
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("stops the newest session of the issue only", async () => {
    const { pc } = await delegatedIssue();
    const NEWER = crypto.randomUUID();
    await env.DB.prepare("UPDATE linear_deliveries SET created_at = created_at - 10").run();
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: NEWER })))).status).toBe(200);
    await pc.inbox.control();
    expect((await send(signedWebhook(unassigned()))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "stop", agent_session: NEWER });
  });

  it.each([
    ["canceled, or a duplicate", "canceled", "22222222-2222-4222-8222-222222222222", "canceled"],
    ["done by a person", "completed", "22222222-2222-4222-8222-222222222222", "completed"],
  ])("stops the session when the issue is %s", async (_n, state, actorId, reason) => {
    const { pc } = await delegatedIssue();
    setState(state);
    expect((await send(signedWebhook(statusChanged({ actorId })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "stop", reason, agent_session: SESSION });
    expect(linearFake.calls.at(-1)!.query).toContain("state { type }");
  });

  it.each([
    ["done by an integration, such as a merged pull request", "completed", null],
    ["done by Arakun itself", "completed", "0a0a0a0a-0a0a-4a0a-8a0a-0a0a0a0a0a0a"],
    ["moved to another open state", "started", "22222222-2222-4222-8222-222222222222"],
  ])("leaves the session alone when the issue is %s", async (_n, state, actorId) => {
    const { pc } = await delegatedIssue();
    setState(state);
    expect((await send(signedWebhook(statusChanged({ actorId })))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("does nothing for an issue with no session, or one whose delegation ran out", async () => {
    const { pc, id } = await delegatedIssue();
    expect((await send(signedWebhook(unassigned({ issueId: UNMAPPED })))).status).toBe(200);
    await env.DB.prepare("UPDATE linear_deliveries SET expired_at = ? WHERE id = ?").bind(nowSeconds(), id).run();
    expect((await send(signedWebhook(unassigned()))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await count("linear_deliveries")).toBe(1);
  });

  it("an issue the app can no longer see is left to the access change", async () => {
    const { pc } = await delegatedIssue();
    linearFake.malformed = "issue-null";
    expect((await send(signedWebhook(statusChanged()))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("an offline computer gets the stop when it is back", async () => {
    const { me, pc, owner } = await delegatedIssue();
    await away(me.label, pc);
    expect((await send(signedWebhook(unassigned()))).status).toBe(200);
    expect(linearFake.activities()).toEqual([SENT]);
    const back = await connectedComputerAgain(me, owner.session);
    const frames = [await back.inbox.control(), await back.inbox.control()];
    expect(frames.find((f) => f["kind"] === "stop")).toMatchObject({ reason: "unassigned", agent_session: SESSION });
  });

  it("stops the sessions of a team Arakun lost, and no others", async () => {
    const { pc } = await delegatedIssue();
    const change = (removedTeamIds: string[]) => ({
      type: "PermissionChange",
      action: "teamAccessChanged",
      organizationId: ORG,
      oauthClientId: "lin-test",
      appUserId: "app-user",
      canAccessAllPublicTeams: false,
      addedTeamIds: [],
      removedTeamIds,
    });
    expect((await send(signedWebhook(change([TEAM2])))).status).toBe(200);
    expect((await send(signedWebhook(change([])))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect((await send(signedWebhook(change([TEAM2, TEAM])))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "stop", reason: "team_access", agent_session: SESSION });
    // Linear's retry stops nothing twice.
    expect((await send(signedWebhook(change([TEAM])))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect((await send(signedWebhook(change(["team"])))).status).toBe(400);
  });

  it("when the app is removed, stops its sessions and shows the workspace as not installed", async () => {
    const { pc, owner } = await delegatedIssue();
    const removed = { type: "OAuthApp", action: "revoked", organizationId: ORG, oauthClientId: "lin-test" };
    expect((await send(signedWebhook(removed))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "stop", reason: "revoked", agent_session: SESSION });
    expect(await count("linear_workspaces")).toBe(0);
    const links = (await (await call("GET", "/linear/link", as(owner.session))).json()) as { links: { installed: boolean }[] };
    expect(links.links).toEqual([expect.objectContaining({ organization_id: ORG, installed: false })]);
    // A retry after the tokens are gone still answers 200 and sends nothing again.
    expect((await send(signedWebhook(removed))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("a workspace quiet for weeks still takes a delegation", async () => {
    const { pc } = await linkedComputerWithRoute({ team: TEAM });
    linearFake.refreshTokens.set("lin_refresh_ws", ACCESS);
    await env.DB.prepare("UPDATE linear_workspaces SET refreshed_at = ?").bind(nowSeconds() - KEEP_SECONDS - 60).run();
    await keepWorkspaces(env);
    // Weeks on: the pair from the install is long gone, and the access token
    // from the keep-alive has run out too. Its refresh token is what is left.
    linearFake.grace = false;
    await env.DB.prepare("UPDATE linear_workspaces SET expires_at = ?").bind(nowSeconds() - 1).run();
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ type: "linear", kind: "created", agent_session: SESSION });
    expect(linearFake.exchanges.filter((f) => f.get("grant_type") === "refresh_token")).toHaveLength(2);
  });

  it("when Linear refuses the refresh token, shows the workspace as not installed until it is installed again", async () => {
    const { owner } = await linkedComputerWithRoute({ team: TEAM });
    const installed = async () =>
      ((await (await call("GET", "/linear/link", as(owner.session))).json()) as { links: { installed: boolean }[] }).links.map((l) => l.installed);
    expect(await installed()).toEqual([true]);
    // The app was removed without the webhook reaching us, or the token ran out.
    linearFake.tokens.clear();
    linearFake.refreshTokens.clear();
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, agentSessionId: SESSION })))).status).toBe(200);
    expect(await installed()).toEqual([false]);
    // Proving the link again says the same.
    linearFake.tokens.set("lin_api_lu", { userId: LU, organizationId: ORG, organizationName: "Acme" });
    expect(await (await call("POST", "/linear/link", as(owner.session), JSON.stringify({ token: "lin_api_lu" }))).json()).toMatchObject({ installed: false });
    await workspace(owner.account_id);
    expect(await installed()).toEqual([true]);
  });

  it("refuses a notification without its ids", async () => {
    await delegatedIssue();
    const n = unassigned();
    expect((await send(signedWebhook({ ...n, notification: { ...(n["notification"] as object), issueId: "x" } }))).status).toBe(400);
    expect((await send(signedWebhook({ ...n, organizationId: "org" }))).status).toBe(400);
  });

  it("ignores the notifications it does not act on", async () => {
    const { pc } = await delegatedIssue();
    expect((await send(signedWebhook(notification({ action: "issueNewComment", issueId: ISSUE })))).status).toBe(200);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("the relay takes a reason on a stop only", async () => {
    const { me } = await linkedComputerWithRoute();
    const d = { id: "a".repeat(32), kind: "created" as const, agent_session: SESSION, issue: ISSUE, organization: ORG, activity: null, created: nowSeconds() };
    await expect(deliver(env, me.label, { ...d, reason: "unassigned" }, null)).rejects.toThrow(/400/);
    await expect(deliver(env, me.label, { ...d, kind: "stop", reason: "bored" as never }, null)).rejects.toThrow(/400/);
    await expect(deliver(env, me.label, { ...d, kind: "stop", reason: "canceled" }, null)).resolves.toEqual({ online: true });
  });
});

describe("what the computer sends back", () => {
  const accept = (id: string, session: string) => call("POST", `/linear/deliveries/${id}/accept`, as(session));
  const activity = (session: string, body: unknown, agentSession = SESSION) =>
    call("POST", `/linear/sessions/${agentSession}/activity`, as(session), typeof body === "string" ? body : JSON.stringify(body));
  const accepted = async (id: string) =>
    (await env.DB.prepare("SELECT accepted_at FROM linear_deliveries WHERE id = ?").bind(id).first<{ accepted_at: number | null }>())?.accepted_at;

  it("accepting removes the delivery from the relay, once or twice, and only for its owner", async () => {
    const { me, owner, id } = await delegatedIssue();
    const other = await stranger();
    expect(await stored(me.label, `linear:${id}`)).toBeDefined();
    const notYours = await accept(id, other.session);
    expect(notYours.status).toBe(404);
    expect(await notYours.json()).toEqual({ error: "no such delivery" });
    expect(await stored(me.label, `linear:${id}`)).toBeDefined();
    expect(await accepted(id)).toBeNull();

    expect((await accept(id, owner.session)).status).toBe(204);
    expect(await stored(me.label, `linear:${id}`)).toBeUndefined();
    const at = await accepted(id);
    expect(at).toBeTypeOf("number");
    // The first time stays.
    await env.DB.prepare("UPDATE linear_deliveries SET accepted_at = 1 WHERE id = ?").bind(id).run();
    expect((await accept(id, owner.session)).status).toBe(204);
    expect(await accepted(id)).toBe(1);
    expect((await accept(id, other.session)).status).toBe(404);
  });

  it("accepts an expired delivery, and refuses an unknown or malformed id", async () => {
    const { owner, id } = await delegatedIssue();
    await env.DB.prepare("UPDATE linear_deliveries SET expired_at = ? WHERE id = ?").bind(nowSeconds(), id).run();
    expect((await accept(id, owner.session)).status).toBe(204);
    expect(await accepted(id)).toBeTypeOf("number");
    const unknown = await accept("0".repeat(32), owner.session);
    expect(unknown.status).toBe(404);
    expect(await unknown.json()).toEqual({ error: "no such delivery" });
    expect((await accept(id.toUpperCase(), owner.session)).status).toBe(404);
    expect((await accept(id.slice(1), owner.session)).status).toBe(404);
  });

  it("needs a session with the flag for both routes", async () => {
    const { id } = await delegatedIssue();
    expect((await call("POST", `/linear/deliveries/${id}/accept`)).status).toBe(401);
    expect((await call("POST", `/linear/sessions/${SESSION}/activity`, {}, JSON.stringify({ type: "thought", body: "x" }))).status).toBe(401);
    const unflagged = await account();
    expect((await accept(id, unflagged.session)).status).toBe(403);
    expect((await activity(unflagged.session, { type: "thought", body: "x" })).status).toBe(403);
    expect(await accepted(id)).toBeNull();
    expect(linearFake.activities()).toEqual([SENT]);
  });

  it("posts the computer's activity as Arakun, with links", async () => {
    const { owner } = await delegatedIssue();
    const res = await activity(owner.session, {
      type: "response",
      body: "Done.",
      links: [{ label: "Pull request", url: "https://github.com/a/b/pull/1" }],
      extra: "ignored",
    });
    expect(res.status).toBe(204);
    expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "response", body: "Done." });
    expect(linearFake.links()).toEqual([{ agentSessionId: SESSION, label: "Pull request", url: "https://github.com/a/b/pull/1" }]);
    // Without links, no session update.
    expect((await activity(owner.session, { type: "thought", body: "Working on it." })).status).toBe(204);
    expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "thought", body: "Working on it." });
    expect(linearFake.links()).toHaveLength(1);
    expect(await dump(env.DB)).not.toMatch(/Done\.|Working on it/);
  });

  it("passes a question's choices on as a select, and only for a question", async () => {
    const { owner } = await delegatedIssue();
    const ask = { type: "elicitation", body: "Which database?", options: ["Postgres", "SQLite"] };
    expect((await activity(owner.session, ask)).status).toBe(204);
    expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "elicitation", body: "Which database?", signal: "select", options: ["Postgres", "SQLite"] });
    // Without choices it is a plain question.
    expect((await activity(owner.session, { type: "elicitation", body: "Go on?", options: [] })).status).toBe(204);
    expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "elicitation", body: "Go on?" });
    for (const bad of [
      { ...ask, type: "thought" },
      { ...ask, options: ["Only one"] },
      { ...ask, options: ["a", " "] },
      { ...ask, options: ["a", 2] },
      { ...ask, options: ["a", "x".repeat(81)] },
      { ...ask, options: Array(11).fill("a") },
      { ...ask, options: "Postgres" },
    ]) {
      expect((await activity(owner.session, bad)).status).toBe(400);
    }
    expect(await dump(env.DB)).not.toMatch(/Postgres|Which database/);
  });

  it("adds links to a session without an activity, only for its owner and only https", async () => {
    const { owner } = await delegatedIssue();
    const links = (session: string, body: unknown) => call("POST", `/linear/sessions/${SESSION}/links`, as(session), JSON.stringify(body));
    const pr = { label: "Pull request", url: "https://github.com/a/b/pull/1" };
    expect((await links(owner.session, { links: [pr], extra: "ignored" })).status).toBe(204);
    expect(linearFake.links()).toEqual([{ agentSessionId: SESSION, ...pr }]);
    expect(linearFake.activities()).toEqual([SENT]);
    for (const bad of [{}, { links: [] }, { links: [{ label: "x", url: "http://github.com/a" }] }, { links: [{ label: "", url: pr.url }] }, { links: Array(6).fill(pr) }]) {
      const res = await links(owner.session, bad);
      expect(res.status).toBe(400);
      expect(await res.json()).toEqual({ error: "bad links" });
    }
    expect((await links((await stranger()).session, { links: [pr] })).status).toBe(404);
    expect((await call("POST", `/linear/sessions/${SESSION}/links`, {}, JSON.stringify({ links: [pr] }))).status).toBe(401);
    expect(linearFake.links()).toHaveLength(1);
  });

  it("sets a session's whole plan, only for its owner and only Linear's statuses", async () => {
    const { owner } = await delegatedIssue();
    const plan = (session: string, body: unknown) => call("PUT", `/linear/sessions/${SESSION}/plan`, as(session), JSON.stringify(body));
    const items = [
      { content: "Read the code", status: "completed" },
      { content: "Fix the bug", status: "inProgress", extra: "ignored" },
      { content: "Test it", status: "pending" },
    ];
    expect((await plan(owner.session, { plan: items })).status).toBe(204);
    expect(linearFake.plans()).toEqual([
      {
        agentSessionId: SESSION,
        plan: [
          { content: "Read the code", status: "completed" },
          { content: "Fix the bug", status: "inProgress" },
          { content: "Test it", status: "pending" },
        ],
      },
    ]);
    // A plan is no link and no activity.
    expect(linearFake.links()).toEqual([]);
    expect(linearFake.activities()).toEqual([SENT]);
    const one = { content: "x", status: "pending" };
    for (const bad of [
      {},
      { plan: "x" },
      { plan: [{ ...one, status: "in_progress" }] },
      { plan: [{ ...one, content: " " }] },
      { plan: [{ ...one, content: "a\nb" }] },
      { plan: [{ ...one, content: "x".repeat(301) }] },
      { plan: [{ status: "pending" }] },
      { plan: Array(51).fill(one) },
    ]) {
      const res = await plan(owner.session, bad);
      expect(res.status).toBe(400);
      expect(await res.json()).toEqual({ error: "bad plan" });
    }
    expect((await plan((await stranger()).session, { plan: [one] })).status).toBe(404);
    expect((await call("PUT", `/linear/sessions/${SESSION}/plan`, {}, JSON.stringify({ plan: [one] }))).status).toBe(401);
    expect((await call("POST", `/linear/sessions/${SESSION}/plan`, as(owner.session), JSON.stringify({ plan: [one] }))).status).toBe(404);
    expect(linearFake.plans()).toHaveLength(1);
    expect(await dump(env.DB)).not.toMatch(/Fix the bug/);
  });

  it("answers 502 when Linear refuses a plan", async () => {
    const { owner } = await delegatedIssue();
    linearFake.refuse = 400;
    linearFake.refuseOnly = "agentSessionUpdate";
    const res = await call("PUT", `/linear/sessions/${SESSION}/plan`, as(owner.session), JSON.stringify({ plan: [{ content: "x", status: "pending" }] }));
    expect(res.status).toBe(502);
  });

  it("passes an ephemeral thought on as one", async () => {
    const { owner } = await delegatedIssue();
    expect((await activity(owner.session, { type: "thought", body: "Running a command", ephemeral: true })).status).toBe(204);
    expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "thought", body: "Running a command", ephemeral: true });
    expect((await activity(owner.session, { type: "thought", body: "Still working.", ephemeral: false })).status).toBe(204);
    expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "thought", body: "Still working." });
  });

  it("passes a step on as an action, with no body", async () => {
    const { owner } = await delegatedIssue();
    const step = { type: "action", action: "Editing", parameter: "src/main.rs", ephemeral: true };
    expect((await activity(owner.session, step)).status).toBe(204);
    expect(linearFake.actions()).toEqual([{ agentSessionId: SESSION, action: "Editing", parameter: "src/main.rs", ephemeral: true }]);
    expect((await activity(owner.session, { ...step, ephemeral: false })).status).toBe(204);
    expect(linearFake.actions().at(-1)).toEqual({ agentSessionId: SESSION, action: "Editing", parameter: "src/main.rs" });
    // At the limits it goes through, counted as characters.
    expect((await activity(owner.session, { ...step, action: "é".repeat(80), parameter: "é".repeat(200) })).status).toBe(204);
    expect(linearFake.issueUpdates()).toEqual([]);
  });

  it("refuses a step that is empty, long, has a body, links or choices, or says it started", async () => {
    const { owner } = await delegatedIssue();
    const step = { type: "action", action: "Editing", parameter: "src/main.rs" };
    for (const bad of [
      { type: "action", action: "Editing" },
      { type: "action", parameter: "src/main.rs" },
      { ...step, action: "" },
      { ...step, parameter: " " },
      { ...step, action: "x".repeat(81) },
      { ...step, parameter: "x".repeat(201) },
      { ...step, parameter: "a\nb" },
      { ...step, action: 7 },
      { ...step, body: "Editing" },
      { ...step, links: [{ label: "Pull request", url: "https://github.com/a/b/pull/1" }] },
      { ...step, options: ["a", "b"] },
      { ...step, started: true },
      { ...step, ephemeral: "yes" },
    ]) {
      const res = await activity(owner.session, bad);
      expect(res.status, JSON.stringify(bad)).toBe(400);
      expect(await res.json()).toEqual({ error: "bad activity" });
    }
    expect((await activity((await stranger()).session, step)).status).toBe(404);
    expect(linearFake.actions()).toEqual([]);
  });

  it("refuses an activity for a session of another account, a bad type, an empty or oversized body, or a link that is not https", async () => {
    const { owner } = await delegatedIssue();
    const other = await stranger();
    const notYours = await activity(other.session, { type: "response", body: "Done." });
    expect(notYours.status).toBe(404);
    expect(await notYours.json()).toEqual({ error: "no such session" });
    expect((await activity(owner.session, { type: "response", body: "Done." }, "55555555-5555-4555-8555-000000000000")).status).toBe(404);
    expect((await activity(owner.session, { type: "response", body: "Done." }, "not-a-session")).status).toBe(404);
    const link = { label: "Pull request", url: "https://github.com/a/b/pull/1" };
    for (const bad of [
      { type: "prompt", body: "Done." },
      { type: "response", body: "" },
      { type: "response", body: "x".repeat(8001) },
      { type: "response", body: "Done.", links: [{ label: "Pull request", url: "http://github.com/a/b/pull/1" }] },
      { type: "response", body: 7 },
      { type: "response", body: "Done.", ephemeral: true },
      { type: "thought", body: "Working.", ephemeral: "yes" },
      { type: "response", body: "Done.", links: [{ ...link, url: "https://github.com/a b" }] },
      { type: "response", body: "Done.", links: [{ ...link, url: `https://x.dev/${"a".repeat(2048)}` }] },
      { type: "response", body: "Done.", links: [{ ...link, label: "" }] },
      { type: "response", body: "Done.", links: [{ ...link, label: "x".repeat(81) }] },
      { type: "response", body: "Done.", links: Array(6).fill(link) },
      { type: "response", body: "Done.", links: link },
      [{ type: "response", body: "Done." }],
      "{not json",
      JSON.stringify({ type: "response", body: "Done.", pad: "x".repeat(64 * 1024) }),
    ]) {
      const res = await activity(owner.session, bad);
      expect(res.status, JSON.stringify(bad).slice(0, 100)).toBe(400);
      expect(await res.json()).toEqual({ error: "bad activity" });
    }
    // At the limits it goes through: 8000 characters, counted as characters.
    expect((await activity(owner.session, { type: "response", body: "é".repeat(8000), links: Array(5).fill({ ...link, label: "x".repeat(80) }) })).status).toBe(204);
    expect(linearFake.activities()).toHaveLength(2);
  });

  it("a refusal from Linear is a 502 for the computer", async () => {
    const { owner } = await delegatedIssue();
    linearFake.refuse = 500;
    linearFake.refuseOnly = "agentActivityCreate";
    const res = await activity(owner.session, { type: "response", body: "Done." });
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({ error: "linear refused" });
    linearFake.refuse = 400;
    linearFake.refuseOnly = "agentSessionUpdate";
    const links = await activity(owner.session, { type: "response", body: "Done.", links: [{ label: "PR", url: "https://github.com/a/b/pull/1" }] });
    expect(links.status).toBe(502);
  });

  describe("the thought that says the session started", () => {
    const START = { type: "thought", body: "Started in arakun on Desk.", started: true };
    const FIRST = "77777777-7777-4777-8777-000000000001";
    const LATER = "77777777-7777-4777-8777-000000000002";
    const startedStates = () => linearFake.startedStates.set(TEAM, [{ id: LATER, position: 2 }, { id: FIRST, position: 1 }]);

    it("moves an issue not begun to the team's first started state and makes Arakun its delegate", async () => {
      const { owner } = await delegatedIssue();
      startedStates();
      for (const stateType of ["triage", "backlog", "unstarted"]) {
        linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: PROJECT, stateType });
        expect((await activity(owner.session, START)).status).toBe(204);
        expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "thought", body: START.body });
        expect(linearFake.issueUpdates().at(-1)).toEqual({ id: ISSUE, input: { stateId: FIRST, delegateId: "app-user" } });
      }
      expect(linearFake.issueUpdates()).toHaveLength(3);
    });

    it("leaves a state someone already moved it to, and a delegate already set", async () => {
      const { owner } = await delegatedIssue();
      startedStates();
      for (const stateType of ["started", "completed", "canceled"]) {
        linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: PROJECT, stateType });
        expect((await activity(owner.session, START)).status).toBe(204);
        expect(linearFake.issueUpdates().at(-1)).toEqual({ id: ISSUE, input: { delegateId: "app-user" } });
      }
      linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: PROJECT, stateType: "backlog", delegateId: "someone-else" });
      expect((await activity(owner.session, START)).status).toBe(204);
      expect(linearFake.issueUpdates().at(-1)).toEqual({ id: ISSUE, input: { stateId: FIRST } });
      const before = linearFake.issueUpdates().length;
      linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: PROJECT, stateType: "started", delegateId: "app-user" });
      expect((await activity(owner.session, START)).status).toBe(204);
      expect(linearFake.issueUpdates()).toHaveLength(before);
    });

    it("sets only the delegate when the team has no started state", async () => {
      const { owner } = await delegatedIssue();
      expect((await activity(owner.session, START)).status).toBe(204);
      expect(linearFake.issueUpdates()).toEqual([{ id: ISSUE, input: { delegateId: "app-user" } }]);
    });

    it("does not touch the issue for any other activity", async () => {
      const { owner } = await delegatedIssue();
      startedStates();
      expect((await activity(owner.session, { type: "thought", body: "Waiting on Desk." })).status).toBe(204);
      expect((await activity(owner.session, { type: "response", body: "Done.", started: false })).status).toBe(204);
      expect(linearFake.issueUpdates()).toEqual([]);
      expect(linearFake.calls.filter((c) => c.query.includes("states("))).toEqual([]);
    });

    it("is for a thought only, and not an ephemeral one", async () => {
      const { owner } = await delegatedIssue();
      for (const bad of [
        { ...START, type: "response" },
        { ...START, ephemeral: true },
        { ...START, started: "yes" },
      ]) {
        const res = await activity(owner.session, bad);
        expect(res.status).toBe(400);
        expect(await res.json()).toEqual({ error: "bad activity" });
      }
      expect(linearFake.issueUpdates()).toEqual([]);
    });

    it("still answers 204 when Linear refuses the update, and logs only the status", async () => {
      const { owner } = await delegatedIssue();
      startedStates();
      const log = vi.spyOn(console, "error").mockImplementation(() => {});
      linearFake.refuse = 403;
      linearFake.refuseOnly = "issueUpdate";
      expect((await activity(owner.session, START)).status).toBe(204);
      expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "thought", body: START.body });
      expect(log.mock.calls).toEqual([["linear start issue: 403"]]);
      log.mockRestore();
    });

    it("posts no issue update when the activity itself was refused", async () => {
      const { owner } = await delegatedIssue();
      startedStates();
      linearFake.refuse = 500;
      linearFake.refuseOnly = "agentActivityCreate";
      expect((await activity(owner.session, START)).status).toBe(502);
      expect(linearFake.calls.filter((c) => c.query.includes("states(") || c.query.includes("issueUpdate"))).toEqual([]);
    });
  });

  it("counts activities per account", async () => {
    const { owner } = await delegatedIssue();
    let last = 0;
    for (let i = 0; i < 40 && last !== 429; i++) last = (await activity(owner.session, { type: "thought", body: "x" })).status;
    expect(last).toBe(429);
    expect(await (await activity(owner.session, { type: "thought", body: "x" })).json()).toEqual({ error: "too many requests" });
  });
});

describe("a delivery dropped after 7 days", () => {
  const dropped = (machine: string) => ({
    agentSessionId: SESSION,
    type: "error",
    body: `${machine} stayed offline for 7 days, so this was dropped. Delegate it again when the computer is on.`,
  });
  const expiredAt = async (id: string) =>
    (await env.DB.prepare("SELECT expired_at FROM linear_deliveries WHERE id = ?").bind(id).first<{ expired_at: number | null }>())?.expired_at;

  it("tells Linear when a delivery was dropped after 7 days", async () => {
    const { id } = await delegatedIssue();
    await expired(env, id);
    expect(linearFake.activities()).toEqual([SENT, dropped("Desk")]);
    expect(await expiredAt(id)).toBeTypeOf("number");
    // Once only.
    await expired(env, id);
    expect(linearFake.activities()).toEqual([SENT, dropped("Desk")]);
  });

  it("says nothing for a follow-up or a delivery the computer took", async () => {
    const { owner, id, pc } = await delegatedIssue();
    expect((await send(signedWebhook(prompted({ userId: LU, body: "make it red", activityId: ACT, agentSessionId: SESSION })))).status).toBe(200);
    const follow = (await pc.inbox.control())["id"] as string;
    await expired(env, follow);
    expect(await expiredAt(follow)).toBeTypeOf("number");
    expect((await call("POST", `/linear/deliveries/${id}/accept`, as(owner.session))).status).toBe(204);
    await expired(env, id);
    expect(linearFake.activities()).toEqual([SENT]);
  });

  it("leaves a delivery the computer accepted unexpired, so its session still takes follow-ups", async () => {
    const { id, pc } = await delegatedIssue();
    // The accept wrote the row, but dropping the relay's copy failed: the
    // relay still expires it a week later.
    await env.DB.prepare("UPDATE linear_deliveries SET accepted_at = ? WHERE id = ?").bind(nowSeconds(), id).run();
    await expired(env, id);
    expect(await expiredAt(id)).toBeNull();
    expect(linearFake.activities()).toEqual([SENT]);
    expect((await send(signedWebhook(prompted({ userId: LU, body: "make it red", agentSessionId: SESSION })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ kind: "prompt", body: "make it red" });
  });

  it("expired() never throws", async () => {
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    // A missing delivery.
    await expect(expired(env, "0".repeat(32))).resolves.toBeUndefined();
    // Linear refusing.
    const { id, owner } = await delegatedIssue();
    linearFake.refuse = 500;
    await expect(expired(env, id)).resolves.toBeUndefined();
    expect(await expiredAt(id)).toBeTypeOf("number");
    expect(log.mock.calls).toEqual([[`linear expiry of ${id}: 500`]]);
    linearFake.refuse = null;
    // A machine that is gone.
    const gone = "f".repeat(32);
    await env.DB.prepare(
      `INSERT INTO linear_deliveries (id, event_key, agent_session_id, organization_id, issue_id, kind, activity_id, account_id, label, created_at)
       VALUES (?, ?, ?, ?, ?, 'created', NULL, ?, 'aaaaaaaaaaaaaaaa', ?)`,
    )
      .bind(gone, `created:${crypto.randomUUID()}`, SESSION, ORG, ISSUE, owner.account_id, nowSeconds())
      .run();
    await expect(expired(env, gone)).resolves.toBeUndefined();
    expect(linearFake.activities()).toEqual([SENT, dropped("This computer")]);
    // The database failing.
    const broken = { ...env, DB: { prepare: () => { throw new TypeError("D1 is down"); } } } as unknown as typeof env;
    await expect(expired(broken, id)).resolves.toBeUndefined();
    expect(log.mock.calls.at(-1)).toEqual([`linear expiry of ${id}: TypeError`]);
  });
});

/// The same computer connects again.
async function connectedComputerAgain(me: Computer, session: string): Promise<PcSocket> {
  const pc = await openPc(me.label, session);
  expect(await claim(pc, me)).toEqual({ type: "claimed" });
  return pc;
}

describe("guidanceOf", () => {
  const rule = (body: unknown, origin: unknown = { type: "Organization" }) => ({ body, origin });

  it("keeps each rule's text and its team, in Linear's order", () => {
    const team = { type: "Team", team: { id: TEAM, key: "ENG", name: "Web", displayName: "Eng / Web", parentId: TEAM2 } };
    expect(guidanceOf({ guidance: [rule("a"), rule("b", team)] })).toEqual([
      { team: null, body: "a" },
      { team: "Eng / Web", body: "b" },
    ]);
    // The name when there is no display name.
    expect(guidanceOf({ guidance: [rule("b", { type: "Team", team: { name: "Web" } })] })).toEqual([{ team: "Web", body: "b" }]);
  });

  it("is null when there is none", () => {
    for (const g of [undefined, null, "Branch from develop.", [], [rule(""), rule("  "), rule(5), null, "x"]]) {
      expect(guidanceOf({ guidance: g })).toBeNull();
    }
  });

  it("skips a broken rule and keeps the rest", () => {
    expect(guidanceOf({ guidance: [rule(5), "x", rule("ok", "Team")] })).toEqual([{ team: null, body: "ok" }]);
  });

  it("passes nothing rather than cut it", () => {
    expect(guidanceOf({ guidance: [rule("x".repeat(MAX_GUIDANCE))] })).toEqual([{ team: null, body: "x".repeat(MAX_GUIDANCE) }]);
    expect(guidanceOf({ guidance: [rule("x".repeat(MAX_GUIDANCE - 1)), rule("yy")] })).toBeNull();
  });
});
