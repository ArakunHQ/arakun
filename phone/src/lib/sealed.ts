// The phone's socket on the computer's own server, `/e2e`
// (core/src/phone/direct.rs): the relay's end-to-end handshake with nobody in
// between, so nobody on the network reads the token or the transcripts. The
// hello must come from the key whose label this phone pinned when it paired;
// with none pinned yet, the key it shows is the one to pin. Inside, the phone
// says who it is (its token, or a pairing code), then speaks JSON as on the
// relay, with `req` for what would be an HTTP request.

import { E2E, type E2eSession } from "./globals";

/** How long the computer may take to answer the hello and say whether it lets us in. */
export const SEALED_OPEN_MS = 15_000;

/** What the computer said when it let this phone in. */
export interface Opened {
  /** The label of the key that answered, to pin. */
  label: string;
  /** Set when the phone brought a pairing code: what to come back with. */
  paired?: { token: string; host: string; id: string };
}

/**
 * Why the socket ended: `refused` with `reason` (`unpaired`, `bad_code`,
 * `bad_request`) when the computer said no, `untrusted` when another key
 * answered, `gone` otherwise.
 */
export interface Closed {
  kind: "refused" | "untrusted" | "gone";
  detail: string;
}

export interface SealedHandlers {
  onopen(o: Opened): void;
  onmessage(msg: any): void;
  onclose(c: Closed): void;
}

export interface SealedOptions {
  /** Who this phone is: its token, or a pairing code and the name to be listed as. */
  who: { token: string } | { code: string; name: string };
  /** The label pinned for this computer, or "" to take the key the first handshake shows. */
  label: string;
  WebSocketImpl?: typeof WebSocket;
}

export interface SealedLink {
  send(obj: unknown): void;
  fetch(path: string, init?: { method?: string; body?: unknown }): Promise<Response>;
  close(): void;
  readonly ready: boolean;
}

let fallback: Promise<void> | null = null;

/** Over plain http the browser keeps WebCrypto from the page: the pure-JS build of the same primitives steps in. */
export function ensureCrypto(): Promise<void> {
  if (E2E.hasWebCrypto()) return Promise.resolve();
  if (!fallback) {
    fallback = import("./purecrypto").then((m) => E2E.usePrimitives(m.primitives));
  }
  return fallback;
}

export function openSealed(opts: SealedOptions, handlers: SealedHandlers): SealedLink {
  const WS = opts.WebSocketImpl ?? WebSocket;
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WS(`${proto}://${location.host}/e2e`);
  ws.binaryType = "arraybuffer";
  let session: E2eSession | null = null;
  let label = "";
  let ready = false;
  let ended = false;
  let nextReq = 1;
  const pending = new Map<number, { resolve: (r: Response) => void; reject: (e: Error) => void }>();
  // Frames are opened and sealed in the order they come and go: the counters say so.
  let reading: Promise<unknown> = Promise.resolve();
  let writing: Promise<unknown> = Promise.resolve();
  const waiting = setTimeout(() => end("gone", "no answer from the computer"), SEALED_OPEN_MS);

  function end(kind: Closed["kind"], detail: string) {
    if (ended) return;
    ended = true;
    clearTimeout(waiting);
    for (const p of pending.values()) p.reject(new Error("link closed"));
    pending.clear();
    try {
      ws.close();
    } catch {
      // Already closed.
    }
    handlers.onclose({ kind, detail });
  }

  function say(obj: unknown) {
    writing = writing
      .then(async () => {
        if (ended || !session) return;
        ws.send(E2E.encodeFrame(E2E.DATA, await session.seal(E2E.utf8(JSON.stringify(obj)))));
      })
      .catch(() => end("gone", "could not send"));
  }

  let handshake: Awaited<ReturnType<typeof E2E.begin>> | null = null;

  async function hello(payload: Uint8Array) {
    if (!handshake) return;
    const shown = await E2E.helloLabel(payload);
    // Another key at this address: nothing more goes out, the token least of all.
    if (!shown || (opts.label && shown !== opts.label)) return end("untrusted", "bad hello: label");
    try {
      session = await handshake.finish(payload, shown, E2E.DIRECT);
    } catch (e) {
      return end("untrusted", (e as Error).message);
    }
    handshake = null;
    label = shown;
    say("token" in opts.who ? { auth: { token: opts.who.token } } : { pair: { code: opts.who.code, name: opts.who.name } });
  }

  function heard(msg: any) {
    if (!ready) {
      if (msg?.refused) return end("refused", String(msg.refused.reason || ""));
      if (msg?.paired || msg?.authed) {
        ready = true;
        clearTimeout(waiting);
        const p = msg.paired;
        return handlers.onopen(p ? { label, paired: { token: String(p.token), host: String(p.host || ""), id: String(p.id || "") } } : { label });
      }
      return end("gone", "unexpected answer");
    }
    if (msg?.res) {
      const r = msg.res;
      const p = pending.get(r.id);
      if (!p) return;
      pending.delete(r.id);
      const body = E2E.fromB64url(r.body || "") || new Uint8Array();
      p.resolve(new Response(body as BodyInit, { status: r.status || 500, headers: { "Content-Type": r.type || "application/octet-stream" } }));
      return;
    }
    handlers.onmessage(msg);
  }

  async function frame(bytes: Uint8Array) {
    const f = E2E.decodeFrame(bytes);
    if (!f || ended) return;
    if (f.type === E2E.HELLO) return hello(f.payload);
    if (f.type !== E2E.DATA || !session) return;
    let msg: unknown;
    try {
      msg = JSON.parse(E2E.text(await session.open(f.payload)));
    } catch (e) {
      return end("untrusted", (e as Error).message);
    }
    heard(msg);
  }

  ws.addEventListener("open", () => {
    void (async () => {
      try {
        await ensureCrypto();
        handshake = await E2E.begin();
        if (!ended) ws.send(E2E.encodeFrame(E2E.HELLO, handshake.hello));
      } catch {
        end("gone", "could not start the handshake");
      }
    })();
  });
  ws.addEventListener("message", (ev) => {
    const data = (ev as MessageEvent).data;
    if (typeof data === "string") return;
    reading = reading.then(() => frame(new Uint8Array(data as ArrayBuffer))).catch((e) => end("gone", (e as Error).message));
  });
  ws.addEventListener("close", () => {
    // What was still being read may hold the refusal: it goes first.
    reading = reading.then(() => end("gone", "closed"));
  });
  ws.addEventListener("error", () => ws.close());

  return {
    send(obj) {
      if (ready && !ended) say(obj);
    },
    fetch(path, init = {}) {
      if (ended || !ready) return Promise.reject(new Error("not connected"));
      const id = nextReq++;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        say({ req: { id, method: init.method || "GET", path, body: init.body == null ? null : String(init.body) } });
      });
    },
    close() {
      if (ended) return;
      ended = true;
      clearTimeout(waiting);
      for (const p of pending.values()) p.reject(new Error("link closed"));
      pending.clear();
      try {
        ws.close();
      } catch {
        // Already closed.
      }
    },
    get ready() {
      return ready && !ended;
    },
  };
}
