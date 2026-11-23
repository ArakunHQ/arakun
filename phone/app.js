// Arakun on the phone: the inbox of sessions and one session live. No bundler,
// no framework; the list of entries is virtual so a long session stays light.
// The server (src/phone) sends a snapshot on connect and deltas from then on.
"use strict";

// ---- pairing and connection ------------------------------------------------

// The pairing code arrives in the QR link (?p=); it is traded for a token that
// stays on this phone. The code itself is never kept: it works once.
const PAIR_CODE = (() => {
  const q = new URLSearchParams(location.search).get("p");
  if (q) history.replaceState(null, "", location.pathname);
  return q || "";
})();

// On the account host, GitHub's sign-in comes back to `/?sign_in=failed` when
// it did not work. The address bar keeps only the page.
const SIGN_IN_FAILED = (() => {
  const q = new URLSearchParams(location.search).get("sign_in");
  if (q) history.replaceState(null, "", location.pathname + location.hash);
  return q === "failed";
})();

const TOKEN_KEY = "arakun.token";
// The token lives in localStorage and, over https, in a cookie too: an iOS
// home-screen app gets its own storage but inherits Safari's cookies, so it
// starts paired instead of asking for a new code.
function cookieToken() {
  const m = document.cookie.match(/(?:^|; )arakun\.token=([^;]*)/);
  return m ? decodeURIComponent(m[1]) : "";
}
function keepToken(t) {
  if (t) localStorage.setItem(TOKEN_KEY, t);
  else localStorage.removeItem(TOKEN_KEY);
  if (location.protocol !== "https:") return;
  const age = t ? 400 * 24 * 3600 : 0;
  document.cookie = `arakun.token=${encodeURIComponent(t)}; Path=/; Max-Age=${age}; Secure; SameSite=Strict`;
}
let token = localStorage.getItem(TOKEN_KEY) || cookieToken();
if (token && (!localStorage.getItem(TOKEN_KEY) || !cookieToken())) keepToken(token);

const state = {
  connected: false,
  host: "",
  inbox: [],
  places: [], // where a session can start: [{ path, project, env, agent }]
  screen: "inbox", // inbox | sessions | session | usage | phone | pair | new | signin | machines
  start: null, // the New session form: { path, agent, text, sent }
  session: null, // { id, title, agent, status, count, from, entries, permission, usage, elapsed_ms, place }
  query: "", // what Sessions is searching for
  project: "", // the project Sessions is narrowed to, or "" for all
  stopArmed: 0, // when Stop was tapped once; the second tap within a few seconds stops
  usage: null, // both agents' plan limits: { agents: [{ agent, name, signed_in, plan, rows, idle, note, tone, at }] }
  pushKey: "", // the computer's VAPID public key, which this phone subscribes with
  me: null, // this phone as the computer keeps it: { id, name, paired_at, notifying, notices: { done, input, error } }
  draft: "",
  shots: [], // photos waiting in the composer: [{ name, b64, thumb }]
  pending: [], // sent and not shown by the computer yet: [{ text, after }], `after` the entry count when sent
  loadingOlder: false,
  pairing: false,
  pairError: "",
  ask: null, // what is picked and typed for the pending question: { id, picked: [[i…]…], typed: [""…] }
  sheet: null, // over the session: { kind: "changes", turns } or { kind: "diff", entry, index, name, diff }
  pcOffline: null, // through the relay: since when (unix) the computer has been away, while it is
  fatal: "", // why this address cannot be used at all; nothing is tried again
};

// How this page reaches the computer: a socket of the computer's own, or a
// link through the relay (`relay` names it). Either way `link` sends and
// fetches; `api` is what would be an HTTP request to the computer.
let relay = null; // on the account host, the computer picked: { label, pub, name, kind, online, last_seen_at }
// On the account host: who is signed in, their computers, and a sentence for
// the list (why the last connection ended). Null on the computer's own server.
let account = null; // { me, machines, said, loaded, blocked }
let link = null;
let retry = 500;
let retryTimer = 0; // the next attempt while the computer is not there
// The newest local attempt. One that Retry overtook while it waited on `/me`
// must not open a second socket or call the current one lost.
let attempt = 0;
let attemptAbort = null;

// A socket of the computer's own is pinged this often and given up on after
// this long without a word, or when it does not open in time. A dead network
// can leave it looking open for minutes, and closing it can take as long, so
// the page lets go of it and tries again at once.
const LOCAL_PING_MS = 15_000;
const LOCAL_SILENT_MS = 40_000;
const LOCAL_OPEN_MS = 15_000;

function openLocal() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${proto}://${location.host}/ws?t=${encodeURIComponent(token)}`);
  // Wall-clock time: a phone that slept counts the time it slept.
  let heard = Date.now();
  let pinger = 0;
  const quiet = () => {
    clearInterval(pinger);
    clearTimeout(opening);
  };
  const dead = () => {
    quiet();
    ws.onclose = null;
    ws.close();
    if (link && link.ws === ws) lost();
  };
  const opening = setTimeout(dead, LOCAL_OPEN_MS);
  ws.addEventListener("open", () => {
    clearTimeout(opening);
    pinger = setInterval(() => {
      if (Date.now() - heard > LOCAL_SILENT_MS) return dead();
      if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "ping" }));
    }, LOCAL_PING_MS);
  });
  ws.addEventListener("message", () => {
    heard = Date.now();
  });
  ws.addEventListener("close", quiet);
  return {
    ws,
    send(obj) {
      if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
    },
    close() {
      quiet();
      ws.onclose = null;
      ws.close();
    },
  };
}

function api(path, init = {}) {
  if (relay) return link && link.ready ? link.fetch(path, init) : Promise.reject(new Error("not connected"));
  const headers = { ...(init.headers || {}) };
  if (token) headers.Authorization = `Bearer ${token}`;
  return fetch(path, { ...init, headers });
}

// Whether this phone holds something the computer gave it when it paired, or
// on the account host, whether a computer is picked.
function paired() {
  return account ? !!relay : !!token;
}

// What the computer will call this phone.
function deviceName() {
  const ua = navigator.userAgent;
  if (/iPhone/.test(ua)) return "iPhone";
  // iPad Safari calls itself a Mac; the touchscreen gives it away.
  if (/iPad/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1)) return "iPad";
  if (/Android/.test(ua)) return /Mobile/.test(ua) ? "Android phone" : "Android tablet";
  if (/Windows/.test(ua)) return "Browser on Windows";
  if (/Macintosh/.test(ua)) return "Browser on a Mac";
  if (/Linux/.test(ua)) return "Browser on Linux";
  return "Browser";
}

async function pair(code) {
  state.pairing = true;
  state.pairError = "";
  render();
  try {
    const res = await fetch("/pair", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, name: deviceName() }),
    });
    if (!res.ok) {
      state.pairError = res.status === 403 ? "That code is wrong or has expired. Make a new one in Arakun." : `Could not pair (${res.status}).`;
      return false;
    }
    const data = await res.json();
    token = data.token;
    keepToken(token);
    state.host = data.host || "";
    return true;
  } catch (e) {
    state.pairError = `Could not reach Arakun. ${location.protocol === "https:" ? "Is Tailscale on, on the phone and on the computer?" : "Is the phone on the same network?"}`;
    return false;
  } finally {
    state.pairing = false;
    render();
  }
}

function forgetToken() {
  token = "";
  keepToken("");
}

// What `/me` says about this phone, kept.
function tookMe(me) {
  state.host = me.host || state.host;
  state.pushKey = me.push_key || "";
  state.me = me;
  // Allowed here but unknown to the computer (it was paired again, or the
  // subscription was made before the computer knew): tell it.
  if (!me.notifying && notifyState() === "on") subscribePush();
}

// Asks the computer whether the token still stands before opening a socket:
// a socket refused looks the same as a computer that is off, and this tells
// the two apart.
async function connect() {
  clearTimeout(retryTimer);
  retryTimer = 0;
  if (account) return relay ? connectRelay() : render();
  if (!token) {
    state.screen = "pair";
    render();
    return;
  }
  const mine = ++attempt;
  if (attemptAbort) attemptAbort.abort();
  const abort = new AbortController();
  attemptAbort = abort;
  // A computer that went away without a word would leave this hanging.
  const timer = setTimeout(() => abort.abort(), LOCAL_OPEN_MS);
  let ok;
  try {
    const res = await api("/me", { signal: abort.signal });
    if (mine !== attempt) return;
    if (res.status === 403) {
      forgetToken();
      state.pairError = "This phone is no longer paired. Pair it again from Arakun.";
      state.screen = "pair";
      render();
      return;
    }
    ok = res.ok;
    if (ok) {
      const me = await res.json();
      if (mine !== attempt) return;
      tookMe(me);
    }
  } catch (e) {
    ok = false;
  } finally {
    clearTimeout(timer);
  }
  if (mine !== attempt) return;
  attemptAbort = null;
  if (!ok) return lost();
  link = openLocal();
  const ws = link.ws;
  ws.onopen = linked;
  ws.onmessage = (ev) => handle(JSON.parse(ev.data));
  ws.onclose = lost;
  // Its own socket: one closed by Retry can still report an error after
  // `link` has moved on to the next.
  ws.onerror = () => ws.close();
}

// The computer is there and knows us: pick up where the screen was.
function linked() {
  state.connected = true;
  state.pcOffline = null;
  retry = 500;
  if (state.screen === "pair") state.screen = "inbox";
  if (state.session) send({ type: "open", session: state.session.id });
  if (state.screen === "usage") send({ type: "usage" });
  if (document.visibilityState !== "visible") send({ type: "visible", visible: false });
  flushQueued();
  if (openAtStart && !state.session) {
    const id = openAtStart;
    openAtStart = "";
    openFromNotice(id);
    return;
  }
  render();
}

// The computer is not there: try again, waiting a little longer each time.
function lost() {
  state.connected = false;
  render();
  clearTimeout(retryTimer);
  retryTimer = setTimeout(connect, retry);
  retry = Math.min(retry * 2, 8000);
}

// Back on screen or back online: an attempt that was waiting goes now.
function reconnectNow() {
  if (!retryTimer) return;
  retry = 500;
  connect();
}

// The Retry button: start over now, also when an attempt is still under way
// and nothing is waiting to go.
function retryNow() {
  if (link) link.close();
  link = null;
  state.connected = false;
  retry = 500;
  connect();
}
window.addEventListener("online", reconnectNow);
window.addEventListener("pageshow", reconnectNow);

// Through the relay the session cookie lets the socket in, and the channel is
// the computer's client once the handshake is done; `/me` follows.
function connectRelay() {
  link = RELAY.open(relay, relayHandlers());
}

function relayHandlers() {
  return {
    onopen: async () => {
      try {
        const res = await link.fetch("/me");
        if (res.ok) tookMe(await res.json());
      } catch (e) {
        // The link closed meanwhile; onclose takes it from here.
        return;
      }
      linked();
    },
    onmessage: handle,
    onstatus: ({ pc, since }) => {
      state.connected = false;
      state.pcOffline = pc ? null : since ? Math.floor(since / 1000) : Math.floor(Date.now() / 1000);
      render();
    },
    onclose: ({ kind, detail }) => {
      link = null;
      state.connected = false;
      state.pcOffline = null;
      if (kind === "signed_out") return signedOut();
      if (kind === "refused") return leaveMachine(detail);
      if (kind === "untrusted") return leaveMachine(`${relay ? relay.name : "The computer"} did not answer with the key your account has for it. Sign in again in Arakun on that computer.`);
      if (kind === "limit") toast(`The relay is busy (${detail}). Trying again in a moment.`);
      lost();
    },
  };
}

// ---- the account host: sign in, then pick a computer --------------------------

// The computer opened last, by label: a tap on a notification opens it again.
const LAST_MACHINE = "arakun.machine";

// What the account host says first: sign in, no computers yet, the one there
// is, or the list. A host that does not answer leaves the list with Retry.
async function startAccount() {
  account = { me: null, machines: [], said: SIGN_IN_FAILED ? "Sign-in with GitHub did not work. Try again." : "", loaded: false, blocked: false };
  let first;
  try {
    first = await RELAY.boot();
  } catch (e) {
    account.said = `Could not reach ${location.host}. Check the connection and try again.`;
    state.screen = "machines";
    return render();
  }
  if (first.screen === "signin") return signedOut(account.said);
  account.loaded = true;
  if (first.screen === "blocked") {
    account.me = first.me;
    account.blocked = true;
    account.said = blockedSentence(first.reason);
    state.screen = "machines";
    return render();
  }
  account.me = first.me;
  account.machines = first.machines;
  if (first.machine) return pick(first.machine);
  // A tap on a notification names a session of the computer opened last.
  const last = openAtStart && account.machines.find((m) => m.label === localStorage.getItem(LAST_MACHINE));
  if (last) return pick(last);
  state.screen = "machines";
  render();
}

function blockedSentence(reason) {
  return RELAY.sentence("blocked") + (reason ? ` ${reason}` : "");
}

// Connects to one of the account's computers, offline ones too: the relay
// says when it comes back.
function pick(machine) {
  relay = machine;
  account.said = "";
  localStorage.setItem(LAST_MACHINE, machine.label);
  state.host = machine.name;
  state.screen = "inbox";
  retry = 500;
  render();
  connect();
}

// Back to the list of computers, with a sentence when the relay sent us there.
function leaveMachine(said = "") {
  dropMachine();
  account.said = said;
  state.screen = "machines";
  render();
  refreshMachines();
}

// Lets go of the computer picked, and of all the page knew about it.
function dropMachine() {
  clearTimeout(retryTimer);
  retryTimer = 0;
  if (link) link.close();
  link = null;
  relay = null;
  state.connected = false;
  state.pcOffline = null;
  state.session = null;
  state.pending = [];
  state.inbox = [];
  state.places = [];
  state.usage = null;
  state.me = null;
  state.pushKey = "";
  state.host = "";
  state.draft = "";
  state.shots = [];
  state.sheet = null;
  state.ask = null;
  state.start = null;
  state.query = "";
  state.project = "";
  noticeOpen = "";
}

// The session is gone (signed out here or elsewhere): only sign-in is left.
function signedOut(said = "") {
  dropMachine();
  account.me = null;
  account.machines = [];
  account.said = said;
  account.loaded = false;
  account.blocked = false;
  account.sessions = undefined;
  state.screen = "signin";
  render();
}

// The list again, for its online states.
async function refreshMachines() {
  if (!account) return;
  let got;
  try {
    got = await RELAY.machines();
  } catch (e) {
    return;
  }
  if (got.signedOut) return signedOut();
  account.blocked = !!got.blocked;
  if (got.blocked) account.said = blockedSentence(got.reason);
  else account.machines = got.list;
  account.loaded = true;
  if (state.screen === "machines") render();
}

// Removes one of the account's computers, once the person says so, then asks
// for the list again.
async function removeMachine(m) {
  if (!confirm(RELAY.removeQuestion(m.name))) return;
  let done;
  try {
    done = await RELAY.forgetMachine(m.label);
  } catch (e) {
    done = { ok: false };
  }
  if (done.signedOut) return signedOut();
  if (!done.ok) toast(`Could not remove ${m.name}`);
  if (localStorage.getItem(LAST_MACHINE) === m.label) localStorage.removeItem(LAST_MACHINE);
  refreshMachines();
}

// The account's signed-in devices, for the Phone tab: asked once each time
// the tab opens. `null` while asking, a string when it failed.
async function loadSessions() {
  if (!account) return;
  account.sessions = null;
  let got;
  try {
    got = await RELAY.sessions();
  } catch (e) {
    account.sessions = "Could not ask your account.";
    if (state.screen === "phone") render();
    return;
  }
  if (got.signedOut) return signedOut();
  account.sessions = got.list;
  if (state.screen === "phone") render();
}

// Signs another of the account's devices out, then asks for the list again.
async function signOutDevice(id, name) {
  if (!confirm(`Sign out ${name}? It needs to sign in again to reach your computers.`)) return;
  let done;
  try {
    done = await RELAY.signOutSession(id);
  } catch (e) {
    done = { ok: false };
  }
  if (done.signedOut) return signedOut();
  if (!done.ok) toast(`Could not sign out ${name}`);
  loadSessions();
}

// The list keeps its online states fresh while it is on screen.
setInterval(() => {
  if (account && state.screen === "machines" && document.visibilityState === "visible") refreshMachines();
}, 15_000);

// Signs this phone out of the account: no more notifications from the
// computer, and the session ends on the account host.
async function signOut() {
  if (!confirm(`Sign out of ${account.me ? account.me.login : "GitHub"} on this phone?`)) return;
  await stopPush();
  try {
    await fetch("/auth/logout", { method: "POST", credentials: "same-origin" });
  } catch (e) {
    // Offline: the page forgets the session here all the same.
  }
  RELAY.forgetAccountState(localStorage);
  signedOut();
}

// Takes this phone off the computer's push list and drops the browser's subscription.
async function stopPush() {
  if (link && link.ready) {
    try {
      await api("/push", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ subscription: null }) });
    } catch (e) {
      // The computer may be away.
    }
  }
  if ("serviceWorker" in navigator && isSecureContext) {
    try {
      const reg = await navigator.serviceWorker.ready;
      const sub = await reg.pushManager.getSubscription();
      if (sub) await sub.unsubscribe();
    } catch (e) {
      // Nothing to undo.
    }
  }
}

// The operator's notices for this account, until dismissed.
function noticeBanners() {
  const list = (account && account.me && account.me.notices) || [];
  return list
    .map(
      (n) => `<div class="setup" data-notice-id="${esc(n.id)}">
      <span class="sicon">${ICONS.bell}</span>
      <span class="body"><div class="t">${esc(n.text)}</div>${RELAY.noticeLink(n.url) ? `<div class="s"><a href="${esc(n.url)}" target="_blank" rel="noopener">${esc(n.url)}</a></div>` : ""}</span>
      <button class="x" data-dismiss="${esc(n.id)}" aria-label="Dismiss">${ICONS.close}</button>
    </div>`
    )
    .join("");
}

function wireNotices() {
  app.querySelectorAll("[data-dismiss]").forEach((b) =>
    b.addEventListener("click", () => {
      const id = b.dataset.dismiss;
      account.me.notices = account.me.notices.filter((n) => String(n.id) !== id);
      render();
      fetch(`/notices/${encodeURIComponent(id)}/read`, { method: "POST", credentials: "same-origin" }).catch(() => {});
    })
  );
}

function renderSignin() {
  app.innerHTML = `<div class="screen">
    <div class="pair">
      <div class="big">🦝</div>
      <h1>Sign in to reach your computers</h1>
      <p>Sign in with the GitHub account you use in Arakun on your computer.</p>
      <a class="btn" href="/auth/github/start">Sign in with GitHub</a>
      <div class="err">${esc(account.said)}</div>
      <div class="hint">${esc(location.host)} · end-to-end encrypted</div>
    </div></div>`;
}

function renderMachines() {
  const list = account.machines;
  const seen = (m) => (m.online ? "online" : m.last_seen_at ? `last seen ${ago(m.last_seen_at)}${ago(m.last_seen_at) === "now" ? "" : " ago"}` : "offline");
  // The pick and Remove sit side by side in the row: a button cannot hold another.
  const rows = list.map(
    (m) => `<div class="prow mrow"><button class="mpick" data-machine="${esc(m.label)}"><span class="ricon">${ICONS.monitor}</span>
      <span class="rbody"><span class="rt">${esc(m.name)}</span><span class="rs">${m.kind === "server" ? "Server" : "Arakun app"}</span></span>
      <span class="badge${m.online ? " idle" : ""}"><span class="dot"></span>${esc(seen(m))}</span></button>
      <button class="btn ghost small" data-remove="${esc(m.label)}" aria-label="Remove ${esc(m.name)}">Remove</button></div>`
  );
  const body = account.blocked
    ? ""
    : !account.loaded
    ? `<div class="empty"><div class="big">🦝</div><div>${esc(account.said)}</div><button class="btn outline" id="again">Try again</button></div>`
    : list.length
      ? `<section class="group"><div class="glabel">Computers</div><div class="gbox">${rows.join("")}</div></section>`
      : `<div class="empty"><div class="big">🦝</div><div>No computers yet. Sign in on a computer running Arakun.</div></div>`;
  // The bar is empty here: it only keeps the header clear of the notch.
  app.innerHTML = `<div class="screen"><div class="statusbar"></div>
    <div class="hdr"><div class="grow"><h1>Computers</h1>${account.me ? `<div class="sub">Signed in as ${esc(account.me.login)}</div>` : ""}</div>
      ${account.me ? '<button class="btn ghost" id="signout">Sign out</button>' : ""}</div>
    <div class="settings">${noticeBanners()}${account.loaded && account.said ? `<div class="err">${esc(account.said)}</div>` : ""}${body}</div></div>`;
  app.querySelectorAll("[data-machine]").forEach((b) =>
    b.addEventListener("click", () => {
      const m = account.machines.find((x) => x.label === b.dataset.machine);
      if (m) pick(m);
    })
  );
  app.querySelectorAll("[data-remove]").forEach((b) =>
    b.addEventListener("click", () => {
      const m = account.machines.find((x) => x.label === b.dataset.remove);
      if (m) removeMachine(m);
    })
  );
  const again = document.getElementById("again");
  if (again) again.addEventListener("click", () => location.reload());
  const out = document.getElementById("signout");
  if (out) out.addEventListener("click", signOut);
  wireNotices();
}

// An old relay host, `<label>.<base>`, from before sign-in.
function renderUpdate() {
  const host = RELAY.accountHost();
  app.innerHTML = `<div class="screen">
    <div class="pair">
      <div class="big">🦝</div>
      <h1>This address has moved</h1>
      <p>${esc(RELAY.sentence("update"))}</p>
      <a class="btn" href="https://${esc(host)}/">Open ${esc(host)}</a>
      <div class="hint">${esc(location.hostname)}</div>
    </div></div>`;
}

function send(msg) {
  if (link) link.send(msg);
}

// The socket stays up in the background; the computer must hear the page left
// the screen, or it would skip the push for the session shown here.
document.addEventListener("visibilitychange", () => {
  send({ type: "visible", visible: document.visibilityState === "visible" });
  if (document.visibilityState !== "visible") return;
  reconnectNow();
  if (account && state.screen === "machines") refreshMachines();
});

// iOS keeps the page its full height under the keyboard and slides it up, so the
// header and the newest lines leave the screen: the page takes the height that
// stays visible instead. A pinch zoom shrinks that height too, and is left alone.
// Measures the visible height again; a no-op where there is no visualViewport.
let fitViewport = () => {};
// Measures it again for a while, as the keyboard comes or goes.
const refit = (...delays) => delays.forEach((ms) => setTimeout(() => fitViewport(), ms));
if (window.visualViewport) {
  const vv = window.visualViewport;
  const root = document.documentElement;
  const typing = () => {
    const el = document.activeElement;
    return !!el && (el.tagName === "TEXTAREA" || (el.tagName === "INPUT" && el.type !== "checkbox") || el.isContentEditable);
  };
  // The tallest the page has been with no keyboard, for this width: iOS does not
  // always say when the keyboard is down again (after sending, say), and a
  // height kept from the keyboard would leave a gap under the composer.
  let full = 0;
  let width = 0;
  const fit = () => {
    if (Math.abs(vv.scale - 1) > 0.01) return;
    if (Math.round(vv.width) !== width) {
      width = Math.round(vv.width);
      full = 0;
    }
    let h = Math.round(vv.height);
    if (typing()) {
      // The keyboard takes more than a toolbar's worth of the window.
      root.classList.toggle("kb", window.innerHeight - vv.height > 120);
    } else {
      // No field has the focus, so no keyboard: the whole height there is.
      full = Math.max(full, h);
      h = full;
      root.classList.remove("kb");
    }
    root.style.setProperty("--vvh", `${h}px`);
    if (vv.offsetTop > 0) window.scrollTo(0, 0);
  };
  vv.addEventListener("resize", fit);
  vv.addEventListener("scroll", fit);
  // While the keyboard goes down, measure again until it is gone.
  document.addEventListener("focusout", () => refit(0, 120, 300, 600));
  document.addEventListener("focusin", () => refit(0, 300));
  fitViewport = fit;
  fit();
}

function handle(msg) {
  const tapped = RELAY.noticeOutcome(noticeOpen, msg);
  if (tapped !== "unrelated") {
    const id = noticeOpen;
    noticeOpen = "";
    if (tapped === "elsewhere" && state.session && state.session.id === id) {
      // This computer does not have the session: the notification came from another.
      state.session = null;
      state.screen = "inbox";
      render();
      toast(RELAY.noticeElsewhere());
      return;
    }
  }
  switch (msg.type) {
    case "pong":
      // Only says the computer is there (openLocal).
      break;
    case "inbox":
      state.host = msg.host;
      state.inbox = msg.sessions;
      state.places = msg.places || [];
      if (state.screen === "inbox") render();
      else if (state.screen === "sessions") fillSessions();
      if (state.screen === "usage" || state.screen === "phone" || state.screen === "sessions") syncBadge();
      else if (state.screen === "session") syncHeader();
      break;
    case "usage":
      state.usage = msg;
      if (state.screen === "usage") renderUsage();
      break;
    case "session": {
      if (!state.session || state.session.id !== msg.id) return;
      const before = state.session.entries;
      const from = state.session.from;
      // Opened again after a reconnect: the computer sends its last entries, which
      // overlap what is loaded here (older ones included).
      const overlaps = before.length > 0 && msg.from >= from && msg.from <= from + before.length;
      Object.assign(state.session, msg, { entries: msg.entries, elapsedAt: Date.now() });
      if (overlaps) {
        // Only what changed is redrawn, and the place being read stays where it is.
        const merged = before.slice(0, msg.from - from).concat(msg.entries);
        state.session.from = from;
        state.session.entries = merged;
        let at = 0;
        const n = Math.min(before.length, merged.length);
        while (at < n && JSON.stringify(before[at]) === JSON.stringify(merged[at])) at++;
        list.items = state.session.entries;
        if (at < Math.max(before.length, merged.length)) list.changed(at);
        render();
        settlePending();
      } else {
        list.reset(state.session.entries);
        render();
        settlePending();
        list.scrollToEnd();
      }
      break;
    }
    case "entries": {
      const s = state.session;
      if (!s || s.id !== msg.id) return;
      s.count = msg.count;
      if (msg.older) {
        // Fills [msg.from, s.from) before what is loaded; the list shares `s.entries`.
        const take = msg.entries.slice(0, Math.max(0, s.from - msg.from));
        s.from = msg.from;
        state.loadingOlder = false;
        list.prepend(take); // may ask for the next batch right away
        renderOlder();
      } else if (msg.from <= s.from) {
        s.from = msg.from;
        s.entries = msg.entries;
        list.reset(s.entries);
      } else {
        const at = msg.from - s.from;
        const had = s.entries.length;
        s.entries.length = Math.min(s.entries.length, at);
        for (const e of msg.entries) s.entries.push(e);
        list.changed(at);
        // Read further up while it grew: a pill says so, and goes back down.
        if (!list.atEnd && s.entries.length > had) unseen(s.entries.length - had);
      }
      settlePending();
      break;
    }
    // The agent's slash commands, for the desktop composer; the phone has none.
    case "commands":
      break;
    // Subagents with their whole transcripts: the server sends them only to an
    // app that read the session from the start, never to the phone.
    case "subagents":
      break;
    case "state": {
      const s = state.session;
      if (!s || s.id !== msg.id) return;
      const changed = JSON.stringify(s.changes) !== JSON.stringify(msg.changes);
      Object.assign(s, msg, { elapsedAt: Date.now() });
      syncHeader();
      renderPermission();
      renderQuestion();
      renderComposer();
      // The list of changes is open and the turn just added some: refresh it.
      if (changed && state.sheet && state.sheet.kind === "changes") send({ type: "changes", session: s.id });
      break;
    }
    case "changes": {
      const s = state.session;
      if (!s || s.id !== msg.id || !state.sheet || state.sheet.kind !== "changes") return;
      state.sheet.turns = msg.turns;
      renderSheet();
      break;
    }
    case "diff": {
      const s = state.session;
      const sh = state.sheet;
      if (!s || s.id !== msg.id || !sh || sh.kind !== "diff") return;
      if (!sh.diffs.some(([e, i]) => e === msg.entry && i === msg.index)) return;
      sh.got[`${msg.entry}:${msg.index}`] = msg;
      renderSheet();
      break;
    }
    case "settings":
      if (state.sheet && state.sheet.kind === "settings" && state.session && state.session.id === msg.id) {
        state.sheet.settings = msg.settings;
        renderSheet();
      }
      break;
    case "options": {
      const f = state.start;
      if (!f || f.path !== msg.env || f.agent !== msg.agent) break;
      f.options = msg.settings;
      if (f.quick) renderQuick();
      else if (state.screen === "new") renderNew();
      break;
    }
    case "started":
      state.start = null;
      renderQuick();
      // The form's step gives way to the session's: back from it goes to the inbox.
      if (layer() === "new") history.replaceState(null, "", location.pathname);
      openSession(msg.id);
      break;
    case "error":
      if (state.start && state.start.sent) {
        state.start.sent = false;
        if (state.start.quick) renderQuick();
        else if (state.screen === "new") renderNew();
      }
      toast(msg.message);
      break;
  }
}

// ---- helpers ---------------------------------------------------------------

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

function ago(unix) {
  if (!unix) return "";
  const d = Math.max(0, Date.now() / 1000 - unix);
  if (d < 60) return "now";
  if (d < 3600) return `${Math.floor(d / 60)}m`;
  if (d < 86400) return `${Math.floor(d / 3600)}h`;
  return `${Math.floor(d / 86400)}d`;
}

// The time of day of a unix moment, as the phone's clock shows it.
function clock(unix) {
  return new Date(unix * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function statusLabel(s) {
  return { working: "Working", needs_input: "Needs you", error: "Error", idle: "Done" }[s] || s;
}

const ICONS = {
  image: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="3"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="M21 15l-5-5L5 21"/></svg>',
  down: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 5v14M5 12l7 7 7-7"/></svg>',
  offline: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h.01"/><path d="M8.5 16.4a5 5 0 0 1 7 0"/><path d="M5 12.9a10 10 0 0 1 5.2-2.8"/><path d="M19 12.9a10 10 0 0 0-2.3-1.7"/><path d="M2 8.8a15 15 0 0 1 4.2-2.6"/><path d="M22 8.8A15 15 0 0 0 10.7 5"/><path d="M2 2l20 20"/></svg>',
  tune: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/></svg>',
  plus: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>',
  back: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 18l-6-6 6-6"/></svg>',
  send: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5M5 12l7-7 7 7"/></svg>',
  stop: '<svg viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="6" width="12" height="12" rx="2"/></svg>',
  refresh: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/></svg>',
  diff: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v6M9 6h6"/><path d="M9 18h6"/><path d="M14 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/></svg>',
  inbox: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="22 12 16 12 14 15 10 15 8 12 2 12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/></svg>',
  gauge: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m12 14 4-4"/><path d="M3.34 19a10 10 0 1 1 17.32 0"/></svg>',
  bell: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/><path d="M3.26 15.33A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.67C19.4 13.9 18 12.4 18 8A6 6 0 0 0 6 8c0 4.4-1.4 5.9-2.74 7.33"/></svg>',
  phone: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="20" x="5" y="2" rx="2" ry="2"/><path d="M12 18h.01"/></svg>',
  close: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18M6 6l12 12"/></svg>',
  monitor: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="20" height="14" x="2" y="3" rx="2"/><path d="M8 21h8M12 17v4"/></svg>',
  check: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>',
  help: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><path d="M12 17h.01"/></svg>',
  alert: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4M12 17h.01"/></svg>',
  unlink: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m18.84 12.25 1.72-1.71a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="m5.17 11.75-1.71 1.71a5 5 0 0 0 7.07 7.07l1.71-1.71"/><path d="M8 2v3M2 8h3M16 19v3M19 16h3"/></svg>',
  layers: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 2 2 7l10 5 10-5-10-5z"/><path d="m2 17 10 5 10-5"/><path d="m2 12 10 5 10-5"/></svg>',
  search: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>',
  folder: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>',
  claude: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round"><path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6L5.6 18.4"/></svg>',
  cloud: '<svg viewBox="0 0 24 24" fill="currentColor"><path d="M7 18.5a4.5 4.5 0 0 1-.6-8.96A6 6 0 0 1 17.8 8.6a4.2 4.2 0 0 1-.3 9.9z"/></svg>',
  branch: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="5" r="2.2"/><circle cx="6" cy="19" r="2.2"/><circle cx="18" cy="7" r="2.2"/><path d="M6 7.2v9.6M18 9.2c0 5-6 3.5-11 7.6"/></svg>',
  expand: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7"/></svg>',
  chevron: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>',
};

let toastTimer;
function toast(text) {
  let t = document.querySelector(".toast");
  if (!t) {
    t = document.createElement("div");
    t.className = "toast";
    document.body.appendChild(t);
  }
  t.textContent = text;
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), 2200);
}

// A small markdown: paragraphs, headings, lists, fenced and inline code,
// bold, italics, links. Enough for what agents write.
function md(src) {
  const lines = String(src ?? "").split("\n");
  let out = "";
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    const fence = line.match(/^\s*```(\w*)/);
    if (fence) {
      const code = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) code.push(lines[i++]);
      const closed = i < lines.length;
      i++;
      // Charts and mermaid blocks are drawn (diagrams.js), once their fence has
      // closed: a block still streaming in would redraw on every word.
      const drawn = closed && window.Diagrams ? Diagrams.render(fence[1], code.join("\n")) : null;
      out += drawn || `<pre><code>${esc(code.join("\n"))}</code></pre>`;
      continue;
    }
    const h = line.match(/^(#{1,6})\s+(.*)/);
    if (h) {
      out += `<h${Math.min(3, h[1].length)}>${inline(h[2])}</h${Math.min(3, h[1].length)}>`;
      i++;
      continue;
    }
    if (/^\s*[-*+]\s+/.test(line) || /^\s*\d+[.)]\s+/.test(line)) {
      const ordered = /^\s*\d+[.)]\s+/.test(line);
      const items = [];
      while (i < lines.length && (/^\s*[-*+]\s+/.test(lines[i]) || /^\s*\d+[.)]\s+/.test(lines[i]))) {
        let item = lines[i].replace(/^\s*([-*+]|\d+[.)])\s+/, "");
        i++;
        while (i < lines.length && /^\s{2,}\S/.test(lines[i]) && !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
          item += " " + lines[i].trim();
          i++;
        }
        items.push(`<li>${inline(item)}</li>`);
      }
      out += ordered ? `<ol>${items.join("")}</ol>` : `<ul>${items.join("")}</ul>`;
      continue;
    }
    if (/^\s*>/.test(line)) {
      const q = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) q.push(lines[i++].replace(/^\s*>\s?/, ""));
      out += `<blockquote>${md(q.join("\n"))}</blockquote>`;
      continue;
    }
    if (/^\s*\|/.test(line) && i + 1 < lines.length && /^\s*\|?\s*:?-+/.test(lines[i + 1])) {
      const rows = [];
      while (i < lines.length && /^\s*\|/.test(lines[i])) rows.push(lines[i++]);
      const raw = (r) => r.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
      // Figures line up on the right, as in a spreadsheet.
      const num = (c) => (/^[-+−]?[$€£]?\s?[\d.,]+\s?(%|ms|s|x|k|M|GB|MB|KB)?$/.test(c) ? ' class="num"' : "");
      const head = raw(rows[0]).map((c) => `<th>${inline(c)}</th>`).join("");
      // Short cells stay on one line; long ones get room to wrap into a few.
      const cls = (c) => num(c) || (c.length > 28 ? ' class="long"' : "");
      const body = rows.slice(2).map((r) => `<tr>${raw(r).map((c) => `<td${cls(c)}>${inline(c)}</td>`).join("")}</tr>`).join("");
      const n = rows.length - 2;
      const bar = `<div class="tbar"><span>${n} row${n === 1 ? "" : "s"} · ${raw(rows[0]).length} columns</span><button type="button" class="tablex" aria-label="Open the table">${ICONS.expand}<span>Open</span></button></div>`;
      out += `<div class="tablewrap">${bar}<div class="tscroll"><table><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table></div></div>`;
      continue;
    }
    if (line.trim() === "") {
      i++;
      continue;
    }
    const para = [];
    while (i < lines.length && lines[i].trim() !== "" && !/^\s*(```|#{1,6}\s|[-*+]\s|\d+[.)]\s|>|\|)/.test(lines[i])) {
      para.push(lines[i++]);
    }
    if (para.length === 0) para.push(lines[i++]);
    out += `<p>${inline(para.join("\n"))}</p>`;
  }
  return out;
}

function inline(s) {
  const codes = [];
  s = s.replace(/`([^`]+)`/g, (_, c) => `\u0000${codes.push(c) - 1}\u0000`);
  // Images out before escaping, so their URLs keep their query separators.
  const shots = [];
  s = s.replace(/!\[([^\]]*)\]\(((?:\/|https?:\/\/|data:)[^\s)]+)\)/g, (_, alt, url) => `\u0001${shots.push(picHtml(alt, url)) - 1}\u0001`);
  s = esc(s)
    .replace(/\*\*(.+?)\*\*/g, "<b>$1</b>")
    .replace(/(^|\W)_(.+?)_(?=\W|$)/g, "$1<i>$2</i>")
    .replace(/(^|\W)\*(.+?)\*(?=\W|$)/g, "$1<i>$2</i>")
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>')
    .replace(/\n/g, "<br>");
  s = s.replace(/\u0001(\d+)\u0001/g, (_, n) => shots[n]);
  return s.replace(/\u0000(\d+)\u0000/g, (_, n) => `<code>${esc(codes[n])}</code>`);
}

// ---- entry rendering -------------------------------------------------------

// "28s", "4m", "4m 12s": how long a turn took, as the computer writes it.
function fmtElapsed(ms) {
  const secs = Math.max(1, Math.round(ms / 1000));
  if (secs < 60) return `${secs}s`;
  return secs % 60 ? `${Math.floor(secs / 60)}m ${secs % 60}s` : `${secs / 60}m`;
}

// The files a turn changed: its shell edits and what its tools edited, read off
// the loaded entries after the prompt.
function turnEdits(prompt) {
  const all = state.session ? state.session.entries : [];
  const start = all.indexOf(prompt);
  if (start < 0) return null;
  const files = new Set();
  let added = 0;
  let removed = 0;
  const count = (path, a, r) => {
    files.add(path);
    added += a || 0;
    removed += r || 0;
  };
  for (const c of prompt.edits || []) count(c.path, c.added, c.removed);
  for (let i = start + 1; i < all.length && all[i].type !== "user"; i++) {
    const t = all[i];
    if (t.type !== "tool" || t.status !== "completed" || !t.diffs || !t.diffs.length) continue;
    for (const d of t.diffs) files.add(d.path);
    added += t.added || 0;
    removed += t.removed || 0;
  }
  return { files: files.size, added, removed };
}

function entryHtml(e) {
  switch (e.type) {
    case "user": {
      const meta = [];
      if (e.at) meta.push(clock(e.at));
      if (e.elapsed_ms) meta.push(`Worked for ${fmtElapsed(e.elapsed_ms)}`);
      const edits = e.elapsed_ms ? turnEdits(e) : null;
      const chip = edits && edits.files
        ? `<button class="tchip">${ICONS.diff}<span>${edits.files} file${edits.files === 1 ? "" : "s"}</span>${counts(edits.added, edits.removed)}</button>`
        : "";
      const text = e.text ? `<div class="text">${esc(e.text)}</div>` : "";
      // Fixed heights: the list measures the bubble before the pictures arrive.
      const images = e.images && e.images.length
        ? `<div class="pics ${e.images.length === 1 ? "one" : ""}">${e.images.map((i) => picHtml(i.name, i.url)).join("")}</div>`
        : "";
      const pastes = e.pastes && e.pastes.length
        ? e.pastes.map((p) => `<details class="paste"><summary>Pasted text · ${p.lines} line${p.lines === 1 ? "" : "s"}</summary><pre>${esc(p.text)}</pre></details>`).join("")
        : "";
      return `<div class="bubble">${text}${images}${pastes}</div>${meta.length || chip ? `<div class="meta"><span>${meta.join(" · ")}</span>${chip}</div>` : ""}`;
    }
    case "agent":
      return `<div class="md">${md(e.text)}</div>`;
    case "thought":
      return `<details class="tool thought"><summary><span class="ttitle">Thought</span><span class="st"></span></summary><div class="body">${esc(e.text)}</div></details>`;
    case "tool": {
      const stats =
        e.added || e.removed ? `<span class="stats"><span class="add">+${e.added}</span> <span class="del">−${e.removed}</span></span>` : "";
      const title = e.command ? `Ran <code>${esc(e.command)}</code>` : esc(e.title);
      const files = e.locations && e.locations.length ? `<div class="files">${e.locations.map((f) => `<code>${esc(f)}</code>`).join("")}</div>` : "";
      const diffs = e.diffs && e.diffs.length
        ? `<div class="files">${e.diffs.map((d) => `<code>${esc(String(d.path).split(/[\\/]/).pop())}</code> <span class="add">+${d.added}</span> <span class="del">−${d.removed}</span>`).join("")}</div>`
        : "";
      const out = e.output ? `<pre>${esc(e.output)}${e.exit_code != null && e.exit_code !== 0 ? `\n[exit ${e.exit_code}]` : ""}</pre>` : "";
      const open = e.status === "failed" ? " open" : "";
      return `<details class="tool ${e.status}"${open}><summary><span class="ttitle">${title}</span>${stats}<span class="st"></span></summary>${diffs}${files}${out}</details>`;
    }
    case "plan":
      return `<ul>${e.entries
        .map((p) => `<li class="${esc(p.status)}"><span class="box">${p.status === "completed" ? "✓" : ""}</span><span>${esc(p.content)}</span></li>`)
        .join("")}</ul>`;
    case "notice":
      return esc(e.text);
    case "compacted":
      return `Context compacted${e.before != null && e.after != null ? ` · ${e.before}% → ${e.after}%` : ""}`;
    default:
      return esc(e.type);
  }
}

// What the agent does between its words: tool calls and thoughts, drawn as one
// branch of a tree, each step on an elbow off the same line.
const isStep = (e) => !!e && (e.type === "tool" || e.type === "thought");

function entryClass(e, prev, next) {
  let c = `entry ${e.type}${e.type === "notice" && e.error ? " error" : ""}`;
  if (isStep(e)) c += ` step${isStep(prev) ? "" : " first"}${isStep(next) ? "" : " last"}`;
  // What comes first after your words carries who is talking.
  if (e.type !== "user" && prev && prev.type === "user") c += " lead";
  return c;
}

// ---- pictures through the relay ----------------------------------------------
// The relay serves no images: the computer sends them inside the channel, and
// the page shows them from blobs. Pictures are put in the page with `data-src`
// and fetched as they appear, wherever the list draws them.

const pics = new Map(); // image link → blob URL
function picSrc(url) {
  // Local signed links go through the channel when connected through the relay;
  // the rest (data:, http) loads straight from the page.
  if (!relay || !url.startsWith("/")) return `src="${esc(url)}"`;
  const got = pics.get(url);
  return got ? `src="${esc(got)}"` : `data-src="${esc(url)}"`;
}

// A picture in the transcript: shown where it is named, full size on tap.
// Never a link: on the relay the /image route lives on the computer, not on
// the page's origin, so navigating there is a blank screen, not the picture.
function picHtml(alt, url) {
  return `<img ${picSrc(url)} alt="${esc(alt)}" class="pic" loading="lazy">`;
}

// Tapping a picture opens it over everything, in place; tapping anywhere
// closes it. The page never navigates away from the app.
document.addEventListener("click", (ev) => {
  const img = ev.target instanceof Element && ev.target.closest("img.pic");
  if (!img) return;
  ev.preventDefault();
  openPic(img);
});

// A table over everything, at full width, scrolling both ways; the phone can turn.
function openTable(table) {
  if (table) openView("Table", table.cloneNode(true));
}

// The same full-screen view for tables and drawings: `node` under a bar with
// `title` and a close button.
function openView(title, node) {
  let view = document.getElementById("tableview");
  if (!view) {
    view = document.createElement("div");
    view.id = "tableview";
    document.body.appendChild(view);
  }
  view.innerHTML = `<div class="tvbar"><span>${esc(title)}</span><button type="button" class="iconbtn" aria-label="Close">${ICONS.close}</button></div><div class="tvbody"></div>`;
  view.querySelector(".tvbody").appendChild(node);
  view.querySelector("button").addEventListener("click", () => {
    view.hidden = true;
    view.replaceChildren();
  });
  view.hidden = false;
}

// A drawing's footer: "Code" swaps the drawing for its source and back,
// "Open" shows the drawing full screen.
function diagramTapped(target, card) {
  const code = target.closest(".dgcode");
  if (code) {
    const on = card.classList.toggle("showcode");
    code.setAttribute("aria-pressed", String(on));
    code.querySelector("span").textContent = on ? code.dataset.back : "Code";
    // The entry changed height with no picture or details to tell the list.
    let n = card;
    while (n && n.idx == null) n = n.parentElement;
    if (n) list.measure(n.idx, n);
    return;
  }
  if (target.closest(".dgopen")) openView(card.dataset.title || "Diagram", card.querySelector(".dgview").cloneNode(true));
}

async function openPic(img) {
  if (img.dataset.src) await loadPic(img);
  let view = document.getElementById("picview");
  if (!view) {
    view = document.createElement("div");
    view.id = "picview";
    view.addEventListener("click", () => {
      view.hidden = true;
      view.replaceChildren();
    });
    document.body.appendChild(view);
  }
  const src = img.currentSrc || img.src;
  if (src) {
    view.innerHTML = `<img src="${esc(src)}" alt="${esc(img.alt)}">`;
    view.hidden = false;
  }
}

async function loadPic(img) {
  const url = img.dataset.src;
  delete img.dataset.src;
  if (!url) return;
  try {
    let got = pics.get(url);
    if (!got) {
      const res = await api(url);
      if (!res.ok) return;
      got = URL.createObjectURL(await res.blob());
    }
    pics.set(url, got);
    img.src = got;
  } catch (e) {
    // The link went away, or the computer with it; the picture stays blank.
  }
}

new MutationObserver((changes) => {
  if (!relay) return;
  for (const c of changes) {
    for (const n of c.addedNodes) {
      if (!(n instanceof Element)) continue;
      if (n.matches("img[data-src]")) loadPic(n);
      n.querySelectorAll("img[data-src]").forEach(loadPic);
    }
  }
}).observe(document.getElementById("app"), { childList: true, subtree: true });

// Rough height before an entry is measured; close enough to keep scrolling smooth.
function estimate(e) {
  const len = (e.text || e.output || e.title || "").length;
  switch (e.type) {
    case "tool":
      return 38 + (e.status === "failed" ? 120 : 0);
    case "thought":
      return 38;
    case "plan":
      return 20 + 30 * (e.entries ? e.entries.length : 1);
    case "notice":
    case "compacted":
      return 28;
    case "agent": {
      // A drawing is much taller than the lines of its source.
      const drawn = (e.text || "").match(/^\s*```(chart|mermaid)\b[\s\S]*?^\s*```/gim) || [];
      const rest = len - drawn.reduce((a, b) => a + b.length, 0);
      return 30 + 21 * Math.ceil(rest / 42) + 330 * drawn.length;
    }
    case "user": {
      const pics = e.images ? (e.images.length === 1 ? 228 : 128 * Math.ceil(e.images.length / 2)) : 0;
      const pastes = e.pastes ? 34 * e.pastes.length : 0;
      return (len ? 30 + 21 * Math.ceil(len / 42) : 16) + pics + pastes;
    }
    default:
      return 30 + 21 * Math.ceil(len / 42);
  }
}

// ---- the virtual list ------------------------------------------------------
// Entries live in `items`; only the ones in view (plus a margin) are in the DOM,
// placed by translateY from summed heights. Heights are guessed first and
// measured once painted; a wrong guess just moves what is below.

// The scroll position is never written while the list moves: on Android any
// write to scrollTop cancels the fling in progress, and a drag fights it. So
// the list never has to compensate for what changes above the viewport:
//
// - Item 0 sits at `base`, below a reserve of empty space sized for the
//   entries not loaded yet. Older entries fill the reserve from the bottom up;
//   nothing on screen moves. An entry above the viewport that measures
//   differently from its guess moves `base` the other way, not the scroll.
// - Only once the scroll rests, with no finger down, `settle()` resizes the
//   reserve to what is still unloaded (or to nothing), moving the scroll by
//   the same amount in the same frame, which is invisible.
// - The list sticks to the end only when content grows while it is there,
//   never in answer to a scroll.

// Entries are asked for when the loaded ones come this close to the viewport.
const LOAD_AHEAD = 1500;
// Each unloaded entry reserves this much more than the loaded average, so the
// reserve rarely runs short (a short reserve is settled at rest, not mid-fling).
const RESERVE_MARGIN = 1.3;
// Silence this long after the last scroll event means the scroll rests.
const REST_MS = 200;
// This close to the bottom still counts as at the end.
const END_SLACK = 24;

class VList {
  constructor() {
    this.items = [];
    this.heights = [];
    this.html = []; // rendered html per index, dropped when the entry changes
    this.open = new Map(); // index → which of its <details> are open, kept while it scrolls away
    this.nodes = new Map();
    this.offsets = new Map(); // the translateY each node was last given
    this.atEnd = true;
    this.grew = false; // content changed since the last layout: the end may stick
    this.el = null;
    this.spacer = null;
    this.frame = 0;
    this.base = 0; // where item 0 sits; negative while room is owed above the top
    this.lastTop = 0;
    this.touching = false;
    this.restTimer = 0;
    this.resizer = null;
  }

  mount(el) {
    this.el = el;
    this.spacer = document.createElement("div");
    this.spacer.className = "spacer";
    el.appendChild(this.spacer);
    this.nodes.clear();
    this.offsets.clear();
    this.base = this.reserveFor(this.unloaded());
    this.lastTop = 0;
    this.touching = false;
    clearTimeout(this.restTimer);
    el.addEventListener("scroll", () => this.onScroll(), { passive: true });
    el.addEventListener("touchstart", () => {
      this.touching = true;
      clearTimeout(this.restTimer);
    }, { passive: true });
    const lift = () => {
      this.touching = false;
      this.armRest();
    };
    el.addEventListener("touchend", lift, { passive: true });
    el.addEventListener("touchcancel", lift, { passive: true });
    // The list gets shorter with no scroll event when the keyboard opens or the
    // draft grows: stay on the last entry if that is where the reader was.
    this.resizer?.disconnect();
    this.resizer = new ResizeObserver(() => {
      if (!this.atEnd || this.touching) return;
      this.grew = true;
      this.schedule();
    });
    this.resizer.observe(el);
    this.schedule();
  }

  onScroll() {
    const el = this.el;
    const top = el.scrollTop;
    // Being at the end is a position, not a direction: iOS moves the scroll up on
    // its own (the bounce at the bottom, the keyboard, a shorter viewport), and
    // reading those as the reader leaving would stop following the conversation.
    this.atEnd = el.scrollHeight - el.clientHeight - top <= END_SLACK;
    if (this.atEnd && state.unseen) seen();
    this.lastTop = top;
    this.armRest();
    this.loadAhead();
    this.schedule();
  }

  // How many entries the session still has before item 0.
  unloaded() {
    return state.session ? state.session.from : 0;
  }

  loadAhead() {
    if (this.unloaded() > 0 && !state.loadingOlder && this.el.scrollTop - this.base < LOAD_AHEAD) loadOlder();
  }

  armRest() {
    clearTimeout(this.restTimer);
    this.restTimer = setTimeout(() => this.settle(), REST_MS);
  }

  // The loaded entries' mean height, for sizing the reserve.
  avg() {
    return this.items.length ? Math.max(40, this.total() / this.items.length) : 80;
  }

  reserveFor(unloaded) {
    return Math.round(unloaded * this.avg() * RESERVE_MARGIN);
  }

  // At rest: resizes the reserve above item 0 to fit what is still unloaded,
  // moving the scroll along in the same frame so nothing on screen moves.
  settle() {
    if (!this.el || this.touching) return;
    const unloaded = this.unloaded();
    const target = this.reserveFor(unloaded);
    const b = this.base;
    // A reserve that runs short (owed room above the top), none left when all is
    // loaded, or one far from the mark: fix it. Small drift can wait.
    const off = b < 0 || (unloaded === 0 && b !== 0) || (unloaded > 0 && (b < target * 0.5 || b > target * 2));
    if (!off) return;
    const d = target - b;
    this.base = target;
    const total = target + this.total();
    // Grow before moving the scroll, or it would be clamped; shrink after.
    if (d > 0) this.spacer.style.height = `${total}px`;
    this.el.scrollTop += d;
    this.lastTop = this.el.scrollTop;
    this.spacer.style.height = `${total}px`;
    this.layout();
  }

  reset(items) {
    this.items = items;
    this.heights = items.map(estimate);
    this.html = [];
    this.open.clear();
    for (const n of this.nodes.values()) n.remove();
    this.nodes.clear();
    this.offsets.clear();
    this.atEnd = true;
    this.grew = true;
    this.base = this.reserveFor(this.unloaded());
    this.schedule();
  }

  // Entries from `at` on changed (the agent is writing, or a tool finished).
  changed(at) {
    // The entry before may have been the end of a branch of steps that now goes on.
    at = Math.max(0, at - 1);
    this.heights.length = Math.min(this.heights.length, at);
    this.html.length = Math.min(this.html.length, at);
    for (let i = at; i < this.items.length; i++) {
      if (this.heights[i] == null) this.heights[i] = estimate(this.items[i]);
    }
    for (const [i, n] of this.nodes) {
      if (i >= at) {
        n.remove();
        this.nodes.delete(i);
        this.offsets.delete(i);
      }
    }
    this.grew = true;
    this.schedule();
  }

  // Older entries go before index 0, into the reserve; what is on screen stays put.
  prepend(older) {
    const n = older.length;
    if (n === 0) return;
    this.items.unshift(...older);
    const fresh = older.map(estimate);
    this.heights = fresh.concat(this.heights);
    this.html = new Array(n).concat(this.html);
    const moved = new Map();
    const offsets = new Map();
    for (const [i, node] of this.nodes) {
      moved.set(i + n, node);
      offsets.set(i + n, this.offsets.get(i));
      node.idx = i + n;
    }
    this.open = new Map([...this.open].map(([i, v]) => [i + n, v]));
    this.nodes = moved;
    this.offsets = offsets;
    const added = fresh.reduce((a, b) => a + b, 0);
    this.atEnd = false;
    // Item 0 moves up by what was added; the scroll and the spacer stay as they are.
    this.base -= added;
    this.layout();
    // The scroll may be resting inside the reserve: keep filling it, and settle
    // the reserve once nothing more is coming.
    this.loadAhead();
    this.armRest();
  }

  total() {
    let y = 0;
    for (const h of this.heights) y += h;
    return y;
  }

  scrollToEnd() {
    this.atEnd = true;
    this.grew = true;
    clearTimeout(this.restTimer);
    this.layout();
    this.el.scrollTop = this.el.scrollHeight;
    this.lastTop = this.el.scrollTop;
    this.schedule();
  }

  schedule() {
    if (this.frame || !this.el) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      this.layout();
    });
  }

  layout() {
    const el = this.el;
    if (!el) return;
    const top = el.scrollTop - 600;
    const bottom = el.scrollTop + el.clientHeight + 600;
    let y = this.base;
    const want = new Set();
    const offsets = new Array(this.items.length);
    for (let i = 0; i < this.items.length; i++) {
      offsets[i] = y;
      const h = this.heights[i];
      if (y + h >= top && y <= bottom) want.add(i);
      y += h;
    }
    // With a negative base the first entries hang above the spacer, out of
    // reach, until settle() brings them back.
    const height = `${Math.max(0, y)}px`;
    if (this.spacer.style.height !== height) this.spacer.style.height = height;
    for (const [i, n] of this.nodes) {
      if (!want.has(i)) {
        n.remove();
        this.nodes.delete(i);
        this.offsets.delete(i);
      }
    }
    let remeasure = false;
    for (const i of want) {
      let n = this.nodes.get(i);
      if (!n) {
        n = document.createElement("div");
        const e = this.items[i];
        n.className = entryClass(e, this.items[i - 1], this.items[i + 1]);
        if (this.html[i] == null) this.html[i] = entryHtml(e);
        const who = n.classList.contains("lead") && state.session ? `<div class="who">${esc(state.session.agent || "Agent")}</div>` : "";
        n.innerHTML = who + this.html[i];
        n.idx = i;
        const open = this.open.get(i);
        if (open) n.querySelectorAll("details").forEach((d, k) => (d.open = !!open[k]));
        n.addEventListener("toggle", () => {
          this.open.set(n.idx, [...n.querySelectorAll("details")].map((d) => d.open));
          this.measure(n.idx, n);
        }, true);
        // A picture arrives after the entry was measured (through the relay it is
        // fetched apart); measure again or what follows would sit on top of it.
        const settle = () => this.measure(n.idx, n);
        n.addEventListener("load", settle, true);
        n.addEventListener("error", settle, true);
        this.spacer.appendChild(n);
        this.nodes.set(i, n);
        remeasure = true;
      }
      if (this.offsets.get(i) !== offsets[i]) {
        this.offsets.set(i, offsets[i]);
        n.style.transform = `translateY(${offsets[i]}px)`;
      }
    }
    if (remeasure) {
      // Entries above the viewport that measured differently would push what
      // is on screen; move item 0 the other way instead of the scroll.
      let moved = false;
      let shift = 0;
      for (const [i, n] of this.nodes) {
        const h = n.offsetHeight;
        if (h && h !== this.heights[i]) {
          if (offsets[i] + this.heights[i] <= el.scrollTop) shift += h - this.heights[i];
          this.heights[i] = h;
          moved = true;
        }
      }
      if (moved) {
        if (shift && !this.atEnd) this.base -= shift;
        this.grew = true;
        this.layout();
        return;
      }
    }
    // Content grew while the list was at the end: keep it there. A scroll alone
    // never sets `grew`, and moving past END_SLACK clears `atEnd`, so a finger is not fought.
    if (this.grew) {
      this.grew = false;
      if (this.atEnd && el.scrollTop + el.clientHeight < el.scrollHeight - 1) {
        el.scrollTop = el.scrollHeight;
        this.lastTop = el.scrollTop;
      }
    }
  }

  measure(i, n) {
    const h = n.offsetHeight;
    if (h && h !== this.heights[i]) {
      this.heights[i] = h;
      this.grew = true;
      this.schedule();
    }
  }
}

const list = new VList();

function loadOlder() {
  const s = state.session;
  if (!s || s.from === 0) return;
  state.loadingOlder = true;
  const from = Math.max(0, s.from - 100);
  send({ type: "entries", session: s.id, from, to: s.from });
  renderOlder();
}

// ---- screens ---------------------------------------------------------------

const app = document.getElementById("app");

function render() {
  if (state.fatal) renderFatal();
  else if (state.screen === "update") renderUpdate();
  else if (state.screen === "signin") renderSignin();
  else if (state.screen === "machines") renderMachines();
  else if (state.screen === "pair") renderPair();
  else if (state.screen === "inbox") renderInbox();
  else if (state.screen === "sessions") renderSessions();
  else if (state.screen === "usage") renderUsage();
  else if (state.screen === "phone") renderPhone();
  else if (state.screen === "new") renderNew();
  else if (app.querySelector(`.screen[data-session="${CSS.escape(state.session ? state.session.id : "")}"]`)) refreshSession();
  else renderSession();
}

// The tabs under the top-level screens.
const TABS = [
  ["inbox", "Inbox", "inbox"],
  ["sessions", "Sessions", "layers"],
  ["usage", "Usage", "gauge"],
  ["phone", "Phone", "phone"],
];

function tabbar() {
  const tab = ([id, label, icon]) =>
    `<button class="tab${state.screen === id ? " on" : ""}" data-tab="${id}" aria-label="${label}"${state.screen === id ? ' aria-current="page"' : ""}>${ICONS[icon]}<span>${label}</span></button>`;
  // A new session sits in the middle, under the thumb.
  const plus = `<button class="tplus" id="tplus" aria-label="New session"${state.connected && state.places.length ? "" : " disabled"}>${ICONS.plus}</button>`;
  return `<nav class="tabs">${TABS.slice(0, 2).map(tab).join("")}${plus}${TABS.slice(2).map(tab).join("")}</nav>`;
}

function wireTabs() {
  app.querySelectorAll(".tab").forEach((b) => b.addEventListener("click", () => showTab(b.dataset.tab)));
  wirePlus(document.getElementById("tplus"));
  syncBadge();
}

function showTab(id) {
  if (state.screen === id) return;
  state.screen = id;
  if (id === "usage") send({ type: "usage" });
  if (id === "phone" && account) loadSessions();
  render();
}

// This address cannot be used; the user has to change something in Arakun first.
function renderFatal() {
  app.innerHTML = `<div class="screen">
    <div class="pair">
      <div class="big">🦝</div>
      <h1>Cannot connect</h1>
      <p>${esc(state.fatal)}</p>
      <button class="btn" id="again">Try again</button>
      <div class="hint">${esc(location.hostname)}</div>
    </div></div>`;
  // Starting over checks everything again; nothing is trusted on the way.
  document.getElementById("again").addEventListener("click", () => location.reload());
}

// Inside a session the lost link is said out loud: the log is what was last seen,
// and what is written now waits on the phone.
function renderOffline() {
  const el = document.getElementById("offline");
  if (!el) return;
  if (state.connected) {
    el.innerHTML = "";
    return;
  }
  const host = esc(state.host || "the computer");
  const title = state.pcOffline ? `Arakun on ${host} is away` : `Can't reach ${host}`;
  const why = state.pcOffline
    ? `Since ${clock(state.pcOffline)}. What you write waits here and goes out when it is back.`
    : "Reconnecting… What you write waits here and goes out when it is back.";
  el.innerHTML = `<div class="offline">${ICONS.offline}<div class="grow"><div class="t">${title}</div><div class="s">${why}</div></div>${
    state.pcOffline ? "" : '<button class="btn outline small" id="retry">Retry</button>'
  }</div>`;
  const b = document.getElementById("retry");
  if (b) b.addEventListener("click", retryNow);
}

function renderPair() {
  const typed = document.getElementById("code") ? document.getElementById("code").value : "";
  app.innerHTML = `<div class="screen">
    <div class="pair">
      <div class="big">🦝</div>
      <h1>Pair this phone</h1>
      <p>In Arakun, open <b>Phone</b> and press <b>Pair a phone</b>. Scan the code it shows, or type it here.</p>
      <form id="pairform" autocomplete="off">
        <input id="code" inputmode="numeric" pattern="[0-9]*" maxlength="6" placeholder="000000" value="${esc(typed)}" ${state.pairing ? "disabled" : ""} aria-label="Pairing code">
        <button class="btn" type="submit" id="pairgo" ${state.pairing ? "disabled" : ""}>${state.pairing ? "Pairing…" : "Pair"}</button>
      </form>
      <div class="err">${esc(state.pairError)}</div>
      <div class="hint">${esc(location.hostname)} · this phone only gets its own key; the computer can revoke it any time.</div>
    </div></div>`;
  const input = document.getElementById("code");
  const go = document.getElementById("pairgo");
  const sync = () => {
    input.value = input.value.replace(/\D/g, "").slice(0, 6);
    go.disabled = state.pairing || input.value.length !== 6;
  };
  input.addEventListener("input", sync);
  sync();
  document.getElementById("pairform").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    if (input.value.length !== 6) return;
    if (await pair(input.value)) connect();
  });
  if (!state.pairing && !typed) input.focus();
}

function statusbar() {
  const word = state.connected ? "connected" : state.pcOffline ? `computer away since ${clock(state.pcOffline)}` : "reconnecting…";
  return `<div class="statusbar${state.connected ? "" : " off"}"><span class="dot"></span><span>${esc(state.host || location.hostname)}</span><span class="spacer"></span><span>${esc(word)}</span></div>`;
}

// What an empty screen says while the computer is not there.
function connecting() {
  if (state.pcOffline) return `Arakun on ${state.host || "the computer"} has been away since ${clock(state.pcOffline)}`;
  return relay ? `Reaching ${relay.name} through the relay` : "Connecting to " + location.hostname;
}
function connectingHint() {
  if (relay) return "Arakun must be running on the computer, signed in to the same GitHub account.";
  if (location.protocol === "https:") return "Make sure Tailscale is on, on the phone and on the computer, and Arakun is running.";
  return "Make sure the phone is on the same network and Arakun is running.";
}

// ---- on the home screen, and told when the agent needs you --------------------
// Both need a secure origin: the Tailscale way in. Over plain http on the LAN
// the browser offers neither, and the cards stay away.

const HIDE_INSTALL = "arakun.hideInstall";
const HIDE_NOTIFY = "arakun.hideNotify";
// The browser's own install offer, held back until the user asks for it.
let installOffer = null;
const standalone = matchMedia("(display-mode: standalone)").matches || navigator.standalone === true;
// A Safari tab on an iPhone or iPad: no install offer ever comes, and push
// exists only once the page is on the home screen, so the card has to say so.
// (Mac Safari has navigator.standalone too, but no touchscreen.)
const appleTab = "standalone" in navigator && navigator.standalone === false && navigator.maxTouchPoints > 0;

if (isSecureContext && "serviceWorker" in navigator) navigator.serviceWorker.register("sw.js").catch(() => {});
window.addEventListener("beforeinstallprompt", (ev) => {
  ev.preventDefault();
  installOffer = ev;
  if (state.screen === "inbox") render();
});
window.addEventListener("appinstalled", () => {
  installOffer = null;
  toast("Arakun is on your home screen");
  if (state.screen === "inbox") render();
});

// Whether this phone can be told: "ask" until the user decides, then "on" or
// "denied"; "off" where the browser cannot do it at all (plain http, no push).
function notifyState() {
  if (!isSecureContext || !("Notification" in window) || !("PushManager" in window)) return "off";
  return Notification.permission === "granted" ? "on" : Notification.permission === "denied" ? "denied" : "ask";
}

// Subscribes this phone at its browser's push service with the computer's key,
// and hands the computer where to send. Needs the permission already given.
async function subscribePush() {
  if (!state.pushKey || !paired()) return false;
  try {
    const reg = await navigator.serviceWorker.ready;
    const key = Uint8Array.from(atob(state.pushKey.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
    // The account host is one origin for every computer, so one subscription,
    // made with one computer's key. Another computer's key replaces it: the
    // browser refuses a second key, and notices follow the computer opened last.
    const old = await reg.pushManager.getSubscription();
    if (old && !sameBytes(new Uint8Array(old.options.applicationServerKey || []), key)) await old.unsubscribe();
    const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
    const res = await api("/push", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ subscription: sub.toJSON() }),
    });
    return res.ok;
  } catch (e) {
    return false;
  }
}

// The session a notification tap opened, until the computer answers for it.
// With several computers the notification may be another computer's.
let noticeOpen = "";

function openFromNotice(id) {
  openSession(id);
  noticeOpen = account && account.machines.length > 1 ? id : "";
}

function sameBytes(a, b) {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

// A tap on a notification brings the page to its session: through the worker
// when the page is open, through the address when it had to be opened.
if ("serviceWorker" in navigator) {
  navigator.serviceWorker.addEventListener("message", (ev) => {
    if (ev.data && ev.data.open) openFromNotice(ev.data.open);
  });
}
let openAtStart = (() => {
  const m = location.hash.match(/^#s=(.+)$/);
  return m ? decodeURIComponent(m[1]) : "";
})();

function setupCards() {
  const card = (id, icon, title, text, action) => `<div class="setup" data-setup="${id}">
      <span class="sicon">${icon}</span>
      <span class="body"><div class="t">${title}</div><div class="s">${text}</div></span>
      ${action ? `<button class="btn small" data-go="${id}">${action}</button>` : ""}
      <button class="x" data-skip="${id}" aria-label="Not now">${ICONS.close}</button>
    </div>`;
  let out = "";
  if (!standalone && !localStorage.getItem(HIDE_INSTALL)) {
    if (installOffer) {
      out += card("install", ICONS.phone, "Add Arakun to your home screen", "Opens full screen, like an app.", "Add");
    } else if (appleTab) {
      out += card("install", ICONS.phone, "Add Arakun to your home screen", "Tap Share, then “Add to Home Screen”. Notifications only work from there.", "");
    }
  }
  if (notifyState() === "ask" && !localStorage.getItem(HIDE_NOTIFY)) {
    out += card("notify", ICONS.bell, "Know when the agent needs you", "A notification for permissions, questions and finished turns.", "Turn on");
  }
  return out;
}

function wireSetup() {
  app.querySelectorAll("[data-go]").forEach((b) =>
    b.addEventListener("click", async () => {
      if (b.dataset.go === "install" && installOffer) {
        const offer = installOffer;
        installOffer = null;
        await offer.prompt();
        render();
      } else if (b.dataset.go === "notify") {
        const got = await Notification.requestPermission();
        if (got === "granted") {
          toast((await subscribePush()) ? "You will be told when the agent needs you" : "Could not turn notifications on");
        }
        render();
      }
    })
  );
  app.querySelectorAll("[data-skip]").forEach((b) =>
    b.addEventListener("click", () => {
      localStorage.setItem(b.dataset.skip === "install" ? HIDE_INSTALL : HIDE_NOTIFY, "1");
      render();
    })
  );
}

function renderInbox() {
  // What waits on you first (a failed run too), then what runs, then what ended
  // lately: older sessions are in Sessions, not here.
  const since = Date.now() / 1000 - RECENT;
  const finished = state.inbox.filter((s) => s.status === "idle");
  const recent = finished.filter((s) => s.updated >= since).slice(0, 12);
  const needs = state.inbox.filter(needsYou);
  const working = state.inbox.filter((s) => s.status === "working");
  const groups = [
    ["needs", "Needs you", needs],
    ["working", "Working now", working],
    ["done", "Finished", recent],
  ].filter(([, , items]) => items.length);
  const older = finished.length - recent.length;
  const dot = { needs: "", working: '<span class="dot" style="background:var(--success)"></span>', done: "" };
  const rows = groups
    .map(
      ([key, name, items]) => `<div class="group ${key}"><h2>${key === "needs" ? ICONS.alert.replace("<svg", '<svg width="15" height="15"') : dot[key]}${name}</h2>${items
        .map(
          (s) => `<button class="srow ${esc(s.status)}${s.asks ? ` asks-${esc(s.asks.kind)}` : ""}" data-id="${esc(s.id)}">
            ${agentAvatar(s.agent)}
            <span class="body">
              <div class="t">${esc(s.title || "Untitled session")}</div>
              ${asksLine(s) || `<div class="s"><span>${esc(s.project)}${s.env && s.env !== s.project ? ` · ${esc(s.env)}` : ""}</span><span class="agent">${esc(s.agent)}</span></div>`}
            </span>
            <span class="right">
              <span class="time">${ago(s.updated)}</span>
              ${s.added || s.removed ? `<span class="stats"><span class="add">+${s.added}</span> <span class="del">−${s.removed}</span></span>` : ""}
            </span>
          </button>`
        )
        .join("")}</div>`
    )
    .join("");
  const empty = state.inbox.length
    ? ""
    : `<div class="empty"><div class="big">🦝</div><div>${state.connected ? "No sessions yet" : esc(connecting())}</div><div class="hint">${state.connected ? "Tap + to start one, or start it on the computer." : esc(connectingHint())}</div></div>`;
  const tiles = state.inbox.length
    ? `<div class="tiles">
        <div class="tile"><div class="l">Needs you</div><div class="v">${needs.length ? '<span class="dot" style="background:var(--accent)"></span>' : ""}${needs.length}</div></div>
        <div class="tile"><div class="l">Working</div><div class="v">${working.length ? '<span class="dot" style="background:var(--success)"></span>' : ""}${working.length}</div></div>
        <div class="tile"><div class="l">Done today</div><div class="v">${recent.length}</div></div>
      </div>`
    : "";
  const more = older > 0 ? `<button class="more" data-tab="sessions">${older} older in Sessions</button>` : "";
  const body = `${noticeBanners()}${setupCards()}${tiles}${rows}${more}${empty}`;
  const hdr = `<div class="ihead"><img class="mark" src="apple-touch-icon.png" alt=""><span class="host">${esc(state.host || "Arakun")}</span>
      <span class="pill ${state.connected ? "on" : "off"}"><span class="dot"></span>${state.connected ? "online" : "away"}</span></div>`;
  // Already on screen: the list is refilled in place, so its scroll stays put and
  // an unchanged list is not touched at all.
  const box = app.querySelector('.screen[data-screen="inbox"] .inbox');
  if (box) {
    const bar = app.querySelector(".statusbar");
    if (bar) bar.outerHTML = statusbar();
    const head = app.querySelector(".screen > .ihead");
    if (head && head.dataset.html !== hdr) {
      head.outerHTML = hdr;
      app.querySelector(".screen > .ihead").dataset.html = hdr;
    }
    if (box.dataset.html === body) return;
    box.dataset.html = body;
    box.innerHTML = body;
  } else {
    app.innerHTML = `<div class="screen" data-screen="inbox">${statusbar()}
    ${hdr}
    <div class="inbox">${body}</div>
    <button class="jump" data-tab="sessions">${ICONS.search}<span>Jump to a session or search…</span></button>${tabbar()}</div>`;
    app.querySelector(".inbox").dataset.html = body;
    app.querySelector(".screen > .ihead").dataset.html = hdr;
    app.querySelector(".jump").addEventListener("click", () => showTab("sessions"));
    wireTabs();
  }
  app.querySelectorAll(".srow").forEach((b) => b.addEventListener("click", () => openSession(b.dataset.id)));
  app.querySelectorAll(".inbox .more").forEach((b) => b.addEventListener("click", () => showTab("sessions")));
  wireSetup();
  wireNotices();
  syncBadge();
}

// How far back Finished reaches in the inbox, in seconds.
const RECENT = 24 * 3600;

function needsYou(s) {
  return s.status === "needs_input" || s.status === "error";
}

// Under a waiting session's title: what it asks, so the inbox alone says it.
function asksLine(s) {
  if (s.asks) {
    const text = s.asks.kind === "permission" ? `Run: ${s.asks.text}` : s.asks.text;
    return `<div class="ask-line ${esc(s.asks.kind)}">${esc(text)}</div>`;
  }
  if (s.status === "error") return `<div class="ask-line error">Stopped with an error</div>`;
  return "";
}

// The Inbox tab counts what waits on you, from any tab.
function syncBadge() {
  const plus = document.getElementById("tplus");
  if (plus) plus.disabled = !(state.connected && state.places.length);
  const tab = app.querySelector('.tab[data-tab="inbox"]');
  if (!tab) return;
  const n = state.inbox.filter(needsYou).length;
  let b = tab.querySelector(".tbadge");
  if (!n) {
    if (b) b.remove();
    return;
  }
  if (!b) {
    b = document.createElement("span");
    b.className = "tbadge";
    tab.appendChild(b);
  }
  b.textContent = String(n);
}

// ---- sessions: every session, project by project, with a search -------------

// Projects in the order of their freshest session.
function projects() {
  const seen = [];
  for (const s of state.inbox) if (!seen.includes(s.project)) seen.push(s.project);
  return seen;
}

// Who runs a session, as a tile: Claude's mark, the cloud for Workers AI, else
// the agent's first letter.
function agentAvatar(agent) {
  const a = String(agent || "").toLowerCase();
  if (a.includes("claude")) return `<span class="av claude">${ICONS.claude}</span>`;
  if (a.includes("workers")) return `<span class="av" style="color:#f38020">${ICONS.cloud}</span>`;
  return `<span class="av">${esc((a[0] || "?").toUpperCase())}</span>`;
}

function sessionRow(s) {
  const sub = [s.env && s.env !== s.project ? s.env : "", s.agent].filter(Boolean);
  return `<button class="srow ${esc(s.status)}" data-id="${esc(s.id)}">
    ${agentAvatar(s.agent)}
    <span class="body">
      <div class="t">${esc(s.title || "New session")}</div>
      <div class="s">${sub.map((x, i) => `<span${i === sub.length - 1 ? ' class="agent"' : ""}>${esc(x)}</span>`).join("")}</div>
    </span>
    <span class="right">
      <span class="time">${ago(s.updated)}</span>
      <span class="badge ${esc(s.status)}"><span class="dot"></span>${statusLabel(s.status)}</span>
    </span>
  </button>`;
}

// The list part of Sessions, as HTML; filled in apart so typing in the search keeps its focus.
function sessionsList() {
  const q = state.query.trim().toLowerCase();
  const names = projects();
  const shown = names.filter((p) => !state.project || p === state.project);
  const matches = (s) => !q || [s.title, s.env, s.agent, s.project].some((x) => String(x || "").toLowerCase().includes(q));
  const groups = shown
    .map((p) => [p, state.inbox.filter((s) => s.project === p && matches(s))])
    .filter(([, items]) => items.length);
  if (!state.inbox.length) {
    return `<div class="empty"><div class="big">🦝</div><div>${state.connected ? "No sessions yet" : esc(connecting())}</div><div class="hint">${state.connected ? "Start one on the computer and it shows up here." : esc(connectingHint())}</div></div>`;
  }
  if (!groups.length) return `<div class="empty"><div>Nothing matches “${esc(state.query.trim())}”</div></div>`;
  return groups
    .map(
      ([p, items]) => `<div class="group">
        <div class="proj"><span class="picon">${ICONS.folder}</span><span class="pname">${esc(p)}</span><span class="pcount">${items.length}</span></div>
        ${items.map(sessionRow).join("")}
      </div>`
    )
    .join("");
}

function fillSessions() {
  const el = document.getElementById("slist");
  if (!el) return;
  el.innerHTML = sessionsList();
  el.querySelectorAll(".srow").forEach((b) => b.addEventListener("click", () => openSession(b.dataset.id)));
  const sub = document.getElementById("ssub");
  if (sub) sub.textContent = sessionsSub();
}

function sessionsSub() {
  const n = state.inbox.length;
  const p = projects().length;
  return `${n} session${n === 1 ? "" : "s"} · ${p} project${p === 1 ? "" : "s"}`;
}

function renderSessions() {
  const names = projects();
  if (state.project && !names.includes(state.project)) state.project = "";
  const chip = (id, label) => `<button class="chip${state.project === id ? " on" : ""}" data-project="${esc(id)}">${esc(label)}</button>`;
  const chips = names.length > 1 ? `<div class="chips">${chip("", "All")}${names.map((p) => chip(p, p)).join("")}</div>` : "";
  app.innerHTML = `<div class="screen">${statusbar()}
    <div class="hdr"><div class="grow"><h1>Sessions</h1><div class="sub" id="ssub">${sessionsSub()}</div></div></div>
    <label class="search">${ICONS.search}<input id="squery" type="search" placeholder="Search sessions…" autocomplete="off" value="${esc(state.query)}"></label>
    ${chips}
    <div class="inbox" id="slist"></div>${tabbar()}</div>`;
  const input = document.getElementById("squery");
  input.addEventListener("input", () => {
    state.query = input.value;
    fillSessions();
  });
  app.querySelectorAll(".chip").forEach((b) =>
    b.addEventListener("click", () => {
      state.project = b.dataset.project;
      app.querySelectorAll(".chip").forEach((c) => c.classList.toggle("on", c === b));
      fillSessions();
    })
  );
  fillSessions();
  wireTabs();
}

// ---- usage: both agents' plan limits, read on the computer -------------------

// The bar's color by how full the window is; each agent has its own calm color.
function barTone(agent, pct) {
  if (pct >= 100) return "full";
  if (pct >= 70) return "high";
  return agent;
}

function usageCard(a) {
  const logo = a.agent === "claude" ? '<img class="logo" src="claude.svg" alt="">' : '<span class="logo factory"></span>';
  if (!a.signed_in) {
    return `<section class="ucard">
      <div class="uhead">${logo}<div class="grow"><div class="uname">${esc(a.name)}</div><div class="uplan">Not signed in</div></div></div>
      <div class="unote">${esc(a.note)}</div></section>`;
  }
  const rows = (a.rows || [])
    .map(
      (r) => `<div class="limit">
        <div class="lhead"><span class="llabel">${esc(r.label)}</span><span class="lval">${r.percent}%${r.detail ? ` · ${esc(r.detail)}` : ""}</span></div>
        <div class="track"><div class="fill ${barTone(a.agent, r.percent)}" style="width:${Math.min(100, r.percent)}%"></div></div>
      </div>`
    )
    .join("");
  const idle = a.idle && a.idle.length ? `<div class="uidle">Not active: ${esc(a.idle.join(", "))}</div>` : "";
  const balance = a.balance ? `<div class="ubal"><span class="grow">Extra usage balance</span><span>${esc(a.balance)}</span></div>` : "";
  const note = a.note ? `<div class="unote ${esc(a.tone || "normal")}">${esc(a.note)}</div>` : "";
  return `<section class="ucard">
    <div class="uhead">${logo}<div class="grow"><div class="uname">${esc(a.name)}</div>${a.plan ? `<div class="uplan">${esc(a.plan)}</div>` : ""}</div></div>
    ${rows}${balance}${idle}${note}</section>`;
}

function renderUsage() {
  const u = state.usage;
  const body = u
    ? u.agents.map(usageCard).join("")
    : `<div class="empty"><div class="big">🦝</div><div>${state.connected ? "Reading usage…" : esc(connecting())}</div></div>`;
  const at = u ? Math.max(0, ...u.agents.map((a) => a.at || 0)) : 0;
  const foot = u
    ? `<div class="ufoot">Read from the agents' own bookkeeping on the computer, same as the sidebar in Arakun.${at ? ` Updated ${ago(at)}${ago(at) === "now" ? "" : " ago"}.` : ""}</div>`
    : "";
  app.innerHTML = `<div class="screen">${statusbar()}
    <div class="hdr"><div class="grow"><h1>Usage</h1></div><button class="iconbtn" id="reload" aria-label="Refresh">${ICONS.refresh}</button></div>
    <div class="usage">${body}${foot}</div>${tabbar()}</div>`;
  document.getElementById("reload").addEventListener("click", () => send({ type: "usage" }));
  wireTabs();
}

// ---- the Phone tab: this computer, what to be told about, and this phone ------

// How this page reaches the computer, from the address it was opened at.
function wayIn() {
  if (relay) return "Connected through the relay, end-to-end encrypted";
  if (/\.ts\.net$/.test(location.hostname)) return "Connected through Tailscale";
  if (location.protocol === "https:") return "Connected over HTTPS";
  return "Connected on the local network";
}

function pairedOn(unix) {
  if (!unix) return "";
  return new Date(unix * 1000).toLocaleDateString(document.documentElement.lang || "en", { month: "short", day: "numeric" });
}

function renderPhone() {
  if (account && account.sessions === undefined) loadSessions();
  const me = state.me || {};
  const notices = me.notices || { done: true, input: true, error: true };
  const row = (icon, title, sub, right = "", cls = "") =>
    `<div class="prow ${cls}"><span class="ricon">${icon}</span><span class="rbody"><span class="rt">${title}</span>${sub ? `<span class="rs">${sub}</span>` : ""}</span>${right}</div>`;
  const toggle = (key, icon, title) =>
    `<div class="prow"><span class="ricon">${icon}</span><span class="rbody"><span class="rt">${title}</span></span>
      <button class="switch${notices[key] ? " on" : ""}" role="switch" aria-checked="${!!notices[key]}" data-notice="${key}" aria-label="${title}"><span class="knob"></span></button></div>`;
  const group = (label, rows) => `<section class="group"><div class="glabel">${label}</div><div class="gbox">${rows.join("")}</div></section>`;

  const ns = notifyState();
  let notify;
  if (ns === "on") {
    notify = [
      toggle("done", ICONS.check, "Turn finished"),
      toggle("input", ICONS.help, "Needs permission or answer"),
      toggle("error", ICONS.alert, "Something went wrong"),
    ];
  } else if (ns === "ask") {
    notify = [row(ICONS.bell, "Notifications", "For permissions, questions, finished turns and errors.", `<button class="btn small" id="notify-on">Turn on</button>`)];
  } else if (ns === "denied") {
    notify = [row(ICONS.bell, "Notifications", "Blocked by the browser. Allow them for this site in its settings to turn them back on.")];
  } else if (appleTab) {
    notify = [row(ICONS.bell, "Notifications", "Add Arakun to your home screen first: tap Share, then “Add to Home Screen”.")];
  } else {
    notify = [row(ICONS.bell, "Notifications", "They need the HTTPS address (Tailscale). The local network cannot deliver them.")];
  }

  // One origin keeps one push subscription: say which computer it is for.
  const note = account && ns !== "off" && ns !== "denied" ? RELAY.pushNote(account.machines, state.host) : "";
  if (note) notify.unshift(row(ICONS.monitor, "From one computer", esc(note)));
  app.innerHTML = `<div class="screen">${statusbar()}
    <div class="hdr"><div class="grow"><h1>Phone</h1></div></div>
    <div class="settings">
      ${group("Desktop", [
        row(ICONS.monitor, esc(state.host || location.hostname), state.connected ? wayIn() : state.pcOffline ? `Away since ${clock(state.pcOffline)}` : "Reconnecting…"),
        ...(account && account.machines.length > 1
          ? [`<button class="prow" id="switch"><span class="ricon">${ICONS.layers}</span><span class="rbody"><span class="rt">All computers</span></span></button>`]
          : []),
      ])}
      ${group("Notifications", notify)}
      ${account ? group("Signed-in devices", signedInList()) : ""}
      ${group(
        "This phone",
        account
          ? [
              row(ICONS.phone, esc(me.name || deviceName()), account.me ? `Signed in as ${esc(account.me.login)}` : ""),
              `<button class="prow danger" id="signout"><span class="ricon">${ICONS.unlink}</span><span class="rbody"><span class="rt">Sign out</span></span></button>`,
            ]
          : [
              row(ICONS.phone, esc(me.name || deviceName()), me.paired_at ? `Paired ${pairedOn(me.paired_at)} · last seen now` : ""),
              `<button class="prow danger" id="unpair"><span class="ricon">${ICONS.unlink}</span><span class="rbody"><span class="rt">Unpair this phone</span></span></button>`,
            ]
      )}
    </div>${tabbar()}</div>`;

  // The whole row is the switch's target, not only the knob.
  app.querySelectorAll("[data-notice]").forEach((b) => {
    const flip = () => setNotice(b.dataset.notice, b.getAttribute("aria-checked") !== "true");
    b.addEventListener("click", (ev) => {
      ev.stopPropagation();
      flip();
    });
    b.closest(".prow").addEventListener("click", flip);
  });
  const on = document.getElementById("notify-on");
  if (on) {
    on.addEventListener("click", async () => {
      const got = await Notification.requestPermission();
      if (got === "granted") toast((await subscribePush()) ? "You will be told when the agent needs you" : "Could not turn notifications on");
      render();
    });
  }
  const unpairBtn = document.getElementById("unpair");
  if (unpairBtn) unpairBtn.addEventListener("click", unpair);
  const out = document.getElementById("signout");
  if (out) out.addEventListener("click", signOut);
  const sw = document.getElementById("switch");
  if (sw) sw.addEventListener("click", () => leaveMachine());
  app.querySelectorAll("[data-signout]").forEach((b) =>
    b.addEventListener("click", () => {
      const s = Array.isArray(account.sessions) && account.sessions.find((x) => x.id === b.dataset.signout);
      if (s) signOutDevice(s.id, s.name);
    })
  );
  wireTabs();
}

// The rows of the account's signed-in devices, this phone marked.
function signedInList() {
  const said = (text) => `<div class="prow"><span class="rbody"><span class="rs">${esc(text)}</span></span></div>`;
  if (account.sessions === undefined || account.sessions === null) return [said("Asking your account…")];
  if (typeof account.sessions === "string") return [said(account.sessions)];
  return RELAY.signedInRows(account.sessions, esc, ago);
}

// One kind of notice on or off; the computer keeps the choice.
async function setNotice(key, on) {
  const me = state.me;
  if (!me) return;
  const before = me.notices || { done: true, input: true, error: true };
  me.notices = { ...before, [key]: on };
  render();
  try {
    const res = await api("/push", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ notices: me.notices }),
    });
    if (!res.ok) throw new Error(res.status);
    me.notices = (await res.json()).notices || me.notices;
  } catch (e) {
    me.notices = before;
    toast("Could not save that");
    render();
  }
}

// This phone lets go of the computer it paired with on the network: its token
// dies there, and here. On the account host the phone signs out instead.
async function unpair() {
  const host = state.host || location.hostname;
  if (!confirm(`Unpair this phone from ${host}?\n\nYou will need a new code from Arakun to pair it again.`)) return;
  try {
    await api("/unpair", { method: "POST" });
  } catch (e) {
    // The computer may be away; the token is dropped here regardless.
  }
  if ("serviceWorker" in navigator && isSecureContext) {
    try {
      const reg = await navigator.serviceWorker.ready;
      const sub = await reg.pushManager.getSubscription();
      if (sub) await sub.unsubscribe();
    } catch (e) {
      // Nothing to undo.
    }
  }
  if (link) link.close();
  link = null;
  forgetToken();
  state.me = null;
  state.connected = false;
  state.pairError = "";
  state.screen = "pair";
  render();
}

function openSession(id) {
  // A session opened by hand is not the one a notification tap waits on.
  noticeOpen = "";
  const row = state.inbox.find((s) => s.id === id);
  state.session = {
    id,
    title: row ? row.title : "",
    agent: row ? row.agent : "",
    status: row ? row.status : "idle",
    count: 0,
    from: 0,
    entries: [],
    permission: null,
    question: null,
    place: row ? row.project : "",
  };
  state.ask = null;
  state.sheet = null;
  state.screen = "session";
  state.draft = localStorage.getItem(kept(DRAFT_KEY) + id) || "";
  state.pending = [];
  // The address names the session, so the service worker knows what is on screen.
  // It is a step in the history too: the phone's back gesture comes back here.
  const url = `#s=${encodeURIComponent(id)}`;
  if (layer() === "session") history.replaceState({ layer: "session" }, "", url);
  else history.pushState({ layer: "session" }, "", url);
  list.reset([]);
  send({ type: "open", session: id });
  renderSession();
}

function closeSession() {
  noticeOpen = "";
  send({ type: "close" });
  state.session = null;
  state.sheet = null;
  state.screen = "inbox";
  history.replaceState(null, "", location.pathname);
  render();
}

const AGENTS = [
  ["claude", "Claude Code"],
  ["droid", "Droid"],
  ["opencode", "OpenCode"],
  ["codex", "Codex"],
  ["devin", "Devin"],
  ["cursor", "Cursor"],
  ["antigravity", "Antigravity"],
];

// The New session form starts on the place used last, with the agent that place picks.
// ---- a new session: the + writes straight away, holding it picks the project ----

// A tap on the + opens the quick sheet on the place used last; holding it opens
// a menu of the places, to start in one of them or reach the full form.
function wirePlus(btn) {
  if (!btn) return;
  let timer = 0;
  let held = false;
  btn.addEventListener("pointerdown", () => {
    held = false;
    timer = setTimeout(() => {
      held = true;
      if (navigator.vibrate) navigator.vibrate(12);
      openPlusMenu();
    }, 420);
  });
  ["pointerup", "pointerleave", "pointercancel"].forEach((t) => btn.addEventListener(t, () => clearTimeout(timer)));
  btn.addEventListener("click", (ev) => {
    ev.preventDefault();
    if (held) return;
    pickProject();
  });
  btn.addEventListener("contextmenu", (ev) => ev.preventDefault());
}

function lastPlace() {
  const last = localStorage.getItem(kept(PLACE_KEY));
  return state.places.find((p) => p.path === last) || state.places[0];
}

const agentName = (key) => (AGENTS.find(([k]) => k === key) || [key, key])[1];

// A place's branch; the environment's name stands in where the computer does not say.
const branchOf = (p) => p.branch || p.env || p.project;

// Projects with their places, freshest session first.
function projectList() {
  const by = new Map();
  for (const p of state.places) {
    if (!by.has(p.project)) by.set(p.project, { name: p.project, places: [], updated: 0, working: 0 });
    by.get(p.project).places.push(p);
  }
  for (const s of state.inbox) {
    const g = by.get(s.project);
    if (!g) continue;
    g.updated = Math.max(g.updated, s.updated || 0);
    if (s.status === "working") g.working++;
  }
  return [...by.values()].sort((a, b) => b.updated - a.updated);
}

// The place a project starts in: the branch used last there, else its checkout.
function placeFor(project) {
  const places = state.places.filter((p) => p.project === project);
  const last = localStorage.getItem(kept(PLACE_KEY));
  return places.find((p) => p.path === last) || places.find((p) => p.env === p.project) || places[0];
}

// A project's tile mark: the raccoon for Arakun's own, else its initial on a tint of its own.
function projectMark(name) {
  if (/^(app|arakun)$/i.test(name)) return '<img src="apple-touch-icon.png" alt="">';
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360;
  return `<span style="background:hsl(${h} 40% 22%);color:hsl(${h} 80% 78%)">${esc(name[0].toUpperCase())}</span>`;
}

// The + first asks where: a tile per project, the freshest first.
function pickProject() {
  if (!state.connected || !state.places.length) return;
  const projects = projectList();
  if (projects.length === 1) return quickSession(placeFor(projects[0].name));
  let el = document.getElementById("projpick");
  if (!el) {
    el = document.createElement("div");
    el.id = "projpick";
    document.body.appendChild(el);
  }
  const sub = (g) => {
    if (g.working) return `<span class="dot"></span>${g.working} working`;
    const n = g.places.length;
    return `${g.updated ? ago(g.updated) : "no sessions yet"}${n > 1 ? ` · ${n} branches` : ""}`;
  };
  el.innerHTML = `<div class="pphead"><b>New session</b><button class="iconbtn" id="ppclose" aria-label="Close">${ICONS.close}</button></div>
    <div class="ppsub">Which project?</div>
    <div class="ppgrid">${projects
      .map((g) => `<button class="pptile" data-project="${esc(g.name)}"><span class="ppmark">${projectMark(g.name)}</span><b>${esc(g.name)}</b><small>${sub(g)}</small></button>`)
      .join("")}</div>`;
  if (layer() !== "new") history.pushState({ layer: "new" }, "");
  el.querySelector("#ppclose").addEventListener("click", () => goBack(closeNew));
  el.querySelectorAll(".pptile").forEach((b) =>
    b.addEventListener("click", () => {
      el.remove();
      quickSession(placeFor(b.dataset.project));
    })
  );
}

function openPlusMenu() {
  if (!state.connected || !state.places.length) return;
  closePlusMenu();
  const places = projectList().slice(0, 6).map((g) => placeFor(g.name));
  const menu = document.createElement("div");
  menu.id = "plusmenu";
  menu.innerHTML = `<div class="pmscrim"></div><div class="pmcard">
    ${places
      .map(
        (p, i) => `<button class="pmitem${i === 0 ? " hl" : ""}" data-path="${esc(p.path)}"><span class="pmic ppmark">${projectMark(p.project)}</span>
          <span class="pmtx">New in <b>${esc(p.project)}</b><small>${esc(branchOf(p))} · ${esc(agentName(p.agent))}</small></span></button>`
      )
      .join("")}
    <div class="pmsep"></div>
    <button class="pmitem" data-full="1"><span class="pmic">${ICONS.tune}</span><span class="pmtx">More options<small>Agent, model and reasoning</small></span></button>
  </div>`;
  document.body.appendChild(menu);
  menu.querySelector(".pmscrim").addEventListener("click", closePlusMenu);
  menu.querySelectorAll(".pmitem").forEach((b) =>
    b.addEventListener("click", () => {
      closePlusMenu();
      if (b.dataset.full) newSession();
      else quickSession(state.places.find((p) => p.path === b.dataset.path));
    })
  );
}

function closePlusMenu() {
  document.getElementById("plusmenu")?.remove();
}

// The quick sheet: what to do, over the screen you are on; where and with whom
// are two chips already set (the place used last, its agent, the model it would
// start with). Send starts the session.
function quickSession(place) {
  place = place || lastPlace();
  if (!place || !state.connected) return;
  state.start = { quick: true, path: place.path, agent: place.agent, text: state.start?.quick ? state.start.text : "", sent: false, options: null, picked: {}, pick: null };
  if (layer() !== "new") history.pushState({ layer: "new" }, "");
  askOptions();
  renderQuick();
  document.getElementById("qtext")?.focus();
}

// What a setting is on now, by name: the pick here, else what the agent would start with.
function currentSetting(f, key) {
  const set = f.options && f.options.find((x) => x.key === key);
  if (!set) return null;
  const id = f.picked[key] ?? set.current;
  const o = set.options.find((x) => x.id === id);
  return o ? o.name : null;
}

function renderQuick() {
  const f = state.start;
  let el = document.getElementById("quick");
  if (!f || !f.quick) {
    el?.remove();
    return;
  }
  const place = state.places.find((p) => p.path === f.path);
  const branches = place ? state.places.filter((p) => p.project === place.project) : [];
  const model = currentSetting(f, "modelId");
  const effort = currentSetting(f, "reasoningEffort");
  const pick =
    f.pick === "place"
      ? `<div class="qlist">${branches
          .map((p) => `<button class="qopt${p.path === f.path ? " on" : ""}" data-path="${esc(p.path)}"><span class="qbr">${ICONS.branch}${esc(branchOf(p))}</span><small>${esc(agentName(p.agent))}</small></button>`)
          .join("")}</div>`
      : f.pick === "agent"
        ? `<div class="qlist"><div class="qhead">Agent</div>${AGENTS.map(([k, label]) => `<button class="qopt${k === f.agent ? " on" : ""}" data-agent="${k}"><span>${label}</span></button>`).join("")}
          ${f.options
            ? f.options
                .map((set) => `<div class="qhead">${esc(set.label)}</div>${set.options
                  .map((o) => `<button class="qopt${o.id === (f.picked[set.key] ?? set.current) ? " on" : ""}" data-key="${esc(set.key)}" data-id="${esc(o.id)}"${o.disabled ? " disabled" : ""}><span>${esc(o.name)}</span>${o.detail ? `<small>${esc(o.detail)}</small>` : ""}</button>`)
                  .join("")}`)
                .join("")
            : '<div class="qhead">Loading models…</div>'}</div>`
        : "";
  const html = `<div class="qscrim"></div><form class="qsheet" id="qform">
    <div class="grab"></div>
    <button type="button" class="qctx" id="qproj">New session in <span class="ppmark">${place ? projectMark(place.project) : ""}</span><b>${esc(place ? place.project : "")}</b>${state.places.length > branches.length ? ICONS.chevron : ""}</button>
    <div class="qchips">
      <button type="button" class="qchip${f.pick === "place" ? " open" : ""}" data-pick="place"${branches.length > 1 ? "" : " disabled"}>${ICONS.branch}<span>${esc(place ? branchOf(place) : "Branch")}</span>${branches.length > 1 ? ICONS.chevron : ""}</button>
      <button type="button" class="qchip${f.pick === "agent" ? " open" : ""}" data-pick="agent"><span>${esc(agentName(f.agent))}${model ? ` · ${esc(model)}` : ""}${effort ? ` · ${esc(effort)}` : ""}</span>${ICONS.chevron}</button>
    </div>
    ${pick}
    <textarea id="qtext" rows="3" placeholder="What should ${esc(agentName(f.agent))} do?">${esc(f.text)}</textarea>
    <div class="qbar"><button type="button" class="qmore" id="qmore">More options</button><span class="grow"></span>
      <button class="btn" id="qgo" type="submit" aria-label="Start"${f.sent || !f.text.trim() ? " disabled" : ""}>${f.sent ? '<span class="spin"></span>' : ICONS.send}</button></div>
  </form>`;
  if (!el) {
    el = document.createElement("div");
    el.id = "quick";
    document.body.appendChild(el);
  }
  const focused = document.activeElement && document.activeElement.id === "qtext";
  el.innerHTML = html;
  const text = document.getElementById("qtext");
  const go = document.getElementById("qgo");
  if (focused) {
    text.focus();
    text.setSelectionRange(text.value.length, text.value.length);
  }
  el.querySelector(".qscrim").addEventListener("click", () => goBack(closeNew));
  // Another project: back to the tiles.
  document.getElementById("qproj").addEventListener("click", () => {
    if (state.places.length === branches.length) return;
    f.quick = false;
    renderQuick();
    state.start = null;
    pickProject();
  });
  text.addEventListener("input", () => {
    f.text = text.value;
    go.disabled = f.sent || !f.text.trim();
  });
  el.querySelectorAll("[data-pick]").forEach((b) =>
    b.addEventListener("click", () => {
      f.pick = f.pick === b.dataset.pick ? null : b.dataset.pick;
      renderQuick();
    })
  );
  el.querySelectorAll("[data-path]").forEach((b) =>
    b.addEventListener("click", () => {
      const p = state.places.find((x) => x.path === b.dataset.path);
      f.path = b.dataset.path;
      if (p) f.agent = p.agent;
      f.pick = null;
      askOptions();
      renderQuick();
    })
  );
  el.querySelectorAll("[data-agent]").forEach((b) =>
    b.addEventListener("click", () => {
      if (f.agent !== b.dataset.agent) {
        f.agent = b.dataset.agent;
        askOptions();
      }
      renderQuick();
    })
  );
  el.querySelectorAll(".qopt[data-key]").forEach((b) =>
    b.addEventListener("click", () => {
      f.picked[b.dataset.key] = b.dataset.id;
      // The reasoning on offer depends on the model: a new model drops the old pick.
      if (b.dataset.key === "modelId") delete f.picked.reasoningEffort;
      renderQuick();
    })
  );
  document.getElementById("qmore").addEventListener("click", () => {
    f.quick = false;
    renderQuick();
    state.screen = "new";
    renderNew();
  });
  document.getElementById("qform").addEventListener("submit", (ev) => {
    ev.preventDefault();
    if (f.sent || !f.text.trim() || !stillLinked()) return;
    f.sent = true;
    if (!nowhereToKeep()) localStorage.setItem(kept(PLACE_KEY), f.path);
    const settings = ["modelId", "reasoningEffort"].filter((k) => f.picked[k]).map((k) => [k, f.picked[k]]);
    send({ type: "start", env: f.path, agent: f.agent, text: f.text, settings });
    text.blur();
    refit(0, 120, 300, 600);
    renderQuick();
  });
}

// The full form, for the agent's model and reasoning before the first message.
function newSession() {
  const place = lastPlace();
  if (!place) return;
  state.start = { path: place.path, agent: place.agent, text: "", sent: false, options: null, picked: {} };
  // A step in the history, so the phone's back gesture closes the form.
  if (layer() !== "new") history.pushState({ layer: "new" }, "");
  state.screen = "new";
  askOptions();
  renderNew();
  document.getElementById("ntext")?.focus();
}

// The model and reasoning a session there would start with, and their choices.
function askOptions() {
  const f = state.start;
  f.options = null;
  f.picked = {};
  send({ type: "options", env: f.path, agent: f.agent });
}

function placeLabel(p) {
  return p.env && p.env !== p.project ? `${p.project} · ${p.env}` : p.project;
}

function renderNew() {
  const f = state.start;
  if (!f) return closeNew();
  const options = state.places
    .map((p) => `<option value="${esc(p.path)}"${p.path === f.path ? " selected" : ""}>${esc(placeLabel(p))}</option>`)
    .join("");
  const agents = AGENTS.map(
    ([key, label]) => `<button class="chip${f.agent === key ? " on" : ""}" data-agent="${key}">${label}</button>`
  ).join("");
  app.innerHTML = `<div class="screen">${statusbar()}
    <div class="hdr">
      <button class="iconbtn" id="back" aria-label="Back">${ICONS.back}</button>
      <div class="grow"><div class="title">New session</div><div class="sub">${esc(state.host)}</div></div>
    </div>
    <div class="newform">
      <label class="nlabel" for="nplace">Project</label>
      <select id="nplace" class="nfield">${options}</select>
      <div class="nlabel">Agent</div>
      <div class="chips">${agents}</div>
      ${f.options ? f.options.map((set) => settingRows(set, f.picked[set.key])).join("") : `<div class="nhint">Loading models…</div>`}
      <label class="nlabel" for="ntext">First message</label>
      <textarea id="ntext" class="nfield" rows="6" placeholder="What should the agent do?">${esc(f.text)}</textarea>
      <button class="btn" id="nstart"${f.sent || !f.text.trim() ? " disabled" : ""}>${f.sent ? "Starting…" : "Start session"}</button>
    </div></div>`;
  document.getElementById("back").addEventListener("click", () => goBack(closeNew));
  const place = document.getElementById("nplace");
  place.addEventListener("change", () => {
    f.path = place.value;
    const p = state.places.find((x) => x.path === f.path);
    if (p) f.agent = p.agent;
    askOptions();
    renderNew();
  });
  app.querySelectorAll("[data-agent]").forEach((b) =>
    b.addEventListener("click", () => {
      if (f.agent === b.dataset.agent) return;
      f.agent = b.dataset.agent;
      askOptions();
      renderNew();
    })
  );
  app.querySelectorAll(".newform .opt").forEach((b) =>
    b.addEventListener("click", () => {
      f.picked[b.dataset.key] = b.dataset.id;
      // The reasoning on offer depends on the model: a new model drops the old pick.
      if (b.dataset.key === "modelId") delete f.picked.reasoningEffort;
      renderNew();
    })
  );
  const text = document.getElementById("ntext");
  const go = document.getElementById("nstart");
  text.addEventListener("input", () => {
    f.text = text.value;
    go.disabled = f.sent || !f.text.trim();
  });
  go.addEventListener("click", () => {
    if (f.sent || !f.text.trim()) return;
    f.sent = true;
    if (!nowhereToKeep()) localStorage.setItem(kept(PLACE_KEY), f.path);
    // The model goes first: the reasoning on offer depends on it.
    const settings = ["modelId", "reasoningEffort"].filter((k) => f.picked[k]).map((k) => [k, f.picked[k]]);
    send({ type: "start", env: f.path, agent: f.agent, text: f.text, settings });
    go.disabled = true;
    go.textContent = "Starting…";
  });
}

function closeNew() {
  document.getElementById("projpick")?.remove();
  const quick = state.start && state.start.quick;
  state.start = null;
  renderQuick();
  if (quick && state.screen !== "new") return;
  state.screen = "inbox";
  render();
}

function renderSession() {
  const s = state.session;
  app.innerHTML = `<div class="screen" data-session="${esc(s.id)}">${statusbar()}
    <div class="hdr">
      <button class="iconbtn" id="back" aria-label="Back">${ICONS.back}</button>
      ${agentAvatar(s.agent)}
      <div class="grow"><div class="title" id="title"></div><div class="sub" id="sub"></div></div>
      <span class="badge" id="badge"><span class="dot"></span><span id="badge-text"></span></span>
      <button class="iconbtn" id="tune" aria-label="Model and reasoning">${ICONS.tune}</button>
    </div>
    <div id="offline"></div>
    <button class="chg" id="changes" hidden></button>
    <div class="older" id="older"></div>
    <div class="logwrap"><div class="log" id="log"></div><button type="button" class="newpill" id="newpill" hidden></button></div>
    <div id="perm"></div>
    <div id="ask"></div>
    <div class="stoprow" id="stoprow" hidden></div>
    <div id="shots" hidden></div>
    <div class="cuts" id="cuts" hidden></div>
    <form class="composer" id="composer">
      <textarea id="draft" rows="1" placeholder="Message ${esc(s.agent || "the agent")}…"></textarea>
      <div class="cbar">
        <button class="iconbtn" id="attach" type="button" aria-label="Attach photos">${ICONS.image}</button>
        <span class="grow"></span>
        <button class="btn" id="go" type="submit" aria-label="Send">${ICONS.send}</button>
      </div>
    </form>
    <input type="file" id="files" accept="image/*" multiple hidden>
    <div id="sheet"></div>
  </div>`;
  document.getElementById("back").addEventListener("click", () => goBack(closeSession));
  document.getElementById("changes").addEventListener("click", openChanges);
  document.getElementById("tune").addEventListener("click", openSettings);
  document.getElementById("stoprow").addEventListener("click", (ev) => {
    if (ev.target.closest("#stop")) stopTapped();
  });
  const log = document.getElementById("log");
  // The turn headers are inside cached entry HTML; their chips open the changes.
  log.addEventListener("click", (ev) => {
    if (ev.target.closest(".tchip")) openChanges();
    const tx = ev.target.closest(".tablex");
    if (tx) openTable(tx.closest(".tablewrap").querySelector("table"));
    const card = ev.target.closest(".diagram");
    if (card) diagramTapped(ev.target, card);
  });
  list.mount(log);
  state.unseen = 0;
  document.getElementById("newpill").addEventListener("click", () => {
    list.scrollToEnd();
    seen();
  });
  const pending = document.createElement("div");
  pending.id = "pending";
  pending.className = "pending";
  log.appendChild(pending);
  renderPending();
  const draft = document.getElementById("draft");
  draft.value = state.draft;
  const grow = () => {
    draft.style.height = "auto";
    draft.style.height = `${Math.min(140, draft.scrollHeight)}px`;
  };
  if (state.draft) grow();
  draft.addEventListener("input", () => {
    state.draft = draft.value;
    keepDraft(s.id, state.draft);
    grow();
    renderComposer();
  });
  wireCuts(draft, log);
  // The keyboard's return key writes a new line: a prompt often has several, and
  // a half-written one sent by accident sets the agent to work. Only the button
  // sends, and sending puts the keyboard away so the reply has the screen.
  document.getElementById("attach").addEventListener("click", () => document.getElementById("files").click());
  const files = document.getElementById("files");
  files.addEventListener("change", () => {
    for (const f of files.files) addShot(f);
    files.value = "";
  });
  document.getElementById("shots").addEventListener("click", (ev) => {
    const chip = ev.target.closest("button[data-shot]");
    if (!chip) return;
    state.shots.splice(Number(chip.dataset.shot), 1);
    renderShots();
    renderComposer();
  });
  renderShots();
  document.getElementById("composer").addEventListener("submit", (ev) => {
    ev.preventDefault();
    submit();
  });
  syncHeader();
  renderOffline();
  renderPermission();
  renderQuestion();
  renderComposer();
  renderOlder();
  renderSheet();
}

// An answer only counts once the computer has it: offline the card stays, and says why.
function stillLinked() {
  if (state.connected) return true;
  toast("Not connected. Try again once it reconnects.");
  return false;
}

// The session on screen got news: everything but the log and the composer is
// redrawn in place, so the keyboard stays up and the list keeps its place.
function refreshSession() {
  const bar = app.querySelector(".statusbar");
  if (bar) bar.outerHTML = statusbar();
  syncHeader();
  renderOffline();
  renderPermission();
  renderQuestion();
  renderComposer();
  renderOlder();
  renderSheet();
}

// What is being written survives a switch of session, a reload, or the phone
// closing the app; each session keeps its own.
const DRAFT_KEY = "arakun.draft:";
// Where the last session started, offered first for the next.
const PLACE_KEY = "arakun.place";

// A key of what the page keeps, per computer on the account host: drafts,
// places and shortcuts of one computer mean nothing on another. Served by the
// computer itself the key stays as it was.
function kept(key) {
  return RELAY.scoped(key, relay ? relay.label : null);
}
// Between computers on the account host there is no key to keep anything under.
function nowhereToKeep() {
  return !!account && !relay;
}
function keepDraft(id, text) {
  if (nowhereToKeep()) return;
  try {
    if (text) localStorage.setItem(kept(DRAFT_KEY) + id, text);
    else localStorage.removeItem(kept(DRAFT_KEY) + id);
  } catch (e) {
    // Storage full or off: the draft lives only in memory.
  }
}

// `now` stops a running turn for this prompt instead of queueing it behind.
function submit(now = false) {
  const s = state.session;
  if (!s) return;
  const text = state.draft.trim();
  const shots = state.shots;
  if (!text && !shots.length) return;
  // Without the computer the message waits on the phone and goes out on reconnect —
  // photos cannot: they need the socket now.
  const queued = !state.connected;
  if (queued && shots.length) {
    toast("Photos wait for a connection");
    return;
  }
  const msg = { type: "send", session: s.id, text };
  if (now) msg.now = true;
  if (shots.length) {
    msg.uploads = shots.map((sh, i) => {
      const id = `${Date.now()}-${i}-${Math.random().toString(36).slice(2, 7)}`;
      sendShot(sh, id, s.id);
      return id;
    });
    if (!text) msg.text = shots.length === 1 ? "(photo)" : `(${shots.length} photos)`;
    state.shots = [];
    renderShots();
  }
  if (!queued) send(msg);
  state.pending.push({ text: msg.text, after: s.from + s.entries.length, queued });
  state.draft = "";
  keepDraft(s.id, "");
  const draft = document.getElementById("draft");
  draft.value = "";
  draft.style.height = "auto";
  draft.blur();
  // The keyboard goes down: the page gets its height back, with no gap under.
  refit(0, 120, 300, 600);
  renderComposer();
  renderPending();
  list.scrollToEnd();
}

function unseen(n) {
  state.unseen = (state.unseen || 0) + n;
  const b = document.getElementById("newpill");
  if (!b) return;
  b.innerHTML = `${ICONS.down}<span>${state.unseen} new</span>`;
  b.hidden = false;
}

function seen() {
  state.unseen = 0;
  const b = document.getElementById("newpill");
  if (b) b.hidden = true;
}

// A sent message shows at once, faded, until the computer's own copy of it
// arrives: each prompt entry after the moment of sending takes one away.
function settlePending() {
  const s = state.session;
  if (!s || !state.pending.length) return;
  let users = 0;
  for (let i = 0; i < s.entries.length; i++) {
    const e = s.entries[i];
    if (e.type === "user" && s.from + i >= state.pending[0].after) users++;
  }
  if (users) {
    state.pending.splice(0, users);
    renderPending();
  }
}

// What was written while the computer was away goes out once it is back.
function flushQueued() {
  const s = state.session;
  if (!s) return;
  let any = false;
  for (const p of state.pending) {
    if (!p.queued) continue;
    send({ type: "send", session: s.id, text: p.text });
    p.queued = false;
    any = true;
  }
  if (any) renderPending();
}

function renderPending() {
  const el = document.getElementById("pending");
  if (!el) return;
  el.innerHTML = state.pending
    .map((p) => `<div class="entry user"><div class="bubble"><div class="text">${esc(p.text)}</div></div><div class="meta"><span>${p.queued ? "Waiting to send · sends when reconnected" : "Sending…"}</span></div></div>`)
    .join("");
}

// Stopping takes two taps a few seconds apart: a thumb brushes the screen too easily.
// With something written, Stop stops the turn and sends it.
const STOP_WINDOW = 3000;
let stopTimer;
function stopTapped() {
  const s = state.session;
  if (!s || s.status !== "working") return;
  if (state.stopArmed && Date.now() - state.stopArmed < STOP_WINDOW) {
    state.stopArmed = 0;
    clearTimeout(stopTimer);
    if (!stillLinked()) return;
    if (hasDraft()) {
      submit(true);
    } else {
      send({ type: "cancel", session: s.id });
      toast("Stopping…");
    }
  } else {
    state.stopArmed = Date.now();
    clearTimeout(stopTimer);
    stopTimer = setTimeout(() => {
      state.stopArmed = 0;
      renderStopRow();
    }, STOP_WINDOW);
  }
  renderStopRow();
}

function hasDraft() {
  return state.draft.trim().length > 0 || state.shots.length > 0;
}

// mm:ss, hours when it comes to that.
function fmtClock(ms) {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const sec = total % 60;
  const two = (n) => String(n).padStart(2, "0");
  return h ? `${h}:${two(m)}:${two(sec)}` : `${two(m)}:${two(sec)}`;
}

// How long the turn has run: what the computer last said, plus the time since.
function liveElapsed() {
  const s = state.session;
  if (!s || s.elapsed_ms == null) return null;
  return s.elapsed_ms + (s.elapsedAt ? Date.now() - s.elapsedAt : 0);
}

// Over the composer while the agent works: the clock, what it costs so far, and Stop.
function renderStopRow() {
  const row = document.getElementById("stoprow");
  const s = state.session;
  if (!row || !s) return;
  const working = s.status === "working";
  row.hidden = !working;
  if (!working) {
    state.stopArmed = 0;
    return;
  }
  const bits = [];
  const ms = liveElapsed();
  if (ms != null) bits.push(fmtClock(ms));
  if (s.usage && s.usage.summary) bits.push(s.usage.summary);
  const armed = state.stopArmed && Date.now() - state.stopArmed < STOP_WINDOW;
  const [word, again, hint] = hasDraft()
    ? ["Stop and send", "Send now?", "Tap again to stop and send"]
    : ["Stop", "Stop?", "Tap again to stop"];
  row.innerHTML = `<span class="live" id="live">${esc(bits.join(" · ")) || "Working…"}</span>
    <button class="btn danger small${armed ? " armed" : ""}" id="stop" aria-label="${armed ? hint : word}">${ICONS.stop}<span>${armed ? again : word}</span></button>`;
}

function syncHeader() {
  const s = state.session;
  if (!s || state.screen !== "session") return;
  const row = state.inbox.find((x) => x.id === s.id);
  if (row) {
    if (row.title) s.title = row.title;
    s.status = row.status;
    s.place = row.project + (row.env && row.env !== row.project ? ` · ${row.env}` : "");
  }
  const title = document.getElementById("title");
  if (!title) return;
  title.textContent = s.title || "New session";
  const bits = [s.place, s.agent];
  // While the agent works, the stop row carries the cost.
  if (s.usage && s.usage.summary && s.status !== "working") bits.push(s.usage.summary);
  document.getElementById("sub").textContent = bits.filter(Boolean).join(" · ");
  const badge = document.getElementById("badge");
  badge.className = `badge ${s.status}`;
  document.getElementById("badge-text").textContent = statusLabel(s.status);
  const chg = document.getElementById("changes");
  const c = s.changes;
  chg.hidden = !c || !c.files;
  if (c && c.files) {
    chg.innerHTML = `${ICONS.diff}<span>${c.files} file${c.files === 1 ? "" : "s"} changed</span>${counts(c.added, c.removed)}`;
  }
  renderComposer();
}

function counts(added, removed) {
  if (!added && !removed) return "";
  return `<span class="stats">${added ? `<span class="add">+${added}</span>` : ""} ${removed ? `<span class="del">−${removed}</span>` : ""}</span>`;
}

// ---- changes: the files a session edited, and each diff, read-only ----------

function openChanges(step = true) {
  const s = state.session;
  if (!s) return;
  if (step && layer() === "session") history.pushState({ layer: "changes" }, "");
  state.sheet = { kind: "changes", turns: null };
  send({ type: "changes", session: s.id });
  renderSheet();
}

// `diffs` are the file's edits in the turn, as [entry, index] pairs; all are shown, in order.
function openDiff(name, diffs) {
  const s = state.session;
  if (!s) return;
  if (layer() === "changes") history.pushState({ layer: "diff" }, "");
  state.sheet = { kind: "diff", name, diffs, got: {} };
  for (const [entry, index] of diffs) send({ type: "diff", session: s.id, entry, index });
  renderSheet();
}

// ---- back: sheet → diff list → session → inbox, by the phone's own gesture ----

// Which step of the history is on screen: "session", "changes", "diff" or none.
function layer() {
  return (history.state && history.state.layer) || null;
}

// The on-screen back buttons go through the history when they put a step there,
// so the phone's back and theirs stay one and the same.
function goBack(close) {
  if (layer()) history.back();
  else close();
}

window.addEventListener("popstate", () => {
  const at = layer();
  const sh = state.sheet;
  if (sh && sh.kind === "diff" && at !== "diff") {
    if (at === "changes") openChanges(false);
    else {
      state.sheet = null;
      renderSheet();
    }
  } else if (sh && at !== "changes" && at !== "diff") {
    state.sheet = null;
    renderSheet();
  }
  if (state.session && !at) closeSession();
  if ((state.screen === "new" || (state.start && state.start.quick) || document.getElementById("projpick")) && at !== "new") closeNew();
});

// The session's model and reasoning, as it runs; a pick applies from the next message.
function openSettings() {
  const s = state.session;
  if (!s) return;
  if (layer() === "session") history.pushState({ layer: "changes" }, "");
  state.sheet = { kind: "settings", settings: null };
  send({ type: "settings", session: s.id });
  renderSheet();
}

// One setting's choices as rows; the current one is checked.
function settingRows(set, picked) {
  const current = picked ?? set.current;
  return `<div class="glabel">${esc(set.label)}</div><div class="opts">${set.options
    .map(
      (o) => `<button class="opt${o.id === current ? " on" : ""}" data-key="${esc(set.key)}" data-id="${esc(o.id)}"${o.disabled ? " disabled" : ""}>
        <span class="on-mark">${o.id === current ? ICONS.check : ""}</span>
        <span class="oname">${esc(o.name)}</span>${o.detail ? `<span class="odetail">${esc(o.detail)}</span>` : ""}
      </button>`
    )
    .join("")}</div>`;
}

function closeSheet() {
  const sh = state.sheet;
  if (!sh) return;
  // A diff goes back to the list it was picked from.
  if (sh.kind === "diff") openChanges(false);
  else {
    state.sheet = null;
    renderSheet();
  }
}

function renderSheet() {
  const el = document.getElementById("sheet");
  const sh = state.sheet;
  if (!el) return;
  if (!sh) {
    el.innerHTML = "";
    el.hidden = true;
    renderCuts();
    return;
  }
  el.hidden = false;
  if (sh.kind === "cuts") return renderCutsSheet(el);
  if (sh.kind === "cut") return renderCutSheet(el, sh);
  if (sh.kind === "settings") {
    const list = sh.settings;
    const body = !list
      ? `<div class="empty"><div class="hint">Loading…</div></div>`
      : list.length
        ? list.map((set) => settingRows(set)).join("")
        : `<div class="empty"><div class="hint">This agent has not said what it offers yet. Try again in a moment.</div></div>`;
    el.innerHTML = `<div class="sheet"><div class="hdr">
        <button class="iconbtn" id="sheet-back" aria-label="Back">${ICONS.back}</button>
        <div class="grow"><div class="title">Model and reasoning</div><div class="sub">From the next message on</div></div>
      </div><div class="sheet-body setform">${body}</div></div>`;
    el.querySelectorAll(".opt").forEach((b) =>
      b.addEventListener("click", () => {
        send({ type: "setting", session: state.session.id, key: b.dataset.key, value: b.dataset.id });
        // Shown at once; the computer's answer confirms it.
        const set = sh.settings.find((x) => x.key === b.dataset.key);
        if (set) set.current = b.dataset.id;
        renderSheet();
      })
    );
    document.getElementById("sheet-back").addEventListener("click", closeSheet);
    return;
  }
  let body;
  if (sh.kind === "changes") {
    const turns = sh.turns;
    if (!turns) body = `<div class="empty"><div class="hint">Loading…</div></div>`;
    else if (!turns.length) body = `<div class="empty"><div class="big">✓</div><div>No files changed</div></div>`;
    else {
      body = `<div class="sheet-body">${turns
        .map(
          (t) => `<div class="turn">
            <div class="turn-h"><span class="turn-t">${esc(t.text) || "<i>Prompt</i>"}</span><span class="turn-when">${t.at ? ago(t.at) : ""}</span></div>
            ${t.files
              .map((f) => {
                const more = f.diffs.length > 1 ? `<span class="more">${f.diffs.length} edits</span>` : "";
                return `<button class="frow" data-name="${esc(f.name)}" data-diffs="${esc(JSON.stringify(f.diffs))}">
                  <span class="fname"><span class="n">${esc(f.name)}</span><span class="d">${esc(dirOf(f.path))}</span></span>
                  ${more}${counts(f.added, f.removed)}
                </button>`;
              })
              .join("")}
          </div>`,
        )
        .join("")}</div>`;
    }
    el.innerHTML = `<div class="sheet"><div class="hdr">
        <button class="iconbtn" id="sheet-back" aria-label="Back">${ICONS.back}</button>
        <div class="grow"><div class="title">Changes</div><div class="sub">${esc(state.session.title || "This session")}</div></div>
      </div>${body}</div>`;
    el.querySelectorAll(".frow").forEach((b) => {
      b.addEventListener("click", () => openDiff(b.dataset.name, JSON.parse(b.dataset.diffs)));
    });
  } else {
    const blocks = sh.diffs.map(([e, i], n) => {
      const d = sh.got[`${e}:${i}`];
      const label = sh.diffs.length > 1 ? `<span class="dnth">Edit ${n + 1} of ${sh.diffs.length}</span>` : "";
      if (!d) return `<div class="dblock"><div class="dmeta">${label}<span class="dnote">Loading…</span></div></div>`;
      const note = d.new_file ? "New file" : d.deleted ? "Deleted" : "";
      const rows = d.lines
        .map((l) => {
          if (typeof l === "string") return `<div class="dl hunk">${esc(l)}</div>`;
          const [mark, num, text] = l;
          const k = mark === "+" ? "a" : mark === "-" ? "r" : "c";
          return `<div class="dl ${k}"><span class="ln">${num ?? ""}</span><span class="mk">${mark === " " ? "" : mark === "-" ? "−" : "+"}</span><span class="tx">${esc(text) || " "}</span></div>`;
        })
        .join("");
      return `<div class="dblock">
        <div class="dmeta">${label}${n === 0 ? `<span class="dpath">${esc(d.path)}</span>` : ""}${note ? `<span class="dnote">${note}</span>` : ""}${counts(d.added, d.removed)}</div>
        <div class="dwrap"><div class="dlines">${rows}</div></div>
        ${d.cut ? `<div class="dcut">… ${d.cut} more lines</div>` : ""}
      </div>`;
    });
    el.innerHTML = `<div class="sheet"><div class="hdr">
        <button class="iconbtn" id="sheet-back" aria-label="Back">${ICONS.back}</button>
        <div class="grow"><div class="title">${esc(sh.name)}</div><div class="sub">Read-only · edit on the computer</div></div>
      </div><div class="sheet-body">${blocks.join("")}</div></div>`;
  }
  document.getElementById("sheet-back").addEventListener("click", () => goBack(closeSheet));
}

function dirOf(path) {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

function renderOlder() {
  const el = document.getElementById("older");
  const s = state.session;
  if (!el || !s) return;
  el.textContent = s.from > 0 ? (state.loadingOlder ? "Loading…" : `${s.from} earlier`) : "";
  el.style.display = s.from > 0 ? "" : "none";
}

// Taps that undo nothing take two: the first arms the button for a few seconds.
const ARM_WINDOW = 3000;
function armed(btn, label) {
  if (btn.dataset.armed && Date.now() - Number(btn.dataset.armed) < ARM_WINDOW) return true;
  const was = btn.dataset.label || btn.textContent;
  btn.dataset.label = was;
  btn.dataset.armed = String(Date.now());
  btn.textContent = label;
  btn.classList.add("armed");
  setTimeout(() => {
    if (!btn.isConnected) return;
    delete btn.dataset.armed;
    btn.textContent = was;
    btn.classList.remove("armed");
  }, ARM_WINDOW);
  return false;
}

// What the agent wants to do, with what it would touch, and the answers: allow
// once and deny side by side, and "always" apart, since it lasts the session.
// The card folds like the question does, to read the log first.
function renderPermission() {
  const el = document.getElementById("perm");
  const s = state.session;
  if (!el || !s) return;
  const p = s.permission;
  if (!p) {
    el.innerHTML = "";
    el.dataset.id = "";
    return;
  }
  if (el.dataset.id === p.tool_call_id && el.firstChild) return;
  el.dataset.id = p.tool_call_id;
  const folded = state.permFolded === p.tool_call_id;
  const tool = s.entries.find((e) => e.type === "tool" && e.id === p.tool_call_id);
  const kind = tool && tool.kind ? String(tool.kind) : "";
  const files = tool && tool.diffs ? tool.diffs.map((d) => d.path.split(/[\\/]/).pop()) : [];
  const where = [s.place, ...files.slice(0, 3)].filter(Boolean).join(" · ");
  const always = p.options.filter((o) => o.kind === "allow_always");
  const main = p.options.filter((o) => o.kind !== "allow_always");
  el.innerHTML = `<div class="perm${folded ? " folded" : ""}">
    <button type="button" class="phead" id="perm-fold" aria-expanded="${folded ? "false" : "true"}">
      <span class="label">${ICONS.alert}Asks permission</span>
      ${kind ? `<span class="kind">${esc(kind)}</span>` : ""}
      <span class="peek">${esc(p.title)}</span>
      ${ICONS.chevron}
    </button>
    <div class="pbody">
      <div class="what"><code>${esc(p.title)}</code></div>
      ${where ? `<div class="where">${esc(where)}</div>` : ""}
      <div class="opts">${main
        .map((o) =>
          o.kind.startsWith("allow")
            ? `<button type="button" class="btn hold" data-opt="${esc(o.id)}" data-hold="1"><i></i><span>Hold to ${esc(o.name.toLowerCase())}</span></button>`
            : `<button type="button" class="btn outline" data-opt="${esc(o.id)}">${esc(o.name)}</button>`
        )
        .reverse()
        .join("")}</div>
      ${always
        .map((o) => `<button type="button" class="always" data-opt="${esc(o.id)}" data-always="1"><span class="warn">!</span><span class="grow"><span class="t">${esc(o.name)}</span><span class="s">For the rest of this session · tap twice</span></span></button>`)
        .join("")}
    </div>
  </div>`;
  const card = el.firstElementChild;
  document.getElementById("perm-fold").addEventListener("click", (ev) => {
    const fold = !card.classList.contains("folded");
    state.permFolded = fold ? p.tool_call_id : null;
    card.classList.toggle("folded", fold);
    ev.currentTarget.setAttribute("aria-expanded", fold ? "false" : "true");
  });
  const answer = (b) => {
    if (!stillLinked()) return;
    const o = p.options.find((x) => x.id === b.dataset.opt);
    send({ type: "permission", session: s.id, option: b.dataset.opt });
    s.permission = null;
    renderPermission();
    if (o) toast(o.kind.startsWith("allow") ? `${o.name} ✓` : o.name);
  };
  el.querySelectorAll("[data-opt]").forEach((b) => {
    if (b.dataset.hold) {
      // Allow answers only once the finger has stayed down the whole fill.
      let timer = 0;
      const stop = () => {
        clearTimeout(timer);
        b.classList.remove("holding");
      };
      b.addEventListener("pointerdown", (ev) => {
        ev.preventDefault();
        b.classList.add("holding");
        timer = setTimeout(() => {
          stop();
          if (navigator.vibrate) navigator.vibrate(15);
          answer(b);
        }, 700);
      });
      ["pointerup", "pointerleave", "pointercancel"].forEach((t) => b.addEventListener(t, stop));
      b.addEventListener("click", (ev) => {
        ev.preventDefault();
        if (!b.classList.contains("holding")) toast("Hold to allow");
      });
      return;
    }
    b.addEventListener("click", () => {
      if (b.dataset.always && !armed(b.querySelector(".t"), "Tap again to always allow")) return;
      answer(b);
    });
  });
}

// The agent's questionnaire, one question at a time ("1 of 3"), with Skip and
// Next or Answer always in reach under it. Each question has its options (one or
// several, as it allows) and a line for an answer in your own words. Picks and
// typed words survive re-renders in `state.ask` until it is sent. The card folds
// to its header so the log underneath can be read first.
function renderQuestion() {
  const el = document.getElementById("ask");
  const s = state.session;
  if (!el || !s) return;
  const q = s.question;
  if (!q) {
    el.innerHTML = "";
    el.dataset.id = "";
    state.ask = null;
    return;
  }
  // A state message while you type must not rebuild the card under your fingers.
  if (el.dataset.id === q.tool_call_id && el.firstChild) return;
  el.dataset.id = q.tool_call_id;
  if (!state.ask || state.ask.id !== q.tool_call_id) {
    state.ask = { id: q.tool_call_id, picked: q.questions.map(() => []), typed: q.questions.map(() => ""), folded: false, page: 0 };
  }
  const ask = state.ask;
  const total = q.questions.length;
  const answerOf = (i) => {
    const qq = q.questions[i];
    const parts = ask.picked[i].map((o) => qq.options[o]);
    const typed = ask.typed[i].trim();
    if (typed) parts.push(typed);
    return parts.join(", ");
  };
  const answers = () => q.questions.map((_, i) => answerOf(i));
  const complete = () => answers().every((a) => a.length > 0);
  const draw = () => {
    const i = ask.page;
    const qq = q.questions[i];
    const last = i === total - 1;
    const segs = q.questions.map((_, k) => `<i class="${k <= i ? "on" : ""}"></i>`).join("");
    el.innerHTML = `<div class="ask${ask.folded ? " folded" : ""}">
    <button type="button" class="ahead" id="ask-fold" aria-expanded="${ask.folded ? "false" : "true"}">
      <span class="label">Question</span>
      <span class="peek">${esc(qq.question)}</span>
      ${total > 1 ? `<span class="n"><span class="segs">${segs}</span>${i + 1} of ${total}</span>` : ""}
      ${ICONS.chevron}
    </button>
    <div class="body">
      <div class="q">
        ${qq.topic && total > 1 ? `<div class="topic">${esc(qq.topic)}</div>` : ""}
        <div class="text">${esc(qq.question)}</div>
        ${qq.multi ? `<div class="multi">Pick any that apply</div>` : ""}
        <div class="opts">${qq.options
          .map((o, j) => `<button type="button" class="opt${ask.picked[i].includes(j) ? " on" : ""}${qq.multi ? " multi" : ""}" data-o="${j}"><span class="mark"></span><span>${esc(o)}</span></button>`)
          .join("")}</div>
        <input class="own" placeholder="Or answer in your own words" value="${esc(ask.typed[i])}" autocomplete="off" enterkeyhint="${last ? "send" : "next"}">
      </div>
    </div>
    <div class="actions">
      <button type="button" class="btn outline" id="ask-skip">Skip</button>
      ${i > 0 ? `<button type="button" class="btn outline" id="ask-prev">Back</button>` : ""}
      <button type="button" class="btn grow" id="ask-send">${last ? "Answer" : "Next question"}</button>
    </div>
  </div>`;
    const card = el.firstElementChild;
    const go = document.getElementById("ask-send");
    const ready = () => (last ? complete() : answerOf(i).length > 0);
    const sync = () => (go.disabled = !ready());
    sync();
    document.getElementById("ask-fold").addEventListener("click", (ev) => {
      ask.folded = !ask.folded;
      card.classList.toggle("folded", ask.folded);
      ev.currentTarget.setAttribute("aria-expanded", ask.folded ? "false" : "true");
    });
    el.querySelectorAll(".opt").forEach((b) =>
      b.addEventListener("click", () => {
        const j = Number(b.dataset.o);
        const at = ask.picked[i].indexOf(j);
        if (qq.multi) {
          if (at >= 0) ask.picked[i].splice(at, 1);
          else ask.picked[i].push(j);
        } else {
          ask.picked[i] = at >= 0 ? [] : [j];
        }
        el.querySelectorAll(".opt").forEach((x) => x.classList.toggle("on", ask.picked[i].includes(Number(x.dataset.o))));
        sync();
      })
    );
    const inp = el.querySelector(".own");
    inp.addEventListener("input", () => {
      ask.typed[i] = inp.value;
      sync();
    });
    inp.addEventListener("keydown", (ev) => {
      if (ev.key !== "Enter" || ev.isComposing) return;
      ev.preventDefault();
      if (ready()) go.click();
    });
    const prev = document.getElementById("ask-prev");
    if (prev)
      prev.addEventListener("click", () => {
        ask.page--;
        draw();
      });
    go.addEventListener("click", () => {
      if (!ready()) return;
      if (!last) {
        ask.page++;
        draw();
        return;
      }
      if (!stillLinked()) return;
      send({ type: "answer", session: s.id, answers: answers() });
      s.question = null;
      renderQuestion();
    });
    // Skipping drops the whole questionnaire, so it asks to be sure.
    document.getElementById("ask-skip").addEventListener("click", (ev) => {
      if (!armed(ev.currentTarget, "Skip all?")) return;
      if (!stillLinked()) return;
      send({ type: "skip_question", session: s.id });
      s.question = null;
      renderQuestion();
    });
  };
  draw();
}

// ---- photos into the composer ----------------------------------------------
// Photos picked on the phone: drawn small and re-encoded as JPEG so what goes
// over the socket is a few hundred kilobytes, then cut into pieces small enough
// for a relay frame; the computer saves each one as a pasted image.

async function addShot(file) {
  if (!file || !/^image\//.test(file.type)) return;
  try {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.src = url;
    await img.decode();
    const max = 1568;
    const scale = Math.min(1, max / Math.max(img.width, img.height));
    const w = Math.max(1, Math.round(img.width * scale));
    const h = Math.max(1, Math.round(img.height * scale));
    const c = document.createElement("canvas");
    c.width = w;
    c.height = h;
    c.getContext("2d").drawImage(img, 0, 0, w, h);
    URL.revokeObjectURL(url);
    const blob = await new Promise((res) => c.toBlob(res, "image/jpeg", 0.82));
    if (!blob) return;
    const b64 = await new Promise((res) => {
      const r = new FileReader();
      r.onload = () => res(String(r.result).split(",")[1] || "");
      r.readAsDataURL(blob);
    });
    if (!b64) return;
    const thumb = c.toDataURL("image/jpeg", 0.3);
    state.shots.push({ name: file.name || "photo.jpg", b64, thumb });
    renderShots();
    renderComposer();
  } catch (e) {
    toast("Could not read that photo");
  }
}

// The pieces a photo travels in: the relay's frames are small, so each carries
// this much of the base64 at most.
const SHOT_PIECE = 160 * 1024;

function sendShot(shot, id, session) {
  const of = Math.max(1, Math.ceil(shot.b64.length / SHOT_PIECE));
  for (let i = 0; i < of; i++) {
    send({ type: "upload", session, upload: id, name: shot.name, part: i, of,
      data: shot.b64.slice(i * SHOT_PIECE, (i + 1) * SHOT_PIECE) });
  }
}

function renderShots() {
  const row = document.getElementById("shots");
  if (!row) return;
  row.innerHTML = state.shots
    .map((sh, i) => `<button class="shotchip" data-shot="${i}" aria-label="Remove photo"><img src="${sh.thumb}" alt=""><b>\xd7</b></button>`)
    .join("");
  row.hidden = !state.shots.length;
}

function renderComposer() {
  const go = document.getElementById("go");
  const s = state.session;
  if (!go || !s) return;
  go.disabled = state.draft.trim().length === 0 && !state.shots.length;
  renderStopRow();
  renderCuts();
}

// ---- shortcuts --------------------------------------------------------------
// Phrases kept on this phone, to send without typing them again. One either
// fills the box (shown while writing) or sends at once (shown while the agent
// waits). The order is the user's own: a finger learns where each one sits.
const CUTS_KEY = "arakun.shortcuts";
// Where a number goes each time the shortcut is used.
const BLANK = "{number}";
const CUT_SHORT = 24;

function loadCuts() {
  try {
    const v = JSON.parse(localStorage.getItem(kept(CUTS_KEY)) || "[]");
    return Array.isArray(v) ? v.filter((c) => c && c.id && typeof c.text === "string" && c.text) : [];
  } catch (e) {
    return [];
  }
}

function storeCuts(cuts) {
  if (nowhereToKeep()) return false;
  try {
    localStorage.setItem(kept(CUTS_KEY), JSON.stringify(cuts));
    return true;
  } catch (e) {
    toast("Could not save the shortcut");
    return false;
  }
}

// A blank has to be filled first, so that one never sends on a tap.
function cutSends(c) {
  return !!c.send && !c.text.includes(BLANK);
}

function cutName(text) {
  const t = text.replace(BLANK, "…").replace(/\s+/g, " ").trim();
  return t.length > 28 ? `${t.slice(0, 27).trimEnd()}…` : t;
}

function renderCuts() {
  const row = document.getElementById("cuts");
  const draft = document.getElementById("draft");
  const s = state.session;
  if (!row || !draft || !s) return;
  const writing = document.activeElement === draft;
  const waiting = s.status !== "working" && !state.draft.trim() && !state.shots.length;
  if (!writing && !waiting) {
    row.hidden = true;
    return;
  }
  const cuts = loadCuts();
  const shown = cuts.filter((c) => cutSends(c) === !writing);
  const html =
    shown
      .map((c) => `<button type="button" class="chip${writing ? "" : " go"}" data-cut="${esc(c.id)}">${esc(c.name || cutName(c.text))}</button>`)
      .join("") +
    `<button type="button" class="chip more" data-cuts="1" aria-label="Shortcuts">${ICONS.plus}${shown.length ? "" : "<span>Shortcuts</span>"}</button>`;
  // Typing redraws the composer on every key: the row is left alone unless it changed.
  if (row.dataset.html !== html) {
    row.dataset.html = html;
    row.innerHTML = html;
  }
  row.hidden = false;
}

function useCut(id) {
  const s = state.session;
  const draft = document.getElementById("draft");
  const cuts = loadCuts();
  const c = cuts.find((x) => x.id === id);
  if (!s || !draft || !c) return;
  c.n = (c.n || 0) + 1;
  storeCuts(cuts);
  if (cutSends(c)) {
    state.draft = c.text;
    submit();
    return;
  }
  const at = c.text.indexOf(BLANK);
  const before = draft.value && !/\s$/.test(draft.value) ? `${draft.value} ` : draft.value;
  const numeric = at >= 0;
  // The keyboard only changes kind when the field is focused again.
  if (numeric !== (draft.inputMode === "numeric")) {
    state.cutSwitch = true;
    draft.blur();
    draft.inputMode = numeric ? "numeric" : "text";
    state.cutSwitch = false;
  }
  draft.value = before + c.text.replace(BLANK, "");
  draft.focus();
  const pos = numeric ? before.length + at : draft.value.length;
  draft.setSelectionRange(pos, pos);
  draft.dispatchEvent(new Event("input"));
}

function wireCuts(draft, log) {
  const row = document.getElementById("cuts");
  // A tap on the row must not take the keyboard down.
  row.addEventListener("mousedown", (ev) => ev.preventDefault());
  row.addEventListener("click", (ev) => {
    const chip = ev.target.closest("button");
    if (!chip) return;
    if (chip.dataset.cuts) openCuts();
    else useCut(chip.dataset.cut);
  });
  draft.addEventListener("focus", renderCuts);
  draft.addEventListener("blur", () => {
    if (state.cutSwitch) return;
    draft.inputMode = "text";
    // After the tap that took the focus has landed on its button.
    setTimeout(renderCuts, 200);
  });
  // Holding a message of yours offers to keep it.
  let hold;
  const drop = () => clearTimeout(hold);
  log.addEventListener("pointerdown", (ev) => {
    const b = ev.target.closest(".entry.user .bubble");
    drop();
    if (!b) return;
    const t = b.querySelector(".text");
    const text = t ? t.textContent.trim() : "";
    if (!text) return;
    hold = setTimeout(() => {
      if (navigator.vibrate) navigator.vibrate(10);
      openCut({ text });
    }, 600);
  });
  for (const type of ["pointerup", "pointercancel", "pointerleave"]) log.addEventListener(type, drop);
  log.addEventListener("scroll", drop, true);
}

function openCuts() {
  if (layer() === "session") history.pushState({ layer: "changes" }, "");
  state.sheet = { kind: "cuts" };
  renderSheet();
}

// The form for a new shortcut, or for the one being changed.
function openCut(seed, fromList) {
  if (layer() === "session") history.pushState({ layer: "changes" }, "");
  const text = seed.text || "";
  state.sheet = {
    kind: "cut",
    fromList: !!fromList,
    named: !!seed.id,
    cut: {
      id: seed.id || null,
      n: seed.n || 0,
      name: seed.name || cutName(text),
      text,
      send: seed.id ? !!seed.send : text.length > 0 && text.length <= CUT_SHORT,
    },
  };
  renderSheet();
}

function leaveCut(sh) {
  if (sh.fromList) {
    state.sheet = { kind: "cuts" };
    renderSheet();
  } else if (layer() === "changes") history.back();
  else {
    state.sheet = null;
    renderSheet();
  }
}

function cutRows(title, note, items) {
  if (!items.length) return "";
  return `<div class="glabel cutlabel"><span>${title}</span><span>${note}</span></div><div class="opts">${items
    .map(
      (c, i) => `<div class="opt cutrow">
        <button type="button" class="cutmain" data-edit="${esc(c.id)}"><span class="oname">${esc(c.name || cutName(c.text))}</span><span class="cuttext">${esc(c.text)}</span></button>
        <span class="odetail">${c.n || 0}\xd7</span>
        <button type="button" class="iconbtn" data-up="${esc(c.id)}" aria-label="Move up"${i ? "" : " disabled"}>${ICONS.send}</button>
      </div>`
    )
    .join("")}</div>`;
}

function renderCutsSheet(el) {
  const cuts = loadCuts();
  const body = cuts.length
    ? cutRows("When you write", "Fills the box", cuts.filter((c) => !cutSends(c))) +
      cutRows("When the agent waits", "Sends right away", cuts.filter(cutSends)) +
      `<div class="cutnote">Tap one to change it. The arrow moves it up its row of buttons.</div>`
    : `<div class="empty"><div class="hint">No shortcuts yet. Hold a message you sent to keep it, or tap + to write one.</div></div>`;
  el.innerHTML = `<div class="sheet"><div class="hdr">
      <button class="iconbtn" id="sheet-back" aria-label="Back">${ICONS.back}</button>
      <div class="grow"><div class="title">Shortcuts</div><div class="sub">Kept on this phone</div></div>
      <button class="iconbtn" id="cut-new" aria-label="New shortcut">${ICONS.plus}</button>
    </div><div class="sheet-body setform">${body}</div></div>`;
  document.getElementById("sheet-back").addEventListener("click", () => {
    if (layer() === "changes") history.back();
    else closeSheet();
  });
  document.getElementById("cut-new").addEventListener("click", () => openCut({ text: state.draft.trim() }, true));
  el.querySelectorAll("[data-edit]").forEach((b) =>
    b.addEventListener("click", () => {
      const c = loadCuts().find((x) => x.id === b.dataset.edit);
      if (c) openCut(c, true);
    })
  );
  el.querySelectorAll("[data-up]").forEach((b) =>
    b.addEventListener("click", () => {
      const all = loadCuts();
      const i = all.findIndex((x) => x.id === b.dataset.up);
      if (i < 0) return;
      // Up within its own group: the one before it that sends the same way.
      let j = i - 1;
      while (j >= 0 && cutSends(all[j]) !== cutSends(all[i])) j--;
      if (j < 0) return;
      [all[i], all[j]] = [all[j], all[i]];
      if (storeCuts(all)) renderSheet();
    })
  );
}

function renderCutSheet(el, sh) {
  const c = sh.cut;
  const blank = c.text.includes(BLANK);
  const digits = /\d+/.test(c.text);
  const sends = c.send && !blank;
  const where = blank
    ? "Off: it has a blank to fill first. It shows above the keyboard and fills the box."
    : sends
      ? "On: one tap sends it. It shows when the agent is waiting for you."
      : "Off: it shows above the keyboard and fills the box, so you can change it first.";
  el.innerHTML = `<div class="sheet"><div class="hdr">
      <button class="iconbtn" id="sheet-back" aria-label="Back">${ICONS.back}</button>
      <div class="grow"><div class="title">${c.id ? "Shortcut" : "New shortcut"}</div></div>
    </div><div class="sheet-body setform">
      <div class="glabel">Name on the button</div>
      <input class="cutin" id="cut-name" maxlength="28" autocomplete="off" value="${esc(c.name)}">
      <div class="glabel">What it sends</div>
      <textarea class="cutin" id="cut-text" rows="3">${esc(c.text)}</textarea>
      ${blank ? `<div class="cutnote">${esc(BLANK)} is a blank: you type the number each time.</div>` : ""}
      ${!blank && digits ? `<button type="button" class="btn outline small cutblank" id="cut-blank">Make the number a blank</button>` : ""}
      <div class="glabel">How it sends</div>
      <div class="opts"><button type="button" class="opt" id="cut-send"${blank ? " disabled" : ""}>
        <span class="oname">Send right away<span class="cuttext wrap">${where}</span></span>
        <span class="switch${sends ? " on" : ""}"><span class="knob"></span></span>
      </button></div>
      <div class="cutbtns">
        ${c.id ? `<button type="button" class="btn danger" id="cut-del">Delete</button>` : ""}
        <button type="button" class="btn" id="cut-save">Save shortcut</button>
      </div>
    </div></div>`;
  const name = document.getElementById("cut-name");
  const text = document.getElementById("cut-text");
  name.addEventListener("input", () => {
    c.name = name.value;
    sh.named = true;
  });
  // Until the name is typed by hand it follows the text.
  text.addEventListener("input", () => {
    c.text = text.value;
    if (!sh.named) {
      c.name = cutName(c.text);
      name.value = c.name;
    }
  });
  text.addEventListener("change", renderSheet);
  document.getElementById("sheet-back").addEventListener("click", () => leaveCut(sh));
  const blankBtn = document.getElementById("cut-blank");
  if (blankBtn)
    blankBtn.addEventListener("click", () => {
      c.text = c.text.replace(/\d+/, BLANK);
      if (!sh.named) c.name = cutName(c.text);
      renderSheet();
    });
  document.getElementById("cut-send").addEventListener("click", () => {
    c.send = !c.send;
    renderSheet();
  });
  const del = document.getElementById("cut-del");
  if (del)
    del.addEventListener("click", () => {
      if (storeCuts(loadCuts().filter((x) => x.id !== c.id))) leaveCut(sh);
    });
  document.getElementById("cut-save").addEventListener("click", () => {
    const body = c.text.trim();
    if (!body) return toast("Write what it sends first");
    const all = loadCuts();
    const kept = { id: c.id || `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`, name: c.name.trim() || cutName(body), text: body, send: !!c.send, n: c.n || 0 };
    const i = all.findIndex((x) => x.id === kept.id);
    if (i >= 0) all[i] = kept;
    else all.push(kept);
    if (!storeCuts(all)) return;
    if (!sh.fromList) toast("Shortcut saved");
    leaveCut(sh);
  });
}

// The clock in the stop row runs on the phone between the computer's updates.
setInterval(() => {
  const live = document.getElementById("live");
  const s = state.session;
  if (!live || !s || s.status !== "working") return;
  const bits = [];
  const ms = liveElapsed();
  if (ms != null) bits.push(fmtClock(ms));
  if (s.usage && s.usage.summary) bits.push(s.usage.summary);
  if (bits.length) live.textContent = bits.join(" · ");
}, 1000);

setInterval(() => {
  if ((state.screen === "inbox" || state.screen === "sessions") && state.inbox.length) {
    document.querySelectorAll(".srow").forEach((b) => {
      const s = state.inbox.find((x) => x.id === b.dataset.id);
      const t = b.querySelector(".time");
      if (s && t) t.textContent = ago(s.updated);
    });
  }
}, 30000);

// Looking at Usage keeps the computer's numbers fresh; it re-reads them when stale.
setInterval(() => {
  if (state.screen === "usage" && state.connected) send({ type: "usage" });
}, 60000);

render();
// Which way in this is: the account host, an old relay host, or the computer itself.
RELAY.forgetOldPairing();
RELAY.detect().then((found) => {
  if (found === "account") return startAccount();
  if (found === "update") {
    state.screen = "update";
    return render();
  }
  if (PAIR_CODE) {
    pair(PAIR_CODE).then((ok) => {
      if (!ok) state.screen = "pair";
      connect();
    });
  } else {
    connect();
  }
});
