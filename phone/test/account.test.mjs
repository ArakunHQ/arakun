// phone/relay.js on the account host: which way in the page takes, what it
// shows first after sign-in, the socket it opens to a computer, and the
// sentence for each reason the relay gives. Run with `node --test phone/test/`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
for (const file of ["e2e.js", "relay.js"]) vm.runInThisContext(readFileSync(join(here, "..", file), "utf8"), { filename: file });
const { E2E, RELAY } = globalThis;

/// A fetch that answers each path from `routes`: a status, or [status, body].
/// Paths it does not know answer 404 in plain text, as the computer does.
function stubFetch(t, routes) {
  const asked = [];
  t.mock.method(globalThis, "fetch", async (path, init = {}) => {
    asked.push(`${init.method || "GET"} ${path}`);
    const r = routes[path];
    if (r === undefined) return new Response("not found", { status: 404, headers: { "Content-Type": "text/plain" } });
    if (r instanceof Error) throw r;
    const [status, body] = Array.isArray(r) ? r : [r, null];
    return body === null ? new Response(null, { status }) : Response.json(body, { status });
  });
  return asked;
}

const machine = (label, online = true) => ({ label, pub: "AAAA", name: `pc-${label}`, kind: "app", online, last_seen_at: 1_700_000_000 });
const me = { github_id: 7, login: "rik", flags: [], plan: "free", notices: [] };

test("the account host is told apart from an old relay host and from the computer itself", async (t) => {
  stubFetch(t, { "/auth/config": [200, { github_client_id: "Ov23" }] });
  assert.equal(await RELAY.detect(), "account");
  t.mock.restoreAll();

  stubFetch(t, { "/_relay": [410, { update: true }] });
  assert.equal(await RELAY.detect(), "update");
  t.mock.restoreAll();

  // Served by the computer: neither route is there.
  stubFetch(t, {});
  assert.equal(await RELAY.detect(), null);
  t.mock.restoreAll();

  // A computer that cannot be reached at all is still the computer's page.
  stubFetch(t, { "/auth/config": new TypeError("offline"), "/_relay": new TypeError("offline") });
  assert.equal(await RELAY.detect(), null);
});

test("a failing account host is not taken for the computer's own server", async (t) => {
  // The computer answers 404 there; a 5xx or a 429 is the account host, or what stands before it.
  for (const status of [500, 502, 503, 429]) {
    const asked = stubFetch(t, { "/auth/config": [status, { error: "internal error" }] });
    assert.equal(await RELAY.detect(), "unreachable", String(status));
    // It does not go on to ask whether this is an old relay host.
    assert.deepEqual(asked, ["GET /auth/config"]);
    t.mock.restoreAll();
  }
  // A page that is not JSON (an error page in front of the Worker) too.
  t.mock.method(globalThis, "fetch", async () => new Response("<html>bad gateway</html>", { status: 502 }));
  assert.equal(await RELAY.detect(), "unreachable");
  t.mock.restoreAll();
  // Other refusals stay the computer's: its server answers 404, and 403 without a token.
  for (const status of [400, 403, 404]) {
    stubFetch(t, { "/auth/config": [status, { error: "no" }] });
    assert.equal(await RELAY.detect(), null, String(status));
    t.mock.restoreAll();
  }
  globalThis.location = { protocol: "https:", host: "app.arakun.dev", hostname: "app.arakun.dev" };
  assert.equal(RELAY.unreachable(), "Could not reach app.arakun.dev. Check the connection and try again.");
});

test("detect says a host that answers its own /me but not /auth/config is the computer", async (t) => {
  // The computer answers its own /me, so only /auth/config tells the hosts apart.
  stubFetch(t, { "/me": [403, { error: "not paired" }] });
  assert.equal(await RELAY.detect(), null);
});

test("signed out, the page offers sign-in", async (t) => {
  const asked = stubFetch(t, { "/me": [401, { error: "not signed in" }] });
  assert.deepEqual(await RELAY.boot(), { screen: "signin" });
  assert.deepEqual(asked, ["GET /me"]);
});

test("signed in with no computers, the page says so", async (t) => {
  stubFetch(t, { "/me": [200, me], "/machines": [200, []] });
  const got = await RELAY.boot();
  assert.equal(got.screen, "none");
  assert.deepEqual(got.me, me);
  assert.deepEqual(got.machines, []);
});

test("boot names the only computer as the one to connect to", async (t) => {
  stubFetch(t, { "/me": [200, me], "/machines": [200, [machine("aaaaaaaaaaaaaaaa", false)]] });
  const got = await RELAY.boot();
  assert.equal(got.screen, "connect");
  assert.equal(got.machine.label, "aaaaaaaaaaaaaaaa");
});

test("signed in with several computers, the page lists them", async (t) => {
  const list = [machine("aaaaaaaaaaaaaaaa"), machine("bbbbbbbbbbbbbbbb", false)];
  stubFetch(t, { "/me": [200, me], "/machines": [200, list] });
  const got = await RELAY.boot();
  assert.equal(got.screen, "list");
  assert.deepEqual(got.machines, list);
  assert.equal(got.machine, null);
});

test("a blocked account hears why", async (t) => {
  stubFetch(t, { "/me": [200, me], "/machines": [403, { error: "blocked", reason: "spam" }] });
  assert.deepEqual(await RELAY.boot(), { screen: "blocked", reason: "spam", me });
});

test("a host that does not answer is an error, not a sign-out", async (t) => {
  stubFetch(t, { "/me": [500, { error: "internal error" }] });
  await assert.rejects(RELAY.boot());
});

test("the socket to a computer names its label and this page", () => {
  globalThis.location = { protocol: "https:", host: "app.arakun.dev", hostname: "app.arakun.dev" };
  const url = RELAY.socketUrl("aaaaaaaaaaaaaaaa");
  assert.match(url, /^wss:\/\/app\.arakun\.dev\/machines\/aaaaaaaaaaaaaaaa\/ws\?page=[0-9a-f]{24}$/);
  // The same page id on every reconnect.
  assert.equal(RELAY.socketUrl("aaaaaaaaaaaaaaaa"), url);
  // Plain ws only for a Worker run by hand.
  globalThis.location = { protocol: "http:", host: "localhost:8787", hostname: "localhost" };
  assert.match(RELAY.socketUrl("bbbbbbbbbbbbbbbb"), /^ws:\/\/localhost:8787\/machines\/bbbbbbbbbbbbbbbb\/ws\?page=/);
});

test("each reason the relay gives becomes what the page does", () => {
  assert.equal(RELAY.kindOf("signed_out", 0), "signed_out");
  assert.equal(RELAY.kindOf("", 4401), "signed_out");
  // `unpaired` too: the computer removed this device, so the page does not dial again.
  for (const r of ["blocked", "not_yours", "no_machine", "update", "disabled", "unpaired"]) assert.equal(RELAY.kindOf(r, 0), "refused", r);
  assert.equal(RELAY.kindOf("bad_hello", 0), "untrusted");
  assert.equal(RELAY.kindOf("limit:daily", 0), "limit");
  assert.equal(RELAY.kindOf("gone", 4410), "gone");
  assert.equal(RELAY.kindOf("silent", 4410), "gone");
  // A relay whose database is down answers 500 to the upgrade: the socket
  // closes with no word, which is a drop to try again, never a refusal.
  assert.equal(RELAY.kindOf("", 1006), "gone");
});

test("each refusal reads as a sentence", () => {
  globalThis.location = { protocol: "https:", host: "app.arakun.dev", hostname: "app.arakun.dev" };
  assert.equal(RELAY.sentence("blocked"), "This GitHub account is blocked from the relay.");
  assert.equal(RELAY.sentence("not_yours"), "That computer is signed in to another GitHub account.");
  assert.equal(RELAY.sentence("no_machine"), "That computer is no longer on your account. Sign in again in Arakun on it.");
  assert.equal(RELAY.sentence("update"), "Update Arakun on the computer and sign in at app.arakun.dev");
  assert.equal(RELAY.sentence("disabled", "Back at noon."), "Back at noon.");
  assert.equal(RELAY.sentence("disabled"), "The relay is closed right now.");
  assert.equal(RELAY.sentence("unpaired"), "That computer removed this phone. Pick it again to connect.");
});

test("removing a computer asks by its name and sends DELETE to its own path only", async (t) => {
  assert.match(RELAY.removeQuestion("Rik's Mac"), /^Remove Rik's Mac from your account\?/);
  assert.equal(RELAY.machinePath("k7f3q2m4xwab4c5d"), "/machines/k7f3q2m4xwab4c5d");
  for (const bad of ["", "../me", "K7F3Q2M4XWAB4C5D", "k7f3q2m4xwab4c5d/pc", "k7f3"]) assert.throws(() => RELAY.machinePath(bad), bad);
  const asked = stubFetch(t, { "/machines/k7f3q2m4xwab4c5d": 204 });
  assert.deepEqual(await RELAY.forgetMachine("k7f3q2m4xwab4c5d"), { ok: true, status: 204 });
  assert.deepEqual(asked, ["DELETE /machines/k7f3q2m4xwab4c5d"]);
  t.mock.restoreAll();
  // Removed already: done all the same. Signed out: says so.
  stubFetch(t, { "/machines/k7f3q2m4xwab4c5d": [404, { error: "no such machine" }] });
  assert.equal((await RELAY.forgetMachine("k7f3q2m4xwab4c5d")).ok, true);
  t.mock.restoreAll();
  stubFetch(t, { "/machines/k7f3q2m4xwab4c5d": 401 });
  assert.deepEqual(await RELAY.forgetMachine("k7f3q2m4xwab4c5d"), { signedOut: true });
});

test("the signed-in devices are listed escaped, this phone marked, the others with Sign out", async (t) => {
  const esc = (v) => String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  const ago = () => "2h";
  const id = "0123456789abcdef0123456789abcdef";
  const rows = RELAY.signedInRows(
    [
      { id: "fedcba9876543210fedcba9876543210", name: "iPhone", kind: "phone", last_used_at: 1, current: true },
      { id, name: '<img src=x onerror="x">', kind: "<b>", last_used_at: 1, current: false },
      { id: '"><script>', name: "Odd", kind: "app", last_used_at: 0, current: false },
    ],
    esc,
    ago,
  );
  assert.equal(rows.length, 3);
  assert.match(rows[0], /This phone/);
  assert.doesNotMatch(rows[0], /data-signout/);
  assert.match(rows[1], new RegExp(`data-signout="${id}"`));
  assert.match(rows[1], /&lt;img src=x onerror=&quot;x&quot;&gt;/);
  assert.doesNotMatch(rows.join(""), /<img|<script|<b>/);
  assert.match(rows[1], /Device · used 2h ago/);
  // An id that is not one gets no button at all.
  assert.doesNotMatch(rows[2], /data-signout/);
  assert.match(rows[2], /Arakun app/);

  const asked = stubFetch(t, { "/sessions": [200, [{ id, name: "Mac" }]], [`/sessions/${id}`]: 204 });
  assert.deepEqual(await RELAY.sessions(), { list: [{ id, name: "Mac" }] });
  assert.equal((await RELAY.signOutSession(id)).ok, true);
  assert.deepEqual(asked, ["GET /sessions", `DELETE /sessions/${id}`]);
  assert.throws(() => RELAY.signOutSession("../me"));
});

test("an old relay host points at the account host", () => {
  globalThis.location = { protocol: "https:", host: "5pmbdp76s6bol6ax.arakun.dev", hostname: "5pmbdp76s6bol6ax.arakun.dev" };
  assert.equal(RELAY.accountHost(), "app.arakun.dev");
  assert.equal(RELAY.sentence("update"), "Update Arakun on the computer and sign in at app.arakun.dev");
  globalThis.location = { protocol: "https:", host: "relay.example.com", hostname: "relay.example.com" };
  assert.equal(RELAY.accountHost(), "relay.example.com");
});

test("an old relay host names the account host the Worker gave, when it is a host name", async (t) => {
  globalThis.location = { protocol: "https:", host: "5pmbdp76s6bol6ax.example.com", hostname: "5pmbdp76s6bol6ax.example.com" };
  stubFetch(t, { "/_relay": [410, { update: true, account_host: "phones.example.net" }] });
  assert.equal(await RELAY.detect(), "update");
  assert.equal(RELAY.accountHost(), "phones.example.net");
  assert.equal(RELAY.sentence("update"), "Update Arakun on the computer and sign in at phones.example.net");
  t.mock.restoreAll();
  // Anything that could leave the link's host is not taken: the guess stands.
  for (const bad of ["evil.example/x", "a b", "user@evil.example", '"><script>', "https://evil.example", "", 7, null]) {
    stubFetch(t, { "/_relay": [410, { update: true, account_host: bad }] });
    assert.equal(await RELAY.detect(), "update");
    assert.equal(RELAY.accountHost(), "app.example.com", String(bad));
    t.mock.restoreAll();
  }
  // A Worker from before it named one.
  stubFetch(t, { "/_relay": [410, { update: true }] });
  assert.equal(await RELAY.detect(), "update");
  assert.equal(RELAY.accountHost(), "app.example.com");
});

test("only the account host's word ends the session on the page", () => {
  assert.equal(RELAY.loggedOut(204), true);
  // Gone already: signed out from another device, or expired.
  assert.equal(RELAY.loggedOut(401), true);
  // No answer (0), an error, a limit, or a page that is not the Worker's (a captive portal's 200).
  for (const status of [0, 200, 302, 403, 404, 429, 500, 502, 503]) assert.equal(RELAY.loggedOut(status), false, String(status));
  globalThis.location = { protocol: "https:", host: "app.arakun.dev", hostname: "app.arakun.dev" };
  assert.equal(RELAY.signOutFailed(), "Signing out did not reach app.arakun.dev, so this phone is still signed in. Check the connection and try again.");
});

test("a notification tap leads somewhere sound wherever the page is", () => {
  const a = machine("aaaaaaaaaaaaaaaa");
  const b = machine("bbbbbbbbbbbbbbbb", false);
  const signedIn = { me, machines: [a, b], blocked: false };
  // Served by the computer itself: the session, as before.
  for (const screen of ["inbox", "session", "pair", "phone"]) assert.deepEqual(RELAY.tapRoute(screen, null, false, null), { go: "open" }, screen);
  // A computer is picked: the session on it.
  assert.deepEqual(RELAY.tapRoute("inbox", signedIn, true, a.label), { go: "open" });
  // None picked, and the one opened last is on the account: it, then the session.
  assert.deepEqual(RELAY.tapRoute("machines", signedIn, false, b.label), { go: "pick", machine: b });
  // None picked and no computer to go by: the list.
  assert.deepEqual(RELAY.tapRoute("machines", signedIn, false, null), { go: "list" });
  assert.deepEqual(RELAY.tapRoute("machines", signedIn, false, "cccccccccccccccc"), { go: "list" });
  // Signed out, a start that failed, blocked, the old host: the page stays where it is.
  assert.deepEqual(RELAY.tapRoute("signin", { me: null, machines: [], blocked: false }, false, a.label), { go: "stay" });
  assert.deepEqual(RELAY.tapRoute("machines", { me: null, machines: [], blocked: false }, false, a.label), { go: "stay" });
  assert.deepEqual(RELAY.tapRoute("machines", { me, machines: [a], blocked: true }, false, a.label), { go: "stay" });
  // No computers yet: the list says so already.
  assert.deepEqual(RELAY.tapRoute("machines", { me, machines: [], blocked: false }, false, a.label), { go: "stay" });
  assert.deepEqual(RELAY.tapRoute("update", null, false, null), { go: "stay" });
  // A host that did not answer shows why, and only that.
  assert.deepEqual(RELAY.tapRoute("fatal", null, false, null), { go: "stay" });
  assert.equal(RELAY.tapSentence(), "Pick the computer that notification came from to open its session.");
});

test("what the old pairing kept is dropped", () => {
  const store = new Map([["arakun.relay", "{}"], ["arakun.place", "/x"]]);
  globalThis.localStorage = { getItem: (k) => store.get(k) ?? null, removeItem: (k) => store.delete(k), setItem: (k, v) => store.set(k, v) };
  const cookies = [];
  globalThis.document = {
    set cookie(v) {
      cookies.push(v);
    },
  };
  globalThis.location = { protocol: "https:", host: "app.arakun.dev", hostname: "app.arakun.dev" };
  RELAY.forgetOldPairing();
  assert.equal(store.has("arakun.relay"), false);
  assert.equal(store.get("arakun.place"), "/x");
  assert.equal(cookies.length, 1);
  assert.match(cookies[0], /^arakun\.relay=; Path=\/; Max-Age=0/);
  delete globalThis.document;
  delete globalThis.localStorage;
});

/// A WebSocket that records what the page sends and lets the test speak for the relay.
class FakeSocket {
  static last = null;
  constructor(url) {
    this.url = url;
    this.sent = [];
    FakeSocket.last = this;
  }
  send(data) {
    this.sent.push(data);
  }
  close() {}
}

const frame = (type, obj) => ({ data: E2E.encodeFrame(type, E2E.utf8(JSON.stringify(obj))).buffer });
/// Waits until `done()` holds; the page's crypto runs off the microtask queue.
async function until(done) {
  for (let i = 0; i < 200 && !done(); i++) await new Promise((r) => setTimeout(r, 5));
  assert.ok(done(), "timed out");
}

/// A computer's answer to the page's hello on `channel`, signed with `identity`.
async function computerHello(identity, pub, label, channel, sentHello) {
  const hello = JSON.parse(E2E.text(sentHello));
  const ePhone = E2E.fromB64url(hello.e);
  const noncePhone = E2E.fromB64url(hello.nonce);
  const pcPair = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
  const ePc = new Uint8Array(await crypto.subtle.exportKey("raw", pcPair.publicKey));
  const noncePc = crypto.getRandomValues(new Uint8Array(16));
  const signed = E2E.transcript(label, channel, ePhone, ePc, noncePhone, noncePc);
  const sig = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, identity.privateKey, signed));
  return { v: 1, pub: E2E.b64url(pub), e: E2E.b64url(ePc), nonce: E2E.b64url(noncePc), sig: E2E.b64url(sig), arakun: "test" };
}

async function key() {
  const identity = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
  const pub = new Uint8Array(await crypto.subtle.exportKey("raw", identity.publicKey));
  return { identity, pub, label: await E2E.labelOf(pub) };
}

/// Opens a link to `expected` and lets `pc` answer the handshake.
async function handshake(expected, pc) {
  globalThis.location = { protocol: "https:", host: "app.arakun.dev", hostname: "app.arakun.dev" };
  globalThis.WebSocket = FakeSocket;
  const heard = [];
  const link = RELAY.open(expected, {
    onopen: (who) => heard.push(["open", who]),
    onclose: (e) => heard.push(["close", e]),
    onstatus() {},
    onmessage() {},
  });
  const ws = FakeSocket.last;
  ws.onmessage(frame(E2E.CONTROL, { type: "open", channel: 3 }));
  await until(() => ws.sent.some((s) => typeof s !== "string"));
  const sent = E2E.decodeFrame(ws.sent.at(-1));
  assert.equal(sent.type, E2E.HELLO);
  ws.onmessage(frame(E2E.HELLO, await computerHello(pc.identity, pc.pub, pc.label, 3, sent.payload)));
  await until(() => heard.length > 0);
  link.close();
  return { ws, heard };
}

test("the link opens on the computer's own socket path and asks nothing after the handshake", async () => {
  const pc = await key();
  const { ws, heard } = await handshake({ label: pc.label, pub: E2E.b64url(pc.pub) }, pc);
  assert.match(ws.url, new RegExp(`/machines/${pc.label}/ws\\?page=`));
  assert.deepEqual(heard, [["open", {}]]);
  // Only the hello went out: no {pair} and no {auth} inside the channel.
  assert.equal(ws.sent.filter((s) => typeof s !== "string").length, 1);
});

test("a computer whose key is not the one the account has is not trusted", async () => {
  const pc = await key();
  const other = await key();
  const { heard } = await handshake({ label: pc.label, pub: E2E.b64url(other.pub) }, pc);
  assert.equal(heard.length, 1);
  assert.equal(heard[0][0], "close");
  assert.equal(heard[0][1].kind, "untrusted");
});

test("a refusal on the socket reaches the page as its sentence", () => {
  globalThis.location = { protocol: "https:", host: "app.arakun.dev", hostname: "app.arakun.dev" };
  globalThis.WebSocket = FakeSocket;
  const said = (bye) => {
    const heard = [];
    RELAY.open({ label: "aaaaaaaaaaaaaaaa", pub: "AAAA" }, { onopen() {}, onclose: (e) => heard.push(e), onstatus() {}, onmessage() {} });
    FakeSocket.last.onmessage(frame(E2E.CONTROL, { type: "bye", ...bye }));
    return heard;
  };
  return Promise.resolve().then(async () => {
    const a = said({ reason: "disabled", message: "Back at noon." });
    const b = said({ reason: "signed_out" });
    const c = said({ reason: "not_yours" });
    await until(() => a.length && b.length && c.length);
    assert.deepEqual(a, [{ kind: "refused", detail: "Back at noon." }]);
    assert.deepEqual(b, [{ kind: "signed_out", detail: "signed_out" }]);
    assert.deepEqual(c, [{ kind: "refused", detail: "That computer is signed in to another GitHub account." }]);
  });
});

test("only an https notice link becomes a link", () => {
  assert.equal(RELAY.noticeLink("https://arakun.dev/status"), "https://arakun.dev/status");
  for (const url of ["http://arakun.dev", "javascript:alert(1)", "//arakun.dev", "", null, undefined]) assert.equal(RELAY.noticeLink(url), null, String(url));
});

test("with several computers the Notifications row names the one they come from", () => {
  assert.equal(RELAY.pushNote([machine("aaaaaaaaaaaaaaaa")], "pc-a"), "");
  assert.equal(
    RELAY.pushNote([machine("aaaaaaaaaaaaaaaa"), machine("bbbbbbbbbbbbbbbb")], "pc-a"),
    "Notifications come only from the computer opened last, pc-a."
  );
  // Served locally there is no list.
  assert.equal(RELAY.pushNote([], ""), "");
});

test("a notification from another computer reads as a sentence", () => {
  assert.equal(RELAY.noticeElsewhere(), "That notification is from another computer. Open it from All computers in the Phone tab.");
});

test("what the page keeps is kept per computer on the account host", () => {
  // Served locally: the keys stay as they were, so nothing is lost there.
  assert.equal(RELAY.scoped("arakun.draft:", null), "arakun.draft:");
  assert.equal(RELAY.scoped("arakun.shortcuts", null), "arakun.shortcuts");
  assert.equal(RELAY.scoped("arakun.draft:", "aaaaaaaaaaaaaaaa"), "arakun.draft@aaaaaaaaaaaaaaaa:");
  assert.equal(RELAY.scoped("arakun.shortcuts", "aaaaaaaaaaaaaaaa"), "arakun.shortcuts@aaaaaaaaaaaaaaaa");

  // A draft written on computer A is not read on B, even for the same session id.
  const store = new Map();
  store.set(RELAY.scoped("arakun.draft:", "aaaaaaaaaaaaaaaa") + "s1", "half a prompt");
  assert.equal(store.get(RELAY.scoped("arakun.draft:", "bbbbbbbbbbbbbbbb") + "s1"), undefined);
  assert.equal(store.get(RELAY.scoped("arakun.draft:", "aaaaaaaaaaaaaaaa") + "s1"), "half a prompt");
});

test("forgetAccountState drops the computer opened last and what is kept per computer", () => {
  const store = new Map([
    ["arakun.machine", "aaaaaaaaaaaaaaaa"],
    ["arakun.draft@aaaaaaaaaaaaaaaa:s1", "x"],
    ["arakun.draft@bbbbbbbbbbbbbbbb:s2", "y"],
    ["arakun.draft:s3", "kept: a computer on the network"],
    ["arakun.shortcuts@aaaaaaaaaaaaaaaa", "[]"],
    ["arakun.place@aaaaaaaaaaaaaaaa", "/x"],
    ["arakun.shortcuts", "[] kept: a computer on the network"],
    ["arakun.place", "/y"],
  ]);
  const storage = {
    get length() {
      return store.size;
    },
    key: (i) => [...store.keys()][i] ?? null,
    removeItem: (k) => store.delete(k),
  };
  RELAY.forgetAccountState(storage);
  assert.deepEqual([...store.keys()], ["arakun.draft:s3", "arakun.shortcuts", "arakun.place"]);
});

test("hanging up settles every request still waiting", async () => {
  const pc = await key();
  globalThis.location = { protocol: "https:", host: "app.arakun.dev", hostname: "app.arakun.dev" };
  globalThis.WebSocket = FakeSocket;
  let link;
  const opened = new Promise((resolve) => {
    link = RELAY.open({ label: pc.label, pub: E2E.b64url(pc.pub) }, { onopen: resolve, onclose() {}, onstatus() {}, onmessage() {} });
  });
  const ws = FakeSocket.last;
  ws.onmessage(frame(E2E.CONTROL, { type: "open", channel: 3 }));
  await until(() => ws.sent.some((s) => typeof s !== "string"));
  const sent = E2E.decodeFrame(ws.sent.at(-1));
  ws.onmessage(frame(E2E.HELLO, await computerHello(pc.identity, pc.pub, pc.label, 3, sent.payload)));
  await opened;
  const waiting = link.fetch("/me");
  link.close();
  await assert.rejects(waiting, { message: "link closed" });
});

test("a notification tap's session either opens, is elsewhere, or is not concerned", () => {
  // The computer does not have it: its answer to the open is an error.
  assert.equal(RELAY.noticeOutcome("s1", { type: "error", message: "that session is gone" }), "elsewhere");
  // It does: the session arrives.
  assert.equal(RELAY.noticeOutcome("s1", { type: "session", id: "s1", entries: [] }), "opened");
  assert.equal(RELAY.noticeOutcome("s1", { type: "entries", id: "s1", entries: [] }), "opened");
  // Other messages say nothing about it.
  assert.equal(RELAY.noticeOutcome("s1", { type: "inbox", sessions: [] }), "unrelated");
  assert.equal(RELAY.noticeOutcome("s1", { type: "session", id: "s2" }), "unrelated");
  // No tap waiting: nothing is about one.
  assert.equal(RELAY.noticeOutcome("", { type: "error", message: "x" }), "unrelated");
});

test("an error after the tapped session opened is not taken for another computer's", () => {
  let pending = "s1";
  const take = (msg) => {
    const outcome = RELAY.noticeOutcome(pending, msg);
    if (outcome !== "unrelated") pending = "";
    return outcome;
  };
  assert.equal(take({ type: "inbox", sessions: [] }), "unrelated");
  assert.equal(take({ type: "session", id: "s1", entries: [] }), "opened");
  assert.equal(take({ type: "error", message: "could not send" }), "unrelated");
});
