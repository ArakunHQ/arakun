// The sealed socket against a computer played here, as core/src/phone/direct.rs
// answers: a signed hello, then who the phone is, then requests inside.
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import e2eScript from "../../public/e2e.js?raw";
import { openSealed, type Closed, type Opened } from "./sealed";

let E2E: any;
beforeAll(() => {
  new Function(e2eScript)();
  E2E = (globalThis as any).E2E;
});

const tick = () => new Promise((r) => setTimeout(r, 0));
async function until(f: () => boolean) {
  for (let i = 0; i < 200 && !f(); i++) await tick();
  expect(f()).toBe(true);
}

/** A computer with an identity key, answering one socket. */
async function computer() {
  const id = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
  const pub = new Uint8Array(await crypto.subtle.exportKey("raw", id.publicKey));
  const label: string = await E2E.labelOf(pub);
  let session: any = null;
  const heard: any[] = [];
  return {
    label,
    heard,
    /** What it sends back for a frame from the phone, if anything. */
    async answer(frame: Uint8Array): Promise<Uint8Array[]> {
      const f = E2E.decodeFrame(frame);
      if (f.type === E2E.HELLO) {
        const hello = JSON.parse(E2E.text(f.payload));
        const ePhone = E2E.fromB64url(hello.e);
        const noncePhone = E2E.fromB64url(hello.nonce);
        const pair = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
        const ePc = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
        const noncePc = crypto.getRandomValues(new Uint8Array(16));
        const signed = E2E.transcript(label, E2E.DIRECT, ePhone, ePc, noncePhone, noncePc);
        const sig = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, id.privateKey, signed));
        const theirs = await crypto.subtle.importKey("raw", ePhone, { name: "ECDH", namedCurve: "P-256" }, false, []);
        const shared = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: theirs }, pair.privateKey, 256));
        const keys = await E2E.deriveKeys(shared, noncePhone, noncePc);
        session = await E2E.sessionFromKeys(keys.pcToPhone, keys.phoneToPc);
        const reply = { v: 1, pub: E2E.b64url(pub), e: E2E.b64url(ePc), nonce: E2E.b64url(noncePc), sig: E2E.b64url(sig), arakun: "t" };
        return [E2E.encodeFrame(E2E.HELLO, E2E.utf8(JSON.stringify(reply)))];
      }
      const msg = JSON.parse(E2E.text(await session.open(f.payload)));
      heard.push(msg);
      const seal = async (o: object) => E2E.encodeFrame(E2E.DATA, await session.seal(JSON.stringify(o)));
      if (msg.auth) return [await seal(msg.auth.token === "tok" ? { authed: { id: "d1" } } : { refused: { reason: "unpaired" } })];
      if (msg.req) {
        const body = E2E.b64url(E2E.utf8(JSON.stringify({ id: "d1", host: "mbp" })));
        return [await seal({ res: { id: msg.req.id, status: 200, type: "application/json", body, more: false } })];
      }
      return [];
    },
  };
}

/** A WebSocket whose other end is `pc`. */
function socketTo(pc: Awaited<ReturnType<typeof computer>>) {
  return class extends EventTarget {
    static OPEN = 1;
    static last: any;
    binaryType = "";
    closed = false;
    constructor(readonly url: string) {
      super();
      (this.constructor as any).last = this;
      setTimeout(() => this.dispatchEvent(new Event("open")), 0);
    }
    send(data: Uint8Array) {
      void pc.answer(data).then((out) => {
        for (const f of out) this.dispatchEvent(new MessageEvent("message", { data: f.slice().buffer }));
      });
    }
    close() {
      if (this.closed) return;
      this.closed = true;
      this.dispatchEvent(new Event("close"));
    }
  };
}

let ended: (() => void) | null = null;
afterEach(() => ended?.());

describe("sealed socket", () => {
  it("checks the computer's key, brings the token sealed, and asks inside", async () => {
    const pc = await computer();
    const WS = socketTo(pc);
    let opened: Opened | null = null;
    const link = openSealed(
      { who: { token: "tok" }, label: pc.label, WebSocketImpl: WS as any },
      { onopen: (o) => (opened = o), onmessage() {}, onclose() {} },
    );
    ended = () => link.close();
    await until(() => opened !== null);
    expect(opened).toEqual({ label: pc.label });
    expect((WS as any).last.url).toMatch(/\/e2e$/);
    expect(pc.heard).toEqual([{ auth: { token: "tok" } }]);
    const res = await link.fetch("/me");
    expect(await res.json()).toEqual({ id: "d1", host: "mbp" });
  });

  it("another key gets nothing: the socket ends before the token goes out", async () => {
    const pc = await computer();
    let closed: Closed | null = null;
    const link = openSealed(
      { who: { token: "tok" }, label: "aaaaaaaaaaaaaaaa", WebSocketImpl: socketTo(pc) as any },
      { onopen() {}, onmessage() {}, onclose: (c) => (closed = c) },
    );
    ended = () => link.close();
    await until(() => closed !== null);
    expect(closed).toEqual({ kind: "untrusted", detail: "bad hello: label" });
    expect(pc.heard).toEqual([]);
  });

  it("a token the computer does not know is refused", async () => {
    const pc = await computer();
    let closed: Closed | null = null;
    const link = openSealed(
      { who: { token: "old" }, label: "", WebSocketImpl: socketTo(pc) as any },
      { onopen() {}, onmessage() {}, onclose: (c) => (closed = c) },
    );
    ended = () => link.close();
    await until(() => closed !== null);
    expect(closed).toEqual({ kind: "refused", detail: "unpaired" });
  });
});
