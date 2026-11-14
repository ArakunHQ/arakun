// One computer's relay: its socket, its phones' sockets, and what it told us
// about who may come in. Sockets hibernate between messages, so everything a
// socket is lives in its attachment, and everything the computer told us in
// storage.
//
// The computer connects at /pc and is challenged: it signs the nonce and its
// label with the key its label derives from. Only then is it the computer, and
// only one computer is: a later claim replaces the earlier socket. Phones
// connect at /ws with a ticket the computer issued (we keep the hashes it sent)
// or, while pairing, with the one-use code the computer opened. Each admitted
// phone gets a channel number; the computer hears `open` and `close` for it,
// the phone hears whether the computer is there. Hello and data frames cross
// unread: a phone's go to the computer on its channel, the computer's go to
// the phone the channel names. How many phones, how many frames and how big
// is the label's plan, read from KV.

import { DurableObject } from "cloudflare:workers";
import {
  CONTROL,
  b64url,
  concat,
  fromB64url,
  parseJson,
  parsePcFrame,
  parsePhoneFrame,
  pcControl,
  pcFrame,
  phoneControl,
  phoneFrame,
  sha256,
  utf8,
} from "./frames";
import type { LabelCard } from "./admin";
import type { Env } from "./index";
import { hex, labelOf } from "./label";
import { planFor, type Plan } from "./limits";

/// What a socket is, kept on it across hibernation.
type Tag =
  | { role: "pc"; nonce: string; claimed: boolean }
  | { role: "phone"; channel: number; device: string | null; ticketHash: string | null };

/// What the computer opened for the next phone to pair with.
interface Pairing {
  hash: string;
  until: number;
}

/// Close codes, in the 4000s so they never collide with the protocol's own.
const CLOSE_BAD_FRAME = 4400;
const CLOSE_UNAUTHORIZED = 4401;
const CLOSE_FORBIDDEN = 4403;
const CLOSE_REPLACED = 4409;
const CLOSE_GONE = 4410;
const CLOSE_LIMIT = 4429;

/// The longest a pairing code may stay open, whatever the computer asks.
const PAIR_TTL_MAX_S = 300;

/// How long the plan read from KV is trusted before asking again.
const PLAN_CACHE_MS = 60_000;

/// The day's count is written to storage this often; a hibernation in between
/// loses at most this many, which is cheaper than a write per frame.
const COUNT_FLUSH_EVERY = 50;

/// A computer not seen for this long has moved on: its object empties itself.
/// Nothing is lost, the computer sends its key and tickets again on connecting.
const PURGE_AFTER_MS = 90 * 24 * 3_600_000;

/// How many frames crossed today; the day is UTC, as days since the epoch.
interface Today {
  day: number;
  n: number;
}

export class Relay extends DurableObject<Env> {
  private plan: { value: Plan; at: number } | null = null;
  private burst = { second: 0, n: 0 };
  private today: Today | null = null;

  override async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const label = request.headers.get("x-arakun-label") ?? "";
    if (!label) return new Response("no label", { status: 400 });
    if (url.pathname === "/_block" && request.method === "POST") return this.blocked();
    if (request.headers.get("Upgrade") !== "websocket") return new Response("expected a websocket", { status: 426 });
    if (url.pathname === "/pc") return this.acceptPc(label);
    if (url.pathname === "/ws") return this.acceptPhone(url.searchParams);
    return new Response("not found", { status: 404 });
  }

  // ---- the computer --------------------------------------------------------

  /// The computer's socket: challenged at once, trusted only after it answers.
  private async acceptPc(label: string): Promise<Response> {
    if ((await this.ctx.storage.get<string>("label")) !== label) await this.ctx.storage.put("label", label);
    const [client, server] = pair();
    const nonce = b64url(crypto.getRandomValues(new Uint8Array(16)));
    this.ctx.acceptWebSocket(server, ["pc"]);
    server.serializeAttachment({ role: "pc", nonce, claimed: false } satisfies Tag);
    server.send(pcControl({ type: "challenge", nonce }));
    return new Response(null, { status: 101, webSocket: client });
  }

  private async fromPc(ws: WebSocket, tag: Tag & { role: "pc" }, bytes: Uint8Array): Promise<void> {
    const frame = parsePcFrame(bytes);
    if (!frame) return this.bye(ws, "bad frame", CLOSE_BAD_FRAME);
    const control = frame.type === CONTROL && frame.channel === 0 ? parseJson(frame.payload) : null;
    if (!tag.claimed) {
      if (!control || control["type"] !== "claim") return this.bye(ws, "claim first", CLOSE_UNAUTHORIZED);
      return this.claim(ws, tag, control);
    }
    if (control) return this.pcSaid(ws, control);
    if (frame.channel === 0) return this.bye(ws, "bad frame", CLOSE_BAD_FRAME);
    const phone = this.phone(frame.channel);
    if (!phone) return void ws.send(pcControl({ type: "close", channel: frame.channel, reason: "gone" }));
    if (frame.type === CONTROL) {
      // The computer's one word to a phone through us: close its channel.
      const msg = parseJson(frame.payload);
      if (msg?.["type"] !== "close") return this.bye(ws, "bad frame", CLOSE_BAD_FRAME);
      return this.bye(phone, typeof msg["reason"] === "string" ? msg["reason"] : "closed", CLOSE_GONE);
    }
    const over = await this.over(frame.payload.length);
    if (over) return this.limited(phone, over);
    phone.send(phoneFrame(frame.type, frame.payload));
  }

  /// `claim{pub, sig}`: the signature of `nonce ‖ label` by the key the label derives from.
  private async claim(ws: WebSocket, tag: Tag & { role: "pc" }, msg: Record<string, unknown>): Promise<void> {
    const pub = typeof msg["pub"] === "string" ? fromB64url(msg["pub"]) : null;
    const sig = typeof msg["sig"] === "string" ? fromB64url(msg["sig"]) : null;
    const label = (await this.ctx.storage.get<string>("label")) ?? "";
    if (!pub || !sig || pub.length !== 65 || sig.length !== 64) return this.bye(ws, "bad claim", CLOSE_BAD_FRAME);
    if ((await labelOf(pub)) !== label) return this.bye(ws, "wrong_label", CLOSE_FORBIDDEN);
    const nonce = fromB64url(tag.nonce) ?? new Uint8Array();
    if (!(await verify(pub, sig, concat(nonce, utf8(label))))) return this.bye(ws, "bad_signature", CLOSE_UNAUTHORIZED);
    const known = await this.ctx.storage.get<string>("pub");
    const pubB64 = msg["pub"] as string;
    if (known && known !== pubB64) return this.bye(ws, "key_mismatch", CLOSE_FORBIDDEN);
    const now = Date.now();
    if (!known) await this.ctx.storage.put({ pub: pubB64, claimed_at: now });
    await this.ctx.storage.put("last_seen", now);
    await this.ctx.storage.deleteAlarm();
    for (const other of this.ctx.getWebSockets("pc")) {
      if (other !== ws) this.bye(other, "replaced", CLOSE_REPLACED);
    }
    ws.serializeAttachment({ ...tag, claimed: true } satisfies Tag);
    ws.send(pcControl({ type: "claimed" }));
    // The computer knows nothing of the phones already waiting: introduce them.
    for (const phone of this.phones()) {
      const t = phone.deserializeAttachment() as Tag & { role: "phone" };
      ws.send(pcControl({ type: "open", channel: t.channel, device: t.device }));
      this.tell(phone, { type: "pc_online" });
    }
    await this.leaveCard(label, now);
  }

  /// Objects cannot be listed, so each leaves a card in KV for the operator,
  /// refreshed at most once a day: KV writes are the scarce thing.
  private async leaveCard(label: string, now: number): Promise<void> {
    const last = (await this.ctx.storage.get<number>("carded_at")) ?? 0;
    if (now - last < 86_400_000) return;
    const card: LabelCard = {
      claimed_at: (await this.ctx.storage.get<number>("claimed_at")) ?? now,
      last_seen: now,
      phones: this.phones().length,
      today: (await this.todayCount(now)).n,
    };
    try {
      await this.env.CONFIG.put(`labels/${label}`, "", { metadata: card });
      await this.ctx.storage.put("carded_at", now);
    } catch {
      // KV down or over quota: the card can wait for the next claim.
    }
  }

  /// The operator blocked this label: everyone connected under it is sent away.
  /// New connections are stopped by the Worker before they get here.
  private blocked(): Response {
    // The computer first, so the one thing it hears is why.
    for (const ws of this.ctx.getWebSockets("pc")) this.bye(ws, "blocked", CLOSE_FORBIDDEN);
    for (const ws of this.ctx.getWebSockets("phone")) this.bye(ws, "blocked", CLOSE_FORBIDDEN);
    return new Response("blocked");
  }

  /// What a claimed computer tells the relay on channel 0.
  private async pcSaid(ws: WebSocket, msg: Record<string, unknown>): Promise<void> {
    switch (msg["type"]) {
      case "tickets": {
        // The whole list each time: what is not in it is out, sockets included.
        const hashes = Array.isArray(msg["hashes"]) ? msg["hashes"].filter((h): h is string => typeof h === "string") : [];
        await this.ctx.storage.put("tickets", hashes);
        for (const phone of this.phones()) {
          const t = phone.deserializeAttachment() as Tag & { role: "phone" };
          if (t.ticketHash && !hashes.includes(t.ticketHash)) this.bye(phone, "revoked", CLOSE_UNAUTHORIZED);
        }
        return;
      }
      case "pair_open": {
        const hash = typeof msg["hash"] === "string" ? msg["hash"] : "";
        const ttl = typeof msg["ttl"] === "number" ? Math.min(Math.max(msg["ttl"], 1), PAIR_TTL_MAX_S) : 0;
        if (!/^[0-9a-f]{64}$/.test(hash) || !ttl) return this.bye(ws, "bad pair_open", CLOSE_BAD_FRAME);
        await this.ctx.storage.put("pair", { hash, until: Date.now() + ttl * 1000 } satisfies Pairing);
        return;
      }
      case "pair_close":
        await this.ctx.storage.delete("pair");
        return;
      default:
        return this.bye(ws, "unknown control", CLOSE_BAD_FRAME);
    }
  }

  // ---- phones --------------------------------------------------------------

  /// A phone's socket: in with a ticket the computer issued, or with the
  /// pairing code while one is open. Anything else is told why and closed; a
  /// refused upgrade would look like a dead network to the page.
  private async acceptPhone(query: URLSearchParams): Promise<Response> {
    const [client, server] = pair();
    const admitted = await this.admit(query);
    const full = typeof admitted === "string" ? null : await this.full();
    if (typeof admitted === "string" || full) {
      this.ctx.acceptWebSocket(server, ["phone"]);
      server.serializeAttachment({ role: "phone", channel: 0, device: null, ticketHash: null } satisfies Tag);
      if (full) this.limited(server, full);
      else this.bye(server, admitted as string, CLOSE_UNAUTHORIZED);
      return new Response(null, { status: 101, webSocket: client });
    }
    const channel = ((await this.ctx.storage.get<number>("next_channel")) ?? 0) + 1;
    await this.ctx.storage.put("next_channel", channel);
    this.ctx.acceptWebSocket(server, ["phone", `ch:${channel}`]);
    server.serializeAttachment({ role: "phone", channel, ...admitted } satisfies Tag);
    server.send(phoneControl({ type: "open", channel }));
    if (!this.introduce(channel, admitted.device)) {
      server.send(phoneControl({ type: "pc_offline", since: (await this.ctx.storage.get<number>("last_seen")) ?? null }));
    }
    return new Response(null, { status: 101, webSocket: client });
  }

  /// Why one more phone cannot come in right now, if it cannot.
  private async full(): Promise<"phones" | "daily" | null> {
    const plan = await this.planNow();
    if (this.phones().length >= plan.phones) return "phones";
    if ((await this.todayCount(Date.now())).n >= plan.daily) return "daily";
    return null;
  }

  /// Who the phone is, or why it is not coming in.
  private async admit(query: URLSearchParams): Promise<{ device: string | null; ticketHash: string | null } | string> {
    const ticket = query.get("t");
    const device = query.get("d");
    const code = query.get("p");
    if (ticket && device) {
      const h = hex(await sha256(utf8(ticket)));
      const tickets = (await this.ctx.storage.get<string[]>("tickets")) ?? [];
      return tickets.includes(h) ? { device, ticketHash: h } : "bad_ticket";
    }
    if (code) {
      const open = await this.ctx.storage.get<Pairing>("pair");
      if (!open || open.until < Date.now()) return "no_pairing";
      if (hex(await sha256(utf8(code))) !== open.hash) return "bad_code";
      await this.ctx.storage.delete("pair"); // one use
      return { device: null, ticketHash: null };
    }
    return "no_credentials";
  }

  /// A phone's frames go to the computer on the phone's channel. Control is
  /// the relay's own voice, so a phone speaking it is sent away.
  private async fromPhone(ws: WebSocket, tag: Tag & { role: "phone" }, bytes: Uint8Array): Promise<void> {
    const frame = parsePhoneFrame(bytes);
    if (!frame || frame.type === CONTROL || !tag.channel) return this.bye(ws, "bad frame", CLOSE_BAD_FRAME);
    const over = await this.over(frame.payload.length);
    if (over) return this.limited(ws, over);
    // Without the computer there is nobody to give it to; the phone was told.
    this.computer()?.send(pcFrame(frame.type, tag.channel, frame.payload));
  }

  // ---- limits --------------------------------------------------------------

  /// The plan this label is on, asked of KV once a minute.
  private async planNow(): Promise<Plan> {
    const now = Date.now();
    if (!this.plan || now - this.plan.at > PLAN_CACHE_MS) this.plan = { value: await planFor(this.env.CONFIG), at: now };
    return this.plan.value;
  }

  /// Which limit a frame of `size` bytes would cross, or null while it is
  /// counted and may go through. The second's count lives in memory only: a
  /// burst keeps the object awake. The day's count survives hibernation
  /// through storage, written every few frames.
  private async over(size: number): Promise<"frame" | "burst" | "daily" | null> {
    const plan = await this.planNow();
    if (size > plan.frame) return "frame";
    const now = Date.now();
    const second = Math.floor(now / 1000);
    if (this.burst.second !== second) this.burst = { second, n: 0 };
    if (this.burst.n >= plan.burst) return "burst";
    const today = await this.todayCount(now);
    if (today.n >= plan.daily) return "daily";
    this.burst.n++;
    today.n++;
    if (today.n % COUNT_FLUSH_EVERY === 0) await this.ctx.storage.put("today", today);
    return null;
  }

  private async todayCount(now: number): Promise<Today> {
    const day = Math.floor(now / 86_400_000);
    if (!this.today) this.today = (await this.ctx.storage.get<Today>("today")) ?? { day, n: 0 };
    if (this.today.day !== day) this.today = { day, n: 0 };
    return this.today;
  }

  /// A phone that crossed a limit is told which and closed; its computer hears
  /// `close{channel, reason: "limit:…"}` like for any other closing.
  private limited(phone: WebSocket, which: string): void {
    this.bye(phone, `limit:${which}`, CLOSE_LIMIT, { type: "limit", which });
  }

  /// Ninety days without the computer: forget everything about it. Phones
  /// still waiting are sent away; the computer rebuilds the state on its next
  /// claim.
  override async alarm(): Promise<void> {
    if (this.computer()) return;
    for (const phone of this.phones()) this.bye(phone, "gone", CLOSE_GONE);
    const label = await this.ctx.storage.get<string>("label");
    if (label) await this.env.CONFIG.delete(`labels/${label}`).catch(() => undefined);
    await this.ctx.storage.deleteAll();
    this.today = null;
  }

  // ---- sockets -------------------------------------------------------------

  override async webSocketMessage(ws: WebSocket, message: ArrayBuffer | string): Promise<void> {
    const tag = ws.deserializeAttachment() as Tag | null;
    if (!tag) return this.bye(ws, "unknown socket", CLOSE_BAD_FRAME);
    if (typeof message === "string") return this.bye(ws, "binary frames only", CLOSE_BAD_FRAME);
    const bytes = new Uint8Array(message);
    if (tag.role === "pc") return this.fromPc(ws, tag, bytes);
    return this.fromPhone(ws, tag, bytes);
  }

  override async webSocketClose(ws: WebSocket, _code: number, reason: string, _wasClean: boolean): Promise<void> {
    const tag = ws.deserializeAttachment() as Tag | null;
    if (!tag) return;
    if (tag.role === "pc") {
      if (!tag.claimed) return;
      const now = Date.now();
      await this.ctx.storage.put("last_seen", now);
      // Replaced means another socket of the computer is already there.
      if (reason === "replaced") return;
      await this.ctx.storage.setAlarm(now + PURGE_AFTER_MS);
      for (const phone of this.phones()) this.tell(phone, { type: "pc_offline", since: now });
      return;
    }
    if (tag.channel) this.channelClosed(tag.channel, reason);
  }

  override async webSocketError(ws: WebSocket, _error: unknown): Promise<void> {
    try {
      ws.close(1011, "error");
    } catch {
      // already gone
    }
  }

  /// The computer's socket, once it has claimed. A socket we closed as replaced
  /// stays listed until its close completes; it must not be the one spoken to.
  private computer(): WebSocket | null {
    for (const ws of this.ctx.getWebSockets("pc")) {
      const tag = ws.deserializeAttachment() as Tag | null;
      if (tag?.role === "pc" && tag.claimed && ws.readyState === WebSocket.READY_STATE_OPEN) return ws;
    }
    return null;
  }

  /// Tells the computer a phone's channel is open; false when no computer is
  /// there to hear it, a socket going away included.
  private introduce(channel: number, device: string | null): boolean {
    const pc = this.computer();
    if (!pc) return false;
    try {
      pc.send(pcControl({ type: "open", channel, device }));
      return true;
    } catch {
      return false;
    }
  }

  /// The phone on a channel, while it is connected.
  private phone(channel: number): WebSocket | null {
    return this.ctx.getWebSockets(`ch:${channel}`)[0] ?? null;
  }

  /// The phones admitted, on a channel.
  private phones(): WebSocket[] {
    return this.ctx.getWebSockets("phone").filter((ws) => {
      const tag = ws.deserializeAttachment() as Tag | null;
      return tag?.role === "phone" && tag.channel > 0;
    });
  }

  private tell(phone: WebSocket, msg: object): void {
    try {
      phone.send(phoneControl(msg));
    } catch {
      // closing
    }
  }

  /// Says why, then closes. Sending on a socket that is closing throws; that is fine.
  /// A close we start never reaches webSocketClose, so the computer is told here
  /// when the phone had a channel.
  private bye(ws: WebSocket, reason: string, code: number, msg: object = { type: "bye", reason }): void {
    const tag = ws.deserializeAttachment() as Tag | null;
    try {
      ws.send(tag?.role === "phone" ? phoneControl(msg) : pcControl(msg));
      ws.close(code, reason);
    } catch {
      // already gone
    }
    if (tag?.role === "phone" && tag.channel) this.channelClosed(tag.channel, reason);
  }

  private channelClosed(channel: number, reason: string): void {
    try {
      this.computer()?.send(pcControl({ type: "close", channel, reason: reason || "closed" }));
    } catch {
      // the computer is going too
    }
  }
}

function pair(): [WebSocket, WebSocket] {
  const p = new WebSocketPair();
  return [p[0], p[1]];
}

async function verify(pub: Uint8Array, sig: Uint8Array, signed: Uint8Array): Promise<boolean> {
  try {
    const key = await crypto.subtle.importKey("raw", pub as BufferSource, { name: "ECDSA", namedCurve: "P-256" }, false, ["verify"]);
    return await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, key, sig as BufferSource, signed as BufferSource);
  } catch {
    return false;
  }
}
