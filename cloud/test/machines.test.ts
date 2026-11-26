// The account's machines: listing and forgetting them, and how a signed-out
// session, a forgotten machine or a foreign page ends or stops a socket.
import { SELF, env, runDurableObjectAlarm, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { refusal, type Tag } from "../src/relay/relay";
import type { Env } from "../src/util";
import { as, call } from "./helpers";
import { HOST, account, claim, computer, computerAndPhone, connectedComputer, device, openPc, openPhone, opened } from "./relay/helpers";

/// Makes every socket of the label's object due for its recheck, and runs the alarm.
async function recheckDue(label: string): Promise<void> {
  const stub = env.RELAY.get(env.RELAY.idFromName(label));
  await runInDurableObject(stub, async (_i, state) => {
    for (const ws of state.getWebSockets()) ws.serializeAttachment({ ...ws.deserializeAttachment(), checked: 0 });
    await state.storage.setAlarm(Date.now());
  });
  expect(await runDurableObjectAlarm(stub)).toBe(true);
}

describe("signing a session out", () => {
  it("closes that device's socket at once, and leaves the others", async () => {
    const { me, pc, owner, dev, phone } = await computerAndPhone();
    const other = await device(owner);
    const stays = await openPhone(me.label, other.session);
    await stays.inbox.control();
    expect(await pc.inbox.control()).toEqual(opened(2, other));

    expect((await call("DELETE", `/sessions/${dev.session_id}`, as(owner.session))).status).toBe(204);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    expect((await phone.inbox.closedWith()).code).toBe(4401);
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "signed_out" });
    expect(stays.inbox.closed).toBeNull();
    expect(pc.inbox.closed).toBeNull();
  });

  it("closes the computer when it is the computer's session, by logout too", async () => {
    const { pc, owner, phone } = await computerAndPhone();
    expect((await call("POST", "/auth/logout", as(owner.session))).status).toBe(204);
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    expect((await pc.inbox.closedWith()).code).toBe(4401);
    // The device's own session still stands.
    expect((await phone.inbox.control())["type"]).toBe("pc_offline");
    expect(phone.inbox.closed).toBeNull();
  });
});

describe("the recheck on the alarm", () => {
  it("closes a socket whose session row is gone from D1", async () => {
    const { me, pc, dev, phone } = await computerAndPhone();
    await env.DB.prepare("DELETE FROM sessions WHERE id = ?").bind(dev.session_id).run();
    await recheckDue(me.label);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "signed_out" });
    expect(pc.inbox.closed).toBeNull();
  });

  it("closes a socket whose session expired, and a blocked account's", async () => {
    const { me, pc, owner, dev, phone } = await computerAndPhone();
    await env.DB.prepare("UPDATE sessions SET expires_at = 1 WHERE id = ?").bind(dev.session_id).run();
    await recheckDue(me.label);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "signed_out" });

    await env.DB.prepare("UPDATE accounts SET blocked_at = 1 WHERE id = ?").bind(owner.account_id).run();
    await recheckDue(me.label);
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "blocked" });
  });

  it("leaves sockets alone while everything holds, and comes back within fifteen minutes", async () => {
    const { me, pc, phone } = await computerAndPhone();
    await recheckDue(me.label);
    await expect(phone.inbox.next(200)).rejects.toThrow();
    expect(pc.inbox.closed).toBeNull();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    const at = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect((at as number) - Date.now()).toBeLessThanOrEqual(15 * 60_000);
    expect((at as number) - Date.now()).toBeGreaterThan(60_000);
  });
});

describe("a recheck that fails", () => {
  it("keeps the alarm, tries again soon, and a later recheck still closes", async () => {
    const { me, pc, owner, dev, phone } = await computerAndPhone();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    // D1 fails the object's next batch, once.
    await runInDurableObject(stub, (instance) => {
      const real = (instance as unknown as { env: Env }).env;
      let failures = 1;
      const db = {
        prepare: (q: string) => real.DB.prepare(q),
        batch: (statements: D1PreparedStatement[]) => {
          if (failures-- > 0) return Promise.reject(new Error("D1 is down"));
          return real.DB.batch(statements);
        },
      };
      (instance as unknown as { env: Env }).env = Object.create(real, { DB: { value: db } });
    });
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});

    // The Worker's recheck after a sign-out fails: the object answers 503 and
    // the Worker says so in the log, but the sign-out itself stands.
    expect((await call("DELETE", `/sessions/${dev.session_id}`, as(owner.session))).status).toBe(204);
    expect(logged.mock.calls.some((c) => String(c[0]).includes("relay recheck failed"))).toBe(true);
    expect(logged.mock.calls.some((c) => String(c[0]).includes(`recheck of ${me.label} answered 503`))).toBe(true);
    expect(phone.inbox.closed).toBeNull();
    const at = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect(at).not.toBeNull();
    expect((at as number) - Date.now()).toBeLessThanOrEqual(30_000);

    // The retry closes the signed-out device.
    await recheckDue(me.label);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
    expect(await pc.inbox.control()).toEqual({ type: "close", channel: 1, reason: "signed_out" });
  });

  it("in the alarm itself does not take the alarm down", async () => {
    const { me, dev, phone } = await computerAndPhone();
    const stub = env.RELAY.get(env.RELAY.idFromName(me.label));
    await runInDurableObject(stub, (instance) => {
      const real = (instance as unknown as { env: Env }).env;
      const db = { prepare: (q: string) => real.DB.prepare(q), batch: () => Promise.reject(new Error("D1 is down")) };
      (instance as unknown as { env: Env }).env = Object.create(real, { DB: { value: db } });
    });
    vi.spyOn(console, "error").mockImplementation(() => {});
    await env.DB.prepare("DELETE FROM sessions WHERE id = ?").bind(dev.session_id).run();
    await recheckDue(me.label);
    expect(phone.inbox.closed).toBeNull();
    const at = await runInDurableObject(stub, (_i, state) => state.storage.getAlarm());
    expect((at as number) - Date.now()).toBeGreaterThan(0);
    expect((at as number) - Date.now()).toBeLessThanOrEqual(30_000);

    // D1 is back: the next alarm closes the socket whose session is gone.
    await runInDurableObject(stub, (instance) => {
      const env2 = (instance as unknown as { env: Env }).env;
      (instance as unknown as { env: Env }).env = Object.getPrototypeOf(env2) as Env;
    });
    await recheckDue(me.label);
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
  });
});

describe("the recheck's rule", () => {
  const base = { label: "aaaaaaaaaaaaaaaa", account: 7, session: "h", plan: "free", checked: 0, nonce: "", until: 0, name: "", kind: "app" };
  const row = { token_hash: "h", account_id: 7, blocked: 0, plan: "free" };

  it("does not judge a computer by an owner read from before its claim", () => {
    const claimed: Tag = { role: "pc", claimed: true, ...base };
    expect(refusal(claimed, row, null)).toBe("no_machine");
    expect(refusal(claimed, row, null, false)).toBeNull();
    // Its session and its account still count.
    expect(refusal(claimed, undefined, null, false)).toBe("signed_out");
    expect(refusal(claimed, { ...row, blocked: 1 }, null, false)).toBe("blocked");
  });
});

describe("forgetting a machine", () => {
  it("closes its sockets and frees its place", async () => {
    const owner = await account();
    const five = [];
    for (let i = 0; i < 5; i++) five.push(await connectedComputer(owner));
    const gone = five[0]!;
    const dev = await device(owner);
    const phone = await openPhone(gone.me.label, dev.session);
    await phone.inbox.control();
    expect(await gone.pc.inbox.control()).toEqual(opened(1, dev));

    expect((await call("DELETE", `/machines/${gone.me.label}`, as(owner.session))).status).toBe(204);
    expect(await gone.pc.inbox.control()).toEqual({ type: "bye", reason: "no_machine" });
    expect(await phone.inbox.control()).toEqual({ type: "bye", reason: "no_machine" });
    const list = (await (await call("GET", "/machines", as(owner.session))).json()) as { label: string }[];
    expect(list.map((m) => m.label)).not.toContain(gone.me.label);

    const sixth = await computer();
    const pc = await openPc(sixth.label, owner.session);
    expect(await claim(pc, sixth)).toEqual({ type: "claimed" });
  });

  it("is only for the account's own machines", async () => {
    const { me, pc } = await connectedComputer();
    const other = await account();
    expect((await call("DELETE", `/machines/${me.label}`, as(other.session))).status).toBe(404);
    expect((await call("DELETE", `/machines/${me.label}`)).status).toBe(401);
    expect((await call("DELETE", "/machines/not-a-label", as(other.session))).status).toBe(404);
    await expect(pc.inbox.next(200)).rejects.toThrow();
  });
});

describe("a device socket with the session cookie", () => {
  it("comes in from our own origin, and is refused with 403 from another or from none", async () => {
    const { me, pc, owner } = await connectedComputer();
    const dev = await device(owner);
    const url = `${HOST}/machines/${me.label}/ws`;
    const cookie = `__Host-arakun_session=${dev.session}`;
    for (const origin of ["https://evil.example", "http://app.arakun.dev", null]) {
      const headers: Record<string, string> = { Upgrade: "websocket", Cookie: cookie };
      if (origin) headers["Origin"] = origin;
      const res = await SELF.fetch(url, { headers });
      expect(res.status, String(origin)).toBe(403);
      expect(res.webSocket).toBeNull();
    }
    await expect(pc.inbox.next(200)).rejects.toThrow();

    const res = await SELF.fetch(url, { headers: { Upgrade: "websocket", Cookie: cookie, Origin: HOST } });
    expect(res.status).toBe(101);
    res.webSocket!.accept();
    expect(await pc.inbox.control()).toEqual(opened(1, dev));
  });
});

describe("the object's own paths", () => {
  it("are not reachable from outside", async () => {
    const { me, owner } = await connectedComputer();
    for (const path of ["_recheck", "_state"]) {
      expect((await call("POST", `/machines/${me.label}/${path}`, as(owner.session))).status).toBe(404);
      expect((await call("GET", `/machines/${me.label}/${path}`, as(owner.session))).status).toBe(404);
    }
  });
});
