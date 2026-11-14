import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { pcControl } from "../src/frames";
import { claim, connectedComputer, issueTickets, openPairing, openPc, openPhone, stored } from "./helpers";

describe("a phone with a ticket", () => {
  it("comes in when the computer listed its hash, and both hear the channel open", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "ticket-one", "ticket-two");
    const phone = await openPhone(me.label, "d=dev-1&t=ticket-two");
    expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await pc.inbox.control()).toEqual({ type: "open", channel: 1, device: "dev-1" });
    expect(phone.inbox.closed).toBeNull();
  });

  it("is told and closed when the ticket is not on the list", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "ticket-one");
    const phone = await openPhone(me.label, "d=dev-1&t=ticket-zero");
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "bad_ticket" });
    expect((await phone.inbox.closedWith()).code).toBe(4401);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });

  it("is told and closed when there are no credentials at all", async () => {
    const { me } = await connectedComputer();
    const phone = await openPhone(me.label, "d=dev-1");
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "no_credentials" });
    expect((await phone.inbox.closedWith()).code).toBe(4401);
  });

  it("is thrown out when the computer sends a list without its hash", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "ticket-one", "ticket-two");
    const kept = await openPhone(me.label, "d=dev-1&t=ticket-one");
    const dropped = await openPhone(me.label, "d=dev-2&t=ticket-two");
    await kept.inbox.control();
    await dropped.inbox.control();
    await pc.inbox.control();
    await pc.inbox.control();
    await issueTickets(pc, me.label, "ticket-one");
    expect(await dropped.inbox.control()).toEqual({ type: "bye", reason: "revoked" });
    expect((await dropped.inbox.closedWith()).code).toBe(4401);
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 2, reason: "revoked" });
    expect(kept.inbox.closed).toBeNull();
    await expect(kept.inbox.next(200)).rejects.toThrow();
  });

  it("gets its own channel number, never reused", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "ticket-one");
    const first = await openPhone(me.label, "d=dev-1&t=ticket-one");
    expect(await first.inbox.control()).toEqual({ type: "open", channel: 1 });
    first.ws.close(1000, "done");
    expect(await pc.inbox.control()).toEqual({ type: "open", channel: 1, device: "dev-1" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "done" });
    const second = await openPhone(me.label, "d=dev-1&t=ticket-one");
    expect(await second.inbox.control()).toEqual({ type: "open", channel: 2 });
  });
});

describe("a phone with a pairing code", () => {
  it("comes in while the computer keeps the code open, and the code is spent", async () => {
    const { me, pc } = await connectedComputer();
    await openPairing(pc, me.label, "code-123");
    const phone = await openPhone(me.label, "p=code-123");
    expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await pc.inbox.control()).toEqual({ type: "open", channel: 1, device: null });
    expect(await stored(me.label, "pair")).toBeUndefined();
    const again = await openPhone(me.label, "p=code-123");
    expect(await again.inbox.control()).toEqual({ type: "bye", reason: "no_pairing" });
    expect((await again.inbox.closedWith()).code).toBe(4401);
  });

  it("is refused when the code is not the one open", async () => {
    const { me, pc } = await connectedComputer();
    await openPairing(pc, me.label, "code-123");
    const phone = await openPhone(me.label, "p=code-124");
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "bad_code" });
    expect((await phone.inbox.closedWith()).code).toBe(4401);
    // A wrong guess does not spend the code.
    expect(await stored(me.label, "pair")).toBeDefined();
  });

  it("is refused once the code has expired", async () => {
    const { me, pc } = await connectedComputer();
    await openPairing(pc, me.label, "code-123");
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, async (_instance, state) => {
      const open = (await state.storage.get<{ hash: string; until: number }>("pair"))!;
      await state.storage.put("pair", { ...open, until: Date.now() - 1 });
    });
    const phone = await openPhone(me.label, "p=code-123");
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "no_pairing" });
  });

  it("is refused after the computer closes pairing", async () => {
    const { me, pc } = await connectedComputer();
    await openPairing(pc, me.label, "code-123");
    pc.ws.send(pcControl({ type: "pair_close" }));
    for (let i = 0; i < 50 && (await stored(me.label, "pair")) !== undefined; i++) await new Promise((r) => setTimeout(r, 20));
    const phone = await openPhone(me.label, "p=code-123");
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "no_pairing" });
  });

  it("cannot be opened for longer than five minutes or with a bad hash", async () => {
    const { me, pc } = await connectedComputer();
    pc.ws.send(pcControl({ type: "pair_open", hash: "a".repeat(64), ttl: 3600 }));
    await new Promise((r) => setTimeout(r, 50));
    const open = (await stored<{ until: number }>(me.label, "pair"))!;
    expect(open.until - Date.now()).toBeLessThanOrEqual(300_000);
    expect(open.until - Date.now()).toBeGreaterThan(290_000);

    pc.ws.send(pcControl({ type: "pair_open", hash: "not-a-hash", ttl: 60 }));
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "bad pair_open" });
    expect((await pc.inbox.closedWith()).code).toBe(4400);
  });
});

describe("the computer's presence", () => {
  it("is announced to phones that arrive while it is away, and again when it returns", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "ticket-one");
    pc.ws.close(1000, "going away");
    await new Promise((r) => setTimeout(r, 50));
    const since = await stored<number>(me.label, "last_seen");
    expect(typeof since).toBe("number");

    const phone = await openPhone(me.label, "d=dev-1&t=ticket-one");
    expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await phone.inbox.control()).toEqual({ type: "pc_offline", since });

    const back = await openPc(me.label);
    expect(await claim(back, me)).toEqual({ type: "claimed" });
    expect(await back.inbox.control()).toEqual({ type: "open", channel: 1, device: "dev-1" });
    expect(await phone.inbox.control()).toEqual({ type: "pc_online" });
  });

  it("is lost to connected phones when it hangs up", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "ticket-one");
    const phone = await openPhone(me.label, "d=dev-1&t=ticket-one");
    await phone.inbox.control();
    await pc.inbox.control();
    pc.ws.close(1000, "going away");
    const told = await phone.inbox.control();
    expect(told["type"]).toBe("pc_offline");
    expect(typeof told["since"]).toBe("number");
    expect(phone.inbox.closed).toBeNull();
  });

  it("is kept for phones when one socket of the computer replaces another", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "ticket-one");
    const phone = await openPhone(me.label, "d=dev-1&t=ticket-one");
    await phone.inbox.control();
    await pc.inbox.control();

    const second = await openPc(me.label);
    expect(await claim(second, me)).toEqual({ type: "claimed" });
    expect(await second.inbox.control()).toEqual({ type: "open", channel: 1, device: "dev-1" });
    expect(await phone.inbox.control()).toEqual({ type: "pc_online" });
    expect(await pc.inbox.closedWith()).toMatchObject({ code: 4409 });
    // No pc_offline follows the replaced socket's closing.
    await expect(phone.inbox.next(200)).rejects.toThrow();
  });

  it("introduces a phone arriving right after a replacement to the new socket", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "ticket-one");
    const second = await openPc(me.label);
    expect(await claim(second, me)).toEqual({ type: "claimed" });
    // The replaced socket is still closing when the phone comes in.
    const phone = await openPhone(me.label, "d=dev-1&t=ticket-one");
    expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
    expect(await second.inbox.control()).toEqual({ type: "open", channel: 1, device: "dev-1" });
    expect(phone.inbox.closed).toBeNull();
    await expect(phone.inbox.next(200)).rejects.toThrow();
  });
});
