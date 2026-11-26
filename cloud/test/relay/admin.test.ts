import { SELF, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ADMIN, as, call } from "../helpers";
import { HOST, account, claim, computer, computerAndPhone, connectedComputer, device, openPc, openPhone, opened } from "./helpers";

const JSON_ADMIN = { ...ADMIN, "Content-Type": "application/json" };

function block(accountId: number, reason?: string): Promise<Response> {
  return call("PUT", `/admin/users/${accountId}/block`, JSON_ADMIN, reason === undefined ? undefined : JSON.stringify({ reason }));
}

describe("the old operator door", () => {
  it("is gone", async () => {
    for (const path of ["/_admin/labels", "/_admin/block/k7f3q2m4xwab4c5d"]) {
      const res = await SELF.fetch(`${HOST}${path}`, { method: path.endsWith("labels") ? "GET" : "POST", headers: ADMIN });
      expect(res.status, path).toBe(404);
    }
  });
});

describe("blocking an account", () => {
  it("closes its computer and devices with blocked, and keeps new sockets out", async () => {
    const { me, pc, owner, phone } = await computerAndPhone();
    const res = await block(owner.account_id, "abuse report 12");
    expect(res.status).toBe(204);
    const row = await env.DB.prepare("SELECT blocked_at, blocked_reason FROM accounts WHERE id = ?").bind(owner.account_id).first();
    expect(row).toEqual({ blocked_at: expect.any(Number), blocked_reason: "abuse report 12" });

    // The computer first, so the one thing it hears is why.
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
    expect((await pc.inbox.closedWith()).code).toBe(4403);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
    expect((await phone.inbox.closedWith()).code).toBe(4403);

    const again = await openPc(me.label, owner.session);
    expect(await again.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
    expect((await again.inbox.closedWith()).code).toBe(4403);
    const dev = await device(owner);
    const late = await openPhone(me.label, dev.session);
    expect(await late.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
  });

  it("tells the account why on its machines, but still lets it read /me and sign out", async () => {
    const { owner } = await connectedComputer();
    expect((await block(owner.account_id, "unpaid")).status).toBe(204);
    const list = await call("GET", "/machines", as(owner.session));
    expect(list.status).toBe(403);
    expect(await list.json()).toEqual({ error: "blocked", reason: "unpaid" });
    const me = await computer();
    const forget = await call("DELETE", `/machines/${me.label}`, as(owner.session));
    expect(forget.status).toBe(403);
    expect(await forget.json()).toEqual({ error: "blocked", reason: "unpaid" });
    expect((await call("GET", "/me", as(owner.session))).status).toBe(200);
    expect((await call("POST", "/auth/logout", as(owner.session))).status).toBe(204);
  });

  it("is undone by DELETE, and the account's sockets come back", async () => {
    const { me, pc, owner } = await connectedComputer();
    expect((await block(owner.account_id)).status).toBe(204);
    await pc.inbox.closedWith();
    const unblock = await call("DELETE", `/admin/users/${owner.account_id}/block`, ADMIN);
    expect(unblock.status).toBe(204);
    const row = await env.DB.prepare("SELECT blocked_at, blocked_reason FROM accounts WHERE id = ?").bind(owner.account_id).first();
    expect(row).toEqual({ blocked_at: null, blocked_reason: null });

    const back = await openPc(me.label, owner.session);
    expect(await claim(back, me)).toEqual({ type: "claimed" });
    const dev = await device(owner);
    const phone = await openPhone(me.label, dev.session);
    expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await back.inbox.control()).toEqual(opened(1, dev));
  });

  it("leaves other accounts alone", async () => {
    const { owner } = await connectedComputer();
    const { pc: theirs } = await connectedComputer();
    expect((await block(owner.account_id)).status).toBe(204);
    await expect(theirs.inbox.next(200)).rejects.toThrow();
  });

  it("works before the account ever signed in, and checks what it is given", async () => {
    // An account made ahead of sign-in; an ID with no account is not one.
    expect((await block(4242)).status).toBe(404);
    const made = await call("PUT", "/admin/github/4242", ADMIN);
    expect(made.status).toBe(200);
    const { account_id: id } = (await made.json()) as { account_id: number };
    expect((await block(id)).status).toBe(204);
    expect(await env.DB.prepare("SELECT login, blocked_at IS NOT NULL AS blocked FROM accounts WHERE id = ?").bind(id).first()).toEqual({ login: "", blocked: 1 });
    expect((await call("PUT", "/admin/users/x/block", ADMIN)).status).toBe(400);
    expect((await block(id, "x".repeat(501))).status).toBe(400);
    expect((await call("PUT", `/admin/users/${id}/block`, JSON_ADMIN, "not json")).status).toBe(400);
    expect((await call("PUT", `/admin/users/${id}/block`)).status).toBe(401);
  });
});

describe("setting an account's plan", () => {
  it("writes the plan, and /me shows it", async () => {
    const owner = await account();
    const res = await call("PUT", `/admin/users/${owner.account_id}/plan`, JSON_ADMIN, JSON.stringify({ plan: "pro" }));
    expect(res.status).toBe(204);
    expect(((await (await call("GET", "/me", as(owner.session))).json()) as { plan: string }).plan).toBe("pro");
  });

  it("wants a plan name", async () => {
    const owner = await account();
    for (const body of ["{}", JSON.stringify({ plan: "" }), JSON.stringify({ plan: "Pro Plan" }), JSON.stringify({ plan: 3 }), "nope"]) {
      expect((await call("PUT", `/admin/users/${owner.account_id}/plan`, JSON_ADMIN, body)).status, body).toBe(400);
    }
    expect((await call("PUT", `/admin/users/${owner.account_id}/plan`, {}, JSON.stringify({ plan: "pro" }))).status).toBe(401);
  });
});

describe("the list of machines", () => {
  it("shows every machine with its owner and times, the last seen first", async () => {
    const first = await connectedComputer();
    const second = await connectedComputer();
    await env.DB.prepare("UPDATE machines SET last_seen_at = last_seen_at - 60 WHERE label = ?").bind(first.me.label).run();
    const res = await call("GET", "/admin/machines", ADMIN);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual([
      { label: second.me.label, account_id: second.owner.account_id, github_id: second.owner.githubId, login: second.owner.login, name: "Desk", kind: "app", created_at: expect.any(Number), last_seen_at: expect.any(Number) },
      { label: first.me.label, account_id: first.owner.account_id, github_id: first.owner.githubId, login: first.owner.login, name: "Desk", kind: "app", created_at: expect.any(Number), last_seen_at: expect.any(Number) },
    ]);
    expect((await call("GET", "/admin/machines")).status).toBe(401);
    expect((await call("GET", "/admin/machines", as(first.owner.session))).status).toBe(401);
  });
});
