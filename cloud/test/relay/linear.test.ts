import { env, runDurableObjectAlarm, runInDurableObject } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DATA, pcControl, pcFrame, phoneFrame } from "../../src/relay/frames";
import * as expire from "../../src/linear_expire";
import { QueueFull, deliver, dropDelivery, type Delivery } from "../../src/machines";
import { account, away, claim, computer, computerAndPhone, connectedComputer, openPc, stored, type Computer, type PcSocket } from "./helpers";

const DAY_MS = 24 * 3_600_000;

function hexId(): string {
  return crypto.randomUUID().replaceAll("-", "");
}

function delivery(kind: Delivery["kind"] = "created", ago = 0): Delivery {
  return {
    id: hexId(),
    kind,
    agent_session: crypto.randomUUID(),
    issue: crypto.randomUUID(),
    organization: crypto.randomUUID(),
    activity: kind === "created" ? null : crypto.randomUUID(),
    created: Math.floor(Date.now() / 1000) - ago,
  };
}

/// What a frame carries of a delivery: everything but its time.
function frame(d: Delivery): Record<string, unknown> {
  return { id: d.id, kind: d.kind, agent_session: d.agent_session, issue: d.issue, organization: d.organization, activity: d.activity };
}

function stub(label: string) {
  return env.RELAY.get(env.RELAY.idFromName(label));
}

/// The computer connects again; the socket before it is replaced.
async function reconnect(me: Computer, session: string): Promise<PcSocket> {
  const pc = await openPc(me.label, session);
  expect(await claim(pc, me)).toEqual({ type: "claimed" });
  return pc;
}

/// A computer that claimed once and left, so its label is registered.
async function awayComputer() {
  const { me, pc, owner } = await connectedComputer();
  await away(me.label, pc);
  return { me, owner };
}

/// Ages a stored delivery past its week and makes the alarm due, in one step.
/// A delivery stored already expired would set an alarm for now, which the
/// runtime may fire on its own before the test runs it.
async function age(label: string, d: Delivery): Promise<void> {
  await runInDurableObject(stub(label), async (_i, state) => {
    await state.storage.put(`linear:${d.id}`, { ...d, created: d.created - 8 * 86_400 });
    await state.storage.setAlarm(Date.now());
  });
}

async function plans(free: object) {
  await env.CONFIG.put("plans", JSON.stringify({ free }));
}

beforeEach(async () => {
  await env.CONFIG.delete("plans");
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Linear deliveries in the relay", () => {
  it("sends a delivery to a connected computer and keeps it until dropped", async () => {
    const { me, pc } = await connectedComputer();
    const D1 = delivery();
    expect(await deliver(env, me.label, D1, null)).toEqual({ online: true });
    expect(await pc.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
    expect(await stored(me.label, `linear:${D1.id}`)).toEqual(D1);
    await dropDelivery(env, me.label, D1.id);
    expect(await stored(me.label, `linear:${D1.id}`)).toBeUndefined();
  });

  it("keeps a delivery for a computer that is away and sends it after the claim", async () => {
    const { me, owner } = await awayComputer();
    const D1 = delivery();
    expect(await deliver(env, me.label, D1, null)).toEqual({ online: false });
    const pc = await reconnect(me, owner.session);
    expect(await pc.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
  });

  it("keeps a delivery for a label no computer has claimed yet", async () => {
    const owner = await account();
    const me = await computer();
    const D1 = delivery();
    expect(await deliver(env, me.label, D1, null)).toEqual({ online: false });
    // Nothing else is pending, so the alarm waits for its expiry.
    const at = await runInDurableObject(stub(me.label), (_i, state) => state.storage.getAlarm());
    expect(at).toBe(D1.created * 1000 + 7 * DAY_MS);
    // The alarm leaves it: it is not state an older relay left behind.
    await runInDurableObject(stub(me.label), (_i, state) => state.storage.setAlarm(Date.now()));
    expect(await runDurableObjectAlarm(stub(me.label))).toBe(true);
    expect(await stored(me.label, `linear:${D1.id}`)).toEqual(D1);
    const pc = await reconnect(me, owner.session);
    expect(await pc.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
  });

  it("holds one that arrives between the socket and the claim until the claim", async () => {
    const owner = await account();
    const me = await computer();
    const pc = await openPc(me.label, owner.session);
    const D1 = delivery();
    expect(await deliver(env, me.label, D1, null)).toEqual({ online: false });
    // claim() reads the challenge first: anything before it would fail there.
    expect(await claim(pc, me)).toEqual({ type: "claimed" });
    expect(await pc.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
  });

  it("sends only to the claimed socket while a second one has not claimed", async () => {
    const { me, pc: first, owner } = await connectedComputer();
    const second = await openPc(me.label, owner.session);
    const D1 = delivery();
    expect(await deliver(env, me.label, D1, null)).toEqual({ online: true });
    expect(await first.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
    expect(await claim(second, me)).toEqual({ type: "claimed" });
    expect(await second.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
    expect(await first.inbox.control()).toEqual({ type: "bye", reason: "replaced" });
  });

  it("sends it again after every claim until it is dropped, oldest first", async () => {
    const { me, pc, owner } = await connectedComputer();
    const newer = delivery("created", 10);
    const older = delivery("created", 20);
    await deliver(env, me.label, newer, null);
    await deliver(env, me.label, older, null);
    expect((await pc.inbox.control())["id"]).toBe(newer.id);
    expect((await pc.inbox.control())["id"]).toBe(older.id);

    const again = await reconnect(me, owner.session);
    expect(await again.inbox.control()).toEqual({ type: "linear", ...frame(older), body: null });
    expect(await again.inbox.control()).toEqual({ type: "linear", ...frame(newer), body: null });

    await dropDelivery(env, me.label, older.id);
    const third = await reconnect(me, owner.session);
    expect(await third.inbox.control()).toEqual({ type: "linear", ...frame(newer), body: null });
    await expect(third.inbox.next(200)).rejects.toThrow();
  });

  it("sends a prompt's body live and never stores it", async () => {
    const { me, pc, owner } = await connectedComputer();
    const PROMPT = delivery("prompt");
    expect(await deliver(env, me.label, PROMPT, "make it blue")).toEqual({ online: true });
    expect(await pc.inbox.control()).toEqual({ type: "linear", ...frame(PROMPT), body: "make it blue" });
    expect(JSON.stringify(await stored(me.label, `linear:${PROMPT.id}`))).not.toContain("make it blue");
    const again = await reconnect(me, owner.session);
    expect(await again.inbox.control()).toEqual({ type: "linear", ...frame(PROMPT), body: null });
  });

  it("the same id twice is one frame", async () => {
    const { me, pc } = await connectedComputer();
    const D1 = delivery();
    expect(await deliver(env, me.label, D1, null)).toEqual({ online: true });
    expect(await deliver(env, me.label, { ...D1, kind: "stop" }, "again")).toEqual({ online: true });
    expect(await pc.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
    await expect(pc.inbox.next(200)).rejects.toThrow();
    expect(await stored(me.label, `linear:${D1.id}`)).toEqual(D1);
  });

  it("drops a delivery after 7 days and tells linear.ts", async () => {
    const spy = vi.spyOn(expire, "expired");
    const { me } = await awayComputer();
    const old = delivery("created", 6 * 86_400);
    const fresh = delivery();
    await deliver(env, me.label, old, null);
    await deliver(env, me.label, fresh, null);
    // The alarm wakes the object for the first expiry, well before the purge.
    const at = await runInDurableObject(stub(me.label), (_i, state) => state.storage.getAlarm());
    expect(at! - Date.now()).toBeLessThanOrEqual(DAY_MS);
    expect(at! - Date.now()).toBeGreaterThan(DAY_MS - 60_000);

    // An alarm that runs early keeps both.
    expect(await runDurableObjectAlarm(stub(me.label))).toBe(true);
    expect(await stored(me.label, `linear:${old.id}`)).toEqual(old);

    // A day later.
    await runInDurableObject(stub(me.label), async (_i, state) => {
      await state.storage.put(`linear:${old.id}`, { ...old, created: old.created - 86_400 });
      await state.storage.setAlarm(Date.now());
    });
    expect(await runDurableObjectAlarm(stub(me.label))).toBe(true);
    expect(await stored(me.label, `linear:${old.id}`)).toBeUndefined();
    expect(await stored(me.label, `linear:${fresh.id}`)).toEqual(fresh);
    expect(spy.mock.calls.map((c) => c[1])).toEqual([old.id]);
    // The machine is not forgotten for it.
    expect(await stored(me.label, "pub")).toBe(me.pubB64);
  });

  it("goes on with the alarm when telling linear.ts fails", async () => {
    vi.spyOn(expire, "expired").mockRejectedValue(new Error("D1 is down"));
    vi.spyOn(console, "error").mockImplementation(() => {});
    const { me } = await awayComputer();
    const old = delivery();
    await deliver(env, me.label, old, null);
    await age(me.label, old);
    expect(await runDurableObjectAlarm(stub(me.label))).toBe(true);
    expect(await stored(me.label, `linear:${old.id}`)).toBeUndefined();
    // Back on the purge.
    const at = await runInDurableObject(stub(me.label), (_i, state) => state.storage.getAlarm());
    expect(at! - Date.now()).toBeGreaterThan(89 * DAY_MS);
  });

  it("refuses the 51st queued delivery", async () => {
    const { me } = await awayComputer();
    for (let i = 0; i < 50; i++) expect(await deliver(env, me.label, delivery(), null)).toEqual({ online: false });
    const extra = delivery();
    await expect(deliver(env, me.label, extra, null)).rejects.toBeInstanceOf(QueueFull);
    expect(await stored(me.label, `linear:${extra.id}`)).toBeUndefined();
  });

  it("forgets the alarm once the last delivery of a label never claimed expires", async () => {
    const me = await computer();
    const old = delivery();
    await deliver(env, me.label, old, null);
    await age(me.label, old);
    expect(await runDurableObjectAlarm(stub(me.label))).toBe(true);
    expect(await stored(me.label, `linear:${old.id}`)).toBeUndefined();
    expect(await runInDurableObject(stub(me.label), (_i, state) => state.storage.getAlarm())).toBeNull();
  });

  it("refuses a malformed delivery and stores nothing", async () => {
    const { me } = await awayComputer();
    const good = delivery();
    const bad: object[] = [
      { ...good, id: "not-hex" },
      { ...good, kind: "update" },
      { ...good, issue: 7 },
      { ...good, activity: undefined },
      { ...good, created: "now" },
    ];
    for (const d of bad) {
      const res = await stub(me.label).fetch("https://relay/_linear", {
        method: "POST",
        headers: { "x-arakun-label": me.label },
        body: JSON.stringify({ delivery: d, body: null }),
      });
      expect(res.status, JSON.stringify(d)).toBe(400);
      await res.text();
    }
    const keys = await runInDurableObject(stub(me.label), async (_i, state) => [...(await state.storage.list({ prefix: "linear:" })).keys()]);
    expect(keys).toEqual([]);
  });

  it("a linear frame does not count toward the plan's frames", async () => {
    await plans({ daily: 2, burst: 2 });
    const { me, pc, phone } = await computerAndPhone();
    for (let i = 0; i < 3; i++) {
      await deliver(env, me.label, delivery(), null);
      expect((await pc.inbox.control())["type"]).toBe("linear");
    }
    phone.ws.send(phoneFrame(DATA, new Uint8Array([1])));
    pc.ws.send(pcFrame(DATA, 1, new Uint8Array([2])));
    expect(((await pc.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(1);
    expect(((await phone.inbox.next()) as { payload: Uint8Array }).payload[0]).toBe(2);
  });

  it("still closes on a control frame from the computer on channel 0", async () => {
    const { me, pc } = await connectedComputer();
    const D1 = delivery();
    await deliver(env, me.label, D1, null);
    await pc.inbox.control();
    pc.ws.send(pcControl({ type: "linear", id: D1.id }));
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "unknown control" });
    expect((await pc.inbox.closedWith()).code).toBe(4400);
  });
});
