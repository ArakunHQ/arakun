import { SELF, env, runInDurableObject } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";
import type { LabelCard } from "../src/admin";
import { claim, connectedComputer, issueTickets, openPc, openPhone, stored } from "./helpers";

const LABEL = "k7f3q2m4xwab4c5d";
const AUTH = { Authorization: "Bearer test-secret" };

beforeEach(async () => {
  await env.CONFIG.delete("blocked");
  const cards = await env.CONFIG.list({ prefix: "labels/" });
  for (const key of cards.keys) await env.CONFIG.delete(key.name);
});

function get(path: string, headers: Record<string, string> = {}) {
  return SELF.fetch(`https://${LABEL}.arakun.dev${path}`, { headers });
}

function post(path: string, headers: Record<string, string> = {}) {
  return SELF.fetch(`https://${LABEL}.arakun.dev${path}`, { method: "POST", headers });
}

describe("the admin door", () => {
  it("opens only to the exact secret", async () => {
    expect((await get("/_admin/labels")).status).toBe(401);
    expect((await get("/_admin/labels", { Authorization: "Bearer test-secre" })).status).toBe(401);
    expect((await get("/_admin/labels", { Authorization: "Bearer test-secret!" })).status).toBe(401);
    expect((await get("/_admin/labels", { Authorization: "test-secret" })).status).toBe(401);
    expect((await get("/_admin/labels", AUTH)).status).toBe(200);
  });

  it("is reached under any host, even one without a label", async () => {
    const res = await SELF.fetch("https://arakun.dev/_admin/labels", { headers: AUTH });
    expect(res.status).toBe(200);
    expect((await SELF.fetch("https://arakun.dev/_admin/labels")).status).toBe(401);
  });

  it("knows only its own paths", async () => {
    expect((await get("/_admin/nothing", AUTH)).status).toBe(404);
    expect((await post("/_admin/labels", AUTH)).status).toBe(404);
    expect((await post("/_admin/block/not-a-label", AUTH)).status).toBe(400);
  });
});

describe("the list of labels", () => {
  it("shows every computer that claimed, newest first, from the cards they left", async () => {
    const first = await connectedComputer();
    await issueTickets(first.pc, first.me.label, "t");
    const phone = await openPhone(first.me.label, "d=dev-1&t=t");
    await phone.inbox.control();
    await first.pc.inbox.control();
    await new Promise((r) => setTimeout(r, 20));
    const second = await connectedComputer();

    // Cards are written after `claimed` is answered: wait for both.
    let labels: (LabelCard & { label: string; blocked: boolean })[] = [];
    for (let i = 0; i < 50 && labels.length < 2; i++) {
      await new Promise((r) => setTimeout(r, 20));
      labels = ((await (await get("/_admin/labels", AUTH)).json()) as { labels: typeof labels }).labels;
    }
    expect(labels.map((l) => l.label)).toEqual([second.me.label, first.me.label]);
    const card = labels[1]!;
    expect(card.phones).toBe(0); // the card was left at the claim, before the phone came
    expect(card.today).toBe(0);
    expect(card.blocked).toBe(false);
    expect(typeof card.claimed_at).toBe("number");
    expect(card.last_seen).toBeGreaterThanOrEqual(card.claimed_at);
  });

  it("gets one card per computer per day, not one per connection", async () => {
    const { me, pc } = await connectedComputer();
    // The card is written after `claimed` is answered: give it a moment.
    const card = async (ok: (c: LabelCard | null) => boolean) => {
      let got: LabelCard | null = null;
      for (let i = 0; i < 50 && !ok((got = (await env.CONFIG.getWithMetadata<LabelCard>(`labels/${me.label}`)).metadata)); i++) {
        await new Promise((r) => setTimeout(r, 20));
      }
      return got;
    };
    const first = await card((c) => c !== null);
    const carded = await stored<number>(me.label, "carded_at");
    expect(first?.last_seen).toBe(carded);

    pc.ws.close(1000, "bye");
    await new Promise((r) => setTimeout(r, 20));
    const back = await openPc(me.label);
    expect(await claim(back, me)).toEqual({ type: "claimed" });
    await new Promise((r) => setTimeout(r, 50));
    expect(await stored<number>(me.label, "carded_at")).toBe(carded);
    expect(await card(() => true)).toEqual(first);

    // A day later the card is written again.
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, (_i, state) => state.storage.put("carded_at", carded! - 2 * 86_400_000));
    const later = await openPc(me.label);
    expect(await claim(later, me)).toEqual({ type: "claimed" });
    const renewed = await card((c) => c !== null && c.last_seen > first!.last_seen);
    expect(renewed?.last_seen).toBeGreaterThan(first!.last_seen);
  });
});

describe("blocking a label", () => {
  it("writes it to KV, drops whoever is connected, and keeps new ones out", async () => {
    const { me, pc } = await connectedComputer();
    await issueTickets(pc, me.label, "t");
    const phone = await openPhone(me.label, "d=dev-1&t=t");
    await phone.inbox.control();
    await pc.inbox.control();

    const res = await post(`/_admin/block/${me.label}`, AUTH);
    expect(await res.json()).toEqual({ label: me.label, blocked: true });
    expect(await env.CONFIG.get("blocked", "json")).toEqual([me.label]);
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
    expect((await pc.inbox.closedWith()).code).toBe(4403);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
    expect((await phone.inbox.closedWith()).code).toBe(4403);
    expect((await SELF.fetch(`https://${me.label}.arakun.dev/_relay`)).status).toBe(403);

    const list = (await (await get("/_admin/labels", AUTH)).json()) as { labels: { label: string; blocked: boolean }[] };
    expect(list.labels.find((l) => l.label === me.label)?.blocked).toBe(true);
  });

  it("is undone by unblock, without touching other blocked labels", async () => {
    await env.CONFIG.put("blocked", JSON.stringify(["aaaaaaaaaaaaaaaa", LABEL]));
    const res = await post(`/_admin/unblock/${LABEL}`, AUTH);
    expect(await res.json()).toEqual({ label: LABEL, blocked: false });
    expect(await env.CONFIG.get("blocked", "json")).toEqual(["aaaaaaaaaaaaaaaa"]);
    expect((await SELF.fetch(`https://${LABEL}.arakun.dev/_relay`)).status).toBe(200);
  });
});
