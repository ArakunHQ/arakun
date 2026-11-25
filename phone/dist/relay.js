// The way to the computer through the relay (cloud/src/machines.ts,
// cloud/src/relay/ and core/src/phone/relay.rs). On the account host the
// person signs in with GitHub and picks one of their computers; everything
// then goes through one WebSocket to the relay, sealed with E2E (e2e.js) so
// the relay only passes bytes along. Inside the channel the phone speaks the
// same JSON it would on a socket of its own, plus `{req}` for what it would
// ask over HTTP; the computer answers `{res}` in pieces, and sends any other
// message too big for one relay frame as `{part}` pieces. Loaded before
// app.js, which uses `RELAY.detect()`, `RELAY.boot()` and the link `open` gives.
"use strict";

const RELAY = (() => {
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
  /// An old relay host is `<label>.<base>`: a label is 16 base32 letters.
  const OLD_HOST = /^[a-z2-7]{16}\.(.+)$/;

  /// Before sign-in, a phone paired through the relay kept a ticket and a
  /// token here and in a cookie. The account's session replaces them both.
  function forgetOldPairing() {
    localStorage.removeItem("arakun.relay");
    if (location.protocol === "https:") document.cookie = "arakun.relay=; Path=/; Max-Age=0; Secure; SameSite=Strict";
  }

  /// The account host an old relay host named in its `/_relay` answer.
  let namedHost = "";

  /// A host name as the account host's setting has it, and nothing that could
  /// leave the link the page builds from it.
  function hostName(value) {
    return typeof value === "string" && /^[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?(:\d{1,5})?$/.test(value) ? value : "";
  }

  /// Which way in this host is: `account` for the account host (it answers
  /// `/auth/config`), `update` for an old relay host (its `/_relay` says 410),
  /// null for a server of the computer's own, which answers neither.
  /// `unreachable` when `/auth/config` failed the way only the account host or
  /// what stands before it does (429 or 5xx): the computer's own server
  /// answers 404 there, so a failure is never taken for it, and the page does
  /// not ask for a pairing code where there is nothing to pair with.
  async function detect() {
    const answers = async (path) => {
      try {
        return await fetch(path, { cache: "no-store" });
      } catch {
        return null;
      }
    };
    const config = await answers("/auth/config");
    if (config && config.ok) {
      try {
        const body = await config.json();
        if (body && "github_client_id" in body) return "account";
      } catch {
        // Not the account host's JSON.
      }
    }
    if (config && (config.status === 429 || config.status >= 500)) return "unreachable";
    const old = await answers("/_relay");
    if (old && old.status === 410) {
      try {
        namedHost = hostName((await old.json()).account_host);
      } catch {
        // An older Worker names none.
        namedHost = "";
      }
      return "update";
    }
    return null;
  }

  /// What the page says when the host failed before saying which way in it is.
  function unreachable() {
    return `Could not reach ${location.host}. Check the connection and try again.`;
  }

  /// Where the account host is: on an old relay host, the one its `/_relay`
  /// named, else `app.` and its base (a Worker from before it named one).
  function accountHost() {
    const m = OLD_HOST.exec(location.hostname);
    return m ? namedHost || `app.${m[1]}` : location.host;
  }

  /// Whether `/auth/logout` ended the session, from its status: 204 says it
  /// did, 401 that it was gone already. Anything else (no answer at all is 0,
  /// a captive portal's 200, a 5xx) leaves the cookie standing: the page
  /// cannot drop it itself, so it must not show itself as signed out.
  function loggedOut(status) {
    return status === 204 || status === 401;
  }

  /// What the page says when signing out did not reach the account host.
  function signOutFailed() {
    return `Signing out did not reach ${location.host}, so this phone is still signed in. Check the connection and try again.`;
  }

  /// Where a tap on a notification leads, from where the page is. `screen` is
  /// the page's, `account` what it knows on the account host (null on the
  /// computer's own server), `picked` whether a computer is picked, `last` the
  /// label of the computer opened last. `open`: the session, on the computer
  /// there is. `pick`: connect to `machine` first, then the session. `list`:
  /// the computers, with `tapSentence`. `stay`: nothing to open from here
  /// (signed out, blocked, the old host, a host that did not answer, or the
  /// `fatal` screen of an address that cannot be used).
  function tapRoute(screen, account, picked, last) {
    if (screen === "update" || screen === "signin" || screen === "fatal") return { go: "stay" };
    if (!account || picked) return { go: "open" };
    // With no computers the list already says how to get one.
    if (!account.me || account.blocked || !account.machines.length) return { go: "stay" };
    const machine = last ? account.machines.find((m) => m.label === last) : null;
    return machine ? { go: "pick", machine } : { go: "list" };
  }

  /// What the list says after a tap it could not follow to a computer.
  function tapSentence() {
    return "Pick the computer that notification came from to open its session.";
  }

  /// A JSON answer from the account host. The session cookie rides along.
  async function ask(path) {
    const res = await fetch(path, { cache: "no-store", credentials: "same-origin" });
    let body = null;
    try {
      body = await res.json();
    } catch {
      // No body, or not JSON.
    }
    return { status: res.status, body };
  }

  /// The account's computers, or `{ signedOut }` or `{ blocked }` when the
  /// account host will not list them. Throws when the host does not answer.
  async function machines() {
    const r = await ask("/machines");
    if (r.status === 401) return { signedOut: true };
    if (r.status === 403 && r.body && r.body.error === "blocked") return { blocked: true, reason: r.body.reason || null };
    if (r.status !== 200 || !Array.isArray(r.body)) throw new Error(`the account host answered ${r.status}`);
    return { list: r.body };
  }

  /// Asks the account host to change something: `{ ok }`, or `{ signedOut }`.
  /// The browser sends Origin with it, which the cookie session needs.
  async function act(method, path) {
    const res = await fetch(path, { method, cache: "no-store", credentials: "same-origin" });
    if (res.status === 401) return { signedOut: true };
    // Gone already counts as done.
    return { ok: res.ok || res.status === 404, status: res.status };
  }

  /// The path that removes one of the account's computers. A label is 16
  /// base32 letters; anything else could reach another route.
  function machinePath(label) {
    if (!/^[a-z2-7]{16}$/.test(String(label))) throw new Error("not a label");
    return `/machines/${label}`;
  }

  /// Removes one of the account's computers.
  function forgetMachine(label) {
    return act("DELETE", machinePath(label));
  }

  /// What Remove asks first, naming the computer. One still signed in comes
  /// back when it next connects, so that is said too.
  function removeQuestion(name) {
    return `Remove ${name} from your account?\n\nIf it is still signed in, it comes back the next time it connects.`;
  }

  /// The account's signed-in devices, or `{ signedOut }`. Throws when the
  /// account host does not answer with a list.
  async function sessions() {
    const r = await ask("/sessions");
    if (r.status === 401) return { signedOut: true };
    if (r.status !== 200 || !Array.isArray(r.body)) throw new Error(`the account host answered ${r.status}`);
    return { list: r.body };
  }

  /// Signs one of the account's devices out. An id is 32 hex digits.
  function signOutSession(id) {
    if (!/^[0-9a-f]{32}$/.test(String(id))) throw new Error("not a session id");
    return act("DELETE", `/sessions/${id}`);
  }

  /// What a signed-in device is, in a word or two.
  function kindText(kind) {
    return { app: "Arakun app", server: "Server", phone: "Phone page", ios: "iPhone app", android: "Android app" }[kind] || "Device";
  }

  /// The rows of the signed-in devices list. Every value from the account
  /// host goes through `esc`; ids only as data attributes, after a check.
  /// `ago` says how long ago a Unix time was.
  function signedInRows(list, esc, ago) {
    return list.map((s) => {
      const used = s.last_used_at ? ago(s.last_used_at) : "";
      const when = used === "now" ? "used now" : used ? `used ${used} ago` : "";
      const sub = [kindText(s.kind), when].filter(Boolean).join(" · ");
      const action = s.current
        ? '<span class="badge idle">This phone</span>'
        : /^[0-9a-f]{32}$/.test(String(s.id))
          ? `<button class="btn ghost small" data-signout="${esc(s.id)}">Sign out</button>`
          : "";
      return `<div class="prow"><span class="rbody"><span class="rt">${esc(s.name)}</span><span class="rs">${esc(sub)}</span></span>${action}</div>`;
    });
  }

  /// What the page shows first on the account host: `signin`, `blocked`,
  /// `none` (no computers yet), `connect` (the only one) or `list`.
  async function boot() {
    const r = await ask("/me");
    if (r.status === 401) return { screen: "signin" };
    if (r.status !== 200 || !r.body) throw new Error(`the account host answered ${r.status}`);
    const me = r.body;
    const m = await machines();
    if (m.signedOut) return { screen: "signin" };
    if (m.blocked) return { screen: "blocked", reason: m.reason, me };
    const list = m.list;
    const screen = list.length === 0 ? "none" : list.length === 1 ? "connect" : "list";
    return { screen, me, machines: list, machine: list.length === 1 ? list[0] : null };
  }

  /// A computer's socket on the relay, as this page.
  function socketUrl(label) {
    // Plain `ws` only ever reaches a relay run by hand (`wrangler dev`).
    const proto = location.protocol === "https:" ? "wss" : "ws";
    return `${proto}://${location.host}/machines/${encodeURIComponent(label)}/ws?page=${PAGE}`;
  }

  /// Why a channel ended, in the page's terms, from what the relay or the
  /// computer said: `signed_out` when the session is gone, `refused` when the
  /// relay will not let this page in (`sentence` says why), `untrusted` when
  /// the computer behind the address is not the one expected, `limit` when the
  /// relay has had enough for now, `gone` otherwise (try again).
  function kindOf(reason, code) {
    if (reason === "signed_out" || (code === 4401 && !reason)) return "signed_out";
    // `unpaired`: the computer's user removed this device there. Dialing
    // again would undo that, so it is a refusal, not a drop.
    if (["blocked", "not_yours", "no_machine", "update", "disabled", "unpaired"].includes(reason)) return "refused";
    if (reason === "untrusted" || reason === "bad_hello") return "untrusted";
    if (code === 4429 || /^limit:/.test(reason)) return "limit";
    return "gone";
  }

  /// A refusal as the page says it. `message` is the operator's text, for `disabled`.
  function sentence(reason, message) {
    switch (reason) {
      case "blocked":
        return "This GitHub account is blocked from the relay.";
      case "not_yours":
        return "That computer is signed in to another GitHub account.";
      case "no_machine":
        return "That computer is no longer on your account. Sign in again in Arakun on it.";
      case "unpaired":
        return "That computer removed this phone. Pick it again to connect.";
      case "update":
        return `Update Arakun on the computer and sign in at ${accountHost()}`;
      case "disabled":
        return message || "The relay is closed right now.";
      default:
        return "The relay closed the connection.";
    }
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

  /// One connection through the relay to `machine`, one of the account's
  /// computers as `/machines` lists it: `{ label, pub }`. The session cookie
  /// lets the socket in; the handlers hear how it goes. The link sends
  /// messages, asks `req`s and closes.
  function open(machine, handlers) {
    const { label } = machine;
    const ws = new WebSocket(socketUrl(label));
    ws.binaryType = "arraybuffer";
    let channel = 0;
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

    /// Ends with what the relay said: a refusal as its sentence, else the reason.
    const refused = (reason, code, message) => {
      const kind = kindOf(reason, code);
      end(kind, kind === "refused" ? sentence(reason, message) : reason || `closed (${code})`);
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
          refused(String(msg.reason || ""), 0, msg.message);
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
      if (msg.res) {
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
          // The key the account has for this computer, and the label it
          // derives, which the hello's signature covers.
          if (!sameKey(f.payload, machine.pub)) throw new Error("bad hello: not the account's key");
          session = await handshake.finish(f.payload, label, channel);
        } catch (e) {
          return end("untrusted", e.message);
        }
        handshake = null;
        // The relay let only this account in, so the channel is ready now:
        // nothing more to prove inside it.
        ready = true;
        clearTimeout(waiting);
        handlers.onopen({});
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
    ws.onclose = (ev) => refused(ev.reason || "", ev.code);
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
        // Whoever waits on an answer hears there will be none.
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

  /// A notice's link, only when it is https; anything else stays text.
  function noticeLink(url) {
    return typeof url === "string" && /^https:\/\/[^/]/.test(url) ? url : null;
  }

  /// The sentence the Notifications row adds when the account has several
  /// computers. The page has one origin, so the browser keeps one push
  /// subscription, made with the key of the computer opened last.
  function pushNote(machines, current) {
    return machines.length > 1 && current ? `Notifications come only from the computer opened last, ${current}.` : "";
  }

  /// A notification tap names a session the computer opened now does not have.
  function noticeElsewhere() {
    return "That notification is from another computer. Open it from All computers in the Phone tab.";
  }

  /// A localStorage key kept per computer on the account host: `label` is the
  /// computer's, or null on the computer's own server, where keys stay as they
  /// were. A key that ends in `:` takes an id after it.
  function scoped(key, label) {
    if (!label) return key;
    return key.endsWith(":") ? `${key.slice(0, -1)}@${label}:` : `${key}@${label}`;
  }

  /// What a message from the computer says about the session a notification
  /// tap asked for (`pending`, "" when none): `elsewhere` when the computer
  /// refused it (its answer to an open is an error with no id), `opened` when
  /// the session came, `unrelated` otherwise. Once it has opened, the page
  /// stops waiting, so a later error is unrelated.
  function noticeOutcome(pending, msg) {
    if (!pending || !msg) return "unrelated";
    if (msg.type === "error") return "elsewhere";
    if ((msg.type === "session" || msg.type === "entries") && msg.id === pending) return "opened";
    return "unrelated";
  }

  /// What signing out drops: the computer opened last, and the drafts,
  /// places and shortcuts kept per computer.
  function forgetAccountState(storage) {
    const doomed = [];
    for (let i = 0; i < storage.length; i++) {
      const k = storage.key(i);
      if (k === "arakun.machine" || /^arakun\.(draft|place|shortcuts)@[a-z2-7]{16}(:|$)/.test(k)) doomed.push(k);
    }
    for (const k of doomed) storage.removeItem(k);
  }

  /// Whether a computer's hello names the public key `pub` (base64url).
  function sameKey(hello, pub) {
    let theirs;
    try {
      theirs = E2E.fromB64url(JSON.parse(text(hello)).pub);
    } catch {
      return false;
    }
    const ours = E2E.fromB64url(pub || "");
    return !!theirs && !!ours && theirs.length === ours.length && theirs.every((b, i) => b === ours[i]);
  }

  return {
    detect,
    boot,
    machines,
    forgetMachine,
    machinePath,
    removeQuestion,
    sessions,
    signOutSession,
    signedInRows,
    accountHost,
    unreachable,
    loggedOut,
    signOutFailed,
    tapRoute,
    tapSentence,
    socketUrl,
    kindOf,
    sentence,
    forgetOldPairing,
    noticeLink,
    pushNote,
    noticeElsewhere,
    noticeOutcome,
    scoped,
    forgetAccountState,
    open,
    joiner,
  };
})();

globalThis.RELAY = RELAY;
