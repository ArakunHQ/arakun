import { SELF, env, runDurableObjectAlarm, runInDurableObject } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";
import { DATA, pcFrame, phoneFrame } from "../../src/relay/frames";
import { FREE, disabledReason, planFor } from "../../src/relay/limits";
import { ADMIN, as, call } from "../helpers";
import { HOST, account, claim, computer, computerAndPhone, connectedComputer, device, openPc, openPhone, opened, stored } from "./helpers";

const LABEL = "k7f3q2m4xwab4c5d";

// KV outlives a test: every test starts from an empty one.
beforeEach(async () => {
  for (const key of ["plans", "disabled"]) await env.CONFIG.delete(key);
});

async function plans(free: object) {
  await env.CONFIG.put("plans", JSON.stringify({ free }));
}

describe("what KV says", () => {
  it("falls back to the free plan, field by field", async () => {
    expect(await planFor(env.CONFIG)).toEqual(FREE);
    await plans({ phones: 1, burst: "lots", daily: -5 });
    expect(await planFor(env.CONFIG)).toEqual({ ...FREE, phones: 1 });
    await env.CONFIG.put("plans", "not json");
    expect(await planFor(env.CONFIG)).toEqual(FREE);
  });

  it("gives the free plan five machines", async () => {
    expect(FREE.machines).toBe(5);
    await plans({ machines: 2 });
    expect((await planFor(env.CONFIG)).machines).toBe(2);
    await env.CONFIG.put("plans", JSON.stringify({ free: { machines: 2 }, pro: { phones: 9 } }));
    expect(await planFor(env.CONFIG, "pro")).toEqual({ ...FREE, phones: 9 });
    // A plan KV does not know is the free plan.
    expect(await planFor(env.CONFIG, "gold")).toEqual({ ...FREE, machines: 2 });
  });

  it("reads the switch", async () => {
    expect(await disabledReason(env.CONFIG)).toBeNull();
    await env.CONFIG.put("disabled", "true");
    expect(await disabledReason(env.CONFIG)).toBe("relay disabled");
    await env.CONFIG.put("disabled", "back at 05:00 UTC");
    expect(await disabledReason(env.CONFIG)).toBe("back at 05:00 UTC");
    await env.CONFIG.put("disabled", "false");
    expect(await disabledReason(env.CONFIG)).toBeNull();
  });
});

describe("the switch", () => {
  it("closes both sockets with bye disabled and the message, and answers plain HTTP with 503", async () => {
    const { me, owner } = await connectedComputer();
    await env.CONFIG.put("disabled", "back soon");
    const pc = await openPc(me.label, owner.session);
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "disabled", message: "back soon" });
    expect((await pc.inbox.closedWith()).code).toBe(4503);
    const phone = await openPhone(me.label, (await device(owner)).session);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "disabled", message: "back soon" });
    expect((await phone.inbox.closedWith()).code).toBe(4503);

    for (const door of ["pc", "ws"]) {
      const res = await SELF.fetch(`${HOST}/machines/${LABEL}/${door}?name=Desk`, { headers: as(owner.session) });
      expect(res.status, door).toBe(503);
      expect(await res.json()).toEqual({ error: "back soon" });
    }
    await env.CONFIG.delete("disabled");
    const res = await SELF.fetch(`${HOST}/machines/${LABEL}/ws`, { headers: as(owner.session) });
    expect(res.status).toBe(426);
  });
});

describe("the plan's limits", () => {
  it("let three devices in and tell the fourth", async () => {
    const { me, pc, owner } = await connectedComputer();
    const dev = await device(owner);
    const phones = [];
    for (let i = 1; i <= 3; i++) {
      const p = await openPhone(me.label, dev.session);
      expect(await p.inbox.control()).toEqual({ type: "open", channel: i });
      await pc.inbox.control();
      phones.push(p);
    }
    const fourth = await openPhone(me.label, dev.session);
    expect(await fourth.inbox.control()).toEqual({ type: "limit", which: "phones" });
    expect((await fourth.inbox.closedWith()).code).toBe(4429);
    await expect(pc.inbox.next(200)).rejects.toThrow();

    phones[0]!.ws.close(1000, "leaving");
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "leaving" });
    const fifth = await openPhone(me.label, dev.session);
    expect(await fifth.inbox.control()).toEqual({ type: "open", channel: 4 });
  });

  it("come from KV: a plan with one device", async () => {
    await plans({ phones: 1 });
    const { me, owner } = await connectedComputer();
    const one = await openPhone(me.label, (await device(owner)).session);
    expect(await one.inbox.control()).toEqual({ type: "open", channel: 1 });
    const two = await openPhone(me.label, (await device(owner)).session);
    expect(await two.inbox.control()).toEqual({ type: "limit", which: "phones" });
  });

  it("follow the account's plan, set by the operator", async () => {
    await env.CONFIG.put("plans", JSON.stringify({ free: {}, pro: { phones: 1 } }));
    const owner = await account();
    expect((await call("PUT", `/admin/users/${owner.github_id}/plan`, { ...ADMIN, "Content-Type": "application/json" }, JSON.stringify({ plan: "pro" }))).status).toBe(204);
    const { me } = await connectedComputer(owner);
    const one = await openPhone(me.label, (await device(owner)).session);
    expect(await one.inbox.control()).toEqual({ type: "open", channel: 1 });
    const two = await openPhone(me.label, (await device(owner)).session);
    expect(await two.inbox.control()).toEqual({ type: "limit", which: "phones" });

    // Another account on the free plan keeps three.
    const { me: other, owner: free } = await connectedComputer();
    for (let i = 1; i <= 3; i++) {
      const p = await openPhone(other.label, (await device(free)).session);
      expect(await p.inbox.control()).toEqual({ type: "open", channel: i });
    }
  });

  it("change for sockets already open when the operator changes the plan", async () => {
    await env.CONFIG.put("plans", JSON.stringify({ free: {}, small: { frame: 1 } }));
    const { pc, owner, phone } = await computerAndPhone();
    phone.ws.send(phoneFrame(DATA, new Uint8Array([1, 2])));
    expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload).toEqual(new Uint8Array([1, 2]));

    const res = await call("PUT", `/admin/users/${owner.github_id}/plan`, { ...ADMIN, "Content-Type": "application/json" }, JSON.stringify({ plan: "small" }));
    expect(res.status).toBe(204);
    phone.ws.send(phoneFrame(DATA, new Uint8Array([3, 4])));
    expect(await phone.inbox.control()).toEqual({ type: "limit", which: "frame" });
  });

  it("cut a burst: the frame over the per-second count closes the channel", async () => {
    await plans({ burst: 3 });
    const { pc, phone } = await computerAndPhone();
    for (let i = 0; i < 4; i++) phone.ws.send(phoneFrame(DATA, new Uint8Array([i])));
    for (let i = 0; i < 3; i++) expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(i);
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "limit:burst" });
    expect(await phone.inbox.control()).toEqual({ type: "limit", which: "burst" });
    expect((await phone.inbox.closedWith()).code).toBe(4429);
  });

  it("stop the day at its count, for frames and for new devices", async () => {
    await plans({ daily: 2 });
    const { me, pc, dev, phone } = await computerAndPhone();
    phone.ws.send(phoneFrame(DATA, new Uint8Array([1])));
    pc.ws.send(pcFrame(DATA, 1, new Uint8Array([2])));
    expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(1);
    expect(((await phone.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(2);
    phone.ws.send(phoneFrame(DATA, new Uint8Array([3])));
    expect(await phone.inbox.control()).toEqual({ type: "limit", which: "daily" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "limit:daily" });
    const late = await openPhone(me.label, dev.session);
    expect(await late.inbox.control()).toEqual({ type: "limit", which: "daily" });
  });

  it("keep the day's count in storage every fifty frames and start over at midnight", async () => {
    await plans({ burst: 1000 });
    const { me, pc, phone } = await computerAndPhone();
    expect(await stored(me.label, "today")).toBeUndefined();
    for (let i = 0; i < 50; i++) phone.ws.send(phoneFrame(DATA, new Uint8Array([1])));
    for (let i = 0; i < 50; i++) await pc.inbox.next();
    const today = await stored<{ day: number; n: number }>(me.label, "today");
    expect(today?.n).toBe(50);
    expect(today?.day).toBe(Math.floor(Date.now() / 86_400_000));

    // Yesterday's count, however high, does not count today.
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, (instance) => {
      (instance as unknown as { today: { day: number; n: number } }).today = { day: today!.day - 1, n: 1_000_000 };
    });
    phone.ws.send(phoneFrame(DATA, new Uint8Array([7])));
    expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(7);
  });

  it("go by the old plan while the new one is read, then by the new one", async () => {
    await plans({ frame: 10 });
    const { me, pc, phone } = await computerAndPhone();
    // The plan is a minute old and KV now allows only one byte.
    await plans({ frame: 1 });
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, (instance) => {
      (instance as unknown as { plan: { at: number } }).plan.at -= 61_000;
    });
    phone.ws.send(phoneFrame(DATA, new Uint8Array([1, 2])));
    expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload).toEqual(new Uint8Array([1, 2]));
    // The read runs behind the frame; wait for it to land.
    for (let i = 0; i < 100; i++) {
      const fresh = await runInDurableObject(stub, (instance) => (instance as unknown as { plan: { value: { frame: number } } }).plan.value.frame === 1);
      if (fresh) break;
      await new Promise((r) => setTimeout(r, 10));
    }
    phone.ws.send(phoneFrame(DATA, new Uint8Array([3, 4])));
    expect(await phone.inbox.control()).toEqual({ type: "limit", which: "frame" });
  });

  it("bound the frame by the plan, either way", async () => {
    await plans({ frame: 10 });
    const { me, pc, owner, phone: a } = await computerAndPhone();
    const devB = await device(owner);
    const b = await openPhone(me.label, devB.session);
    await b.inbox.control();
    await pc.inbox.control();

    a.ws.send(phoneFrame(DATA, new Uint8Array(10)));
    expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload.length).toBe(10);
    a.ws.send(phoneFrame(DATA, new Uint8Array(11)));
    expect(await a.inbox.control()).toEqual({ type: "limit", which: "frame" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "limit:frame" });

    pc.ws.send(pcFrame(DATA, 2, new Uint8Array(11)));
    expect(await b.inbox.control()).toEqual({ type: "limit", which: "frame" });
    expect((await b.inbox.closedWith()).code).toBe(4429);
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 2, reason: "limit:frame" });
  });
});

describe("a computer gone for ninety days", () => {
  it("has its object emptied by the alarm, and starts clean when it returns", async () => {
    const { me, pc, owner } = await connectedComputer();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    // Only the recheck of the computer's session is due.
    const first = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect((first as number) - Date.now()).toBeLessThanOrEqual(5 * 60_000);

    pc.ws.close(1000, "shutting down");
    await new Promise((r) => setTimeout(r, 50));
    const at = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect(at).not.toBeNull();
    expect(at! - Date.now()).toBeGreaterThan(89 * 24 * 3_600_000);

    const phone = await openPhone(me.label, (await device(owner)).session);
    await phone.inbox.control();
    expect((await phone.inbox.control())["type"]).toBe("pc_offline");

    // An alarm that runs early leaves everything as it was.
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await stored(me.label, "pub")).toBe(me.pubB64);
    await runInDurableObject(stub, async (_i, state) => {
      await state.storage.put("last_seen", Date.now() - 91 * 24 * 3_600_000);
      await state.storage.setAlarm(Date.now());
    });
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "gone" });
    expect((await phone.inbox.closedWith()).code).toBe(4410);
    expect(await stored(me.label, "pub")).toBeUndefined();
    expect(await stored(me.label, "account")).toBeUndefined();

    // The machine is still the account's, so a device may wait for it again.
    // Its channel number is all the object stores, and no reason to purge.
    const waiting = await openPhone(me.label, (await device(owner)).session);
    expect(await waiting.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await waiting.inbox.control()).toEqual({ type: "pc_offline", since: null });
    const next = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect((next as number) - Date.now()).toBeGreaterThan(1000);
    await runInDurableObject(stub, (_i, state) => state.storage.setAlarm(Date.now()));
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(waiting.inbox.closed).toBeNull();
    expect(await stored(me.label, "next_channel")).toBe(1);

    const back = await openPc(me.label, owner.session);
    expect(await claim(back, me)).toEqual({ type: "claimed" });
    expect((await back.inbox.control())["type"]).toBe("open");
    expect(await waiting.inbox.control()).toEqual({ type: "pc_online" });
    expect(await stored(me.label, "pub")).toBe(me.pubB64);
    expect(await stored(me.label, "account")).toBe(owner.github_id);
  });

  it("is not purged while another socket of it is connected", async () => {
    const { me, pc, owner } = await connectedComputer();
    const second = await openPc(me.label, owner.session);
    expect(await claim(second, me)).toEqual({ type: "claimed" });
    await pc.inbox.closedWith();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, (_i, state) => state.storage.setAlarm(Date.now() + 1000));
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await stored(me.label, "pub")).toBe(me.pubB64);
  });
});

describe("a socket at /pc that never claims", () => {
  it("stores nothing, and is closed by the alarm when its time is up", async () => {
    const me = await computer();
    const pc = await openPc(me.label, (await account()).session);
    expect((await pc.inbox.control())["type"]).toBe("challenge");
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    const [keys, at] = await runInDurableObject(stub, async (_i, state) => [
      [...(await state.storage.list()).keys()],
      await state.storage.getAlarm(),
    ]);
    expect(keys).toEqual([]);
    expect(at).not.toBeNull();
    expect((at as number) - Date.now()).toBeLessThanOrEqual(30_000);

    // Early: still waiting.
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(pc.inbox.closed).toBeNull();

    await runInDurableObject(stub, async (_i, state) => {
      for (const ws of state.getWebSockets("pc")) ws.serializeAttachment({ ...ws.deserializeAttachment(), until: 0 });
      await state.storage.setAlarm(Date.now());
    });
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "claim_timeout" });
    expect((await pc.inbox.closedWith()).code).toBe(4401);
    expect(await runInDurableObject(stub, (_i, state) => state.storage.getAlarm())).toBeNull();
  });

  it("does not keep the alarm from purging a computer that left", async () => {
    const { me, pc, owner } = await connectedComputer();
    pc.ws.close(1000, "shutting down");
    await new Promise((r) => setTimeout(r, 50));
    const stranger = await openPc(me.label, owner.session);
    await stranger.inbox.control();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    const at = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect((at as number) - Date.now()).toBeLessThanOrEqual(30_000);

    // Once the stranger is gone, the alarm is back on the purge.
    stranger.ws.close(1000, "bye");
    await new Promise((r) => setTimeout(r, 50));
    const later = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect((later as number) - Date.now()).toBeGreaterThan(89 * 24 * 3_600_000);
  });

  it("clears what an older relay stored for it", async () => {
    const me = await computer();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, (_i, state) => state.storage.put("label", me.label));
    // The challenge waits in the inbox for claim() below.
    const pc = await openPc(me.label, (await account()).session);
    expect(await runInDurableObject(stub, (_i, state) => state.storage.getAlarm())).not.toBeNull();
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await stored(me.label, "label")).toBeUndefined();
    // The socket still has its time to claim.
    expect(pc.inbox.closed).toBeNull();
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
    expect(await stored(me.label, "label")).toBe(me.label);
  });
});

describe("a device that switched networks", () => {
  it("replaces its own old socket instead of hitting the device limit", async () => {
    await plans({ phones: 2 });
    const { me, pc, owner } = await connectedComputer();
    const dev1 = await device(owner);
    const dev2 = await device(owner);
    const old = await openPhone(me.label, dev1.session, "page=aaaaaaaa");
    expect(await old.inbox.control()).toEqual({ type: "open", channel: 1 });
    await pc.inbox.control();
    const other = await openPhone(me.label, dev2.session, "page=bbbbbbbb");
    expect(await other.inbox.control()).toEqual({ type: "open", channel: 2 });
    await pc.inbox.control();

    const again = await openPhone(me.label, dev1.session, "page=aaaaaaaa");
    expect(await old.inbox.control()).toEqual({ type: "bye", reason: "replaced" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "replaced" });
    expect(await again.inbox.control()).toEqual({ type: "open", channel: 3 });
    expect(await pc.inbox.control()).toEqual(opened(3, dev1));
    expect(other.inbox.closed).toBeNull();

    // Another device still finds the limit, even with the same page id.
    const third = await openPhone(me.label, (await device(owner)).session, "page=aaaaaaaa");
    expect(await third.inbox.control()).toEqual({ type: "limit", which: "phones" });
  });

  it("never replaces another live tab of the same device, or a page that sends no id", async () => {
    await plans({ phones: 1 });
    const { me, owner } = await connectedComputer();
    const dev = await device(owner);
    const tab = await openPhone(me.label, dev.session, "page=aaaaaaaa");
    expect(await tab.inbox.control()).toEqual({ type: "open", channel: 1 });

    const otherTab = await openPhone(me.label, dev.session, "page=bbbbbbbb");
    expect(await otherTab.inbox.control()).toEqual({ type: "limit", which: "phones" });
    const noId = await openPhone(me.label, dev.session);
    expect(await noId.inbox.control()).toEqual({ type: "limit", which: "phones" });
    expect(tab.inbox.closed).toBeNull();
  });
});

describe("a device that went quiet", () => {
  it("gets a pong from the relay itself, and is closed once its pings stop", async () => {
    const { me, pc, phone } = await computerAndPhone();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    // The alarm comes back to it, as it does for every device.
    const at = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect((at as number) - Date.now()).toBeLessThanOrEqual(90_000);

    phone.ws.send("ping");
    expect(await phone.inbox.next()).toEqual({ text: "pong" });

    // Pinging recently: it stays.
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(phone.inbox.closed).toBeNull();

    await runInDurableObject(stub, async (instance, state) => {
      (instance as unknown as { silentMs: number }).silentMs = 0;
      await state.storage.setAlarm(Date.now());
    });
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "silent" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "silent" });
  });

  it("is left alone while it has never pinged, as a page from before pings", async () => {
    const { me, phone } = await computerAndPhone();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, async (instance, state) => {
      (instance as unknown as { silentMs: number }).silentMs = 0;
      await state.storage.setAlarm(Date.now());
    });
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(phone.inbox.closed).toBeNull();
  });
});

describe("a device while the places are taken", () => {
  it("gets the place of a socket that never pinged, once it could have", async () => {
    await plans({ phones: 1 });
    const { me, pc, owner, phone: left } = await computerAndPhone();
    const dev2 = await device(owner);

    // Still within the time a pinging socket may be quiet: it stays.
    const early = await openPhone(me.label, dev2.session);
    expect(await early.inbox.control()).toEqual({ type: "limit", which: "phones" });
    expect(left.inbox.closed).toBeNull();

    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, (instance) => {
      (instance as unknown as { silentMs: number }).silentMs = 0;
    });
    const late = await openPhone(me.label, dev2.session);
    expect(await left.inbox.control()).toEqual({ type: "bye", reason: "silent" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "silent" });
    expect(await late.inbox.control()).toEqual({ type: "open", channel: 2 });
  });

  it("never gets the place of a socket that pings", async () => {
    await plans({ phones: 1 });
    const { me, owner, phone } = await computerAndPhone();
    phone.ws.send("ping");
    expect(await phone.inbox.next()).toEqual({ text: "pong" });
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, (instance) => {
      (instance as unknown as { silentMs: number }).silentMs = 0;
    });

    const other = await openPhone(me.label, (await device(owner)).session);
    expect(await other.inbox.control()).toEqual({ type: "limit", which: "phones" });
    expect(phone.inbox.closed).toBeNull();
  });
});
