// The sealed socket (sealed.ts) against a computer played here with WebCrypto,
// as core/src/phone/lan.rs plays it: the computer signs the handshake for
// channel 0, takes the greeting, then answers `{req}`s and says things.
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import e2eSource from "../../public/e2e.js?raw";
import { openSealed, type SealedHandlers, type SealedOptions } from "./sealed";

const E2E: any = {};
beforeAll(() => {
  new Function(e2eSource)();
  Object.assign(E2E, (globalThis as any).E2E);
});
afterEach(() => {
  Computer.all = [];
});

const real = () => crypto.subtle;

/** A computer with a key of its own, behind a fake socket. */
class Computer extends EventTarget {
  static all: Computer[] = [];
  static identity: Promise<{ key: CryptoKeyPair; pub: Uint8Array; label: string }>;
  binaryType = "";
  greeting: any = null;
  heard: any[] = [];
  closed = false;
  /** What the computer says to the greeting. */
  static answer: (greeting: any) => object = () => ({ type: "welcome", id: "p1", host: "mbp" });
  private session: any = null;
  private queue = Promise.resolve();

  constructor(readonly url: string) {
    super();
    Computer.all.push(this);
    setTimeout(() => this.dispatchEvent(new Event("open")), 0);
  }

  send(data: Uint8Array) {
    this.queue = this.queue.then(() => this.receive(data));
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.dispatchEvent(new Event("close"));
  }

  private emit(type: number, payload: Uint8Array) {
    const bytes = E2E.encodeFrame(type, payload);
    this.dispatchEvent(new MessageEvent("message", { data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.length) }));
  }

  async say(obj: unknown) {
    this.emit(E2E.DATA, await this.session.seal(JSON.stringify(obj)));
  }

  private async receive(data: Uint8Array) {
    const f = E2E.decodeFrame(data);
    if (f.type === E2E.HELLO) return this.answerHello(f.payload);
    const msg = JSON.parse(E2E.text(await this.session.open(f.payload)));
    if (!this.greeting) {
      this.greeting = msg;
      await this.say(Computer.answer(msg));
      return;
    }
    this.heard.push(msg);
    if (msg.req) {
      const body = btoa(JSON.stringify({ asked: msg.req.path }));
      await this.say({ res: { id: msg.req.id, status: 200, type: "application/json", body: body.replace(/=+$/, ""), more: false } });
    }
  }

  private async answerHello(payload: Uint8Array) {
    const { key, pub, label } = await Computer.identity;
    const hello = JSON.parse(E2E.text(payload));
    const ePhone = E2E.fromB64url(hello.e);
    const noncePhone = E2E.fromB64url(hello.nonce);
    const pair = await real().generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
    const ePc = new Uint8Array(await real().exportKey("raw", pair.publicKey));
    const noncePc = crypto.getRandomValues(new Uint8Array(16));
    const signed = E2E.transcript(label, 0, ePhone, ePc, noncePhone, noncePc);
    const sig = new Uint8Array(await real().sign({ name: "ECDSA", hash: "SHA-256" }, key.privateKey, signed));
    const theirs = await real().importKey("raw", ePhone, { name: "ECDH", namedCurve: "P-256" }, false, []);
    const shared = new Uint8Array(await real().deriveBits({ name: "ECDH", public: theirs }, pair.privateKey, 256));
    const keys = await E2E.deriveKeys(shared, noncePhone, noncePc);
    this.session = await E2E.sessionFromKeys(keys.pcToPhone, keys.phoneToPc);
    const reply = { v: 1, pub: E2E.b64url(pub), e: E2E.b64url(ePc), nonce: E2E.b64url(noncePc), sig: E2E.b64url(sig), arakun: "t" };
    this.emit(E2E.HELLO, E2E.utf8(JSON.stringify(reply)));
  }
}

async function newIdentity() {
  const key = (await real().generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign", "verify"])) as CryptoKeyPair;
  const pub = new Uint8Array(await real().exportKey("raw", key.publicKey));
  return { key, pub, label: await E2E.labelOf(pub) };
}

/** Opens a sealed link to the computer and records what its handlers hear. */
function dial(opts: Omit<SealedOptions, "WebSocketImpl">) {
  const seen = { opened: null as null | { answer: any; label: string }, messages: [] as any[], closed: null as null | { kind: string; detail: string } };
  let wake = () => {};
  const changed = () => new Promise<void>((r) => (wake = r));
  const handlers: SealedHandlers = {
    onopen: (answer, label) => {
      seen.opened = { answer, label };
      wake();
    },
    onmessage: (msg) => {
      seen.messages.push(msg);
      wake();
    },
    onclose: (info) => {
      seen.closed = info;
      wake();
    },
  };
  const link = openSealed({ ...opts, WebSocketImpl: Computer as any }, handlers);
  return { link, seen, changed };
}

describe("the sealed socket", () => {
  it("brings the token only to the computer it was paired with, then talks", async () => {
    Computer.identity = newIdentity();
    const { label } = await Computer.identity;
    Computer.answer = () => ({ type: "welcome", id: "p1", host: "mbp" });
    const { link, seen, changed } = dial({ label, greeting: { auth: { token: "tok" } } });
    while (!seen.opened) await changed();
    expect(seen.opened).toEqual({ answer: { type: "welcome", id: "p1", host: "mbp" }, label });
    const pc = Computer.all[0];
    expect(pc.url).toMatch(/\/e2e$/);
    expect(pc.greeting).toEqual({ auth: { token: "tok" } });
    expect(link.ready).toBe(true);

    const res = await link.fetch("/me");
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ asked: "/me" });
    link.send({ type: "open", session: "s1" });
    await pc.say({ type: "inbox", sessions: [] });
    while (!seen.messages.length) await changed();
    expect(seen.messages).toEqual([{ type: "inbox", sessions: [] }]);
    while (pc.heard.length < 2) await new Promise((r) => setTimeout(r, 5));
    expect(pc.heard).toContainEqual({ type: "open", session: "s1" });
    link.close();
    expect(pc.closed).toBe(true);
  });

  it("refuses a computer with another key before the token goes out", async () => {
    Computer.identity = newIdentity();
    const other = (await newIdentity()).label;
    const { seen, changed } = dial({ label: other, greeting: { auth: { token: "tok" } } });
    while (!seen.closed) await changed();
    expect(seen.closed).toEqual({ kind: "untrusted", detail: "bad hello: label" });
    expect(seen.opened).toBeNull();
    await new Promise((r) => setTimeout(r, 10));
    expect(Computer.all[0].greeting).toBeNull();
  });

  it("pairing by a typed code takes the key that answers", async () => {
    Computer.identity = newIdentity();
    const { label } = await Computer.identity;
    Computer.answer = (g) => ({ type: "paired", token: `t-${g.pair.code}`, id: "p2", name: g.pair.name, host: "mbp" });
    const { seen, changed } = dial({ label: "", greeting: { pair: { code: "123456", name: "iPhone" } } });
    while (!seen.opened) await changed();
    expect(seen.opened!.label).toBe(label);
    expect(seen.opened!.answer).toMatchObject({ type: "paired", token: "t-123456" });
  });

  it("says why the computer turned the greeting down", async () => {
    Computer.identity = newIdentity();
    const { label } = await Computer.identity;
    Computer.answer = () => ({ type: "refused", reason: "unpaired" });
    const { link, seen, changed } = dial({ label, greeting: { auth: { token: "old" } } });
    while (!seen.closed) await changed();
    expect(seen.closed).toEqual({ kind: "refused", detail: "unpaired" });
    await expect(link.fetch("/me")).rejects.toThrow("not connected");
  });
});
