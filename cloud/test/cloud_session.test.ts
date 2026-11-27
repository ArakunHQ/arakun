// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Cloud sessions: made only by the Worker for one machine we host, and good
// only at that machine's computer door.
import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { MAX_SESSIONS, newCloudSession } from "../src/auth";
import { ADMIN, as, call, freshIp, github, nowSeconds, signInRequest } from "./helpers";
import { account, claim, computer, connectedComputer, openPc, openPhone } from "./relay/helpers";

async function cloudSession(accountId: number, label: string, name = "Cloud") {
  const made = await newCloudSession(env, accountId, label, name);
  if (!made) throw new Error("no cloud session");
  return made;
}

/// The owner, their machine's key, and a cloud session for that machine.
async function cloudMachine() {
  const owner = await account();
  const me = await computer();
  const cloud = await cloudSession(owner.account_id, me.label);
  return { owner, me, cloud };
}

describe("a cloud session", () => {
  it("claims the label it was made for", async () => {
    const { owner, me, cloud } = await cloudMachine();
    const pc = await openPc(me.label, cloud.session, "name=Cloud&kind=server");
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
    const row = await env.DB.prepare("SELECT account_id, kind FROM machines WHERE label = ?").bind(me.label).first();
    expect(row).toEqual({ account_id: owner.account_id, kind: "server" });
  });

  it("is refused at another label's computer door", async () => {
    const { cloud } = await cloudMachine();
    const other = await computer();
    const pc = await openPc(other.label, cloud.session, "name=Cloud&kind=server");
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    expect((await pc.inbox.closedWith()).code).toBe(4401);
  });

  it("is refused at any device door, its own label's too", async () => {
    const { owner, me, cloud } = await cloudMachine();
    const laptop = await connectedComputer(owner);
    for (const label of [me.label, laptop.me.label]) {
      const phone = await openPhone(label, cloud.session);
      expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
      expect((await phone.inbox.closedWith()).code).toBe(4401);
    }
  });

  it("lists the account's sessions by id, without their names", async () => {
    // What arakun-server asks to keep the devices the relay let in; a 401
    // here reads to it as the end of its own sign-in.
    const { owner, cloud } = await cloudMachine();
    const res = await call("GET", "/sessions", as(cloud.session));
    expect(res.status).toBe(200);
    const list = (await res.json()) as { id: string; name: string; created_at: number; last_used_at: number; current: boolean }[];
    expect(list.map((s) => [s.id, s.name, s.created_at, s.last_used_at, s.current]).sort()).toEqual(
      [
        [owner.session_id, "", 0, 0, false],
        [cloud.session_id, "", 0, 0, true],
      ].sort(),
    );
  });

  it("is not signed in anywhere else", async () => {
    const { owner, me, cloud } = await cloudMachine();
    await call("PUT", `/admin/users/${owner.account_id}/flags/teams`, ADMIN);
    for (const [method, path] of [
      ["GET", "/me"],
      ["GET", "/machines"],
      ["DELETE", `/machines/${me.label}`],
      ["DELETE", `/sessions/${owner.session_id}`],
      ["DELETE", `/sessions/${cloud.session_id}`],
      ["POST", "/auth/logout"],
      ["POST", "/notices/01JD0000000000000000000000/read"],
      ["GET", "/teams"],
      ["GET", "/devices"],
    ] as const) {
      const res = await call(method, path, as(cloud.session));
      expect(res.status, `${method} ${path}`).toBe(401);
    }
    // None of those ended a session.
    const n = await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions WHERE account_id = ?").bind(owner.account_id).first<number>("n");
    expect(n).toBe(2);
  });

  it("is listed for the owner with kind cloud", async () => {
    const { owner, cloud } = await cloudMachine();
    const list = await (await call("GET", "/sessions", as(owner.session))).json<{ id: string; kind: string; name: string }[]>();
    expect(list.find((s) => s.id === cloud.session_id)).toMatchObject({ kind: "cloud", name: "Cloud" });
  });

  it("is not evicted by the session cap, nor counted in it", async () => {
    const { owner, me, cloud } = await cloudMachine();
    // Fills the cap with sessions used before the cloud one, as sign-ins would.
    const t = nowSeconds();
    await env.DB.batch(
      Array.from({ length: MAX_SESSIONS - 1 }, (_, i) =>
        env.DB.prepare(
          "INSERT INTO sessions (token_hash, id, name, kind, account_id, created_at, expires_at, last_used_at) VALUES (?, ?, 'old', 'app', ?, ?, ?, ?)",
        ).bind(`old${i}`, `old${i}`, owner.account_id, t - 10_000, t + 10_000, t - 10_000 - i),
      ),
    );
    // Used longest ago of all, so the cap would pick it first.
    await env.DB.prepare("UPDATE sessions SET last_used_at = ? WHERE id = ?").bind(t - 20_000, cloud.session_id).run();
    github.users.set("gho_cap", { id: owner.githubId, login: owner.login });
    expect((await signInRequest("gho_cap", freshIp())).status).toBe(200);
    const kinds = await env.DB.prepare("SELECT kind, COUNT(*) AS n FROM sessions WHERE account_id = ? GROUP BY kind ORDER BY kind")
      .bind(owner.account_id)
      .all<{ kind: string; n: number }>();
    expect(kinds.results).toEqual([
      { kind: "app", n: MAX_SESSIONS },
      { kind: "cloud", n: 1 },
    ]);
    const pc = await openPc(me.label, cloud.session, "name=Cloud&kind=server");
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
  });

  it("ends when the owner signs it out, and its socket closes", async () => {
    const { owner, me, cloud } = await cloudMachine();
    const pc = await openPc(me.label, cloud.session, "name=Cloud&kind=server");
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
    expect((await call("DELETE", `/sessions/${cloud.session_id}`, as(owner.session))).status).toBe(204);
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    const again = await openPc(me.label, cloud.session, "name=Cloud&kind=server");
    expect(await again.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
  });

  it("ends when the user is signed out everywhere", async () => {
    const { owner, me, cloud } = await cloudMachine();
    const pc = await openPc(me.label, cloud.session, "name=Cloud&kind=server");
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
    expect(await (await call("DELETE", `/admin/users/${owner.account_id}/sessions`, ADMIN)).json()).toEqual({ account_id: owner.account_id, closed: 2 });
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
  });

  it("is refused while the account is blocked", async () => {
    const { owner, me, cloud } = await cloudMachine();
    await env.DB.prepare("UPDATE accounts SET blocked_at = 1 WHERE id = ?").bind(owner.account_id).run();
    const pc = await openPc(me.label, cloud.session, "name=Cloud&kind=server");
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
    expect(await newCloudSession(env, owner.account_id, (await computer()).label, "Cloud")).toBeNull();
  });

  it("is refused once expired", async () => {
    const { me, cloud } = await cloudMachine();
    await env.DB.prepare("UPDATE sessions SET expires_at = 1 WHERE id = ?").bind(cloud.session_id).run();
    const pc = await openPc(me.label, cloud.session, "name=Cloud&kind=server");
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
  });

  it("replaces the machine's cloud session before it, and closes its socket", async () => {
    const { owner, me, cloud } = await cloudMachine();
    const pc = await openPc(me.label, cloud.session, "name=Cloud&kind=server");
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
    const next = await cloudSession(owner.account_id, me.label);
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    const ids = await env.DB.prepare("SELECT id FROM sessions WHERE account_id = ? AND kind = 'cloud'").bind(owner.account_id).all<{ id: string }>();
    expect(ids.results).toEqual([{ id: next.session_id }]);
  });

  it("is made only for a user who signed in", async () => {
    expect(await newCloudSession(env, 987_654, (await computer()).label, "Cloud")).toBeNull();
  });

  it("cannot be made by signing in", async () => {
    github.users.set("gho_kind", { id: 1001, login: "octocat" });
    const res = await signInRequest("gho_kind", freshIp(), { kind: "cloud" });
    expect(res.status).toBe(400);
    expect(github.calls).toHaveLength(0);
  });

  it("cannot be stored without its label, nor a label on another kind", async () => {
    const owner = await account();
    const t = nowSeconds();
    const insert = (kind: string, label: string | null) =>
      env.DB.prepare(
        "INSERT INTO sessions (token_hash, id, name, kind, machine_label, account_id, created_at, expires_at, last_used_at) VALUES (?, ?, 'x', ?, ?, ?, ?, ?, ?)",
      )
        .bind(`h-${kind}`, `i-${kind}`, kind, label, owner.account_id, t, t + 100, t)
        .run();
    await expect(insert("cloud", null)).rejects.toThrow(/CHECK/);
    await expect(insert("server", "aaaaaaaaaaaaaaaa")).rejects.toThrow(/CHECK/);
  });
});
