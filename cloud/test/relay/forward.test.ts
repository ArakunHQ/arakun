import { describe, expect, it } from "vitest";
import { DATA, HELLO, pcControl, pcFrame, phoneControl, phoneFrame, utf8 } from "../../src/relay/frames";
import { connectedComputer, issueTickets, openPhone, type PcSocket } from "./helpers";

/// A computer with two phones admitted, all `open` messages already consumed.
async function room() {
  const { me, pc } = await connectedComputer();
  await issueTickets(pc, me.label, "ticket-one", "ticket-two");
  const a = await openPhone(me.label, "d=dev-a&t=ticket-one");
  const b = await openPhone(me.label, "d=dev-b&t=ticket-two");
  expect(await a.inbox.control()).toEqual({ type: "open", channel: 1 });
  expect(await b.inbox.control()).toEqual({ type: "open", channel: 2 });
  expect(await pc.inbox.control()).toEqual({ type: "open", channel: 1, device: "dev-a" });
  expect(await pc.inbox.control()).toEqual({ type: "open", channel: 2, device: "dev-b" });
  return { me, pc, a, b };
}

async function frame(inbox: PcSocket["inbox"]) {
  const got = (await inbox.next()) as { type: number; channel?: number; payload: Uint8Array };
  return { ...got, payload: Array.from(got.payload) };
}

describe("frames from a phone", () => {
  it("reach the computer on the phone's channel, unread", async () => {
    const { pc, a, b } = await room();
    a.ws.send(phoneFrame(HELLO, utf8('{"v":1}')));
    b.ws.send(phoneFrame(DATA, new Uint8Array([0, 0, 0, 0, 0, 0, 0, 0, 9, 9])));
    expect(await frame(pc.inbox)).toEqual({ type: HELLO, channel: 1, payload: Array.from(utf8('{"v":1}')) });
    expect(await frame(pc.inbox)).toEqual({ type: DATA, channel: 2, payload: [0, 0, 0, 0, 0, 0, 0, 0, 9, 9] });
  });

  it("are dropped while the computer is away", async () => {
    const { pc, a } = await room();
    pc.ws.close(1000, "going away");
    expect((await a.inbox.control())["type"]).toBe("pc_offline");
    a.ws.send(phoneFrame(DATA, new Uint8Array([1])));
    await expect(a.inbox.next(200)).rejects.toThrow();
    expect(a.inbox.closed).toBeNull();
  });

  it("must be binary, hello or data", async () => {
    const { a, b } = await room();
    a.ws.send("text");
    expect(await a.inbox.control()).toEqual({ type: "bye", reason: "binary frames only" });
    expect((await a.inbox.closedWith()).code).toBe(4400);
    b.ws.send(phoneControl({ type: "open", channel: 7 }));
    expect(await b.inbox.control()).toEqual({ type: "bye", reason: "bad frame" });
    expect((await b.inbox.closedWith()).code).toBe(4400);
  });

  it("may fill 256 KB", async () => {
    const { pc, a } = await room();
    a.ws.send(phoneFrame(DATA, new Uint8Array(256 * 1024)));
    const got = await frame(pc.inbox);
    expect(got.channel).toBe(1);
    expect(got.payload.length).toBe(256 * 1024);
  });
});

describe("frames from the computer", () => {
  it("reach the phone the channel names, without the channel", async () => {
    const { pc, a, b } = await room();
    pc.ws.send(pcFrame(DATA, 2, new Uint8Array([2, 2])));
    pc.ws.send(pcFrame(HELLO, 1, utf8("hi")));
    expect(await frame(b.inbox)).toEqual({ type: DATA, payload: [2, 2] });
    expect(await frame(a.inbox)).toEqual({ type: HELLO, payload: Array.from(utf8("hi")) });
  });

  it("to a channel nobody holds are answered with close{gone}", async () => {
    const { pc, a } = await room();
    a.ws.close(1000, "bye");
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "bye" });
    pc.ws.send(pcFrame(DATA, 1, new Uint8Array([1])));
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "gone" });
    pc.ws.send(pcFrame(DATA, 99, new Uint8Array([1])));
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 99, reason: "gone" });
  });

  it("can close a phone's channel with a reason", async () => {
    const { pc, a, b } = await room();
    pc.ws.send(pcFrame(3, 1, utf8(JSON.stringify({ type: "close", reason: "bad_token" }))));
    expect(await a.inbox.control()).toEqual({ type: "bye", reason: "bad_token" });
    expect((await a.inbox.closedWith()).code).toBe(4410);
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "bad_token" });
    expect(b.inbox.closed).toBeNull();
    pc.ws.send(pcFrame(DATA, 2, new Uint8Array([5])));
    expect(await frame(b.inbox)).toEqual({ type: DATA, payload: [5] });
  });

  it("may not carry hello or data on channel 0, nor other control to a phone", async () => {
    const { pc } = await room();
    pc.ws.send(pcFrame(DATA, 0, new Uint8Array([1])));
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "bad frame" });
    expect((await pc.inbox.closedWith()).code).toBe(4400);

    const { pc: pc2 } = await room();
    pc2.ws.send(pcFrame(3, 1, utf8(JSON.stringify({ type: "tickets", hashes: [] }))));
    expect(await pc2.inbox.control()).toEqual({ type: "bye", reason: "bad frame" });
  });

  it("may fill 256 KB", async () => {
    const { pc, a } = await room();
    pc.ws.send(pcFrame(DATA, 1, new Uint8Array(256 * 1024)));
    expect((await frame(a.inbox)).payload.length).toBe(256 * 1024);
  });

  it("still speak control on channel 0 as before", async () => {
    const { pc, a } = await room();
    pc.ws.send(pcControl({ type: "tickets", hashes: [] }));
    expect(await a.inbox.control()).toEqual({ type: "bye", reason: "revoked" });
  });
});

describe("a close the other side starts", () => {
  it("is answered with the same code, so the phone sees a clean close", async () => {
    const { a } = await room();
    a.ws.close(4000, "leaving");
    expect(await a.inbox.closedWith()).toEqual({ code: 4000, reason: "leaving" });
  });

  it("is answered on the computer's socket too", async () => {
    const { pc } = await room();
    pc.ws.close(1000, "going away");
    expect((await pc.inbox.closedWith()).code).toBe(1000);
  });
});
