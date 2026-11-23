import { describe, expect, it } from "vitest";
import { b64url, pcControl, pcFrame, DATA } from "../../src/relay/frames";
import { claim, computer, connectedComputer, openPc, sign } from "./helpers";

describe("the computer's claim", () => {
  it("is accepted when the signature holds and the key derives the label", async () => {
    const { pc } = await connectedComputer();
    expect(pc.inbox.closed).toBeNull();
  });

  it("is refused when the signature is wrong", async () => {
    const me = await computer();
    const pc = await openPc(me.label);
    const challenge = await pc.inbox.control();
    expect(challenge["type"]).toBe("challenge");
    const sig = await sign(me.key, new TextEncoder().encode("something else"));
    pc.ws.send(pcControl({ type: "claim", pub: me.pubB64, sig: b64url(sig) }));
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "bad_signature" });
    expect((await pc.inbox.closedWith()).code).toBe(4401);
  });

  it("is refused when the key does not derive the label it is sent to", async () => {
    const me = await computer();
    const other = await computer();
    const pc = await openPc(other.label);
    const answer = await claim(pc, me, other.label);
    expect(answer).toEqual({ type: "bye", reason: "wrong_label" });
    expect((await pc.inbox.closedWith()).code).toBe(4403);
  });

  it("wants the claim before anything else", async () => {
    const me = await computer();
    const pc = await openPc(me.label);
    await pc.inbox.control();
    pc.ws.send(pcFrame(DATA, 7, new Uint8Array([1, 2, 3])));
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "claim first" });
    expect((await pc.inbox.closedWith()).code).toBe(4401);
  });

  it("speaks binary only", async () => {
    const me = await computer();
    const pc = await openPc(me.label);
    await pc.inbox.control();
    pc.ws.send("hello?");
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "binary frames only" });
    expect((await pc.inbox.closedWith()).code).toBe(4400);
  });

  it("lets a second connection of the same computer replace the first", async () => {
    const { me, pc: first } = await connectedComputer();
    const second = await openPc(me.label);
    expect(await claim(second, me)).toEqual({ type: "claimed" });
    expect(await first.inbox.control()).toEqual({ type: "bye", reason: "replaced" });
    expect((await first.inbox.closedWith()).code).toBe(4409);
    expect(second.inbox.closed).toBeNull();
  });

  it("keeps the first key it saw for a label", async () => {
    // Two keys cannot derive the same label, so a mismatch is forced here by
    // pretending: the second claimant signs well for the label but is another key.
    // That is caught as wrong_label first; key_mismatch needs the stored key to
    // differ, which labelOf makes impossible to reach honestly. Check the storage
    // path through the object directly.
    const { me } = await connectedComputer();
    const { env, runInDurableObject } = await import("cloudflare:test");
    const id = env.RELAY.idFromName(me.label);
    const stub = env.RELAY.get(id);
    await runInDurableObject(stub, async (_instance, state) => {
      expect(await state.storage.get("pub")).toBe(me.pubB64);
      expect(await state.storage.get("label")).toBe(me.label);
      expect(typeof (await state.storage.get("claimed_at"))).toBe("number");
      await state.storage.put("pub", "someone-else");
    });
    const again = await openPc(me.label);
    expect(await claim(again, me)).toEqual({ type: "bye", reason: "key_mismatch" });
    expect((await again.inbox.closedWith()).code).toBe(4403);
  });
});
