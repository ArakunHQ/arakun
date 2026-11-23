import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { b64url, pcControl, pcFrame, parsePcFrame, DATA } from "../../src/relay/frames";
import { hex, sha256 } from "../../src/util";
import type { SignedIn } from "../helpers";
import { Inbox, account, claim, computer, connectedComputer, openPc, sign, type PcSocket } from "./helpers";

async function hashOf(token: string): Promise<string> {
  return hex(await sha256(token));
}

/// A computer socket opened on the object directly, with the headers the
/// Worker would set for `githubId`: the object's own checks, behind the Worker's.
async function pcOnObject(label: string, who: SignedIn, plan = "free"): Promise<PcSocket> {
  const res = await env.RELAY.get(env.RELAY.idFromName(label)).fetch("https://relay/pc", {
    headers: {
      Upgrade: "websocket",
      "x-arakun-label": label,
      "x-arakun-account": String(who.github_id),
      "x-arakun-plan": plan,
      "x-arakun-session": await hashOf(who.session),
      "x-arakun-machine-name": "Desk",
      "x-arakun-machine-kind": "app",
    },
  });
  const ws = res.webSocket!;
  const inbox = new Inbox(ws, (b) => parsePcFrame(b) ?? { bad: true });
  ws.accept();
  ws.binaryType = "arraybuffer";
  return { ws, inbox };
}

async function machineRow(label: string) {
  return env.DB.prepare("SELECT github_id, name, kind FROM machines WHERE label = ?").bind(label).first();
}

describe("the computer's claim", () => {
  it("is accepted when the signature holds and the key derives the label", async () => {
    const { pc } = await connectedComputer();
    expect(pc.inbox.closed).toBeNull();
  });

  it("is refused when the signature is wrong", async () => {
    const me = await computer();
    const pc = await openPc(me.label, (await account()).session);
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
    const pc = await openPc(other.label, (await account()).session);
    const answer = await claim(pc, me, other.label);
    expect(answer).toEqual({ type: "bye", reason: "wrong_label" });
    expect((await pc.inbox.closedWith()).code).toBe(4403);
  });

  it("wants the claim before anything else", async () => {
    const me = await computer();
    const pc = await openPc(me.label, (await account()).session);
    await pc.inbox.control();
    pc.ws.send(pcFrame(DATA, 7, new Uint8Array([1, 2, 3])));
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "claim first" });
    expect((await pc.inbox.closedWith()).code).toBe(4401);
  });

  it("speaks binary only", async () => {
    const me = await computer();
    const pc = await openPc(me.label, (await account()).session);
    await pc.inbox.control();
    pc.ws.send("hello?");
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "binary frames only" });
    expect((await pc.inbox.closedWith()).code).toBe(4400);
  });

  it("lets a second connection of the same computer replace the first", async () => {
    const { me, pc: first, owner } = await connectedComputer();
    const second = await openPc(me.label, owner.session);
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
    const { me, owner } = await connectedComputer();
    const { runInDurableObject } = await import("cloudflare:test");
    const id = env.RELAY.idFromName(me.label);
    const stub = env.RELAY.get(id);
    await runInDurableObject(stub, async (_instance, state) => {
      expect(await state.storage.get("pub")).toBe(me.pubB64);
      expect(await state.storage.get("label")).toBe(me.label);
      expect(typeof (await state.storage.get("claimed_at"))).toBe("number");
      await state.storage.put("pub", "someone-else");
    });
    const again = await openPc(me.label, owner.session);
    expect(await claim(again, me)).toEqual({ type: "bye", reason: "key_mismatch" });
    expect((await again.inbox.closedWith()).code).toBe(4403);
  });

  it("registers the machine in D1 under the account, with its name and kind", async () => {
    const { me, owner } = await connectedComputer();
    expect(await machineRow(me.label)).toEqual({ github_id: owner.github_id, name: "Desk", kind: "app" });
  });

  it("is checked against D1 by the object too: another account's claim is label_taken", async () => {
    const { me, owner } = await connectedComputer();
    const intruder = await account();
    const pc = await pcOnObject(me.label, intruder);
    expect(await claim(pc, me)).toEqual({ type: "bye", reason: "label_taken" });
    expect((await pc.inbox.closedWith()).code).toBe(4403);
    expect(await machineRow(me.label)).toEqual({ github_id: owner.github_id, name: "Desk", kind: "app" });
  });

  it("is checked against the plan's machines by the object too", async () => {
    await env.CONFIG.put("plans", JSON.stringify({ free: { machines: 1 } }));
    const { owner } = await connectedComputer();
    const second = await computer();
    const pc = await pcOnObject(second.label, owner);
    expect(await claim(pc, second)).toEqual({ type: "bye", reason: "machines_full" });
    expect((await pc.inbox.closedWith()).code).toBe(4429);
    expect(await machineRow(second.label)).toBeNull();
  });

  it("is refused signed_out when the session ended between the socket and the claim", async () => {
    const owner = await account();
    const me = await computer();
    const pc = await openPc(me.label, owner.session);
    await env.DB.prepare("DELETE FROM sessions WHERE id = ?").bind(owner.session_id).run();
    expect(await claim(pc, me)).toEqual({ type: "bye", reason: "signed_out" });
    expect((await pc.inbox.closedWith()).code).toBe(4401);
    expect(await machineRow(me.label)).toBeNull();
  });

  it("is refused blocked when the account was blocked between the socket and the claim", async () => {
    const owner = await account();
    const me = await computer();
    const pc = await openPc(me.label, owner.session);
    await env.DB.prepare("UPDATE users SET blocked_at = 1 WHERE github_id = ?").bind(owner.github_id).run();
    expect(await claim(pc, me)).toEqual({ type: "bye", reason: "blocked" });
    expect((await pc.inbox.closedWith()).code).toBe(4403);
    expect(await machineRow(me.label)).toBeNull();
  });
});
