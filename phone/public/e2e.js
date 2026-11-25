// The phone's half of the end-to-end encryption with the computer, through the
// relay (core/src/phone/e2e.rs is the computer's). WebCrypto only: P-256 ECDH for the
// shared secret, the computer's P-256 ECDSA signature over the whole handshake,
// HKDF-SHA256 for the two session keys, AES-256-GCM for the frames. Loaded as a
// plain script before app.js; phone/test/e2e.test.mjs checks it against the
// vectors the Rust side writes.
"use strict";

const E2E = (() => {
  const VERSION = 1;
  const HELLO = 0x01;
  const DATA = 0x02;
  const CONTROL = 0x03;
  const TRANSCRIPT_TAG = "arakun-phone-v1";
  const KEYS_INFO = "arakun-phone-v1 keys";
  const ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";
  const subtle = crypto.subtle;
  const utf8 = (s) => new TextEncoder().encode(s);
  const text = (b) => new TextDecoder().decode(b);

  function b64url(bytes) {
    let s = "";
    for (const b of bytes) s += String.fromCharCode(b);
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  function fromB64url(s) {
    if (typeof s !== "string" || /[^A-Za-z0-9_-]/.test(s)) return null;
    try {
      const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
      return Uint8Array.from(bin, (c) => c.charCodeAt(0));
    } catch {
      return null;
    }
  }

  function concat(...parts) {
    const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }

  // RFC 4648 base32 without padding, lower case: 10 bytes make 16 characters.
  function base32(bytes) {
    let out = "";
    let acc = 0;
    let bits = 0;
    for (const b of bytes) {
      acc = ((acc << 8) | b) >>> 0;
      bits += 8;
      while (bits >= 5) {
        out += ALPHABET[(acc >>> (bits - 5)) & 31];
        bits -= 5;
      }
    }
    if (bits > 0) out += ALPHABET[(acc << (5 - bits)) & 31];
    return out;
  }

  /// The label a computer's public key (65 bytes, uncompressed) derives.
  async function labelOf(pub) {
    const digest = new Uint8Array(await subtle.digest("SHA-256", pub));
    return base32(digest.subarray(0, 10));
  }

  /// A field of exactly `len` bytes, or a named failure.
  function field(obj, name, len) {
    const bytes = fromB64url(obj[name]);
    if (!bytes || bytes.length !== len) throw new Error(`bad hello: ${name}`);
    return bytes;
  }

  /// What the computer signs: tag, label, channel (u32 big-endian), both
  /// ephemeral keys, both nonces; phone's before computer's.
  function transcript(label, channel, ePhone, ePc, noncePhone, noncePc) {
    const ch = new Uint8Array(4);
    new DataView(ch.buffer).setUint32(0, channel);
    return concat(utf8(TRANSCRIPT_TAG), utf8(label), ch, ePhone, ePc, noncePhone, noncePc);
  }

  /// Checks the computer's hello answers ours and comes from the key `label`
  /// names; gives its parts back.
  async function verifyPc(hello, label, channel, ePhone, noncePhone) {
    if (hello.v !== VERSION) throw new Error("bad hello: version");
    const pub = field(hello, "pub", 65);
    if ((await labelOf(pub)) !== label) throw new Error("bad hello: label");
    const e = field(hello, "e", 65);
    const nonce = field(hello, "nonce", 16);
    const sig = field(hello, "sig", 64);
    const key = await subtle.importKey("raw", pub, { name: "ECDSA", namedCurve: "P-256" }, false, ["verify"]);
    const signed = transcript(label, channel, ePhone, e, noncePhone, nonce);
    if (!(await subtle.verify({ name: "ECDSA", hash: "SHA-256" }, key, sig, signed))) throw new Error("bad hello: sig");
    return { pub, e, nonce, arakun: typeof hello.arakun === "string" ? hello.arakun : "" };
  }

  /// The two session keys out of the shared secret: phone to computer, then
  /// computer to phone. Salt is both nonces, phone's first.
  async function deriveKeys(shared, noncePhone, noncePc) {
    const ikm = await subtle.importKey("raw", shared, "HKDF", false, ["deriveBits"]);
    const bits = new Uint8Array(
      await subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt: concat(noncePhone, noncePc), info: utf8(KEYS_INFO) }, ikm, 512),
    );
    return { phoneToPc: bits.slice(0, 32), pcToPhone: bits.slice(32) };
  }

  /// Four zero bytes, then the counter as u64 big-endian.
  function nonceFor(counter) {
    const n = new Uint8Array(12);
    new DataView(n.buffer).setBigUint64(4, BigInt(counter));
    return n;
  }

  /// One encrypted channel: the key this side seals with, the key it opens
  /// with, and a counter for each.
  async function sessionFromKeys(sealRaw, openRaw) {
    const aes = (raw, use) => subtle.importKey("raw", raw, "AES-GCM", false, [use]);
    const [sealKey, openKey] = await Promise.all([aes(sealRaw, "encrypt"), aes(openRaw, "decrypt")]);
    let sealed = 0;
    let opened = 0;
    const aad = new Uint8Array([DATA]);
    return {
      /// `plain` (bytes or a string) as a data frame payload: its number, then
      /// the ciphertext and tag.
      async seal(plain) {
        const counter = sealed++;
        const body = typeof plain === "string" ? utf8(plain) : plain;
        const cipher = new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv: nonceFor(counter), additionalData: aad }, sealKey, body));
        const head = new Uint8Array(8);
        new DataView(head.buffer).setBigUint64(0, BigInt(counter));
        return concat(head, cipher);
      },
      /// The plaintext of a data frame payload, which must be the next one.
      async open(payload) {
        if (payload.length < 8) throw new Error("frame does not decrypt");
        const counter = new DataView(payload.buffer, payload.byteOffset).getBigUint64(0);
        if (counter !== BigInt(opened)) throw new Error("frame out of order");
        let plain;
        try {
          plain = new Uint8Array(await subtle.decrypt({ name: "AES-GCM", iv: nonceFor(opened), additionalData: aad }, openKey, payload.subarray(8)));
        } catch {
          throw new Error("frame does not decrypt");
        }
        opened++;
        return plain;
      },
    };
  }

  /// Starts a handshake: `hello` is what to send the computer; `finish` takes
  /// its answer and gives the session, or throws with why it is not trusted.
  async function begin() {
    const pair = await subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
    const e = new Uint8Array(await subtle.exportKey("raw", pair.publicKey));
    const nonce = crypto.getRandomValues(new Uint8Array(16));
    const hello = utf8(JSON.stringify({ v: VERSION, e: b64url(e), nonce: b64url(nonce) }));
    return {
      hello,
      async finish(reply, label, channel) {
        let obj;
        try {
          obj = JSON.parse(text(reply));
        } catch {
          throw new Error("bad hello: not json");
        }
        const pc = await verifyPc(obj, label, channel, e, nonce);
        const theirs = await subtle.importKey("raw", pc.e, { name: "ECDH", namedCurve: "P-256" }, false, []);
        const shared = new Uint8Array(await subtle.deriveBits({ name: "ECDH", public: theirs }, pair.privateKey, 256));
        const keys = await deriveKeys(shared, nonce, pc.nonce);
        const session = await sessionFromKeys(keys.phoneToPc, keys.pcToPhone);
        session.arakun = pc.arakun;
        return session;
      },
    };
  }

  /// A phone frame: the type byte, then the payload.
  function encodeFrame(type, payload) {
    return concat(new Uint8Array([type]), payload);
  }

  function decodeFrame(bytes) {
    if (bytes.length < 1 || (bytes[0] !== HELLO && bytes[0] !== DATA && bytes[0] !== CONTROL)) return null;
    return { type: bytes[0], payload: bytes.subarray(1) };
  }

  return { VERSION, HELLO, DATA, CONTROL, labelOf, transcript, verifyPc, deriveKeys, sessionFromKeys, begin, encodeFrame, decodeFrame, b64url, fromB64url, utf8, text };
})();

globalThis.E2E = E2E;
