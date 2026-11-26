import { env } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { linear } from "../src/linear";
import { saveWorkspace } from "../src/linear_api";
import { deliver, dropDelivery } from "../src/machines";
import { verified } from "../src/linear_webhook";
import { ADMIN, HOOK_ORG, HOOK_SESSION, as, call, created, dump, linearFake, nowSeconds, prompted, signedWebhook } from "./helpers";
import { account, claim, connectedComputer, openPc, untilStored, type Computer, type PcSocket } from "./relay/helpers";

const ORG = HOOK_ORG;
const SESSION = HOOK_SESSION;
const LU = "22222222-2222-4222-8222-222222222222";
const OTHER = "22222222-2222-4222-8222-999999999999";
const ISSUE = "66666666-6666-4666-8666-666666666666";
const UNMAPPED = "66666666-6666-4666-8666-777777777777";
const TEAM = "33333333-3333-4333-8333-333333333333";
const TEAM2 = "33333333-3333-4333-8333-444444444444";
const PROJECT = "44444444-4444-4444-8444-444444444444";
const ACCESS = "lin_oauth_ws";

type Hook = { text: string; headers: Record<string, string> };

const body = created({ creatorId: LU, issueId: ISSUE });

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
  await call("PUT", `/admin/users/${owner.github_id}/flags/linear`, ADMIN);
  if ((await count("linear_workspaces")) === 0) await workspace(owner.github_id);
  await env.DB.prepare("INSERT INTO linear_links (organization_id, linear_user_id, github_id, linked_at) VALUES (?, ?, ?, ?)")
    .bind(ORG, linearUser, owner.github_id, nowSeconds())
    .run();
  const { me, pc } = await connectedComputer(owner);
  const routes = { label: me.label, organization_id: ORG, teams: project ? [] : [team], projects: project ? [project] : [] };
  const res = await call("PUT", "/linear/routes", as(owner.session), JSON.stringify(routes));
  if (res.status !== 204) throw new Error(`routes refused: ${res.status}`);
  linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: PROJECT });
  linearFake.issues.set(UNMAPPED, { teamId: TEAM2, projectId: null });
  return { me, pc, owner };
}

/// The computer leaves, so the relay keeps what comes for it.
async function away(label: string, pc: PcSocket): Promise<void> {
  pc.ws.close(1000, "shutting down");
  await pc.inbox.closedWith();
  await untilStored<number>(label, "last_seen", (v) => v !== undefined);
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
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE })))).status).toBe(200);
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
      github_id: owner.github_id,
      label: me.label,
      accepted_at: null,
      expired_at: null,
    });
    expect(rows[0]!["id"]).toMatch(/^[0-9a-f]{32}$/);
    expect(rows[0]!["announced_at"]).toBeTypeOf("number");
  });

  it("takes a project route too", async () => {
    const { pc } = await linkedComputerWithRoute({ project: PROJECT });
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE })))).status).toBe(200);
    expect(await pc.inbox.control()).toMatchObject({ type: "linear", issue: ISSUE });
  });

  it("says the computer is offline and keeps the delivery", async () => {
    const { me, pc, owner } = await linkedComputerWithRoute();
    await away(me.label, pc);
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE })))).status).toBe(200);
    expect(linearFake.activities()).toEqual([
      { agentSessionId: SESSION, type: "thought", body: "Desk is offline. The session starts when it is back, for up to 7 days." },
    ]);
    expect(await count("linear_deliveries")).toBe(1);
    const back = await connectedComputerAgain(me, owner.session);
    expect(await back.inbox.control()).toMatchObject({ type: "linear", kind: "created", issue: ISSUE });
  });

  it("the same webhook twice is one delivery and one activity", async () => {
    const { pc } = await linkedComputerWithRoute();
    const hook = signedWebhook(created({ creatorId: LU, issueId: ISSUE }));
    expect((await send(hook)).status).toBe(200);
    expect((await send(hook)).status).toBe(200);
    expect(await count("linear_deliveries")).toBe(1);
    expect(linearFake.activities()).toHaveLength(1);
    await pc.inbox.control();
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("a retry after Linear refused the activity finishes the same delivery", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    await linkedComputerWithRoute();
    const hook = signedWebhook(created({ creatorId: LU, issueId: ISSUE }));
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
    const hook = signedWebhook(created({ creatorId: LU, issueId: ISSUE, promptContext: "SECRET-CONTEXT" }));
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
    const hook = signedWebhook(created({ creatorId: LU, issueId: ISSUE }));
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
    ["no creator", created({ creatorId: null, issueId: ISSUE }), "Arakun starts work only for a person. Delegate this issue yourself."],
    ["no issue", created({ creatorId: LU, issueId: null }), "Arakun works on issues. Mention it on an issue instead."],
    [
      "an unlinked creator",
      created({ creatorId: OTHER, issueId: ISSUE }),
      'The person who delegated this has not connected Linear in Arakun. In Arakun, open the Linear tab and turn on "Let Linear start sessions on this computer".',
    ],
    [
      "no route",
      created({ creatorId: LU, issueId: UNMAPPED }),
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
    ["blocked", "UPDATE users SET blocked_at = 1 WHERE github_id = ?"],
    ["without the flag", "DELETE FROM user_flags WHERE github_id = ?"],
  ])("treats a creator whose account is %s as not linked", async (_n, sql) => {
    const { owner } = await linkedComputerWithRoute();
    await env.DB.prepare(sql).bind(owner.github_id).run();
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

  it("answers a follow-up with 200 for now", async () => {
    await linkedComputerWithRoute();
    const res = await send(signedWebhook(prompted({ userId: LU, body: "make it red", activityId: crypto.randomUUID() })));
    expect(res.status).toBe(200);
    expect(linearFake.calls).toEqual([]);
  });

  it.each([
    ["organizationId", { ...body, organizationId: "org" }],
    ["agentSession.id", { ...body, agentSession: { ...(body["agentSession"] as object), id: "s" } }],
    ["webhookId", { ...body, webhookId: 7 }],
    ["agentSession", { ...body, agentSession: null }],
  ])("refuses a bad %s", async (_n, b) => {
    const res = await send(signedWebhook(b));
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "bad webhook" });
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
    expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE })))).status).toBe(200);
    expect(await mine.pc.inbox.control()).toMatchObject({ type: "linear", issue: ISSUE });
    await expect(theirs.pc.inbox.next(200)).rejects.toThrow();
    const [row] = await deliveries();
    expect(row).toMatchObject({ github_id: mine.owner.github_id, label: mine.me.label });
  });
});

/// The same computer connects again.
async function connectedComputerAgain(me: Computer, session: string): Promise<PcSocket> {
  const pc = await openPc(me.label, session);
  expect(await claim(pc, me)).toEqual({ type: "claimed" });
  return pc;
}
