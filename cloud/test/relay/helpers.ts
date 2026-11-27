// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// A computer and devices made of WebCrypto and signed-in accounts, talking to
// the Worker under test.
import { SELF, env, runInDurableObject } from "cloudflare:test";
import { expect } from "vitest";
import { CONTROL, b64url, concat, parseJson, parsePcFrame, parsePhoneFrame, pcControl, utf8 } from "../../src/relay/frames";
import { labelOf } from "../../src/relay/label";
import { signIn, type SignedIn } from "../helpers";

export interface Computer {
  key: CryptoKeyPair;
  pub: Uint8Array;
  pubB64: string;
  label: string;
}

/// A computer with a fresh identity: its key, and the label that key derives.
export async function computer(): Promise<Computer> {
  const key = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const pub = new Uint8Array((await crypto.subtle.exportKey("raw", key.publicKey)) as ArrayBuffer);
  return { key, pub, pubB64: b64url(pub), label: await labelOf(pub) };
}

export async function sign(key: CryptoKeyPair, data: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key.privateKey, data as BufferSource));
}

/// What a socket said, in order, with a way to wait for the next thing.
export class Inbox {
  private queue: unknown[] = [];
  private waiting: ((v: unknown) => void)[] = [];
  closed: { code: number; reason: string } | null = null;

  constructor(ws: WebSocket, parse: (bytes: Uint8Array) => unknown) {
    ws.addEventListener("message", (ev) => {
      const data = ev.data;
      this.push(typeof data === "string" ? { text: data } : parse(new Uint8Array(data as ArrayBuffer)));
    });
    ws.addEventListener("close", (ev) => {
      this.closed = { code: ev.code, reason: ev.reason };
      this.push({ closed: this.closed });
    });
  }

  private push(v: unknown) {
    const w = this.waiting.shift();
    if (w) w(v);
    else this.queue.push(v);
  }

  next(ms = 2000): Promise<unknown> {
    const got = this.queue.shift();
    if (got !== undefined) return Promise.resolve(got);
    return new Promise((resolve, reject) => {
      const waiter = (v: unknown) => {
        clearTimeout(t);
        resolve(v);
      };
      // A waiter that gave up must not eat the next thing that arrives.
      const t = setTimeout(() => {
        this.waiting = this.waiting.filter((w) => w !== waiter);
        reject(new Error("nothing arrived in time"));
      }, ms);
      this.waiting.push(waiter);
    });
  }

  /// The next control message, as JSON.
  async control(): Promise<Record<string, unknown>> {
    const got = (await this.next()) as { type?: number; payload?: Uint8Array; closed?: unknown };
    if (got.closed) throw new Error(`socket closed: ${JSON.stringify(got.closed)}`);
    if (got.type !== CONTROL || !got.payload) throw new Error(`not a control frame: ${JSON.stringify(got)}`);
    const msg = parseJson(got.payload);
    if (!msg) throw new Error("control frame without JSON");
    return msg;
  }

  /// Waits for the close event (anything before it is dropped).
  async closedWith(): Promise<{ code: number; reason: string }> {
    for (;;) {
      const got = (await this.next()) as { closed?: { code: number; reason: string } };
      if (got.closed) return got.closed;
    }
  }
}

export interface PcSocket {
  ws: WebSocket;
  inbox: Inbox;
}

/// The account host: every host that is not `<label>.<BASE_DOMAIN>`.
export const HOST = "https://app.arakun.dev";

let nextAccount = 50_000;

/// A new account, signed in once: the session a computer uses.
export async function account(name = "Desk"): Promise<SignedIn> {
  const id = nextAccount++;
  return signIn(id, `user${id}`, `gho_pc_${id}`, { name, kind: "app" });
}

let nextDevice = 0;

/// Another session of the same account: a device.
export async function device(owner: SignedIn, name = "Phone", kind = "phone"): Promise<SignedIn> {
  return signIn(owner.githubId, owner.login, `gho_dev_${owner.githubId}_${nextDevice++}`, { name, kind });
}

/// What the computer hears when a device's channel opens.
export function opened(channel: number, dev: SignedIn, name = "Phone", kind = "phone"): Record<string, unknown> {
  return { type: "open", channel, device: dev.session_id, name, kind, account: dev.github_id };
}

async function socket(url: string, headers: Record<string, string>): Promise<WebSocket> {
  const res = await SELF.fetch(url, { headers: { Upgrade: "websocket", ...headers } });
  if (res.status !== 101 || !res.webSocket) throw new Error(`no socket: ${res.status} ${await res.text()}`);
  return res.webSocket;
}

function bearerOf(session: string | null): Record<string, string> {
  return session ? { Authorization: `Bearer ${session}` } : {};
}

/// Opens the computer's socket for a label, with the computer's session.
export async function openPc(
  label: string,
  session: string | null,
  query = "name=Desk&kind=app",
  headers: Record<string, string> = {},
): Promise<PcSocket> {
  const ws = await socket(`${HOST}/machines/${label}/pc?${query}`, { ...bearerOf(session), ...headers });
  const inbox = new Inbox(ws, (b) => parsePcFrame(b) ?? { bad: true });
  ws.accept();
  // Newer compatibility dates hand binary frames over as Blob unless asked.
  ws.binaryType = "arraybuffer";
  return { ws, inbox };
}

/// Opens a device's socket for a label, with a device's session.
export async function openPhone(
  label: string,
  session: string | null,
  query = "",
  headers: Record<string, string> = {},
): Promise<PcSocket> {
  const ws = await socket(`${HOST}/machines/${label}/ws${query ? `?${query}` : ""}`, { ...bearerOf(session), ...headers });
  const inbox = new Inbox(ws, (b) => parsePhoneFrame(b) ?? { bad: true });
  ws.accept();
  ws.binaryType = "arraybuffer";
  return { ws, inbox };
}

/// What a computer signs to claim `label`: the tag, the nonce, the label.
/// `untagged` is what computers from before the tag sign.
export function claimMessage(nonce: Uint8Array, label: string, untagged = false): Uint8Array {
  return untagged ? concat(nonce, utf8(label)) : concat(utf8("arakun-relay-claim-v1\0"), nonce, utf8(label));
}

/// Answers the challenge as this computer would, and returns what the relay said next.
export async function claim(pc: PcSocket, me: Computer, label = me.label, untagged = false): Promise<Record<string, unknown>> {
  const challenge = await pc.inbox.control();
  if (challenge["type"] !== "challenge") throw new Error(`expected a challenge, got ${JSON.stringify(challenge)}`);
  const nonce = Uint8Array.from(atob((challenge["nonce"] as string).replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
  const sig = await sign(me.key, claimMessage(nonce, label, untagged));
  pc.ws.send(pcControl({ type: "claim", pub: me.pubB64, sig: b64url(sig) }));
  return pc.inbox.control();
}

/// A computer connected and claimed under an account (a new one if none is given).
export async function connectedComputer(owner?: SignedIn): Promise<{ me: Computer; pc: PcSocket; owner: SignedIn }> {
  const who = owner ?? (await account());
  const me = await computer();
  const pc = await openPc(me.label, who.session);
  const answer = await claim(pc, me);
  if (answer["type"] !== "claimed") throw new Error(`claim refused: ${JSON.stringify(answer)}`);
  return { me, pc, owner: who };
}

/// A connected computer and one device of its account, the `open` on both sides read.
export async function computerAndPhone(): Promise<{ me: Computer; pc: PcSocket; owner: SignedIn; dev: SignedIn; phone: PcSocket }> {
  const { me, pc, owner } = await connectedComputer();
  const dev = await device(owner);
  const phone = await openPhone(me.label, dev.session);
  expect(await phone.inbox.control()).toEqual({ type: "open", channel: 1 });
  expect(await pc.inbox.control()).toEqual(opened(1, dev));
  return { me, pc, owner, dev, phone };
}

/// What the label's object has under a key right now.
export async function stored<T>(label: string, key: string): Promise<T | undefined> {
  const stub = env.RELAY.get(env.RELAY.idFromName(label));
  return runInDurableObject(stub, (_instance, state) => state.storage.get<T>(key));
}

/// Waits until the object has stored something that satisfies `ok`. A message
/// sent on a socket and a fetch of the same object race; this settles the race.
export async function untilStored<T>(label: string, key: string, ok: (v: T | undefined) => boolean): Promise<void> {
  for (let i = 0; i < 50; i++) {
    if (ok(await stored<T>(label, key))) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`${key} never settled`);
}

/// The computer leaves, and this waits until its object counts it offline,
/// as `deliver` asks. `last_seen` cannot tell: the claim already set it.
export async function away(label: string, pc: PcSocket): Promise<void> {
  pc.ws.close(1000, "shutting down");
  await pc.inbox.closedWith();
  const stub = env.RELAY.get(env.RELAY.idFromName(label));
  for (let i = 0; i < 50; i++) {
    const res = await stub.fetch("https://relay/_state", { headers: { "x-arakun-label": label } });
    if (!((await res.json()) as { online: boolean }).online) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error("the computer never went offline");
}
