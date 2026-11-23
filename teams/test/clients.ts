// Users, devices and team keys made of WebCrypto, so every signature and
// envelope the Worker checks is a real one.
import { SELF } from "cloudflare:test";
import { b64url, fromB64url, ulid, utf8 } from "../src/util";
import { ADMIN, call, signIn } from "./helpers";

export interface User {
  id: number;
  login: string;
  session: string;
}

/// Signs in, and grants the `teams` flag unless told not to.
export async function user(id: number, login: string, flag = true): Promise<User> {
  const got = await signIn(id, login);
  if (flag) await call("PUT", `/admin/users/${id}/flags/teams`, ADMIN);
  return { id, login, session: got.session };
}

export interface Device {
  sig: CryptoKeyPair;
  enc: CryptoKeyPair;
  sigPub: string;
  encPub: string;
  id: string;
}

async function rawPub(key: CryptoKey): Promise<Uint8Array> {
  return new Uint8Array((await crypto.subtle.exportKey("raw", key)) as ArrayBuffer);
}

/// A device's two key pairs, and the ID the server will derive.
export async function makeDevice(): Promise<Device> {
  const sig = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const enc = (await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"])) as CryptoKeyPair;
  const sigRaw = await rawPub(sig.publicKey);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", sigRaw as BufferSource));
  return { sig, enc, sigPub: b64url(sigRaw), encPub: b64url(await rawPub(enc.publicKey)), id: b64url(digest.slice(0, 16)) };
}

/// ECDSA P-256 SHA-256 over the UTF-8 text, raw r||s, base64url.
export async function signText(d: Device, text: string): Promise<string> {
  const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, d.sig.privateKey, utf8(text) as BufferSource);
  return b64url(new Uint8Array(sig));
}

export interface Options {
  device?: Device | string;
  body?: unknown;
  headers?: Record<string, string>;
}

export function api(u: User, method: string, path: string, opts: Options = {}): Promise<Response> {
  const headers: Record<string, string> = { Authorization: `Bearer ${u.session}`, ...opts.headers };
  if (opts.device) headers["X-Arakun-Device"] = typeof opts.device === "string" ? opts.device : opts.device.id;
  if (opts.body !== undefined) headers["Content-Type"] = "application/json";
  return SELF.fetch(`https://teams.test${path}`, {
    method,
    headers,
    body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
  });
}

/// The JSON of an answer that must have the given status.
export async function expectJson<T = any>(res: Response | Promise<Response>, status: number): Promise<T> {
  const r = await res;
  const text = await r.text();
  if (r.status !== status) throw new Error(`expected ${status}, got ${r.status}: ${text}`);
  return text ? JSON.parse(text) : (undefined as T);
}

export async function register(u: User, d: Device, endorser: Device | null = null, name = "laptop"): Promise<Response> {
  const endorsement = endorser ? { by: endorser.id, sig: await signText(endorser, `arakun-teams/v1/device|${u.id}|${d.sigPub}|${d.encPub}`) } : null;
  return api(u, "POST", "/devices", { body: { sig_pub: d.sigPub, enc_pub: d.encPub, name, endorsement } });
}

/// One epoch's team key, sealed to one device as PROTOCOL.md says.
export async function envelope(teamId: string, epoch: number, teamKey: Uint8Array, to: Device, from: Device): Promise<Record<string, unknown>> {
  const eph = (await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"])) as CryptoKeyPair;
  const ephRaw = await rawPub(eph.publicKey);
  const theirs = await crypto.subtle.importKey("raw", fromB64url(to.encPub)! as BufferSource, { name: "ECDH", namedCurve: "P-256" }, false, []);
  // The runtime reads `public`; workers-types spells it `$public`.
  const ecdh = { name: "ECDH", public: theirs } as unknown as SubtleCryptoDeriveKeyAlgorithm;
  const shared = await crypto.subtle.deriveBits(ecdh, eph.privateKey, 256);
  const hkdf = await crypto.subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
  const info = utf8(`arakun-teams/v1/envelope|${teamId}|${epoch}|${to.id}`);
  const key = await crypto.subtle.deriveKey(
    { name: "HKDF", hash: "SHA-256", salt: ephRaw as BufferSource, info: info as BufferSource },
    hkdf,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt"],
  );
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce, additionalData: info as BufferSource }, key, teamKey as BufferSource));
  const e = { v: 1, team_id: teamId, epoch, device_id: to.id, from: from.id, eph_pub: b64url(ephRaw), nonce: b64url(nonce), ct: b64url(ct) };
  const sig = await signText(from, `arakun-teams/v1/envelope|${teamId}|${epoch}|${to.id}|${e.eph_pub}|${e.nonce}|${e.ct}`);
  return { ...e, sig };
}

async function aes(teamKey: Uint8Array): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", teamKey as BufferSource, "AES-GCM", false, ["encrypt"]);
}

/// A Sealed value: AES-256-GCM with the team key, PROTOCOL.md's associated data.
export async function seal(teamKey: Uint8Array, epoch: number, ad: string, plaintext: string): Promise<{ v: 1; epoch: number; nonce: string; ct: string }> {
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce, additionalData: utf8(ad) as BufferSource }, await aes(teamKey), utf8(plaintext) as BufferSource);
  return { v: 1, epoch, nonce: b64url(nonce), ct: b64url(new Uint8Array(ct)) };
}

export interface Wire {
  id: string;
  device_id: string;
  epoch: number;
  nonce: string;
  ct: string;
  sig: string;
}

/// A text message as a client sends it: sealed, then signed by the device.
export async function message(teamId: string, channelId: string, d: Device, epoch: number, teamKey: Uint8Array, text: string): Promise<Wire> {
  const id = ulid();
  const body = JSON.stringify({ kind: "text", text, reply_to: null, mentions: [], bot: null, decision: null });
  const sealed = await seal(teamKey, epoch, `arakun-teams/v1/message|${teamId}|${channelId}|${id}`, body);
  const sig = await signText(d, `arakun-teams/v1/message|${teamId}|${channelId}|${id}|${epoch}|${sealed.nonce}|${sealed.ct}`);
  return { id, device_id: d.id, epoch, nonce: sealed.nonce, ct: sealed.ct, sig };
}

export function newKey(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(32));
}

/// Every presence list the socket got, in order.
export class Presence {
  private got: number[][] = [];
  private waiting: (() => void)[] = [];

  /// How many lists arrived so far.
  get count(): number {
    return this.got.length;
  }

  /// The last list received, if any.
  get latest(): number[] | undefined {
    return this.got.at(-1);
  }

  push(v: unknown): void {
    this.got.push(v as number[]);
    for (const w of this.waiting.splice(0)) w();
  }

  /// Waits until the latest list equals `want`.
  async until(want: number[], ms = 2000): Promise<void> {
    const deadline = Date.now() + ms;
    for (;;) {
      const last = this.got.at(-1);
      if (last && JSON.stringify(last) === JSON.stringify(want)) return;
      const left = deadline - Date.now();
      if (left <= 0) throw new Error(`presence is ${JSON.stringify(last)}, wanted ${JSON.stringify(want)}`);
      await new Promise<void>((resolve) => {
        const t = setTimeout(resolve, left);
        this.waiting.push(() => {
          clearTimeout(t);
          resolve();
        });
      });
    }
  }
}

/// What a socket said, in order, with a way to wait for the next thing. The
/// same pattern as relay/test/helpers.ts, for JSON text frames.
export class Inbox {
  private queue: unknown[] = [];
  private waiting: ((v: unknown) => void)[] = [];
  closed: { code: number; reason: string } | null = null;
  /// Presence frames go here instead, so tests about messages never see them.
  readonly here = new Presence();

  constructor(ws: WebSocket) {
    ws.addEventListener("message", (ev) => {
      const v = JSON.parse(ev.data as string) as { type?: string; here?: number[] };
      if (v.type === "presence") this.here.push(v.here);
      else this.push(v);
    });
    ws.addEventListener("close", (ev) => {
      this.closed = { code: ev.code, reason: ev.reason };
      this.push({ closed: this.closed });
    });
  }

  private push(v: unknown) {
    const w = this.waiting.shift();
    if (w) w(v);
    else this.queue.push(v);
  }

  next(ms = 2000): Promise<any> {
    const got = this.queue.shift();
    if (got !== undefined) return Promise.resolve(got);
    return new Promise((resolve, reject) => {
      const waiter = (v: unknown) => {
        clearTimeout(t);
        resolve(v);
      };
      // A waiter that gave up must not eat the next thing that arrives.
      const t = setTimeout(() => {
        this.waiting = this.waiting.filter((w) => w !== waiter);
        reject(new Error("nothing arrived in time"));
      }, ms);
      this.waiting.push(waiter);
    });
  }

  /// Waits for the close event (anything before it is dropped).
  async closedWith(): Promise<{ code: number; reason: string }> {
    for (;;) {
      const got = (await this.next()) as { closed?: { code: number; reason: string } };
      if (got.closed) return got.closed;
    }
  }
}

export interface Socket {
  ws: WebSocket;
  inbox: Inbox;
  send(m: unknown): void;
}

export async function openChannel(u: User, d: Device, teamId: string, channelId: string, headers: Record<string, string> = {}, query = ""): Promise<Socket> {
  const res = await api(u, "GET", `/teams/${teamId}/channels/${channelId}/ws${query}`, { device: d, headers: { Upgrade: "websocket", ...headers } });
  if (res.status !== 101 || !res.webSocket) throw new Error(`no socket: ${res.status} ${await res.text()}`);
  const ws = res.webSocket;
  const inbox = new Inbox(ws);
  ws.accept();
  return { ws, inbox, send: (m) => ws.send(JSON.stringify({ type: "send", message: m })) };
}

export interface Member {
  user: User;
  device: Device;
}

export interface TeamSetup {
  owner: Member;
  teamId: string;
  channelId: string;
  /// Team key by epoch.
  keys: Map<number, Uint8Array>;
}

let nextId = 5000;

/// A team with its owner, the epoch-1 key on the owner's device, and a channel.
export async function team(): Promise<TeamSetup> {
  const id = nextId++;
  const owner = { user: await user(id, `owner-${id}`), device: await makeDevice() };
  await expectJson(register(owner.user, owner.device), 201);
  const { team_id: teamId } = await expectJson<{ team_id: string }>(api(owner.user, "POST", "/teams", { body: { name: "Arakun" } }), 201);
  const keys = new Map([[1, newKey()]]);
  await expectJson(
    api(owner.user, "PUT", `/teams/${teamId}/envelopes`, { device: owner.device, body: { envelopes: [await envelope(teamId, 1, keys.get(1)!, owner.device, owner.device)] } }),
    200,
  );
  const channelId = ulid();
  const name_ct = await seal(keys.get(1)!, 1, `arakun-teams/v1/channel|${teamId}|${channelId}|${channelId}`, "general");
  await expectJson(api(owner.user, "POST", `/teams/${teamId}/channels`, { body: { channel_id: channelId, name_ct } }), 201);
  return { owner, teamId, channelId, keys };
}

/// Invite → accept → approve with envelopes for every epoch in `keys`, which
/// holds the keys that were uploaded.
export async function join(t: TeamSetup, login?: string): Promise<Member> {
  const id = nextId++;
  const m = { user: await user(id, login ?? `member-${id}`), device: await makeDevice() };
  await expectJson(register(m.user, m.device), 201);
  await expectJson(api(t.owner.user, "POST", `/teams/${t.teamId}/invites`, { body: { github_login: m.user.login } }), 201);
  const [inv] = await expectJson<{ invite_id: string }[]>(api(m.user, "GET", "/invites"), 200);
  await expectJson(api(m.user, "POST", `/invites/${inv!.invite_id}/accept`, { device: m.device }), 200);
  const pending = await expectJson<{ github_id: number; request_id: string }[]>(api(t.owner.user, "GET", `/teams/${t.teamId}/pending`), 200);
  const request_id = pending.find((p) => p.github_id === id)!.request_id;
  const envelopes = await Promise.all([...t.keys.keys()].map((e) => envelope(t.teamId, e, t.keys.get(e)!, m.device, t.owner.device)));
  await expectJson(api(t.owner.user, "POST", `/teams/${t.teamId}/pending/${id}/approve`, { device: t.owner.device, body: { request_id, envelopes } }), 200);
  return m;
}
