#!/usr/bin/env node
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// A fake computer and two fake devices against a running Worker, to see the
// relay work end to end before Arakun itself speaks the protocol.
//
//   node scripts/peers.mjs --base <url> --session-pc <token> --session-phone <token>
//   node scripts/peers.mjs --base http://127.0.0.1:8787 --local
//
// Both sessions belong to one account: sign in twice (the app's Account
// panel, or POST /auth/github) and pass the two session tokens. `--local`
// skips GitHub for a `wrangler dev` Worker: it writes a test account and two
// sessions straight into the local D1. The computer's key lives in
// .peers-key.json next to package.json; its label is derived from it.
// Node 24 or newer: the script imports the Worker's own .ts helpers.

import { execFileSync } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { CONTROL, DATA, HELLO, b64url, concat, parsePcFrame, parsePhoneFrame, pcControl, pcFrame, phoneFrame, sha256, utf8 } from "../src/relay/frames.ts";
import { hex, labelOf } from "../src/relay/label.ts";

const KEY_FILE = fileURLToPath(new URL("../.peers-key.json", import.meta.url));
const CLOUD_DIR = fileURLToPath(new URL("..", import.meta.url));
const TIMEOUT_MS = 8000;
/// The account `--local` makes; far from any real GitHub ID.
const LOCAL_ACCOUNT = 9_000_000_001;

// ---- the dance -------------------------------------------------------------

async function main() {
  const args = new Map();
  for (let i = 2; i < process.argv.length; i++) {
    const name = process.argv[i];
    if (name === "--local") args.set(name, "1");
    else args.set(name, process.argv[++i] ?? "");
  }

  const me = await identity();
  console.log(`computer label: ${me.label}`);

  const base = (args.get("--base") ?? "").replace(/\/$/, "");
  const local = args.has("--local");
  let pcSession = args.get("--session-pc");
  let phoneSession = args.get("--session-phone");
  if (!base || (!local && (!pcSession || !phoneSession))) {
    console.log("");
    console.log("Run it with the Worker's URL and two sessions of one account:");
    console.log("  node scripts/peers.mjs --base https://<host> --session-pc <token> --session-phone <token>");
    console.log("Or against `npx wrangler dev`, with sessions made in the local D1:");
    console.log("  node scripts/peers.mjs --base http://127.0.0.1:8787 --local");
    return;
  }
  if (local) [pcSession, phoneSession] = await localSessions();

  const wsBase = base.replace(/^http/, "ws");
  const bearer = (token) => ({ headers: { Authorization: `Bearer ${token}` } });
  let steps = 0;
  const step = (name, ok, detail = "") => {
    steps++;
    console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `  (${detail})` : ""}`);
    if (!ok) throw new Error(name);
  };

  try {
    const config = await fetch(`${base}/auth/config`);
    step("the Worker answers /auth/config", config.ok, String(config.status));

    // The computer claims its label with the account's session.
    const pc = await Peer.open(`${wsBase}/machines/${me.label}/pc?name=peers&kind=app`, parsePcFrame, bearer(pcSession));
    const challenge = await pc.control("challenge");
    const nonce = fromB64url(challenge.nonce);
    const sig = await sign(me.key, concat(utf8("arakun-relay-claim-v1\0"), nonce, utf8(me.label)));
    pc.send(pcControl({ type: "claim", pub: me.pubB64, sig: b64url(sig) }));
    const claimed = await pc.control();
    step("computer claims", claimed.type === "claimed", JSON.stringify(claimed));

    // The account's list has it, online, with its key.
    const list = await (await fetch(`${base}/machines`, bearer(phoneSession))).json();
    const listed = Array.isArray(list) ? list.find((m) => m.label === me.label) : null;
    step("the account lists the machine online", listed?.online === true && listed.pub === me.pubB64, JSON.stringify(listed ?? list));

    // Two pages of a device come in with the device's session.
    const a = await Peer.open(`${wsBase}/machines/${me.label}/ws?page=${pageId()}`, parsePhoneFrame, bearer(phoneSession));
    const aOpen = await a.control();
    step("device A comes in", aOpen.type === "open" && aOpen.channel > 0, JSON.stringify(aOpen));
    const aSeen = await pc.control();
    step("computer hears device A open, and who it is", aSeen.type === "open" && aSeen.channel === aOpen.channel && typeof aSeen.device === "string" && typeof aSeen.account === "number", JSON.stringify(aSeen));

    const b = await Peer.open(`${wsBase}/machines/${me.label}/ws?page=${pageId()}`, parsePhoneFrame, bearer(phoneSession));
    const bOpen = await b.control();
    step("device B comes in", bOpen.type === "open" && bOpen.channel === aOpen.channel + 1, JSON.stringify(bOpen));
    const bSeen = await pc.control();
    step("computer hears device B open", bSeen.type === "open" && bSeen.channel === bOpen.channel, JSON.stringify(bSeen));

    // Bytes cross both ways, each device getting only its own. Two sockets'
    // frames may reach the computer in either order.
    a.send(phoneFrame(HELLO, utf8("hello from A")));
    b.send(phoneFrame(DATA, new Uint8Array([1, 2, 3])));
    const arrived = [await pc.frame(), await pc.frame()];
    const fromA = arrived.find((f) => f.channel === aOpen.channel);
    const fromB = arrived.find((f) => f.channel === bOpen.channel);
    step("computer gets A's hello on A's channel", fromA?.type === HELLO && text(fromA.payload) === "hello from A", describe(fromA));
    step("computer gets B's data on B's channel", fromB?.type === DATA && [...fromB.payload].join() === "1,2,3", describe(fromB));
    pc.send(pcFrame(DATA, bOpen.channel, new Uint8Array([9, 9])));
    pc.send(pcFrame(HELLO, aOpen.channel, utf8("hello A")));
    const toB = await b.frame();
    const toA = await a.frame();
    step("device B gets its data", toB.type === DATA && [...toB.payload].join() === "9,9", describe(toB));
    step("device A gets its hello", toA.type === HELLO && text(toA.payload) === "hello A", describe(toA));

    // Without a session, nobody comes in.
    const stranger = await Peer.open(`${wsBase}/machines/${me.label}/ws`, parsePhoneFrame);
    const refused = await stranger.control();
    step("a device without a session is told signed_out", refused.type === "bye" && refused.reason === "signed_out", JSON.stringify(refused));

    // Devices leave; the computer hears it.
    a.ws.close(1000, "done");
    const aClosed = await pc.control();
    step("computer hears device A close", aClosed.type === "close" && aClosed.channel === aOpen.channel, JSON.stringify(aClosed));
    pc.send(pcFrame(CONTROL, bOpen.channel, utf8(JSON.stringify({ type: "close", reason: "bye" }))));
    const bGone = await b.control();
    step("the computer closes device B's channel", bGone.type === "bye" && bGone.reason === "bye", JSON.stringify(bGone));

    console.log(`\nall ${steps} steps passed against ${base}`);
    process.exitCode = 0;
  } catch (err) {
    console.error(`\nstopped: ${err.message}`);
    process.exitCode = 1;
  }
  // Let every socket finish closing before Node goes: exiting with sockets
  // mid-close trips an assertion in libuv on Windows.
  await Peer.closeAll();
}

// ---- helpers --------------------------------------------------------------

/// Two sessions of a test account, written into `wrangler dev`'s local D1.
/// Only the hashes go in, as sign-in stores them.
async function localSessions() {
  const tokens = [b64url(crypto.getRandomValues(new Uint8Array(32))), b64url(crypto.getRandomValues(new Uint8Array(32)))];
  const hashes = await Promise.all(tokens.map(async (t) => hex(await sha256(utf8(t)))));
  const now = Math.floor(Date.now() / 1000);
  const rows = tokens.map((_t, i) => {
    const hash = hashes[i];
    const id = hex(crypto.getRandomValues(new Uint8Array(16)));
    const [name, kind] = i === 0 ? ["peers computer", "app"] : ["peers device", "phone"];
    return `INSERT INTO sessions (token_hash, id, name, kind, github_id, created_at, expires_at, last_used_at) VALUES ('${hash}', '${id}', '${name}', '${kind}', ${LOCAL_ACCOUNT}, ${now}, ${now + 86_400}, ${now});`;
  });
  const sql = [
    `INSERT INTO users (github_id, login, created_at, last_seen_at) VALUES (${LOCAL_ACCOUNT}, 'peers', ${now}, ${now}) ON CONFLICT DO NOTHING;`,
    ...rows,
  ].join(" ");
  execFileSync("npx", ["wrangler", "d1", "execute", "arakun-cloud-dev", "--local", "--command", sql], { cwd: CLOUD_DIR, stdio: "ignore" });
  console.log(`two sessions of account ${LOCAL_ACCOUNT} written to the local D1`);
  return tokens;
}

/// A page id, as the page makes one.
function pageId() {
  return hex(crypto.getRandomValues(new Uint8Array(8)));
}

async function identity() {
  let jwk;
  try {
    jwk = JSON.parse(await readFile(KEY_FILE, "utf8"));
  } catch {
    const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
    jwk = await crypto.subtle.exportKey("jwk", pair.privateKey);
    await writeFile(KEY_FILE, JSON.stringify(jwk, null, 2));
    console.log(`new key written to ${KEY_FILE}`);
  }
  const key = await crypto.subtle.importKey("jwk", jwk, { name: "ECDSA", namedCurve: "P-256" }, true, ["sign"]);
  const { d: _d, ...publicJwk } = jwk;
  const publicKey = await crypto.subtle.importKey("jwk", { ...publicJwk, key_ops: ["verify"] }, { name: "ECDSA", namedCurve: "P-256" }, true, ["verify"]);
  const pub = new Uint8Array(await crypto.subtle.exportKey("raw", publicKey));
  return { key, pub, pubB64: b64url(pub), label: await labelOf(pub) };
}

async function sign(key, data) {
  return new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, data));
}

function fromB64url(s) {
  return Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
}

function text(bytes) {
  return new TextDecoder().decode(bytes);
}

/// A frame in a line: type, channel if any, and its bytes as text or numbers.
function describe(frame) {
  if (!frame) return "nothing";
  const where = frame.channel === undefined ? "" : ` ch ${frame.channel}`;
  const body = frame.type === DATA ? [...frame.payload].join() : text(frame.payload);
  return `type ${frame.type}${where}: ${body}`;
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

/// One socket with a queue of what arrived, parsed with the side's frame parser.
class Peer {
  static all = [];

  /// `init` may carry headers: Node's WebSocket sends them with the upgrade.
  static open(url, parse, init = {}) {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url, init);
      ws.binaryType = "arraybuffer";
      const t = setTimeout(() => reject(new Error(`no socket at ${url}`)), TIMEOUT_MS);
      ws.addEventListener("open", () => {
        clearTimeout(t);
        resolve(new Peer(ws, parse));
      });
      ws.addEventListener("error", () => {
        clearTimeout(t);
        reject(new Error(`socket failed: ${url}`));
      });
    });
  }

  /// Closes what is still open and waits for every close event, briefly.
  static async closeAll() {
    const pending = Peer.all.filter((p) => !p.closed);
    for (const p of pending) p.ws.close(1000, "done");
    await Promise.race([Promise.all(pending.map((p) => p.untilClosed())), sleep(2000)]);
  }

  constructor(ws, parse) {
    this.ws = ws;
    this.queue = [];
    this.waiting = [];
    this.closed = false;
    this.onClose = [];
    Peer.all.push(this);
    ws.addEventListener("message", (ev) => {
      const data = ev.data;
      this.push(typeof data === "string" ? { text: data } : (parse(new Uint8Array(data)) ?? { bad: true }));
    });
    ws.addEventListener("close", (ev) => {
      this.closed = true;
      for (const f of this.onClose) f();
      this.push({ closed: { code: ev.code, reason: ev.reason } });
    });
  }

  untilClosed() {
    return this.closed ? Promise.resolve() : new Promise((r) => this.onClose.push(r));
  }

  push(v) {
    const w = this.waiting.shift();
    if (w) w(v);
    else this.queue.push(v);
  }

  send(bytes) {
    this.ws.send(bytes);
  }

  next() {
    const got = this.queue.shift();
    if (got !== undefined) return Promise.resolve(got);
    return new Promise((resolve, reject) => {
      const waiter = (v) => {
        clearTimeout(t);
        resolve(v);
      };
      const t = setTimeout(() => {
        this.waiting = this.waiting.filter((w) => w !== waiter);
        reject(new Error("nothing arrived in time"));
      }, TIMEOUT_MS);
      this.waiting.push(waiter);
    });
  }

  async frame() {
    const got = await this.next();
    if (got.closed) throw new Error(`socket closed: ${JSON.stringify(got.closed)}`);
    return got;
  }

  async control(expected) {
    const got = await this.frame();
    if (got.type !== CONTROL) throw new Error(`not a control frame: ${JSON.stringify(got)}`);
    const msg = JSON.parse(text(got.payload));
    if (expected && msg.type !== expected) throw new Error(`expected ${expected}, got ${JSON.stringify(msg)}`);
    return msg;
  }
}

await main();
