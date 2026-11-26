import { SELF, env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { parsePhoneFrame, pcControl } from "../../src/relay/frames";
import { FREE } from "../../src/relay/limits";
import { hex, sha256, type Env } from "../../src/util";
import { as, call, type SignedIn } from "../helpers";
import { HOST, Inbox, account, claim, computer, computerAndPhone, connectedComputer, device, openPc, openPhone, opened, stored } from "./helpers";

interface Machine {
  label: string;
  pub: string;
  name: string;
  kind: string;
  online: boolean;
  last_seen_at: number;
}

async function machinesOf(session: string): Promise<Machine[]> {
  const res = await call("GET", "/machines", as(session));
  expect(res.status).toBe(200);
  return res.json();
}

describe("a computer with its account's session", () => {
  it("claims its label, and the account's machines list it online with its key", async () => {
    const owner = await account();
    const me = await computer();
    const pc = await openPc(me.label, owner.session, "name=Rik%E2%80%99s%20desk&kind=server");
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
    const list = await machinesOf(owner.session);
    expect(list).toEqual([{ label: me.label, pub: me.pubB64, name: "Rik’s desk", kind: "server", online: true, last_seen_at: expect.any(Number) }]);
    expect(await stored(me.label, "account")).toBe(owner.github_id);

    pc.ws.close(1000, "going away");
    await pc.inbox.closedWith();
    for (let i = 0; i < 50 && (await machinesOf(owner.session))[0]!.online; i++) await new Promise((r) => setTimeout(r, 20));
    expect((await machinesOf(owner.session))[0]!.online).toBe(false);
  });

  it("lists no machine of another account", async () => {
    await connectedComputer();
    const other = await account();
    expect(await machinesOf(other.session)).toEqual([]);
    expect((await call("GET", "/machines")).status).toBe(401);
  });

  it("is refused a machine past the free plan, but a known one comes back", async () => {
    const owner = await account();
    const full = [];
    for (let i = 0; i < FREE.machines; i++) full.push(await connectedComputer(owner));
    const sixth = await computer();
    const pc = await openPc(sixth.label, owner.session);
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "machines_full" });
    expect((await pc.inbox.closedWith()).code).toBe(4429);
    expect((await machinesOf(owner.session)).map((m) => m.label).sort()).toEqual(full.map((c) => c.me.label).sort());

    // One of them reconnecting is not a new machine.
    const again = await openPc(full[0]!.me.label, owner.session);
    expect(await claim(again, full[0]!.me)).toEqual({ type: "claimed" });
  });

  it("of another account cannot take a label that is taken, and leaves the owner's machine as it was", async () => {
    const { me, owner } = await connectedComputer();
    const before = await machinesOf(owner.session);
    const intruder = await account();
    const pc = await openPc(me.label, intruder.session, "name=Mine&kind=server");
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "label_taken" });
    expect((await pc.inbox.closedWith()).code).toBe(4403);
    expect(await machinesOf(owner.session)).toEqual(before);
    expect(await machinesOf(intruder.session)).toEqual([]);
  });

  it("is told signed_out without a valid session", async () => {
    const me = await computer();
    for (const session of [null, "A".repeat(43), "not a token"]) {
      const pc = await openPc(me.label, session);
      expect(await pc.inbox.control(), String(session)).toEqual({ type: "bye", reason: "signed_out" });
      expect((await pc.inbox.closedWith()).code).toBe(4401);
    }
  });

  it("must name itself as an app or a server", async () => {
    const owner = await account();
    const me = await computer();
    for (const query of ["name=Desk&kind=phone", "name=&kind=app", `name=${"x".repeat(101)}&kind=app`]) {
      const res = await SELF.fetch(`${HOST}/machines/${me.label}/pc?${query}`, { headers: { Upgrade: "websocket", ...as(owner.session) } });
      expect(res.status, query).toBe(400);
    }
  });

  it("is closed with unknown control when it still sends tickets or pairing", async () => {
    for (const msg of [{ type: "tickets", hashes: [] }, { type: "pair_open", hash: "a".repeat(64), ttl: 60 }, { type: "pair_close" }]) {
      const { pc } = await connectedComputer();
      pc.ws.send(pcControl(msg));
      expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "unknown control" });
      expect((await pc.inbox.closedWith()).code).toBe(4400);
    }
  });

  it("wants a socket", async () => {
    const owner = await account();
    const me = await computer();
    expect((await call("GET", `/machines/${me.label}/pc`, as(owner.session))).status).toBe(426);
    expect((await call("GET", `/machines/${me.label}/ws`, as(owner.session))).status).toBe(426);
  });
});

describe("a device", () => {
  it("of the machine's account comes in, and the computer hears who it is", async () => {
    const { me, pc, owner } = await connectedComputer();
    const dev = await device(owner, "Rik’s iPhone 📱", "ios");
    const phone = await openPhone(me.label, dev.session, "page=aaaaaaaa");
    expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await pc.inbox.control()).toEqual({ type: "open", channel: 1, device: dev.session_id, name: "Rik’s iPhone 📱", kind: "ios", account: owner.github_id });
    expect(phone.inbox.closed).toBeNull();
  });

  it("of another account is told not_yours, and the computer hears nothing", async () => {
    const { me, pc } = await connectedComputer();
    const stranger = await device(await account());
    const phone = await openPhone(me.label, stranger.session);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "not_yours" });
    expect((await phone.inbox.closedWith()).code).toBe(4403);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("cannot pass as another account or device with headers of its own", async () => {
    const { me, pc, owner } = await connectedComputer();
    const stranger = await device(await account());
    const forged = {
      "x-arakun-account": String(owner.github_id),
      "x-arakun-label": me.label,
      "x-arakun-session": "0".repeat(64),
      "x-arakun-plan": "unlimited",
    };
    const phone = await openPhone(me.label, stranger.session, "", forged);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "not_yours" });
    await expect(pc.inbox.next(200)).rejects.toThrow();

    const dev = await device(owner);
    const own = await openPhone(me.label, dev.session, "", { "x-arakun-device": "someone-else", "x-arakun-device-name": "Admin", "x-arakun-device-kind": "app" });
    expect(await own.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await pc.inbox.control()).toEqual(opened(1, dev));
  });

  it("is told signed_out without a valid session", async () => {
    const { me, pc } = await connectedComputer();
    for (const session of [null, "A".repeat(43)]) {
      const phone = await openPhone(me.label, session);
      expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
      expect((await phone.inbox.closedWith()).code).toBe(4401);
    }
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("is told no_machine for a label nobody registered", async () => {
    const dev = await device(await account());
    const nobody = await computer();
    const phone = await openPhone(nobody.label, dev.session);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "no_machine" });
    expect((await phone.inbox.closedWith()).code).toBe(4404);
  });

  it("gets its own channel number, never reused", async () => {
    const { me, pc, dev, phone: first } = await computerAndPhone();
    first.ws.close(1000, "done");
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "done" });
    const second = await openPhone(me.label, dev.session);
    expect(await second.inbox.control()).toEqual({ type: "open", channel: 2 });
  });

  it("is refused by the object when the object's account is another one", async () => {
    // The Worker checks D1 first; this is the object's own check behind it.
    const { me, pc, owner } = await connectedComputer();
    const stranger = await account();
    const res = await env.RELAY.get(env.RELAY.idFromName(me.label)).fetch("https://relay/ws", {
      headers: {
        Upgrade: "websocket",
        "x-arakun-label": me.label,
        "x-arakun-account": String(stranger.github_id),
        "x-arakun-plan": "free",
        "x-arakun-session": "0".repeat(64),
        "x-arakun-device": "d",
        "x-arakun-device-name": "x",
        "x-arakun-device-kind": "phone",
      },
    });
    expect(res.status).toBe(101);
    const ws = res.webSocket!;
    const inbox = new Inbox(ws, (b) => parsePhoneFrame(b) ?? { bad: true });
    ws.accept();
    ws.binaryType = "arraybuffer";
    expect(await inbox.control()).toEqual({ type: "bye", reason: "not_yours" });
    expect((await inbox.closedWith()).code).toBe(4403);
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await stored(me.label, "account")).toBe(owner.github_id);
  });
});

/// A device's socket as machines.ts opens it on the label's object, once the
/// Worker has checked the session: what the object gets when that check is
/// already stale.
async function pastTheDoor(label: string, dev: SignedIn): Promise<{ ws: WebSocket; inbox: Inbox }> {
  const res = await env.RELAY.get(env.RELAY.idFromName(label)).fetch("https://relay/ws", { headers: doorHeaders(label, dev, await hashOf(dev)) });
  expect(res.status).toBe(101);
  const ws = res.webSocket!;
  const inbox = new Inbox(ws, (b) => parsePhoneFrame(b) ?? { bad: true });
  ws.accept();
  ws.binaryType = "arraybuffer";
  return { ws, inbox };
}

function doorHeaders(label: string, dev: SignedIn, hash: string): Record<string, string> {
  return {
    Upgrade: "websocket",
    "x-arakun-label": label,
    "x-arakun-account": String(dev.github_id),
    "x-arakun-plan": "free",
    "x-arakun-session": hash,
    "x-arakun-device": dev.session_id,
    "x-arakun-device-name": "Phone",
    "x-arakun-device-kind": "phone",
  };
}

async function hashOf(dev: SignedIn): Promise<string> {
  return hex(await sha256(dev.session));
}

describe("a device the Worker let through just before", () => {
  it("its session was signed out is told signed_out by the object, and gets no channel", async () => {
    const { me, pc, owner } = await connectedComputer();
    const dev = await device(owner);
    // The sign-out and its recheck are over before the object hears of the device.
    expect((await call("DELETE", `/sessions/${dev.session_id}`, as(owner.session))).status).toBe(204);
    const phone = await pastTheDoor(me.label, dev);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    expect((await phone.inbox.closedWith()).code).toBe(4401);
    await expect(pc.inbox.next(200)).rejects.toThrow();

    // A device still signed in comes in by the same way.
    const stays = await pastTheDoor(me.label, await device(owner));
    expect(await stays.inbox.control()).toEqual({ type: "open", channel: 1 });
  });

  it("its session expired is told signed_out", async () => {
    const { me, owner } = await connectedComputer();
    const dev = await device(owner);
    await env.DB.prepare("UPDATE sessions SET expires_at = 1 WHERE id = ?").bind(dev.session_id).run();
    const phone = await pastTheDoor(me.label, dev);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
  });

  it("its account was blocked is told blocked", async () => {
    const { me, owner } = await connectedComputer();
    const dev = await device(owner);
    await env.DB.prepare("UPDATE accounts SET blocked_at = 1 WHERE id = ?").bind(owner.account_id).run();
    const phone = await pastTheDoor(me.label, dev);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
    expect((await phone.inbox.closedWith()).code).toBe(4403);
  });

  it("its machine was forgotten is told no_machine", async () => {
    const { me, owner } = await connectedComputer();
    const dev = await device(owner);
    await env.DB.prepare("DELETE FROM machines WHERE label = ?").bind(me.label).run();
    const phone = await pastTheDoor(me.label, dev);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "no_machine" });
    expect((await phone.inbox.closedWith()).code).toBe(4404);
  });

  it("is refused when the sign-out and its recheck land while the object reads D1", async () => {
    const { me, pc, owner } = await connectedComputer();
    const dev = await device(owner);
    const hash = await hashOf(dev);
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    // The object's first read of the device's session answers as D1 stood
    // before the sign-out, and only after the sign-out's recheck has run.
    let reads = 0;
    await runInDurableObject(stub, (instance) => {
      const real = (instance as unknown as { env: Env }).env;
      const db = {
        batch: (statements: D1PreparedStatement[]) => real.DB.batch(statements),
        prepare: (q: string) => {
          const statement = real.DB.prepare(q);
          if (!q.includes("AS owner") || reads++ > 0) return statement;
          return {
            bind: (...values: unknown[]) => ({
              first: async () => {
                const row = await statement.bind(...values).first();
                await real.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(hash).run();
                const res = await instance.fetch(new Request("https://relay/_recheck", { method: "POST", headers: { "x-arakun-label": me.label } }));
                expect(res.status).toBe(204);
                return row;
              },
            }),
          };
        },
      };
      (instance as unknown as { env: Env }).env = Object.create(real, { DB: { value: db } });
    });
    const res = await stub.fetch("https://relay/ws", { headers: doorHeaders(me.label, dev, hash) });
    const ws = res.webSocket!;
    const inbox = new Inbox(ws, (b) => parsePhoneFrame(b) ?? { bad: true });
    ws.accept();
    ws.binaryType = "arraybuffer";
    expect(await inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    expect((await inbox.closedWith()).code).toBe(4401);
    expect(reads).toBe(2);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });
});

describe("the computer's presence", () => {
  it("is announced to devices that arrive while it is away, and again when it returns", async () => {
    const { me, pc, owner } = await connectedComputer();
    pc.ws.close(1000, "going away");
    await new Promise((r) => setTimeout(r, 50));
    const since = await stored<number>(me.label, "last_seen");
    expect(typeof since).toBe("number");

    const dev = await device(owner);
    const phone = await openPhone(me.label, dev.session);
    expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await phone.inbox.control()).toEqual({ type: "pc_offline", since });

    const back = await openPc(me.label, owner.session);
    expect(await claim(back, me)).toEqual({ type: "claimed" });
    expect(await back.inbox.control()).toEqual(opened(1, dev));
    expect(await phone.inbox.control()).toEqual({ type: "pc_online" });
  });

  it("is lost to connected devices when it hangs up", async () => {
    const { pc, phone } = await computerAndPhone();
    pc.ws.close(1000, "going away");
    const told = await phone.inbox.control();
    expect(told["type"]).toBe("pc_offline");
    expect(typeof told["since"]).toBe("number");
    expect(phone.inbox.closed).toBeNull();
  });

  it("is kept for devices when one socket of the computer replaces another", async () => {
    const { me, pc, owner, dev, phone } = await computerAndPhone();
    const second = await openPc(me.label, owner.session);
    expect(await claim(second, me)).toEqual({ type: "claimed" });
    expect(await second.inbox.control()).toEqual(opened(1, dev));
    expect(await phone.inbox.control()).toEqual({ type: "pc_online" });
    expect(await pc.inbox.closedWith()).toMatchObject({ code: 4409 });
    // No pc_offline follows the replaced socket's closing.
    await expect(phone.inbox.next(200)).rejects.toThrow();
  });

  it("introduces a device arriving right after a replacement to the new socket", async () => {
    const { me, owner } = await connectedComputer();
    const second = await openPc(me.label, owner.session);
    expect(await claim(second, me)).toEqual({ type: "claimed" });
    // The replaced socket is still closing when the device comes in.
    const dev = await device(owner);
    const phone = await openPhone(me.label, dev.session);
    expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await second.inbox.control()).toEqual(opened(1, dev));
    expect(phone.inbox.closed).toBeNull();
    await expect(phone.inbox.next(200)).rejects.toThrow();
  });
});
