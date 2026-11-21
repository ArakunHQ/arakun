// phone/e2e.js against the vectors core/src/phone/e2e.rs wrote (vectors/e2e.json):
// the phone must trust the computer's hello, derive the same keys, open the
// computer's frames and seal its own to the very same bytes. Run with
// `node --test phone/test/` (Node 20+, WebCrypto built in).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
vm.runInThisContext(readFileSync(join(here, "..", "e2e.js"), "utf8"), { filename: "e2e.js" });
const E2E = globalThis.E2E;
const vectors = JSON.parse(readFileSync(join(here, "vectors", "e2e.json"), "utf8"));
const bytes = (name) => E2E.fromB64url(vectors[name]);
const labelVector = JSON.parse(readFileSync(join(here, "vectors", "label.json"), "utf8"));

test("the label of a public key is the one Rust and the Worker derive", async () => {
  const pub = Uint8Array.from(labelVector.public_key_hex.match(/../g), (h) => parseInt(h, 16));
  assert.equal(await E2E.labelOf(pub), labelVector.label);
  assert.equal(await E2E.labelOf(bytes("pc_pub")), vectors.label);
});

test("the computer's hello is trusted when it answers ours and signs the exchange", async () => {
  const hello = JSON.parse(vectors.hello_pc);
  const pc = await E2E.verifyPc(hello, vectors.label, vectors.channel, bytes("e_phone"), bytes("nonce_phone"));
  assert.deepEqual(pc.e, bytes("e_pc"));
  assert.deepEqual(pc.nonce, bytes("nonce_pc"));
  assert.equal(pc.arakun, "0.0.0-vectors");

  const rejects = async (hello, label, channel, ePhone, noncePhone, why) =>
    assert.rejects(E2E.verifyPc(hello, label, channel, ePhone, noncePhone), { message: `bad hello: ${why}` });
  await rejects(hello, "aaaaaaaaaaaaaaaa", vectors.channel, bytes("e_phone"), bytes("nonce_phone"), "label");
  await rejects(hello, vectors.label, vectors.channel + 1, bytes("e_phone"), bytes("nonce_phone"), "sig");
  await rejects(hello, vectors.label, vectors.channel, bytes("e_pc"), bytes("nonce_phone"), "sig");
  await rejects(hello, vectors.label, vectors.channel, bytes("e_phone"), bytes("nonce_pc"), "sig");
  await rejects({ ...hello, v: 2 }, vectors.label, vectors.channel, bytes("e_phone"), bytes("nonce_phone"), "version");
  await rejects({ ...hello, sig: hello.sig.slice(1) }, vectors.label, vectors.channel, bytes("e_phone"), bytes("nonce_phone"), "sig");
  const forged = E2E.fromB64url(hello.sig);
  forged[3] ^= 1;
  await rejects({ ...hello, sig: E2E.b64url(forged) }, vectors.label, vectors.channel, bytes("e_phone"), bytes("nonce_phone"), "sig");
});

test("the same shared secret gives the same two keys", async () => {
  const keys = await E2E.deriveKeys(bytes("shared"), bytes("nonce_phone"), bytes("nonce_pc"));
  assert.deepEqual(keys.phoneToPc, bytes("key_phone_to_pc"));
  assert.deepEqual(keys.pcToPhone, bytes("key_pc_to_phone"));
});

test("frames seal to the same bytes and open on the other side", async () => {
  const phone = await E2E.sessionFromKeys(bytes("key_phone_to_pc"), bytes("key_pc_to_phone"));
  const pc = await E2E.sessionFromKeys(bytes("key_pc_to_phone"), bytes("key_phone_to_pc"));
  for (const f of vectors.frames) {
    const sealed = E2E.fromB64url(f.sealed);
    const [from, to] = f.from === "pc" ? [pc, phone] : [phone, pc];
    assert.deepEqual(await from.seal(f.plain), sealed, `${f.from} seals ${JSON.stringify(f.plain)}`);
    assert.equal(E2E.text(await to.open(sealed)), f.plain);
  }
});

test("a frame out of order or changed does not open", async () => {
  const a = await E2E.sessionFromKeys(bytes("key_pc_to_phone"), bytes("key_phone_to_pc"));
  const b = await E2E.sessionFromKeys(bytes("key_phone_to_pc"), bytes("key_pc_to_phone"));
  const first = await a.seal("one");
  const second = await a.seal("two");
  await assert.rejects(b.open(second), { message: "frame out of order" });
  assert.equal(E2E.text(await b.open(first)), "one");
  await assert.rejects(b.open(first), { message: "frame out of order" });
  const forged = Uint8Array.from(second);
  forged[12] ^= 1;
  await assert.rejects(b.open(forged), { message: "frame does not decrypt" });
  assert.equal(E2E.text(await b.open(second)), "two");
  await assert.rejects(b.open(new Uint8Array(3)), { message: "frame does not decrypt" });
});

test("a whole handshake with a computer played here agrees on the keys", async () => {
  // The computer: an identity and its answer, as core/src/phone/e2e.rs makes them.
  const identity = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
  const pub = new Uint8Array(await crypto.subtle.exportKey("raw", identity.publicKey));
  const label = await E2E.labelOf(pub);
  const channel = 42;

  const phone = await E2E.begin();
  const hello = JSON.parse(E2E.text(phone.hello));
  assert.equal(hello.v, E2E.VERSION);
  const ePhone = E2E.fromB64url(hello.e);
  const noncePhone = E2E.fromB64url(hello.nonce);
  assert.equal(ePhone.length, 65);
  assert.equal(noncePhone.length, 16);

  const pcPair = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
  const ePc = new Uint8Array(await crypto.subtle.exportKey("raw", pcPair.publicKey));
  const noncePc = crypto.getRandomValues(new Uint8Array(16));
  const signed = E2E.transcript(label, channel, ePhone, ePc, noncePhone, noncePc);
  const sig = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, identity.privateKey, signed));
  const reply = E2E.utf8(JSON.stringify({ v: 1, pub: E2E.b64url(pub), e: E2E.b64url(ePc), nonce: E2E.b64url(noncePc), sig: E2E.b64url(sig), arakun: "test" }));
  const theirs = await crypto.subtle.importKey("raw", ePhone, { name: "ECDH", namedCurve: "P-256" }, false, []);
  const shared = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: theirs }, pcPair.privateKey, 256));
  const keys = await E2E.deriveKeys(shared, noncePhone, noncePc);
  const pc = await E2E.sessionFromKeys(keys.pcToPhone, keys.phoneToPc);

  const session = await phone.finish(reply, label, channel);
  assert.equal(session.arakun, "test");
  assert.equal(E2E.text(await session.open(await pc.seal("hello phone"))), "hello phone");
  assert.equal(E2E.text(await pc.open(await session.seal("hello computer"))), "hello computer");

  const again = await E2E.begin();
  await assert.rejects(again.finish(reply, label, channel), { message: "bad hello: sig" });
  await assert.rejects(again.finish(E2E.utf8("nope"), label, channel), { message: "bad hello: not json" });
});

test("phone frames carry the type byte in front", () => {
  const f = E2E.encodeFrame(E2E.DATA, E2E.utf8("hi"));
  assert.deepEqual(f, new Uint8Array([2, 104, 105]));
  assert.deepEqual(E2E.decodeFrame(f), { type: 2, payload: E2E.utf8("hi") });
  assert.equal(E2E.decodeFrame(new Uint8Array([9, 1])), null);
  assert.equal(E2E.decodeFrame(new Uint8Array(0)), null);
});
