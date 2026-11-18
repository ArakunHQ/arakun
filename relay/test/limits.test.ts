import { SELF, env, runDurableObjectAlarm, runInDurableObject } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";
import { DATA, pcFrame, phoneFrame } from "../src/frames";
import { FREE, disabledReason, isBlocked, planFor } from "../src/limits";
import { claim, computer, connectedComputer, issueTickets, openPc, openPhone, stored } from "./helpers";

const LABEL = "k7f3q2m4xwab4c5d";

// KV outlives a test: every test starts from an empty one.
beforeEach(async () => {
  for (const key of ["plans", "disabled", "blocked"]) await env.CONFIG.delete(key);
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

  it("reads the switch and the block list", async () => {
    expect(await disabledReason(env.CONFIG)).toBeNull();
    await env.CONFIG.put("disabled", "true");
    expect(await disabledReason(env.CONFIG)).toBe("relay disabled");
    await env.CONFIG.put("disabled", "back at 05:00 UTC");
    expect(await disabledReason(env.CONFIG)).toBe("back at 05:00 UTC");
    await env.CONFIG.put("disabled", "false");
    expect(await disabledReason(env.CONFIG)).toBeNull();

    expect(await isBlocked(env.CONFIG, LABEL)).toBe(false);
    await env.CONFIG.put("blocked", JSON.stringify([LABEL]));
    expect(await isBlocked(env.CONFIG, LABEL)).toBe(true);
    expect(await isBlocked(env.CONFIG, "aaaaaaaaaaaaaaaa")).toBe(false);
  });
});

describe("the switch and the block list", () => {
  it("close the sockets and say so at /_relay while the page still loads", async () => {
    await env.CONFIG.put("disabled", "back soon");
    const status = await SELF.fetch(`https://${LABEL}.arakun.dev/_relay`);
    expect(status.status).toBe(503);
    expect(await status.json()).toEqual({ label: LABEL, disabled: "back soon" });
    const ws = await SELF.fetch(`https://${LABEL}.arakun.dev/ws?p=x`, { headers: { Upgrade: "websocket" } });
    expect(ws.status).toBe(503);
    expect(await ws.text()).toBe("back soon");
    expect((await SELF.fetch(`https://${LABEL}.arakun.dev/pc`, { headers: { Upgrade: "websocket" } })).status).toBe(503);
    expect((await SELF.fetch(`https://${LABEL}.arakun.dev/`)).status).toBe(200);
  });

  it("shut one label out and leave the others alone", async () => {
    await env.CONFIG.put("blocked", JSON.stringify([LABEL]));
    const status = await SELF.fetch(`https://${LABEL}.arakun.dev/_relay`);
    expect(status.status).toBe(403);
    expect(await status.json()).toEqual({ label: LABEL, blocked: true });
    expect((await SELF.fetch(`https://${LABEL}.arakun.dev/pc`, { headers: { Upgrade: "websocket" } })).status).toBe(403);
    const { pc } = await connectedComputer();
    expect(pc.inbox.closed).toBeNull();
  });
});

describe("the plan's limits", () => {
  it("let three phones in and tell the fourth", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t");
    const phones = [];
    for (let i = 1; i <= 3; i++) {
      const p = await openPhone(me.label, `d=dev-${i}&t=t`);
      expect(await p.inbox.control()).toEqual({ type: "open", channel: i });
      await pc.inbox.control();
      phones.push(p);
    }
    const fourth = await openPhone(me.label, "d=dev-4&t=t");
    expect(await fourth.inbox.control()).toEqual({ type: "limit", which: "phones" });
    expect((await fourth.inbox.closedWith()).code).toBe(4429);
    await expect(pc.inbox.next(200)).rejects.toThrow();

    phones[0]!.ws.close(1000, "leaving");
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "leaving" });
    const fifth = await openPhone(me.label, "d=dev-5&t=t");
    expect(await fifth.inbox.control()).toEqual({ type: "open", channel: 4 });
  });

  it("come from KV: a plan with one phone", async () => {
    await plans({ phones: 1 });
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t");
    const one = await openPhone(me.label, "d=dev-1&t=t");
    expect(await one.inbox.control()).toEqual({ type: "open", channel: 1 });
    const two = await openPhone(me.label, "d=dev-2&t=t");
    expect(await two.inbox.control()).toEqual({ type: "limit", which: "phones" });
  });

  it("cut a burst: the frame over the per-second count closes the channel", async () => {
    await plans({ burst: 3 });
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t");
    const phone = await openPhone(me.label, "d=dev-1&t=t");
    await phone.inbox.control();
    await pc.inbox.control();
    for (let i = 0; i < 4; i++) phone.ws.send(phoneFrame(DATA, new Uint8Array([i])));
    for (let i = 0; i < 3; i++) expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(i);
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "limit:burst" });
    expect(await phone.inbox.control()).toEqual({ type: "limit", which: "burst" });
    expect((await phone.inbox.closedWith()).code).toBe(4429);
  });

  it("stop the day at its count, for frames and for new phones", async () => {
    await plans({ daily: 2 });
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t");
    const phone = await openPhone(me.label, "d=dev-1&t=t");
    await phone.inbox.control();
    await pc.inbox.control();
    phone.ws.send(phoneFrame(DATA, new Uint8Array([1])));
    pc.ws.send(pcFrame(DATA, 1, new Uint8Array([2])));
    expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(1);
    expect(((await phone.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(2);
    phone.ws.send(phoneFrame(DATA, new Uint8Array([3])));
    expect(await phone.inbox.control()).toEqual({ type: "limit", which: "daily" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "limit:daily" });
    const late = await openPhone(me.label, "d=dev-2&t=t");
    expect(await late.inbox.control()).toEqual({ type: "limit", which: "daily" });
  });

  it("keep the day's count in storage every fifty frames and start over at midnight", async () => {
    await plans({ burst: 1000 });
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t");
    const phone = await openPhone(me.label, "d=dev-1&t=t");
    await phone.inbox.control();
    await pc.inbox.control();
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

  it("bound the frame by the plan, either way", async () => {
    await plans({ frame: 10 });
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t", "u");
    const a = await openPhone(me.label, "d=dev-1&t=t");
    const b = await openPhone(me.label, "d=dev-2&t=u");
    await a.inbox.control();
    await b.inbox.control();
    await pc.inbox.control();
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
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t");
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    expect(await runInDurableObject(stub, (_i, state) => state.storage.getAlarm())).toBeNull();

    pc.ws.close(1000, "shutting down");
    await new Promise((r) => setTimeout(r, 50));
    const at = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect(at).not.toBeNull();
    expect(at! - Date.now()).toBeGreaterThan(89 * 24 * 3_600_000);

    const phone = await openPhone(me.label, "d=dev-1&t=t");
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
    expect(await stored(me.label, "tickets")).toBeUndefined();

    const back = await openPc(me.label);
    expect(await claim(back, me)).toEqual({ type: "claimed" });
    expect(await stored(me.label, "pub")).toBe(me.pubB64);
    expect(await runInDurableObject(stub, (_i, state) => state.storage.getAlarm())).toBeNull();
  });

  it("is not purged while another socket of it is connected", async () => {
    const { me, pc } = await connectedComputer();
    const second = await openPc(me.label);
    expect(await claim(second, me)).toEqual({ type: "claimed" });
    await pc.inbox.closedWith();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    expect(await runInDurableObject(stub, (_i, state) => state.storage.getAlarm())).toBeNull();
    await runInDurableObject(stub, (_i, state) => state.storage.setAlarm(Date.now() + 1000));
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await stored(me.label, "pub")).toBe(me.pubB64);
  });
});

describe("a socket at /pc that never claims", () => {
  it("stores nothing, and is closed by the alarm when its time is up", async () => {
    const me = await computer();
    const pc = await openPc(me.label);
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
    const { me, pc } = await connectedComputer();
    pc.ws.close(1000, "shutting down");
    await new Promise((r) => setTimeout(r, 50));
    const stranger = await openPc(me.label);
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
    const pc = await openPc(me.label);
    expect(await runInDurableObject(stub, (_i, state) => state.storage.getAlarm())).not.toBeNull();
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await stored(me.label, "label")).toBeUndefined();
    // The socket still has its time to claim.
    expect(pc.inbox.closed).toBeNull();
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
    expect(await stored(me.label, "label")).toBe(me.label);
  });
});

describe("a phone that switched networks", () => {
  it("replaces its own old socket instead of hitting the phone limit", async () => {
    await plans({ phones: 2 });
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t", "u");
    const old = await openPhone(me.label, "d=dev-1&t=t&s=aaaaaaaa");
    expect(await old.inbox.control()).toEqual({ type: "open", channel: 1 });
    await pc.inbox.control();
    const other = await openPhone(me.label, "d=dev-2&t=u&s=bbbbbbbb");
    expect(await other.inbox.control()).toEqual({ type: "open", channel: 2 });
    await pc.inbox.control();

    const again = await openPhone(me.label, "d=dev-1&t=t&s=aaaaaaaa");
    expect(await old.inbox.control()).toEqual({ type: "bye", reason: "replaced" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "replaced" });
    expect(await again.inbox.control()).toEqual({ type: "open", channel: 3 });
    expect(other.inbox.closed).toBeNull();

    // Another device still finds the limit.
    const third = await openPhone(me.label, "d=dev-3&t=u&s=cccccccc");
    expect(await third.inbox.control()).toEqual({ type: "limit", which: "phones" });
  });

  it("never replaces another live tab of the same phone, or a page that sends no name", async () => {
    await plans({ phones: 1 });
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t");
    const tab = await openPhone(me.label, "d=dev-1&t=t&s=aaaaaaaa");
    expect(await tab.inbox.control()).toEqual({ type: "open", channel: 1 });
    await pc.inbox.control();

    const otherTab = await openPhone(me.label, "d=dev-1&t=t&s=bbbbbbbb");
    expect(await otherTab.inbox.control()).toEqual({ type: "limit", which: "phones" });
    const olderPage = await openPhone(me.label, "d=dev-1&t=t");
    expect(await olderPage.inbox.control()).toEqual({ type: "limit", which: "phones" });
    expect(tab.inbox.closed).toBeNull();
  });
});
