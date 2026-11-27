// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { Channel } from "../src/channel";
import { b64url, ulid } from "../src/util";
import { ADMIN, call, roomInWindow } from "./helpers";
import { api, envelope, expectJson, join, makeDevice, message, newKey, openChannel, register, seal, team, user, type Socket, type TeamSetup } from "./clients";

/// The next frame, which must be a stored message.
async function stored(s: Socket): Promise<{ seq: number; at: number; message: { id: string } }> {
  const got = await s.inbox.next();
  if (got.type !== "message") throw new Error(`expected a message, got ${JSON.stringify(got)}`);
  return got;
}

async function refused(s: Socket): Promise<{ id: string | null; code?: string; error: string }> {
  const got = await s.inbox.next();
  if (got.type !== "error") throw new Error(`expected an error, got ${JSON.stringify(got)}`);
  return got;
}

async function storedCount(t: TeamSetup): Promise<number> {
  const all = await expectJson<unknown[]>(api(t.owner.user, "GET", `/teams/${t.teamId}/channels/${t.channelId}/messages`, { device: t.owner.device }), 200);
  return all.length;
}

describe("channel sockets", () => {
  it("fan a message out to every member, the sender's own copy being its ack", async () => {
    const t = await team();
    const m = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);

    const sent = await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "hola");
    a.send(sent);
    const ack = await stored(a);
    const got = await stored(b);
    expect(ack).toEqual({ type: "message", seq: 1, at: expect.any(Number), message: sent });
    expect(got).toEqual(ack);

    const reply = await message(t.teamId, t.channelId, m.device, 1, t.keys.get(1)!, "qué tal");
    b.send(reply);
    expect((await stored(a)).message).toEqual(reply);
    expect((await stored(b)).seq).toBe(2);
    a.ws.close();
    b.ws.close();
  });

  it("refuses a bad signature, another device's message and a reused id", async () => {
    const t = await team();
    const m = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const key = t.keys.get(1)!;

    const good = await message(t.teamId, t.channelId, t.owner.device, 1, key, "x");
    a.send({ ...good, ct: b64url(new Uint8Array(32)) });
    expect(await refused(a)).toEqual({ type: "error", id: good.id, error: "bad signature" });
    // Signed for another channel.
    a.send({ ...(await message(t.teamId, t.teamId, t.owner.device, 1, key, "x")) });
    expect((await refused(a)).error).toBe("bad signature");
    // Posting as the member's device from the owner's socket.
    a.send(await message(t.teamId, t.channelId, m.device, 1, key, "x"));
    expect((await refused(a)).error).toMatch(/device/);
    a.send(good);
    await stored(a);
    a.send(good);
    expect(await refused(a)).toEqual({ type: "error", id: good.id, code: "exists", error: "a message with this id exists" });
    a.send({ ...good, nonce: "short" });
    expect((await refused(a)).id).toBe(good.id);
    a.ws.send("not json");
    expect((await refused(a)).id).toBeNull();
    expect(await storedCount(t)).toBe(1);
    a.ws.close();
  });

  it("refuses a ct over 64 KiB", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const key = t.keys.get(1)!;
    // The text that makes the sealed JSON exactly 64 KiB with its 16-byte tag.
    const wrapper = JSON.stringify({ kind: "text", text: "", reply_to: null, mentions: [], bot: null, decision: null }).length;
    const fits = await message(t.teamId, t.channelId, t.owner.device, 1, key, "x".repeat(64 * 1024 - 16 - wrapper));
    a.send(fits);
    expect((await stored(a)).message).toEqual(fits);
    const big = await message(t.teamId, t.channelId, t.owner.device, 1, key, "x".repeat(64 * 1024));
    a.send(big);
    expect((await refused(a)).error).toMatch(/64 KiB/);
    a.ws.close();
  });

  it("are closed to non-members, and a member who lost membership is refused and closed", async () => {
    const t = await team();
    const outsider = await join(await team());
    const res = await api(outsider.user, "GET", `/teams/${t.teamId}/channels/${t.channelId}/ws`, { device: outsider.device, headers: { Upgrade: "websocket" } });
    expect(res.status).toBe(404);

    const m = await join(t);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    // Membership gone without the Worker telling the channel: the send itself checks.
    await env.DB.prepare("DELETE FROM members WHERE account_id = ?").bind(m.user.id).run();
    b.send(await message(t.teamId, t.channelId, m.device, 1, t.keys.get(1)!, "still here?"));
    expect((await refused(b)).error).toBe("not a member of this team");
    expect((await b.inbox.closedWith()).code).toBe(4403);
    expect(await storedCount(t)).toBe(0);
  });

  it("close a socket whose session ended or whose user lost the flag, before it gets anything", async () => {
    const t = await team();
    const out = await join(t);
    const unflagged = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(out.user, out.device, t.teamId, t.channelId);
    const c = await openChannel(unflagged.user, unflagged.device, t.teamId, t.channelId);
    expect((await api(out.user, "POST", "/auth/logout")).status).toBe(204);
    await expectJson(call("DELETE", `/admin/users/${unflagged.user.id}/flags/teams`, ADMIN), 200);

    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "only for members"));
    await stored(a);
    expect(await b.inbox.next()).toEqual({ closed: { code: 4403, reason: "not a member of this team" } });
    expect(await c.inbox.next()).toEqual({ closed: { code: 4403, reason: "not a member of this team" } });
    a.ws.close();
  });

  it("close a blocked account's socket before it gets anything, and take no message from it", async () => {
    const t = await team();
    const blocked = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(blocked.user, blocked.device, t.teamId, t.channelId);
    expect((await call("PUT", `/admin/users/${blocked.user.id}/block`, ADMIN)).status).toBe(204);

    b.send(await message(t.teamId, t.channelId, blocked.device, 1, t.keys.get(1)!, "still here"));
    expect((await refused(b)).error).toBe("not a member of this team");
    expect(await b.inbox.closedWith()).toEqual({ code: 4403, reason: "not a member of this team" });
    // A new socket is refused, and so is the history.
    await expect(openChannel(blocked.user, blocked.device, t.teamId, t.channelId)).rejects.toThrow();
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "only for members"));
    await stored(a);
    const history = await expectJson<{ message: { device_id: string } }[]>(
      api(t.owner.user, "GET", `/teams/${t.teamId}/channels/${t.channelId}/messages`, { device: t.owner.device }),
      200,
    );
    expect(history.map((m) => m.message.device_id)).toEqual([t.owner.device.id]);
    a.ws.close();
  });

  it("check a socket admitted while a send checks the others, so a logout in between is not missed", async () => {
    const t = await team();
    const m = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const { token_hash: mine } = (await env.DB.prepare("SELECT token_hash FROM sessions WHERE account_id = ?").bind(m.user.id).first<{ token_hash: string }>())!;
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`));
    // Holds the member's admission after its check passed, until the owner's
    // send is checking the open sockets; the member logs out in between.
    // allowed() is private; the test swaps it on the live object. The gate
    // lives in the object: promises cannot cross from the test into it.
    await runInDurableObject<Channel, void>(stub as DurableObjectStub, (instance) => {
      const ch = instance as any;
      const allowed = ch.allowed.bind(ch);
      let open!: () => void;
      const gate = new Promise<void>((r) => (open = r));
      ch.held = false;
      ch.allowed = async (teamId: string, sessions: string[]) => {
        const got: Set<string> = await allowed(teamId, sessions);
        if (sessions.length === 1 && sessions[0] === mine) {
          ch.held = true;
          await gate;
        } else {
          // The send's read: the admission finishes while it is in flight.
          open();
          for (let i = 0; i < 200 && ch.ctx.getWebSockets().length < 2; i++) await new Promise((r) => setTimeout(r, 1));
        }
        return got;
      };
    });
    const opening = openChannel(m.user, m.device, t.teamId, t.channelId);
    while (!(await runInDurableObject<Channel, boolean>(stub as DurableObjectStub, (ch) => (ch as any).held))) await new Promise((r) => setTimeout(r, 1));
    await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(mine).run();
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "not for a logged-out session"));
    await stored(a);
    const b = await opening;
    expect(await b.inbox.next()).toEqual({ closed: { code: 4403, reason: "not a member of this team" } });
    a.ws.close();
  });

  it("check a socket admitted after a send began checking, so a logout before delivery is not missed", async () => {
    const t = await team();
    const m = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const hash = async (id: number) =>
      (await env.DB.prepare("SELECT token_hash FROM sessions WHERE account_id = ?").bind(id).first<{ token_hash: string }>())!.token_hash;
    const owner = await hash(t.owner.user.id);
    const mine = await hash(m.user.id);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    // Holds the send's first read until the member was admitted, with a check
    // of its own that passed, and has logged out.
    await runInDurableObject<Channel, void>(stub, (instance) => {
      const ch = instance as any;
      const allowed = ch.allowed.bind(ch);
      const gate = new Promise<void>((r) => (ch.release = r));
      ch.held = false;
      ch.allowed = async (teamId: string, sessions: string[]) => {
        const got: Set<string> = await allowed(teamId, sessions);
        if (!ch.held && sessions.includes(owner)) {
          ch.held = true;
          await gate;
        }
        return got;
      };
    });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "not for a logged-out session"));
    while (!(await runInDurableObject<Channel, boolean>(stub, (ch) => (ch as any).held))) await new Promise((r) => setTimeout(r, 1));
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(mine).run();
    await runInDurableObject<Channel, void>(stub, (ch) => (ch as any).release());
    await stored(a);
    expect(await b.inbox.next()).toEqual({ closed: { code: 4403, reason: "not a member of this team" } });
    a.ws.close();
  });

  it("refuse a send denied by the final session check without storing or delivering it", async () => {
    const t = await team();
    const m = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    await runInDurableObject<Channel, void>(stub, (instance) => {
      const ch = instance as any;
      const allowed = ch.allowed.bind(ch);
      // Stand in for an ongoing admission and a policy that denies the recheck.
      ch.admitting.set("test-admission", 1);
      let reads = 0;
      ch.allowed = async (teamId: string, sessions: string[]) => {
        if (sessions.includes("test-admission") && ++reads === 2) {
          ch.admitting.delete("test-admission");
          return new Map();
        }
        return allowed(teamId, sessions);
      };
    });
    const key = t.keys.get(1)!;
    const sent = await message(t.teamId, t.channelId, t.owner.device, 1, key, "denied");
    a.send(sent);
    expect(await refused(a)).toEqual({ type: "error", id: sent.id, error: "not a member of this team" });
    expect((await a.inbox.closedWith()).code).toBe(4403);
    expect(await storedCount(t)).toBe(0);
    const next = await message(t.teamId, t.channelId, m.device, 1, key, "still connected");
    b.send(next);
    expect(await stored(b)).toMatchObject({ seq: 1, message: { id: next.id } });
    b.ws.close();
  });

  it("close a socket a send could not check, so its client reads the history again", async () => {
    const t = await team();
    const m = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    // Sockets keep arriving during each of the send's reads (made-up sessions
    // stand in for them), and the member's real one arrives during the last.
    await runInDurableObject<Channel, void>(stub, (instance) => {
      const ch = instance as any;
      const allowed = ch.allowed.bind(ch);
      const sessions = ch.sessions.bind(ch);
      let n = 0;
      ch.sessions = () => [...sessions(), `arriving-${n++}`];
      const gate = new Promise<void>((r) => (ch.release = r));
      ch.held = false;
      ch.allowed = async (teamId: string, list: string[]) => {
        const got: Set<string> = await allowed(teamId, list);
        if (list.includes("arriving-3")) {
          ch.held = true;
          await gate;
        }
        return got;
      };
    });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "while they opened"));
    while (!(await runInDurableObject<Channel, boolean>(stub, (ch) => (ch as any).held))) await new Promise((r) => setTimeout(r, 1));
    // The member opens and reads the history before the message is stored.
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    const path = `/teams/${t.teamId}/channels/${t.channelId}/messages`;
    expect(await expectJson(api(m.user, "GET", path, { device: m.device }), 200)).toEqual([]);
    await runInDurableObject<Channel, void>(stub, (ch) => (ch as any).release());
    await stored(a);
    expect(await b.inbox.next()).toEqual({ closed: { code: 4503, reason: "reconnect and read the history" } });
    expect(await expectJson<unknown[]>(api(m.user, "GET", path, { device: m.device }), 200)).toHaveLength(1);
    a.ws.close();
  });

  it("answer a send with an error when its checks cannot run, and store nothing", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    await runInDurableObject<Channel, void>(stub, (instance) => {
      const ch = instance as any;
      ch.allowed = async () => {
        throw new Error("D1_ERROR: queue full");
      };
    });
    const msg = await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "not now");
    a.send(msg);
    expect(await refused(a)).toEqual({ type: "error", id: msg.id, code: "retry", error: "the server could not check the message: send it again" });
    expect(await storedCount(t)).toBe(0);
    a.ws.close();
  });

  it("ignore trusted headers a client sends", async () => {
    const t = await team();
    const m = await join(t);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId, {
      "X-Teams-Account-Id": String(t.owner.user.id),
      "X-Teams-Device-Id": t.owner.device.id,
      "X-Teams-Sig-Pub": t.owner.device.sigPub,
      "X-Teams-Session-Hash": "0".repeat(64),
    });
    b.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "as the owner"));
    expect((await refused(b)).error).toMatch(/device/);
    b.send(await message(t.teamId, t.channelId, m.device, 1, t.keys.get(1)!, "as myself"));
    expect((await stored(b)).message).toMatchObject({ device_id: m.device.id });
    b.ws.close();
  });

  it("need a WebSocket upgrade and a channel of the team", async () => {
    const t = await team();
    const path = `/teams/${t.teamId}/channels/${t.channelId}/ws`;
    await expectJson(api(t.owner.user, "GET", path, { device: t.owner.device }), 426);
    const other = await team();
    const res = await api(t.owner.user, "GET", `/teams/${t.teamId}/channels/${other.channelId}/ws`, { device: t.owner.device, headers: { Upgrade: "websocket" } });
    expect(res.status).toBe(404);
  });

  it("limit each device to 60 messages per 10 seconds", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const key = t.keys.get(1)!;
    const msgs = await Promise.all(Array.from({ length: 61 }, () => message(t.teamId, t.channelId, t.owner.device, 1, key, "x")));
    await roomInWindow(10);
    for (const msg of msgs) a.send(msg);
    const frames = [];
    for (let i = 0; i < 61; i++) frames.push(await a.inbox.next());
    expect(frames.filter((f) => f.type === "message")).toHaveLength(60);
    expect(frames.filter((f) => f.type === "error").map((f) => f.error)).toEqual(["too many messages, slow down"]);
    a.ws.close();
  });
});

describe("rotation", () => {
  it("takes only the current epoch once it has envelopes, so nothing is stored while a rotation is pending", async () => {
    const t = await team();
    const m = await join(t);
    const gone = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${gone.user.id}`), 200);

    // Rotation pending: the removed member holds the old key, so it is refused
    // at once, with a code the client reads the team again on.
    const old = await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "old key");
    a.send(old);
    expect(await refused(a)).toEqual({
      type: "error",
      id: old.id,
      code: "rotation_pending",
      error: "a key rotation is pending: epoch 1 is retired, so wait for the new team key and send again",
    });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 3, newKey(), "future"));
    expect(await refused(a)).toMatchObject({ code: "unknown_epoch", error: "unknown epoch" });
    // Epoch 2 has no envelopes yet, and a second removal could skip it for good.
    const key2 = newKey();
    a.send(await message(t.teamId, t.channelId, t.owner.device, 2, key2, "current epoch, before its envelopes"));
    expect(await refused(a)).toMatchObject({ code: "rotation_pending" });
    expect(await storedCount(t)).toBe(0);

    const envelopes = [await envelope(t.teamId, 2, key2, t.owner.device, t.owner.device), await envelope(t.teamId, 2, key2, m.device, t.owner.device)];
    await expectJson(api(t.owner.user, "PUT", `/teams/${t.teamId}/envelopes`, { device: t.owner.device, body: { envelopes } }), 200);

    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "old key again"));
    expect(await refused(a)).toMatchObject({ code: "stale_epoch", error: "stale epoch: fetch the new team key" });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 2, key2, "new key"));
    expect((await stored(a)).message).toMatchObject({ epoch: 2 });
    a.ws.close();
  });

  it("names the epoch in every refusal of one, so a client from before the code reads the team again", async () => {
    const t = await team();
    const gone = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${gone.user.id}`), 200);
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "old key"));
    const got = await refused(a);
    expect(got.error).toMatch(/epoch/);
    // Such a client treats a refusal that says "exists" as already stored.
    expect(got.error).not.toMatch(/exists/);
    a.ws.close();
  });

  /// Holds the owner's next send in its membership read, until `release`.
  async function holdOwnerSend(t: TeamSetup): Promise<DurableObjectStub> {
    const owner = (await env.DB.prepare("SELECT token_hash FROM sessions WHERE account_id = ?").bind(t.owner.user.id).first<{ token_hash: string }>())!.token_hash;
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    await runInDurableObject<Channel, void>(stub, (instance) => {
      const ch = instance as any;
      const allowed = ch.allowed.bind(ch);
      const gate = new Promise<void>((r) => (ch.release = r));
      ch.held = false;
      ch.allowed = async (teamId: string, sessions: string[]) => {
        const got: Set<string> = await allowed(teamId, sessions);
        if (!ch.held && sessions[0] === owner) {
          ch.held = true;
          await gate;
        }
        return got;
      };
    });
    return stub;
  }

  async function untilHeld(stub: DurableObjectStub): Promise<void> {
    while (!(await runInDurableObject<Channel, boolean>(stub, (ch) => (ch as any).held))) await new Promise((r) => setTimeout(r, 1));
  }

  it("refuses a send that a removal overtook while it was checked", async () => {
    const t = await team();
    const gone = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const stub = await holdOwnerSend(t);
    const old = await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "sent before the removal");
    a.send(old);
    await untilHeld(stub);
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${gone.user.id}`), 200);
    await runInDurableObject<Channel, void>(stub, (ch) => (ch as any).release());
    expect(await refused(a)).toMatchObject({ id: old.id, code: "rotation_pending" });
    expect(await storedCount(t)).toBe(0);
    a.ws.close();
  });

  it("refuses an old-epoch send that a rotation and a new-epoch send overtook while it was checked", async () => {
    const t = await team();
    const m = await join(t);
    const gone = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    const stub = await holdOwnerSend(t);
    const old = await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "sent before the rotation");
    a.send(old);
    await untilHeld(stub);
    // A removal, the rotation that ends it, and the member sends with the new key meanwhile.
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${gone.user.id}`), 200);
    const key2 = newKey();
    const envelopes = [await envelope(t.teamId, 2, key2, t.owner.device, t.owner.device), await envelope(t.teamId, 2, key2, m.device, t.owner.device)];
    await expectJson(api(t.owner.user, "PUT", `/teams/${t.teamId}/envelopes`, { device: t.owner.device, body: { envelopes } }), 200);
    b.send(await message(t.teamId, t.channelId, m.device, 2, key2, "new key"));
    expect((await stored(b)).message).toMatchObject({ epoch: 2 });
    await runInDurableObject<Channel, void>(stub, (ch) => (ch as any).release());
    expect((await stored(a)).message).toMatchObject({ epoch: 2 });
    expect(await refused(a)).toEqual({ type: "error", id: old.id, code: "stale_epoch", error: "stale epoch: fetch the new team key" });
    expect(await storedCount(t)).toBe(1);
    a.ws.close();
    b.ws.close();
  });

  it("closes the removed member's sockets and bumps the epoch; leaving does the same", async () => {
    const t = await team();
    const m = await join(t);
    const leaver = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    const c = await openChannel(leaver.user, leaver.device, t.teamId, t.channelId);

    expect(await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${m.user.id}`), 200)).toEqual({ epoch: 2, rotation_pending: true });
    expect(await b.inbox.closedWith()).toEqual({ code: 4403, reason: "removed from the team" });
    await expectJson(api(m.user, "GET", `/teams/${t.teamId}`), 404);
    await expectJson(api(m.user, "GET", `/teams/${t.teamId}/envelopes`, { device: m.device }), 404);
    const left = await env.DB.prepare("SELECT COUNT(*) AS n FROM envelopes WHERE device_id = ?").bind(m.device.id).first<{ n: number }>();
    expect(left?.n).toBe(0);

    // A member leaves on their own.
    expect(await expectJson(api(leaver.user, "DELETE", `/teams/${t.teamId}/members/${leaver.user.id}`), 200)).toEqual({ epoch: 3, rotation_pending: true });
    expect((await c.inbox.closedWith()).code).toBe(4403);

    // The others stay connected, and hear that the rotation is pending.
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "still here"));
    expect(await refused(a)).toMatchObject({ code: "rotation_pending" });
    expect(a.inbox.closed).toBeNull();
    a.ws.close();
  });
});

describe("presence", () => {
  it("tells everyone on the channel who has it open, as people come and go", async () => {
    const t = await team();
    const m = await join(t);
    const ids = [t.owner.user.id, m.user.id].sort((x, y) => x - y);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await a.inbox.here.until([t.owner.user.id]);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    await a.inbox.here.until(ids);
    await b.inbox.here.until(ids);
    // A second socket of the same person is still one entry.
    const a2 = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await a2.inbox.here.until(ids);

    b.ws.close();
    await a.inbox.here.until([t.owner.user.id]);
    a2.ws.close();
    a.ws.close();
  });

  it("tells nobody whose session ended or who lost the flag, and leaves them out", async () => {
    const t = await team();
    const out = await join(t);
    const unflagged = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(out.user, out.device, t.teamId, t.channelId);
    const c = await openChannel(unflagged.user, unflagged.device, t.teamId, t.channelId);
    const all = [t.owner.user.id, out.user.id, unflagged.user.id].sort((x, y) => x - y);
    await b.inbox.here.until(all);
    await c.inbox.here.until(all);
    expect((await api(out.user, "POST", "/auth/logout")).status).toBe(204);
    await expectJson(call("DELETE", `/admin/users/${unflagged.user.id}/flags/teams`, ADMIN), 200);
    const [bSeen, cSeen] = [b.inbox.here.count, c.inbox.here.count];

    const a2 = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await a2.inbox.here.until([t.owner.user.id]);
    await a.inbox.here.until([t.owner.user.id]);
    // A send closes b and c; once their close arrived, anything sent to them
    // before it has arrived too.
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "barrier"));
    expect((await b.inbox.closedWith()).code).toBe(4403);
    expect((await c.inbox.closedWith()).code).toBe(4403);
    expect(b.inbox.here.count).toBe(bSeen);
    expect(c.inbox.here.count).toBe(cSeen);
    for (const s of [a, a2]) s.ws.close();
  });

  it("never lists a socket that opened during an older list's check and then lost the flag", async () => {
    const t = await team();
    const m = await join(t);
    const hash = async (id: number) =>
      (await env.DB.prepare("SELECT token_hash FROM sessions WHERE account_id = ?").bind(id).first<{ token_hash: string }>())!.token_hash;
    const owner = await hash(t.owner.user.id);
    const mine = await hash(m.user.id);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await a.inbox.here.until([t.owner.user.id]);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    // Holds the list that a second owner socket starts (old), and the one the
    // member's socket starts (new) until the member lost the flag.
    await runInDurableObject<Channel, void>(stub, (instance) => {
      const ch = instance as any;
      const allowed = ch.allowed.bind(ch);
      const oldGate = new Promise<void>((r) => (ch.releaseOld = r));
      const newGate = new Promise<void>((r) => (ch.releaseNew = r));
      ch.heldOld = ch.heldNew = false;
      // The second owner socket's admission checks the owner first, then its list.
      let ownerOnly = 0;
      ch.allowed = async (teamId: string, list: string[]) => {
        if (!ch.heldOld && list.length === 1 && list[0] === owner && ++ownerOnly === 2) {
          const got = await allowed(teamId, list);
          ch.heldOld = true;
          await oldGate;
          // The rest of the old run has no await, so it is over before the
          // next event reaches the object.
          ch.oldResumed = true;
          return got;
        }
        if (!ch.heldNew && list.length > 1 && list.includes(mine)) {
          ch.heldNew = true;
          await newGate;
        }
        return allowed(teamId, list);
      };
    });
    const until = async (key: string) => {
      while (!(await runInDurableObject<Channel, boolean>(stub, (ch) => (ch as any)[key]))) await new Promise((r) => setTimeout(r, 1));
    };
    const a2 = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await until("heldOld");
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    await until("heldNew");
    await expectJson(call("DELETE", `/admin/users/${m.user.id}/flags/teams`, ADMIN), 200);
    // The list the new run sends must be a new frame: the owner-only list it
    // reports was already received above, so `until` alone could pass on it.
    const had = a.inbox.here.count;
    await runInDurableObject<Channel, void>(stub, (ch) => (ch as any).releaseNew());
    const deadline = Date.now() + 2000;
    while (a.inbox.here.count <= had) {
      if (Date.now() > deadline) throw new Error("the newer run sent no presence frame");
      await new Promise((r) => setTimeout(r, 5));
    }
    expect(a.inbox.here.latest).toEqual([t.owner.user.id]);
    await runInDurableObject<Channel, void>(stub, (ch) => (ch as any).releaseOld());
    await until("oldResumed");
    // A refused frame is a barrier that closes no socket: a close would start
    // a presence list of its own. Its answer comes after anything the old run
    // sent.
    a.ws.send(JSON.stringify({ type: "barrier" }));
    await refused(a);
    // The older run sent nothing: no frame past the newer run's.
    expect(a.inbox.here.count).toBe(had + 1);
    expect(b.inbox.here.count).toBe(0);
    for (const s of [a, a2, b]) s.ws.close();
  });

  it("leaves a quiet socket out until it asks to count, and again once it stops", async () => {
    const t = await team();
    const m = await join(t);
    const ids = [t.owner.user.id, m.user.id].sort((x, y) => x - y);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await a.inbox.here.until([t.owner.user.id]);
    const seen = a.inbox.here.count;
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId, {}, "?presence=0");
    // A quiet socket still gets messages.
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "to both"));
    await stored(a);
    expect((await b.inbox.next()).type).toBe("message");
    // A refused frame is a barrier: anything sent before it has arrived.
    a.ws.send(JSON.stringify({ type: "barrier" }));
    await refused(a);
    expect(a.inbox.here.count).toBe(seen);
    expect(b.inbox.here.count).toBe(0);

    b.ws.send(JSON.stringify({ type: "presence", on: true }));
    await a.inbox.here.until(ids);
    await b.inbox.here.until(ids);
    // Asking for what it has already changes nothing.
    b.ws.send(JSON.stringify({ type: "presence", on: true }));
    b.ws.send(JSON.stringify({ type: "presence", on: false }));
    await a.inbox.here.until([t.owner.user.id]);
    // Closing a quiet socket starts no presence list.
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    await runInDurableObject<Channel, void>(stub, (instance) => {
      const ch = instance as any;
      const [presence, close] = [ch.presence.bind(ch), ch.webSocketClose.bind(ch)];
      ch.lists = 0;
      ch.presence = (...args: unknown[]) => (ch.lists++, presence(...args));
      ch.webSocketClose = async (ws: WebSocket) => {
        await close(ws);
        ch.closed = true;
      };
    });
    b.ws.close();
    while (!(await runInDurableObject<Channel, boolean>(stub, (ch) => (ch as any).closed))) await new Promise((r) => setTimeout(r, 1));
    expect(await runInDurableObject<Channel, number>(stub, (ch) => (ch as any).lists)).toBe(0);
    a.ws.close();
  });

  it("refuses a bad presence frame and a bad presence parameter", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId, {}, "?presence=1");
    await a.inbox.here.until([t.owner.user.id]);
    a.ws.send(JSON.stringify({ type: "presence", on: "yes" }));
    expect((await refused(a)).id).toBeNull();
    const res = await api(t.owner.user, "GET", `/teams/${t.teamId}/channels/${t.channelId}/ws?presence=off`, {
      device: t.owner.device,
      headers: { Upgrade: "websocket" },
    });
    expect(res.status).toBe(400);
    a.ws.close();
  });

  it("drops a removed member", async () => {
    const t = await team();
    const m = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    await a.inbox.here.until([t.owner.user.id, m.user.id].sort((x, y) => x - y));
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${m.user.id}`), 200);
    await b.inbox.closedWith();
    await a.inbox.here.until([t.owner.user.id]);
    a.ws.close();
  });
});

describe("history", () => {
  it("remembers an accepted message ID after trimming its ciphertext", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const first = await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "early");
    a.send(first);
    await stored(a);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    await runInDurableObject<Channel, void>(stub, (_, state) => {
      state.storage.sql.exec("UPDATE sqlite_sequence SET seq = 10000 WHERE name = 'messages'");
    });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "latest"));
    expect((await stored(a)).seq).toBe(10001);
    expect(await storedCount(t)).toBe(1);
    a.send(first);
    expect(await refused(a)).toMatchObject({ code: "exists" });
    expect(await storedCount(t)).toBe(1);
    a.ws.close();
  });

  it("trims in runs, not on every send after the first trim", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    // As after a trim: 9,000 messages, seq 1,001 to 10,000.
    await runInDurableObject<Channel, void>(stub, (_, state) => {
      state.storage.sql.exec(
        `WITH RECURSIVE n(seq) AS (SELECT 1001 UNION ALL SELECT seq + 1 FROM n WHERE seq < 10000)
         INSERT INTO messages SELECT seq, 'm' || seq, 0, 'd', '{"epoch":1}' FROM n`,
      );
    });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "kept"));
    expect((await stored(a)).seq).toBe(10001);
    const count = await runInDurableObject<Channel, number>(stub, (_, state) =>
      state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM messages").one().n,
    );
    expect(count).toBe(9001);
    a.ws.close();
  });

  it("forgets retired IDs only of epochs no send can use", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    await runInDurableObject<Channel, void>(stub, (_, state) => {
      const sql = state.storage.sql;
      sql.exec("INSERT INTO retired_ids VALUES ('older', 0), ('current', 1), ('unknown', NULL)");
      sql.exec("INSERT INTO messages VALUES (1, 'first', 0, 'd', '{\"epoch\":1}')");
      sql.exec("UPDATE sqlite_sequence SET seq = 10000 WHERE name = 'messages'");
    });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "trims"));
    await stored(a);
    const left = await runInDurableObject<Channel, string[]>(stub, (_, state) =>
      state.storage.sql.exec<{ id: string }>("SELECT id FROM retired_ids ORDER BY id").toArray().map((r) => r.id),
    );
    expect(left).toEqual(["current", "first", "unknown"]);
    a.ws.close();
  });

  it("keeps older history when a message cannot be inserted", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    await runInDurableObject<Channel, void>(stub, (_, state) => {
      const sql = state.storage.sql;
      // Two rows straddling the old error-recovery cutoff.
      sql.exec("INSERT INTO messages VALUES (1, 'first', 0, 'd', '{}'), (10000, 'last', 0, 'd', '{}')");
      sql.exec("CREATE TRIGGER refuse_insert BEFORE INSERT ON messages BEGIN SELECT RAISE(ABORT, 'test insert failure'); END");
    });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "not stored"));
    expect((await refused(a)).error).toBe("the channel could not store the message");
    expect(await storedCount(t)).toBe(2);
    a.ws.close();
  });

  it("pages with after and limit, oldest first", async () => {
    const t = await team();
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const sent = [];
    for (let i = 0; i < 5; i++) {
      const msg = await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, `n${i}`);
      a.send(msg);
      await stored(a);
      sent.push(msg);
    }
    a.ws.close();
    const path = `/teams/${t.teamId}/channels/${t.channelId}/messages`;
    const m = await join(t);
    const all = await expectJson<{ seq: number; at: number; message: unknown }[]>(api(m.user, "GET", path, { device: m.device }), 200);
    expect(all.map((r) => r.seq)).toEqual([1, 2, 3, 4, 5]);
    expect(all.map((r) => r.message)).toEqual(sent);
    const page = await expectJson<{ seq: number }[]>(api(m.user, "GET", `${path}?after=2&limit=2`, { device: m.device }), 200);
    expect(page.map((r) => r.seq)).toEqual([3, 4]);
    for (const [cursor, want] of [["latest", [4, 5]], ["4", [2, 3]], ["2", [1]]] as const) {
      const older = await expectJson<{ seq: number }[]>(api(m.user, "GET", `${path}?before=${cursor}&limit=2`, { device: m.device }), 200);
      expect(older.map((r) => r.seq)).toEqual(want);
    }
    await expectJson(api(m.user, "GET", `${path}?before=latest&after=1`, { device: m.device }), 400);
    expect(await expectJson(api(m.user, "GET", `${path}?after=5`, { device: m.device }), 200)).toEqual([]);
    for (const q of ["?limit=0", "?limit=201", "?after=-1", "?after=x"]) await expectJson(api(m.user, "GET", `${path}${q}`, { device: m.device }), 400);
    await expectJson(api(m.user, "GET", path), 400);
  });
});

describe("senders", () => {
  it("give the devices of someone who wrote in the channel and left, and of nobody else", async () => {
    const t = await team();
    const leaver = await join(t);
    const desk = await makeDevice();
    await expectJson(register(leaver.user, desk, leaver.device, "desk"), 201);
    const newcomer = await join(t);
    const silent = await join(t);
    const c = await openChannel(leaver.user, leaver.device, t.teamId, t.channelId);
    c.send(await message(t.teamId, t.channelId, leaver.device, 1, t.keys.get(1)!, "bye"));
    await stored(c);
    c.ws.close();
    await expectJson(api(leaver.user, "DELETE", `/teams/${t.teamId}/members/${leaver.user.id}`), 200);

    // The directory no longer lists them: they share no team with anyone.
    await expectJson(api(newcomer.user, "GET", `/users/${leaver.user.id}/devices`), 404);
    const path = (d: string) => `/teams/${t.teamId}/channels/${t.channelId}/senders/${d}`;
    const got = await expectJson<{ github_id: number; login: string; devices: { device_id: string }[] }>(
      api(newcomer.user, "GET", path(leaver.device.id), { device: newcomer.device }),
      200,
    );
    expect(got.github_id).toBe(leaver.user.id);
    expect(got.login).toBe(leaver.user.login);
    const byId = new Map(got.devices.map((d) => [d.device_id, d]));
    expect(byId.size).toBe(2);
    expect(byId.get(leaver.device.id)).toEqual({
      device_id: leaver.device.id,
      sig_pub: leaver.device.sigPub,
      enc_pub: leaver.device.encPub,
      name: "laptop",
      created_at: expect.any(Number),
      endorsed_by: null,
      endorsement_sig: null,
    });
    // The endorsement comes along, so the chain can be checked.
    expect(byId.get(desk.id)).toMatchObject({ endorsed_by: leaver.device.id, endorsement_sig: expect.any(String) });

    // A device that never wrote here, a member's or a stranger's, is not found.
    await expectJson(api(newcomer.user, "GET", path(silent.device.id), { device: newcomer.device }), 404);
    const stranger = { user: await user(9_100_001, "stranger"), device: await makeDevice() };
    await expectJson(register(stranger.user, stranger.device), 201);
    await expectJson(api(newcomer.user, "GET", path(stranger.device.id), { device: newcomer.device }), 404);
    await expectJson(api(newcomer.user, "GET", path("not-a-device"), { device: newcomer.device }), 404);
    // Only members ask, from one of their devices.
    await expectJson(api(leaver.user, "GET", path(leaver.device.id), { device: leaver.device }), 404);
    await expectJson(api(stranger.user, "GET", path(leaver.device.id), { device: stranger.device }), 404);
    await expectJson(api(newcomer.user, "GET", path(leaver.device.id)), 400);
  });

  it("are looked up per channel: a message in another channel does not count", async () => {
    const t = await team();
    const m = await join(t);
    const c = await openChannel(m.user, m.device, t.teamId, t.channelId);
    c.send(await message(t.teamId, t.channelId, m.device, 1, t.keys.get(1)!, "only here"));
    await stored(c);
    c.ws.close();
    const other = ulid();
    const name_ct = await seal(t.keys.get(1)!, 1, `arakun-teams/v1/channel|${t.teamId}|${other}|${other}`, "random");
    await expectJson(api(t.owner.user, "POST", `/teams/${t.teamId}/channels`, { body: { channel_id: other, name_ct } }), 201);
    const ask = (ch: string) => api(t.owner.user, "GET", `/teams/${t.teamId}/channels/${ch}/senders/${m.device.id}`, { device: t.owner.device });
    expect((await expectJson<{ github_id: number }>(ask(t.channelId), 200)).github_id).toBe(m.user.id);
    await expectJson(ask(other), 404);
  });
});
