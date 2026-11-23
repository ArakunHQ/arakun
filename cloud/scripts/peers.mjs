#!/usr/bin/env node
// A fake computer and two fake phones against a deployed relay, to see a
// deployment work end to end before Arakun itself speaks the protocol.
//
//   node scripts/peers.mjs                  prints the computer's label (and makes its key)
//   node scripts/peers.mjs --base <url>     runs the whole dance against that relay
//
// The key lives in .peers-key.json next to package.json: the label is derived
// from it, and the Worker has to be reachable under that label (deployed as
// `wrangler deploy --name <label>`, or `wrangler dev` with LABEL_OVERRIDE).
// Node 24 or newer: the script imports the Worker's own .ts helpers.

import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { CONTROL, DATA, HELLO, b64url, concat, parsePcFrame, parsePhoneFrame, pcControl, pcFrame, phoneFrame, sha256, utf8 } from "../src/relay/frames.ts";
import { hex, labelOf } from "../src/relay/label.ts";

const KEY_FILE = fileURLToPath(new URL("../.peers-key.json", import.meta.url));
const TIMEOUT_MS = 8000;

// ---- the dance -------------------------------------------------------------

async function main() {
  const args = new Map();
  for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i], process.argv[i + 1] ?? "");

  const me = await identity();
  console.log(`computer label: ${me.label}`);

  const base = args.get("--base");
  if (!base) {
    console.log("");
    console.log("Deploy the relay under that label and run again with --base:");
    console.log(`  npx wrangler deploy --name ${me.label}`);
    console.log(`  node scripts/peers.mjs --base https://${me.label}.<account>.workers.dev`);
    console.log("Or locally:");
    console.log(`  echo LABEL_OVERRIDE=${me.label} > .dev.vars && npx wrangler dev`);
    console.log("  node scripts/peers.mjs --base http://127.0.0.1:8787");
    return;
  }

  const wsBase = base.replace(/^http/, "ws").replace(/\/$/, "");
  let steps = 0;
  const step = (name, ok, detail = "") => {
    steps++;
    console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `  (${detail})` : ""}`);
    if (!ok) throw new Error(name);
  };

  try {
    const status = await fetch(`${base}/_relay`);
    const body = await status.json().catch(() => ({}));
    step("relay answers /_relay with our label", status.ok && body.label === me.label, JSON.stringify(body));

    // The computer claims its label.
    const pc = await Peer.open(`${wsBase}/pc`, parsePcFrame);
    const challenge = await pc.control("challenge");
    const nonce = fromB64url(challenge.nonce);
    const sig = await sign(me.key, concat(nonce, utf8(me.label)));
    pc.send(pcControl({ type: "claim", pub: me.pubB64, sig: b64url(sig) }));
    const claimed = await pc.control();
    step("computer claims", claimed.type === "claimed", JSON.stringify(claimed));

    // It issues a ticket and opens pairing.
    const ticket = b64url(crypto.getRandomValues(new Uint8Array(32)));
    const code = b64url(crypto.getRandomValues(new Uint8Array(16)));
    pc.send(pcControl({ type: "tickets", hashes: [hex(await sha256(utf8(ticket)))] }));
    pc.send(pcControl({ type: "pair_open", hash: hex(await sha256(utf8(code))), ttl: 120 }));
    await sleep(300); // the two are in flight; no acknowledgement to wait for

    // A phone pairs with the code.
    const a = await Peer.open(`${wsBase}/ws?p=${code}`, parsePhoneFrame);
    const aOpen = await a.control();
    step("phone A enters with the pairing code", aOpen.type === "open" && aOpen.channel > 0, JSON.stringify(aOpen));
    const aSeen = await pc.control();
    step("computer hears phone A open", aSeen.type === "open" && aSeen.channel === aOpen.channel && aSeen.device === null, JSON.stringify(aSeen));

    // Another phone enters with the ticket.
    const b = await Peer.open(`${wsBase}/ws?d=peers-b&t=${ticket}`, parsePhoneFrame);
    const bOpen = await b.control();
    step("phone B enters with the ticket", bOpen.type === "open" && bOpen.channel === aOpen.channel + 1, JSON.stringify(bOpen));
    const bSeen = await pc.control();
    step("computer hears phone B open", bSeen.type === "open" && bSeen.device === "peers-b", JSON.stringify(bSeen));

    // Bytes cross both ways, each phone getting only its own. Two sockets'
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
    step("phone B gets its data", toB.type === DATA && [...toB.payload].join() === "9,9", describe(toB));
    step("phone A gets its hello", toA.type === HELLO && text(toA.payload) === "hello A", describe(toA));

    // A spent code does not work twice.
    const again = await Peer.open(`${wsBase}/ws?p=${code}`, parsePhoneFrame);
    const refused = await again.control();
    step("the pairing code is spent", refused.type === "bye" && refused.reason === "no_pairing", JSON.stringify(refused));

    // Phones leave; the computer hears it.
    a.ws.close(1000, "done");
    const aClosed = await pc.control();
    step("computer hears phone A close", aClosed.type === "close" && aClosed.channel === aOpen.channel, JSON.stringify(aClosed));
    pc.send(pcControl({ type: "tickets", hashes: [] }));
    const bGone = await b.control();
    step("phone B is revoked when its ticket goes", bGone.type === "bye" && bGone.reason === "revoked", JSON.stringify(bGone));
    const bClosed = await pc.control();
    step("computer hears phone B close", bClosed.type === "close" && bClosed.channel === bOpen.channel, JSON.stringify(bClosed));

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

  static open(url, parse) {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
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
