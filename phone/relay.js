// The way to the computer through the relay (relay/ and src/phone/relay.rs).
// The page asks the host whether it is a relay (`/_relay`); if so, everything
// goes through one WebSocket to it, sealed with E2E (e2e.js) so the relay only
// passes bytes along. Inside the channel the phone speaks the same JSON it
// would on a socket of its own, plus `{req}` for what it would ask over HTTP;
// the computer answers `{res}` in pieces, and sends any other message too big
// for one relay frame as `{part}` pieces. Loaded before app.js, which uses
// `RELAY.detect()` and the link it gives back.
"use strict";

const RELAY = (() => {
  const CREDS_KEY = "arakun.relay";
  /// This page's own name to the relay. It stays the same across reconnects,
  /// so a socket left behind by a network switch can be replaced, and differs
  /// between tabs, even duplicated ones, so live tabs never replace each other.
  const PAGE = Array.from(crypto.getRandomValues(new Uint8Array(12)), (b) => b.toString(16).padStart(2, "0")).join("");
  const { HELLO, DATA, CONTROL, utf8, text } = E2E;
  /// The page pings the relay this often, and the relay answers by itself
  /// (relay.ts), without waking up or counting a frame. A link that hears
  /// nothing for two pings and some is dead, though the socket may not know
  /// it yet; the relay closes ours after as long without a ping.
  const PING_MS = 30_000;
  const SILENT_MS = 75_000;
  /// How long the computer may take to answer a hello. The relay can't tell
  /// that the computer's socket died, and keeps passing the hello into it.
  const HELLO_MS = 15_000;

  /// What the computer gave this phone when it paired, kept on the phone: in
  /// localStorage and in a cookie, since an iOS home-screen app gets storage of
  /// its own but inherits Safari's cookies (as the token does off the relay).
  function creds() {
    const ok = (c) => (c && c.id && c.ticket && c.token ? c : null);
    const parse = (s) => {
      try {
        return ok(JSON.parse(s || "null"));
      } catch {
        return null;
      }
    };
    const m = document.cookie.match(/(?:^|; )arakun\.relay=([^;]*)/);
    const cookie = m ? parse(decodeURIComponent(m[1])) : null;
    const stored = parse(localStorage.getItem(CREDS_KEY));
    // Paired before the cookie existed, or in a home-screen app: fill the other in.
    if (stored && !cookie) keep(stored);
    if (!stored && cookie) localStorage.setItem(CREDS_KEY, JSON.stringify(cookie));
    return stored || cookie;
  }

  function keep(c) {
    if (c) localStorage.setItem(CREDS_KEY, JSON.stringify(c));
    else localStorage.removeItem(CREDS_KEY);
    if (location.protocol !== "https:") return;
    const age = c ? 400 * 24 * 3600 : 0;
    document.cookie = `arakun.relay=${encodeURIComponent(c ? JSON.stringify(c) : "")}; Path=/; Max-Age=${age}; Secure; SameSite=Strict`;
  }

  /// Whether this host is a relay: its label, or null for a server of the
  /// computer's own. A relay that is closed or has blocked this address says so.
  async function detect() {
    let res;
    try {
      res = await fetch("/_relay", { cache: "no-store" });
    } catch {
      return null;
    }
    if (res.status === 404) return null;
    let info = null;
    try {
      info = await res.json();
    } catch {
      return null;
    }
    if (!info || typeof info.label !== "string") return null;
    const closed = res.status === 503 ? info.disabled || "The relay is closed right now." : res.status === 403 ? "The relay has blocked this address." : null;
    return { label: info.label, closed };
  }

  /// Why a channel ended, in the page's terms, from what the relay or the
  /// computer said: `paired` when the credentials are no longer good,
  /// `untrusted` when the computer behind the address is not the one expected,
  /// `pairing` when the link to pair with did not work, `limit` when the relay
  /// has had enough for now, `gone` otherwise (try again).
  function kindOf(reason, code) {
    if (code === 4401 || ["unauthorized", "revoked", "bad_ticket", "unpaired", "wrong_secret"].includes(reason)) return "paired";
    if (reason === "no_pairing" || reason === "bad_code" || reason === "no_credentials") return "pairing";
    if (reason === "untrusted" || reason === "bad_hello") return "untrusted";
    if (code === 4429 || /^limit:/.test(reason)) return "limit";
    return "gone";
  }

  /// Joins the `{part: {body, more}}` pieces of a message too big for one
  /// relay frame: given each piece in turn, it gives back the whole message
  /// with the last one and null before that.
  function joiner() {
    let parts = [];
    return (part) => {
      parts.push(E2E.fromB64url(part.body || "") || new Uint8Array());
      if (part.more) return null;
      const whole = join(parts);
      parts = [];
      return JSON.parse(text(whole));
    };
  }

  function join(parts) {
    const whole = new Uint8Array(parts.reduce((n, b) => n + b.length, 0));
    let at = 0;
    for (const b of parts) {
      whole.set(b, at);
      at += b.length;
    }
    return whole;
  }

  /// One connection to the computer through the relay. `with` is either
  /// `{ creds }` or `{ pairing: { code, secret, name } }`; the handlers hear
  /// how it goes. The link sends messages, asks `req`s and closes.
  function open(label, with_, handlers) {
    const query = with_.creds
      ? `d=${encodeURIComponent(with_.creds.id)}&t=${encodeURIComponent(with_.creds.ticket)}&s=${PAGE}`
      : `p=${encodeURIComponent(with_.pairing.code)}`;
    // Plain `ws` only ever reaches a relay run by hand (`wrangler dev`).
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws?${query}`);
    ws.binaryType = "arraybuffer";
    let channel = 0;
    // What the next hello proves: the pairing secret works once, so after it
    // pays off the credentials it gave take over for every later hello.
    let creds = with_.creds || null;
    let handshake = null;
    let session = null;
    let ready = false;
    let ended = false;
    let nextReq = 1;
    let joined = joiner();
    const pending = new Map(); // req id → { resolve, reject, status, type, parts }
    // Frames must be opened and sealed in the order they come and go: the
    // counters say so. Each direction waits its turn.
    let reading = Promise.resolve();
    let writing = Promise.resolve();
    // Wall-clock time: a phone that slept counts the time it slept.
    let heard = Date.now();
    let pinger = 0;
    let waiting = 0; // the handshake's deadline, while one is running

    const quiet = () => {
      clearInterval(pinger);
      clearTimeout(waiting);
    };
    /// Gives up unless the computer has answered within HELLO_MS; armed from
    /// the start, so a relay that never says `open` is given up on too.
    const expect = () => {
      clearTimeout(waiting);
      waiting = setTimeout(() => end("gone", "no answer from the computer"), HELLO_MS);
    };
    expect();

    const end = (kind, detail) => {
      if (ended) return;
      ended = true;
      quiet();
      for (const p of pending.values()) p.reject(new Error("link closed"));
      pending.clear();
      try {
        ws.close();
      } catch {
        // Already closed.
      }
      handlers.onclose({ kind, detail });
    };

    const say = (obj) => {
      writing = writing
        .then(async () => {
          if (ended || !session) return;
          ws.send(E2E.encodeFrame(DATA, await session.seal(utf8(JSON.stringify(obj)))));
        })
        .catch(() => end("gone", "could not send"));
    };

    const greet = async () => {
      session = null;
      ready = false;
      // Pieces from before belong to a message that will not be finished.
      joined = joiner();
      handshake = await E2E.begin();
      ws.send(E2E.encodeFrame(HELLO, handshake.hello));
      expect();
    };

    const control = async (msg) => {
      switch (msg.type) {
        case "open":
          channel = msg.channel;
          await greet();
          break;
        case "pc_offline":
          // Nobody to answer until `pc_online`: no deadline meanwhile.
          clearTimeout(waiting);
          session = null;
          ready = false;
          for (const p of pending.values()) p.reject(new Error("computer offline"));
          pending.clear();
          handlers.onstatus({ pc: false, since: msg.since || null });
          break;
        case "pc_online":
          handlers.onstatus({ pc: true });
          // The computer is new to this channel: it waits for a hello.
          await greet();
          break;
        case "bye":
          end(kindOf(String(msg.reason || ""), 0), msg.reason);
          break;
        case "limit":
          end("limit", msg.which);
          break;
      }
    };

    const sealed = async (payload) => {
      if (!session) return;
      let msg;
      try {
        msg = JSON.parse(text(await session.open(payload)));
        if (msg.part) msg = joined(msg.part);
      } catch (e) {
        return end("untrusted", e.message);
      }
      if (!msg) return; // more pieces to come
      if (msg.paired) {
        creds = { id: msg.paired.id, ticket: msg.paired.ticket, token: msg.paired.token };
        keep(creds);
        ready = true;
        clearTimeout(waiting);
        handlers.onopen({ id: msg.paired.id, name: msg.paired.name, host: msg.paired.host, paired: true });
      } else if (msg.ok) {
        ready = true;
        clearTimeout(waiting);
        handlers.onopen({ id: msg.ok.id, name: msg.ok.name, host: msg.ok.host, paired: false });
      } else if (msg.res) {
        const r = msg.res;
        const p = pending.get(r.id);
        if (!p) return;
        if (r.status != null) p.status = r.status;
        if (r.type != null) p.type = r.type;
        p.parts.push(E2E.fromB64url(r.body || "") || new Uint8Array());
        if (!r.more) {
          pending.delete(r.id);
          p.resolve(new Response(join(p.parts), { status: p.status || 500, headers: { "Content-Type": p.type || "application/octet-stream" } }));
        }
      } else {
        handlers.onmessage(msg);
      }
    };

    const frame = async (bytes) => {
      const f = E2E.decodeFrame(bytes);
      if (!f || ended) return;
      if (f.type === CONTROL) {
        let msg;
        try {
          msg = JSON.parse(text(f.payload));
        } catch {
          return;
        }
        return control(msg);
      }
      if (f.type === HELLO) {
        if (!handshake) return;
        try {
          session = await handshake.finish(f.payload, label, channel);
        } catch (e) {
          return end("untrusted", e.message);
        }
        handshake = null;
        say(creds ? { auth: { token: creds.token } } : { pair: { secret: with_.pairing.secret, name: with_.pairing.name } });
        return;
      }
      return sealed(f.payload);
    };

    ws.onopen = () => {
      const ping = () => {
        if (Date.now() - heard > SILENT_MS) return end("gone", "the relay went quiet");
        try {
          ws.send("ping");
        } catch {
          // Closing; onclose takes it from here.
        }
      };
      ping();
      pinger = setInterval(ping, PING_MS);
    };
    ws.onmessage = (ev) => {
      heard = Date.now();
      // The relay's "pong": it only says it is there.
      if (typeof ev.data === "string") return;
      reading = reading.then(() => frame(new Uint8Array(ev.data))).catch((e) => end("gone", e.message));
    };
    ws.onclose = (ev) => end(kindOf(ev.reason || "", ev.code), ev.reason || `closed (${ev.code})`);
    ws.onerror = () => ws.close();

    return {
      /// A message to the computer, as on a socket of our own.
      send(obj) {
        if (ready) say(obj);
      },
      /// What would have been an HTTP request: a Response comes back.
      fetch(path, init = {}) {
        if (ended || !ready) return Promise.reject(new Error("not connected"));
        const id = nextReq++;
        return new Promise((resolve, reject) => {
          pending.set(id, { resolve, reject, status: 0, type: "", parts: [] });
          say({ req: { id, method: init.method || "GET", path, body: init.body == null ? null : String(init.body) } });
        });
      },
      /// Hangs up without a word; the handlers hear nothing more.
      close() {
        if (ended) return;
        ended = true;
        quiet();
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

  return { detect, open, creds, keep, joiner };
})();

globalThis.RELAY = RELAY;
