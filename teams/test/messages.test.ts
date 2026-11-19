import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { Channel } from "../src/channel";
import { b64url } from "../src/util";
import { ADMIN, call } from "./helpers";
import { api, envelope, expectJson, join, message, newKey, openChannel, team, type Socket, type TeamSetup } from "./clients";

/// The next frame, which must be a stored message.
async function stored(s: Socket): Promise<{ seq: number; at: number; message: { id: string } }> {
  const got = await s.inbox.next();
  if (got.type !== "message") throw new Error(`expected a message, got ${JSON.stringify(got)}`);
  return got;
}

async function refused(s: Socket): Promise<{ id: string | null; error: string }> {
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
    expect((await refused(a)).error).toMatch(/exists/);
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
    await env.DB.prepare("DELETE FROM members WHERE github_id = ?").bind(m.user.id).run();
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

  it("check a socket admitted while a send checks the others, so a logout in between is not missed", async () => {
    const t = await team();
    const m = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const { token_hash: mine } = (await env.DB.prepare("SELECT token_hash FROM sessions WHERE github_id = ?").bind(m.user.id).first<{ token_hash: string }>())!;
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
      (await env.DB.prepare("SELECT token_hash FROM sessions WHERE github_id = ?").bind(id).first<{ token_hash: string }>())!.token_hash;
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
    expect(await refused(a)).toEqual({ type: "error", id: msg.id, error: "the server could not check the message: send it again" });
    expect(await storedCount(t)).toBe(0);
    a.ws.close();
  });

  it("ignore trusted headers a client sends", async () => {
    const t = await team();
    const m = await join(t);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId, {
      "X-Teams-Github-Id": String(t.owner.user.id),
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
    for (const msg of msgs) a.send(msg);
    const frames = [];
    for (let i = 0; i < 61; i++) frames.push(await a.inbox.next());
    expect(frames.filter((f) => f.type === "message")).toHaveLength(60);
    expect(frames.filter((f) => f.type === "error").map((f) => f.error)).toEqual(["too many messages, slow down"]);
    a.ws.close();
  });
});

describe("rotation", () => {
  it("takes only the newest epoch with envelopes, so a stale one is refused once the new one has them", async () => {
    const t = await team();
    const m = await join(t);
    const gone = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${gone.user.id}`), 200);

    // Rotation pending: the old key still works.
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "old key"));
    expect((await stored(a)).message).toMatchObject({ epoch: 1 });
    a.send(await message(t.teamId, t.channelId, t.owner.device, 3, newKey(), "future"));
    expect((await refused(a)).error).toBe("unknown epoch");
    // Epoch 2 has no envelopes yet, and a second removal could skip it for good.
    const key2 = newKey();
    a.send(await message(t.teamId, t.channelId, t.owner.device, 2, key2, "current epoch, before its envelopes"));
    expect((await refused(a)).error).toMatch(/epoch 2 has no envelopes yet/);

    const envelopes = [await envelope(t.teamId, 2, key2, t.owner.device, t.owner.device), await envelope(t.teamId, 2, key2, m.device, t.owner.device)];
    await expectJson(api(t.owner.user, "PUT", `/teams/${t.teamId}/envelopes`, { device: t.owner.device, body: { envelopes } }), 200);

    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "old key again"));
    expect((await refused(a)).error).toMatch(/stale epoch/);
    a.send(await message(t.teamId, t.channelId, t.owner.device, 2, key2, "new key"));
    expect((await stored(a)).message).toMatchObject({ epoch: 2 });
    a.ws.close();
  });

  it("refuses an old-epoch send that a new-epoch send overtook while it was checked", async () => {
    const t = await team();
    const m = await join(t);
    const gone = await join(t);
    const a = await openChannel(t.owner.user, t.owner.device, t.teamId, t.channelId);
    const b = await openChannel(m.user, m.device, t.teamId, t.channelId);
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${gone.user.id}`), 200);
    const owner = (await env.DB.prepare("SELECT token_hash FROM sessions WHERE github_id = ?").bind(t.owner.user.id).first<{ token_hash: string }>())!.token_hash;
    const stub = env.CHANNEL.get(env.CHANNEL.idFromName(`${t.teamId}/${t.channelId}`)) as DurableObjectStub;
    // Holds the owner's epoch-1 send after it read the team as pending.
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
    const old = await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "sent before the rotation");
    a.send(old);
    while (!(await runInDurableObject<Channel, boolean>(stub, (ch) => (ch as any).held))) await new Promise((r) => setTimeout(r, 1));
    // The rotation ends and the member sends with the new key meanwhile.
    const key2 = newKey();
    const envelopes = [await envelope(t.teamId, 2, key2, t.owner.device, t.owner.device), await envelope(t.teamId, 2, key2, m.device, t.owner.device)];
    await expectJson(api(t.owner.user, "PUT", `/teams/${t.teamId}/envelopes`, { device: t.owner.device, body: { envelopes } }), 200);
    b.send(await message(t.teamId, t.channelId, m.device, 2, key2, "new key"));
    expect((await stored(b)).message).toMatchObject({ epoch: 2 });
    await runInDurableObject<Channel, void>(stub, (ch) => (ch as any).release());
    expect((await stored(a)).message).toMatchObject({ epoch: 2 });
    expect(await refused(a)).toEqual({ type: "error", id: old.id, error: "stale epoch: fetch the new team key" });
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

    // The others stay connected.
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "still here"));
    await stored(a);
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
      (await env.DB.prepare("SELECT token_hash FROM sessions WHERE github_id = ?").bind(id).first<{ token_hash: string }>())!.token_hash;
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
    // A send is a barrier: its ack comes after any presence sent before it.
    a.send(await message(t.teamId, t.channelId, t.owner.device, 1, t.keys.get(1)!, "barrier"));
    await stored(a);
    // The older run sent nothing: no frame past the newer run's.
    expect(a.inbox.here.count).toBe(had + 1);
    expect(b.inbox.here.count).toBe(0);
    for (const s of [a, a2]) s.ws.close();
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
    expect(await expectJson(api(m.user, "GET", `${path}?after=5`, { device: m.device }), 200)).toEqual([]);
    for (const q of ["?limit=0", "?limit=201", "?after=-1", "?after=x"]) await expectJson(api(m.user, "GET", `${path}${q}`, { device: m.device }), 400);
    await expectJson(api(m.user, "GET", path), 400);
  });
});
