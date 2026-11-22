// Fixed vectors for src/arakun_teams/crypto.rs, made with WebCrypto exactly as
// the teams Worker and its tests (teams/test/clients.ts) make and check them.
//
//   node src/arakun_teams/testdata/vectors.mjs > src/arakun_teams/testdata/vectors.json
//       makes new WebCrypto vectors (keys, a signature, an envelope, a message
//       and a channel name). The device keys are new each time, so the old
//       `rust` part no longer holds and is left out (`rust: null`). Make it
//       again and put what it prints in the file as `rust`:
//         cargo test --locked --no-default-features print_rust_vectors -- --ignored --nocapture
//   node src/arakun_teams/testdata/vectors.mjs --check
//       checks the `rust` part (values crypto.rs made, see its tests) with
//       WebCrypto, as the Worker would: exits 1 if one does not hold.

import { readFileSync } from "node:fs";
import { webcrypto as crypto } from "node:crypto";

const here = new URL(".", import.meta.url);
const subtle = crypto.subtle;
const utf8 = (s) => new TextEncoder().encode(s);
const b64url = (b) => Buffer.from(b).toString("base64url");
const unb64 = (s) => new Uint8Array(Buffer.from(s, "base64url"));
const P256 = { name: "ECDSA", namedCurve: "P-256" };
const ECDH = { name: "ECDH", namedCurve: "P-256" };

async function device() {
  const sig = await subtle.generateKey(P256, true, ["sign", "verify"]);
  const enc = await subtle.generateKey(ECDH, true, ["deriveBits"]);
  const sigPub = new Uint8Array(await subtle.exportKey("raw", sig.publicKey));
  const encPub = new Uint8Array(await subtle.exportKey("raw", enc.publicKey));
  const digest = new Uint8Array(await subtle.digest("SHA-256", sigPub));
  return {
    keys: { sig, enc },
    sig_pkcs8: b64url(new Uint8Array(await subtle.exportKey("pkcs8", sig.privateKey))),
    enc_d: (await subtle.exportKey("jwk", enc.privateKey)).d,
    sig_pub: b64url(sigPub),
    enc_pub: b64url(encPub),
    device_id: b64url(digest.slice(0, 16)),
  };
}

async function sign(d, text) {
  return b64url(new Uint8Array(await subtle.sign({ name: "ECDSA", hash: "SHA-256" }, d.keys.sig.privateKey, utf8(text))));
}

async function verify(sigPub, text, sig) {
  const key = await subtle.importKey("raw", unb64(sigPub), P256, false, ["verify"]);
  const raw = unb64(sig);
  return raw.length === 64 && subtle.verify({ name: "ECDSA", hash: "SHA-256" }, key, raw, utf8(text));
}

async function envelope(teamId, epoch, teamKey, to, from) {
  const eph = await subtle.generateKey(ECDH, true, ["deriveBits"]);
  const ephRaw = new Uint8Array(await subtle.exportKey("raw", eph.publicKey));
  const theirs = await subtle.importKey("raw", unb64(to.enc_pub), ECDH, false, []);
  const shared = await subtle.deriveBits({ name: "ECDH", public: theirs }, eph.privateKey, 256);
  const hkdf = await subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
  const info = utf8(`arakun-teams/v1/envelope|${teamId}|${epoch}|${to.device_id}`);
  const key = await subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt: ephRaw, info }, hkdf, { name: "AES-GCM", length: 256 }, false, ["encrypt"]);
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv: nonce, additionalData: info }, key, teamKey));
  const e = { v: 1, team_id: teamId, epoch, device_id: to.device_id, from: from.device_id, eph_pub: b64url(ephRaw), nonce: b64url(nonce), ct: b64url(ct) };
  return { ...e, sig: await sign(from, `arakun-teams/v1/envelope|${teamId}|${epoch}|${to.device_id}|${e.eph_pub}|${e.nonce}|${e.ct}`) };
}

/// The team key out of an envelope, as a recipient would open it.
async function openEnvelope(e, to, encD) {
  const jwk = { kty: "EC", crv: "P-256", d: encD, x: b64url(unb64(to.enc_pub).slice(1, 33)), y: b64url(unb64(to.enc_pub).slice(33)) };
  const mine = await subtle.importKey("jwk", jwk, ECDH, false, ["deriveBits"]);
  const eph = await subtle.importKey("raw", unb64(e.eph_pub), ECDH, false, []);
  const shared = await subtle.deriveBits({ name: "ECDH", public: eph }, mine, 256);
  const hkdf = await subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
  const info = utf8(`arakun-teams/v1/envelope|${e.team_id}|${e.epoch}|${e.device_id}`);
  const key = await subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt: unb64(e.eph_pub), info }, hkdf, { name: "AES-GCM", length: 256 }, false, ["decrypt"]);
  return new Uint8Array(await subtle.decrypt({ name: "AES-GCM", iv: unb64(e.nonce), additionalData: info }, key, unb64(e.ct)));
}

async function seal(teamKey, nonce, ad, plaintext) {
  const key = await subtle.importKey("raw", teamKey, "AES-GCM", false, ["encrypt"]);
  return b64url(new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv: nonce, additionalData: utf8(ad) }, key, utf8(plaintext))));
}

async function open(teamKey, nonce, ad, ct) {
  const key = await subtle.importKey("raw", teamKey, "AES-GCM", false, ["decrypt"]);
  return new TextDecoder().decode(await subtle.decrypt({ name: "AES-GCM", iv: unb64(nonce), additionalData: utf8(ad) }, key, unb64(ct)));
}

async function make() {
  const githubId = 9001;
  const teamId = "01JD0000000000000000000000";
  const channelId = "01JD0000000000000000000001";
  const messageId = "01JD0000000000000000000002";
  const epoch = 3;
  const teamKey = Uint8Array.from({ length: 32 }, (_, i) => i);
  const nonce = Uint8Array.from({ length: 12 }, (_, i) => 0xa0 + i);
  const sender = await device();
  const recipient = await device();
  const endorsed = `arakun-teams/v1/device|${githubId}|${recipient.sig_pub}|${recipient.enc_pub}`;
  const body = JSON.stringify({ kind: "text", text: "hello, team", reply_to: null, mentions: [], bot: null, decision: null });
  const ct = await seal(teamKey, nonce, `arakun-teams/v1/message|${teamId}|${channelId}|${messageId}`, body);
  const n = b64url(nonce);
  const strip = ({ keys, ...d }) => d;
  return {
    about: "Made by vectors.mjs with WebCrypto; crypto.rs's tests read it.",
    github_id: githubId,
    team_id: teamId,
    channel_id: channelId,
    epoch,
    team_key: b64url(teamKey),
    sender: strip(sender),
    recipient: strip(recipient),
    endorsement: { text: endorsed, sig: await sign(sender, endorsed) },
    envelope: await envelope(teamId, epoch, teamKey, recipient, sender),
    message: {
      body,
      wire: {
        id: messageId,
        device_id: sender.device_id,
        epoch,
        nonce: n,
        ct,
        sig: await sign(sender, `arakun-teams/v1/message|${teamId}|${channelId}|${messageId}|${epoch}|${n}|${ct}`),
      },
    },
    channel_name: {
      name: "general",
      sealed: { v: 1, epoch, nonce: n, ct: await seal(teamKey, nonce, `arakun-teams/v1/channel|${teamId}|${channelId}|${channelId}`, "general") },
    },
    // Signed and sealed with the keys of the file this replaces: see the top.
    rust: null,
  };
}

async function check() {
  const v = JSON.parse(readFileSync(new URL("vectors.json", here), "utf8"));
  const r = v.rust;
  if (!r) throw new Error("vectors.json has no `rust` part yet");
  const fails = [];
  // Rust signed as the recipient device; the sender verifies with its sig_pub.
  if (!(await verify(v.recipient.sig_pub, r.endorsement.text, r.endorsement.sig))) fails.push("endorsement signature");
  const e = r.envelope;
  const signed = `arakun-teams/v1/envelope|${e.team_id}|${e.epoch}|${e.device_id}|${e.eph_pub}|${e.nonce}|${e.ct}`;
  if (!(await verify(v.recipient.sig_pub, signed, e.sig))) fails.push("envelope signature");
  const key = await openEnvelope(e, v.sender, v.sender.enc_d);
  if (b64url(key) !== v.team_key) fails.push("envelope key");
  const m = r.message.wire;
  const msigned = `arakun-teams/v1/message|${v.team_id}|${v.channel_id}|${m.id}|${m.epoch}|${m.nonce}|${m.ct}`;
  if (!(await verify(v.recipient.sig_pub, msigned, m.sig))) fails.push("message signature");
  const text = await open(unb64(v.team_key), m.nonce, `arakun-teams/v1/message|${v.team_id}|${v.channel_id}|${m.id}`, m.ct);
  if (text !== r.message.body) fails.push("message body");
  if (fails.length) {
    console.error(`WebCrypto refused: ${fails.join(", ")}`);
    process.exit(1);
  }
  console.log("WebCrypto accepts every value crypto.rs made.");
}

if (process.argv.includes("--check")) await check();
else console.log(JSON.stringify(await make(), null, 2));
