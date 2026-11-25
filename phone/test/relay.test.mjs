// phone/relay.js joining the pieces of a message the computer split for the
// relay (`say` in core/src/phone/relay.rs), against the pieces Rust wrote
// (vectors/part.json). Run with `node --test phone/test/`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
for (const file of ["e2e.js", "relay.js"]) vm.runInThisContext(readFileSync(join(here, "..", "public", file), "utf8"), { filename: file });
const { E2E, RELAY } = globalThis;
const vectors = JSON.parse(readFileSync(join(here, "vectors", "part.json"), "utf8"));

test("the pieces the computer makes join into its message", () => {
  for (const c of vectors.cases) {
    const joined = RELAY.joiner();
    const got = c.parts.map((p) => joined(JSON.parse(p).part));
    assert.ok(got.slice(0, -1).every((m) => m === null), `chunk ${c.chunk}`);
    assert.deepEqual(got.at(-1), JSON.parse(vectors.message), `chunk ${c.chunk}`);
  }
  assert.ok(vectors.cases[0].parts.length > 1);
});

/// What the computer does: the message's bytes in pieces of `size`.
function pieces(msg, size) {
  const bytes = E2E.utf8(JSON.stringify(msg));
  const out = [];
  for (let at = 0; at < bytes.length; at += size) out.push({ body: E2E.b64url(bytes.subarray(at, at + size)), more: at + size < bytes.length });
  return out;
}

test("the pieces of a big message join into the message", () => {
  // Letters of more than one byte, and a piece size that ends inside them.
  const msg = { type: "entries", text: "é€".repeat(300_000) };
  const parts = pieces(msg, 160 * 1024 + 1);
  assert.ok(parts.length > 5);
  const joined = RELAY.joiner();
  for (const part of parts.slice(0, -1)) assert.equal(joined(part), null);
  assert.deepEqual(joined(parts.at(-1)), msg);
});

test("the next message starts from nothing", () => {
  const joined = RELAY.joiner();
  for (const msg of [{ a: "x".repeat(10) }, { b: 2 }]) {
    const got = pieces(msg, 4).map(joined);
    assert.deepEqual(got.at(-1), msg);
    assert.ok(got.slice(0, -1).every((m) => m === null));
  }
});

/// A WebSocket that records what the page sends and lets the test speak for
/// the relay.
class FakeSocket {
  static last = null;
  constructor(url) {
    this.url = url;
    this.sent = [];
    this.closed = false;
    FakeSocket.last = this;
  }
  send(data) {
    this.sent.push(data);
  }
  close() {
    this.closed = true;
  }
}

/// A link through the relay on a fake socket, with fake clocks; what it ended with lands in `ended`.
function fakeLink(t) {
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval", "Date"] });
  globalThis.location = { protocol: "https:", host: "relay.test" };
  globalThis.WebSocket = FakeSocket;
  const ended = [];
  RELAY.open({ label: "aaaaaaaaaaaaaaaa", pub: "AAAA" }, { onclose: (e) => ended.push(e), onstatus() {}, onopen() {}, onmessage() {} });
  return { ws: FakeSocket.last, ended };
}

const control = (msg) => ({ data: E2E.encodeFrame(E2E.CONTROL, E2E.utf8(JSON.stringify(msg))).buffer });
const settle = () => new Promise((r) => setImmediate(r));

test("a computer that never answers is given up on", (t) => {
  const { ended } = fakeLink(t);
  t.mock.timers.tick(14_000);
  assert.deepEqual(ended, []);
  t.mock.timers.tick(1_000);
  assert.deepEqual(ended, [{ kind: "gone", detail: "no answer from the computer" }]);
});

test("the relay is pinged, and a link that hears nothing back is given up on", async (t) => {
  const { ws, ended } = fakeLink(t);
  ws.onopen();
  assert.deepEqual(ws.sent, ["ping"]);
  // The computer is away: no answer is due, so only the pings count.
  ws.onmessage(control({ type: "pc_offline" }));
  await settle();
  t.mock.timers.tick(30_000);
  assert.deepEqual(ws.sent, ["ping", "ping"]);
  ws.onmessage({ data: "pong" });
  t.mock.timers.tick(60_000);
  assert.deepEqual(ended, []);
  t.mock.timers.tick(30_000);
  assert.deepEqual(ended, [{ kind: "gone", detail: "the relay went quiet" }]);
  assert.ok(ws.closed);
});
