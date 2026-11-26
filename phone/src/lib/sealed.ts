// The sealed socket to the computer's own server (core/src/phone/lan.rs). It
// speaks the relay's end-to-end protocol (e2e.js) without the relay: the
// computer signs the handshake with its key, the page checks that key is the
// one it was paired with (or, pairing by a typed code, takes the one it meets),
// and from then on every frame is sealed. The first sealed message says who
// this is: `{pair: {code, name}}` or `{auth: {token}}`. Then the page speaks
// as through the relay: the same messages, and `{req}` for what would be HTTP.

import { E2E, type E2eHandshake, type E2eSession } from "./globals";

export const SEALED_PATH = "/e2e";
/** The channel the computer's own server signs its handshake for. */
export const SEALED_CHANNEL = 0;

/** What the computer said to the greeting. */
export type Answer =
  | { type: "paired"; token: string; id: string; name: string; host: string }
  | { type: "welcome"; id: string; host: string };

export interface SealedHandlers {
  /** The computer took the greeting: `label` is its key's. */
  onopen(answer: Answer, label: string): void;
  onmessage(msg: any): void;
  /**
   * The socket is over. `refused`: the computer turned the greeting down
   * (`detail` is its reason: `code`, `unpaired` or `bad`). `untrusted`: the
   * other end is not the computer this phone was paired with, or broke the
   * protocol. `gone`: it dropped; another try may do.
   */
  onclose(info: { kind: "refused" | "untrusted" | "gone"; detail: string }): void;
}

export interface SealedOptions {
  /** The label the handshake must be signed by; "" takes the one that answers. */
  label: string;
  greeting: { pair: { code: string; name: string } } | { auth: { token: string } };
  WebSocketImpl?: typeof WebSocket;
}

export interface SealedLink {
  send(obj: unknown): void;
  fetch(path: string, init?: { method?: string; body?: unknown }): Promise<Response>;
  close(): void;
  readonly ready: boolean;
}

/** Loads the stand-in for WebCrypto when the page has none (plain http). */
export async function ensureCrypto(): Promise<void> {
  const g = globalThis as { crypto?: Crypto; E2E_SUBTLE?: unknown };
  if (g.crypto?.subtle || g.E2E_SUBTLE) return;
  const { installSubtle } = await import("./subtle");
  installSubtle();
}

/** The sealed socket's address on the host that served the page. */
export function sealedUrl(): string {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  return `${proto}://${location.host}${SEALED_PATH}`;
}

export function openSealed(opts: SealedOptions, handlers: SealedHandlers): SealedLink {
  const WS = opts.WebSocketImpl ?? WebSocket;
  const { HELLO, DATA } = E2E;
  let ws: WebSocket | null = null;
  let handshake: E2eHandshake | null = null;
  let session: E2eSession | null = null;
  let greeted = false;
  let ended = false;
  let nextReq = 1;
  const pending = new Map<number, { resolve(r: Response): void; reject(e: Error): void; status: number; type: string; parts: Uint8Array[] }>();
  // Frames are opened and sealed in the order they come and go: the counters
  // say so. Each direction waits its turn.
  let reading = Promise.resolve();
  let writing = Promise.resolve();

  const end = (kind: "refused" | "untrusted" | "gone", detail: string) => {
    if (ended) return;
    ended = true;
    for (const p of pending.values()) p.reject(new Error("link closed"));
    pending.clear();
    try {
      ws?.close();
    } catch {
      // Already closed.
    }
    handlers.onclose({ kind, detail });
  };

  const sealAndSend = (obj: unknown) => {
    writing = writing
      .then(async () => {
        if (ended || !session || !ws) return;
        ws.send(E2E.encodeFrame(DATA, await session.seal(E2E.utf8(JSON.stringify(obj)))));
      })
      .catch(() => end("gone", "could not send"));
  };

  const heard = (msg: any) => {
    if (!greeted) {
      if (msg?.type === "paired" || msg?.type === "welcome") {
        greeted = true;
        handlers.onopen(msg as Answer, pinned);
        return;
      }
      return end("refused", typeof msg?.reason === "string" ? msg.reason : "bad");
    }
    if (msg?.res) {
      const r = msg.res;
      const p = pending.get(r.id);
      if (!p) return;
      if (r.status != null) p.status = r.status;
      if (r.type != null) p.type = r.type;
      p.parts.push(E2E.fromB64url(r.body || "") || new Uint8Array());
      if (!r.more) {
        pending.delete(r.id);
        p.resolve(new Response(join(p.parts) as BodyInit, { status: p.status || 500, headers: { "Content-Type": p.type || "application/octet-stream" } }));
      }
      return;
    }
    handlers.onmessage(msg);
  };

  let pinned = opts.label;
  const frame = async (bytes: Uint8Array) => {
    if (ended) return;
    const f = E2E.decodeFrame(bytes);
    if (!f) return end("untrusted", "bad frame");
    if (f.type === HELLO) {
      if (!handshake) return end("untrusted", "a second hello");
      try {
        if (!pinned) pinned = await E2E.helloLabel(f.payload);
        if (!pinned) throw new Error("bad hello: pub");
        session = await handshake.finish(f.payload, pinned, SEALED_CHANNEL);
      } catch (e) {
        return end("untrusted", (e as Error).message);
      }
      handshake = null;
      sealAndSend(opts.greeting);
      return;
    }
    if (f.type !== DATA || !session) return end("untrusted", "bad frame");
    let msg;
    try {
      msg = JSON.parse(E2E.text(await session.open(f.payload)));
    } catch (e) {
      return end("untrusted", (e as Error).message);
    }
    heard(msg);
  };

  const start = async () => {
    try {
      await ensureCrypto();
      handshake = await E2E.begin();
    } catch (e) {
      return end("gone", (e as Error).message);
    }
    if (ended) return;
    ws = new WS(sealedUrl());
    ws.binaryType = "arraybuffer";
    ws.addEventListener("open", () => {
      if (handshake && ws) ws.send(E2E.encodeFrame(HELLO, handshake.hello));
    });
    ws.addEventListener("message", (ev) => {
      const data = (ev as MessageEvent).data;
      // Only binary frames on this socket.
      if (typeof data === "string") return end("untrusted", "a frame in the clear");
      reading = reading.then(() => frame(new Uint8Array(data))).catch((e) => end("gone", (e as Error).message));
    });
    ws.addEventListener("close", () => end("gone", "closed"));
    // Its own socket: one closed by Retry can still report an error later.
    ws.addEventListener("error", () => ws?.close());
  };
  void start();

  return {
    send(obj) {
      if (greeted && !ended) sealAndSend(obj);
    },
    fetch(path, init = {}) {
      if (ended || !greeted) return Promise.reject(new Error("not connected"));
      const id = nextReq++;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject, status: 0, type: "", parts: [] });
        sealAndSend({ req: { id, method: init.method || "GET", path, body: init.body == null ? null : String(init.body) } });
      });
    },
    close() {
      if (ended) return;
      ended = true;
      for (const p of pending.values()) p.reject(new Error("link closed"));
      pending.clear();
      try {
        ws?.close();
      } catch {
        // Already closed.
      }
    },
    get ready() {
      return greeted && !ended;
    },
  };
}

function join(parts: Uint8Array[]): Uint8Array {
  const whole = new Uint8Array(parts.reduce((n, b) => n + b.length, 0));
  let at = 0;
  for (const b of parts) {
    whole.set(b, at);
    at += b.length;
  }
  return whole;
}
