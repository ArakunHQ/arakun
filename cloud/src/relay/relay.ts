// One computer's relay: its socket and its devices' sockets. Sockets hibernate
// between messages, so everything a socket is lives in its attachment, and
// what the claim established in storage.
//
// The Worker lets a socket in only with a session of the right account, and
// tells us who it is in headers (see machines.ts). The computer connects at
// /pc and is challenged: it signs a tag, the nonce and its label with the key
// its label derives from. Only then is it the computer, and only one computer is:
// a later claim replaces the earlier socket. The claim registers the label to
// the account in D1, unless another account has it. Nothing is stored before
// the claim but the channel counter, and a socket that does not claim in time
// is closed by the alarm.
// Devices connect at /ws. The Worker's check may be stale by the time a device
// arrives here, so its session and account are read from D1 once more before
// it gets a channel. Each gets a channel number; the computer hears
// `open` (with who the device is) and `close` for it, the device hears
// whether the computer is there. Hello and data frames cross unread: a
// device's go to the computer on its channel, the computer's go to the device
// the channel names. How many devices, how many frames and how big is the
// account's plan, read from KV. Every few minutes, and when the Worker asks,
// each socket's session and account are checked again in D1.

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
  PAGE_ID,
  phoneControl,
  phoneFrame,
  utf8,
} from "./frames";
import { expired } from "../linear_expire";
import type { Delivery, GuidanceRule } from "../machines";
import type { Env } from "../util";
import { labelOf } from "./label";
import { planFor, type Plan } from "./limits";

/// Whose a socket is, as the Worker said when it opened, and when that was
/// last checked against D1.
interface Owner {
  label: string;
  /// The account ID.
  account: number;
  /// The SHA-256 of the session token, as `sessions.token_hash`.
  session: string;
  /// The name of the account's plan when the socket opened.
  plan: string;
  checked: number;
}

/// What a socket is, kept on it across hibernation.
export type Tag =
  | ({ role: "pc"; nonce: string; claimed: boolean; until: number; name: string; kind: string } & Owner)
  | ({
      role: "phone";
      channel: number;
      device: string;
      name: string;
      kind: string;
      page: string | null;
      since?: number;
      /// The computer heard this channel close already: we closed it.
      told?: boolean;
    } & Owner);

/// Who a device is, once admitted.
type Admitted = Omit<Tag & { role: "phone" }, "role" | "channel" | "since" | "told">;

/// Close codes, in the 4000s so they never collide with the protocol's own.
export const CLOSE_BAD_FRAME = 4400;
export const CLOSE_UNAUTHORIZED = 4401;
export const CLOSE_FORBIDDEN = 4403;
export const CLOSE_NOT_FOUND = 4404;
export const CLOSE_REPLACED = 4409;
export const CLOSE_GONE = 4410;
export const CLOSE_LIMIT = 4429;
export const CLOSE_UNAVAILABLE = 4503;

/// How long the computer has to answer the challenge.
const CLAIM_TIMEOUT_MS = 30_000;

/// Starts what the computer signs to claim its label. The same key signs the
/// end-to-end handshake, which starts with a different tag, so a claim can
/// never be read as a handshake.
const CLAIM_TAG = utf8("arakun-relay-claim-v1\0");

/// How often each socket's session and account are checked again. A sign-out,
/// a block, a revoke and a plan change ask at once; this catches what those
/// calls missed and sessions that ran out. Each one is an alarm, which is
/// billed as a whole request, and a D1 read, for every computer online.
const RECHECK_MS = 15 * 60_000;

/// How soon a recheck that failed (D1 did not answer) is tried again.
const RECHECK_RETRY_MS = 30_000;

/// How long the plan read from KV is trusted before asking again. A stale
/// plan still answers while the new one is read, so no frame waits for KV.
const PLAN_CACHE_MS = 60_000;

/// The day's count is written to storage this often; a hibernation in between
/// loses at most this many, which is cheaper than a write per frame.
const COUNT_FLUSH_EVERY = 100;

/// The page pings ("ping", as text) and the runtime answers "pong" itself, so a
/// ping neither wakes the object nor counts as a frame. A phone that pinged once
/// and then went this long without one is gone without having closed (it left
/// the network, its tab froze): it is closed, so the computer hears the channel
/// close and sends its notifications again. Pages ping every 30 seconds.
/// Only the computer needs to hear it, so while none is connected no alarm
/// waits for it: a silent device goes when the computer claims, or when
/// another device wants in.
const PHONE_SILENT_MS = 90_000;

/// A computer not seen for this long has moved on: its object empties itself.
/// Nothing is lost, the computer claims again on connecting.
const PURGE_AFTER_MS = 90 * 24 * 3_600_000;

/// A Linear delivery the computer has not taken in a week is given up: the
/// person who delegated the issue hears so rather than wait on a machine
/// that may never return.
const LINEAR_KEEP_MS = 7 * 24 * 60 * 60 * 1000;

/// How many deliveries wait for one computer at most. A few are normal; this
/// many means it is away, and more would only grow its storage.
const LINEAR_QUEUE_MAX = 50;

/// Where a delivery is stored: the prefix, then its id.
const LINEAR_KEY = "linear:";

/// How many frames crossed today; the day is UTC, as days since the epoch.
interface Today {
  day: number;
  n: number;
}

export class Relay extends DurableObject<Env> {
  /// The name of the account's plan, as the last socket or recheck said.
  /// Null after a wake until storage is read.
  private planName: string | null = null;
  private plan: { value: Plan; at: number } | null = null;
  /// The KV read under way, so a burst of frames asks only once.
  private planRead: Promise<Plan> | null = null;
  private burst = { second: 0, n: 0 };
  private today: Today | null = null;
  /// PHONE_SILENT_MS; tests shorten it rather than wait.
  private silentMs = PHONE_SILENT_MS;
  /// How many rechecks have started. A device being admitted reads D1 while
  /// other events run; a recheck among them means a sign-out or block may be
  /// newer than what it read.
  private rechecks = 0;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  /// Only the Worker reaches this: machines.ts for the sockets, and for
  /// `/_recheck`, `/_state`, `/_linear` and `/_linear_drop`.
  override async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const h = request.headers;
    const label = h.get("x-arakun-label") ?? "";
    if (!label) return new Response("no label", { status: 400 });
    if (url.pathname === "/_recheck" && request.method === "POST") {
      const ok = await this.tryRecheck();
      if (!ok) await this.schedule();
      return new Response(null, { status: ok ? 204 : 503 });
    }
    if (url.pathname === "/_state" && request.method === "GET") return Response.json({ online: this.computer() !== null });
    if (url.pathname === "/_linear" && request.method === "POST") return this.linearDeliver(request);
    if (url.pathname === "/_linear_drop" && request.method === "POST") return this.linearDrop(request);
    if (h.get("Upgrade") !== "websocket") return new Response("expected a websocket", { status: 426 });
    const account = Number(h.get("x-arakun-account"));
    const session = h.get("x-arakun-session") ?? "";
    const plan = h.get("x-arakun-plan") ?? "";
    if (!Number.isSafeInteger(account) || account <= 0 || !session || !plan) return new Response("no account", { status: 400 });
    const owner: Owner = { label, account, session, plan, checked: Date.now() };
    if (url.pathname === "/pc") {
      return this.acceptPc(owner, decoded(h.get("x-arakun-machine-name")), h.get("x-arakun-machine-kind") ?? "app");
    }
    if (url.pathname === "/ws") {
      const page = url.searchParams.get("page");
      return this.acceptPhone({
        ...owner,
        device: h.get("x-arakun-device") ?? "",
        name: decoded(h.get("x-arakun-device-name")),
        kind: h.get("x-arakun-device-kind") ?? "",
        page: page && PAGE_ID.test(page) ? page : null,
      });
    }
    return new Response("not found", { status: 404 });
  }

  // ---- the computer --------------------------------------------------------

  /// The computer's socket: challenged at once, trusted only after it answers.
  private async acceptPc(owner: Owner, name: string, kind: string): Promise<Response> {
    const [client, server] = pair();
    const nonce = b64url(crypto.getRandomValues(new Uint8Array(16)));
    this.ctx.acceptWebSocket(server, ["pc"]);
    server.serializeAttachment({ role: "pc", nonce, claimed: false, until: Date.now() + CLAIM_TIMEOUT_MS, name, kind, ...owner } satisfies Tag);
    server.send(pcControl({ type: "challenge", nonce }));
    await this.schedule();
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
    // `tickets`, `pair_open` and `pair_close` are gone with pairing through
    // the relay; an older computer is told so.
    if (control) return this.bye(ws, "unknown control", CLOSE_BAD_FRAME);
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

  /// `claim{pub, sig}`: the signature of `"arakun-relay-claim-v1\0" ‖ nonce ‖ label`
  /// by the key the label derives from.
  private async claim(ws: WebSocket, tag: Tag & { role: "pc" }, msg: Record<string, unknown>): Promise<void> {
    const pub = typeof msg["pub"] === "string" ? fromB64url(msg["pub"]) : null;
    const sig = typeof msg["sig"] === "string" ? fromB64url(msg["sig"]) : null;
    const label = tag.label;
    if (!pub || !sig || pub.length !== 65 || sig.length !== 64) return this.bye(ws, "bad claim", CLOSE_BAD_FRAME);
    if ((await labelOf(pub)) !== label) return this.bye(ws, "wrong_label", CLOSE_FORBIDDEN);
    const nonce = fromB64url(tag.nonce) ?? new Uint8Array();
    // Computers from before the tag sign `nonce ‖ label`. Accepting that too
    // is safe: the risk was the computer signing untagged, not us checking
    // it. Drop it once no supported app version sends it.
    const tagged = await verify(pub, sig, concat(CLAIM_TAG, nonce, utf8(label)));
    if (!tagged && !(await verify(pub, sig, concat(nonce, utf8(label))))) return this.bye(ws, "bad_signature", CLOSE_UNAUTHORIZED);
    const known = await this.ctx.storage.get<string>("pub");
    const pubB64 = msg["pub"] as string;
    if (known && known !== pubB64) return this.bye(ws, "key_mismatch", CLOSE_FORBIDDEN);
    // D1 says whose the label is, not our storage: a machine the account
    // forgot may have been registered by another account since.
    const refused = await this.register(tag, pubB64);
    if (refused) {
      const code = refused === "machines_full" ? CLOSE_LIMIT : refused === "signed_out" ? CLOSE_UNAUTHORIZED : CLOSE_FORBIDDEN;
      return this.bye(ws, refused, code);
    }
    // D1 lets other events in: a recheck may have closed the socket meanwhile.
    if (ws.readyState !== WebSocket.READY_STATE_OPEN) return;
    const now = Date.now();
    this.setPlan(tag.plan);
    if (!known) await this.ctx.storage.put({ pub: pubB64, claimed_at: now });
    await this.ctx.storage.put({ label, last_seen: now, account: tag.account, plan: tag.plan });
    for (const other of this.ctx.getWebSockets("pc")) {
      if (other !== ws) this.bye(other, "replaced", CLOSE_REPLACED);
    }
    // No alarm watched the waiting devices: the silent ones go before the
    // computer counts as here, so it never hears of channels it never saw.
    this.closeSilent(now);
    ws.serializeAttachment({ ...tag, claimed: true } satisfies Tag);
    await this.schedule();
    // Read before `claimed` goes out, so nothing awaits between the two and
    // the deliveries follow it at once.
    const queued = await this.deliveries();
    ws.send(pcControl({ type: "claimed" }));
    // What waited for the computer, or what it was sent before and has not
    // finished: it tells deliveries apart by id.
    for (const d of queued) ws.send(pcControl(linearMessage(d, null, null)));
    // The computer knows nothing of the devices already waiting: introduce
    // them. One of another account (the label changed hands) goes instead.
    for (const phone of this.phones()) {
      const t = phone.deserializeAttachment() as Tag & { role: "phone" };
      if (t.account !== tag.account) {
        this.bye(phone, "not_yours", CLOSE_FORBIDDEN);
        continue;
      }
      ws.send(pcControl(openMessage(t)));
      this.tell(phone, { type: "pc_online" });
    }
  }

  /// Registers the label to the claiming account in D1, or says why not: the
  /// session was signed out or the account blocked since the socket opened,
  /// the label is another account's, or the account has as many machines as
  /// its plan allows (a label it has already does not count again). One
  /// statement, so two claims at once cannot both get the last place, and a
  /// sign-out or block cannot slip in between the check and the write.
  /// A cloud session counts as signed out at any label but its own. The
  /// Worker refuses it first; this holds should a door ever forget.
  private async register(
    tag: Tag & { role: "pc" },
    pub: string,
  ): Promise<"signed_out" | "blocked" | "label_taken" | "machines_full" | null> {
    const db = this.env.DB;
    const plan = await planFor(this.env.CONFIG, tag.plan);
    const now = seconds(Date.now());
    const result = await db
      .prepare(
        `INSERT INTO machines (label, account_id, pub, name, kind, created_at, last_seen_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?6
         WHERE (EXISTS (SELECT 1 FROM machines WHERE label = ?1 AND account_id = ?2)
                OR (SELECT COUNT(*) FROM machines WHERE account_id = ?2) < ?7)
           AND EXISTS (SELECT 1 FROM sessions s JOIN accounts u ON u.id = s.account_id
                       WHERE s.token_hash = ?8 AND s.account_id = ?2 AND s.expires_at > ?6 AND u.blocked_at IS NULL
                         AND (s.machine_label IS NULL OR s.machine_label = ?1))
         ON CONFLICT (label) DO UPDATE SET name = excluded.name, kind = excluded.kind, last_seen_at = excluded.last_seen_at
         WHERE machines.account_id = excluded.account_id`,
      )
      .bind(tag.label, tag.account, pub, tag.name, tag.kind, now, plan.machines, tag.session)
      .run();
    if (result.meta.changes > 0) return null;
    const [session, machine] = await db.batch([
      db
        .prepare(
          `SELECT u.blocked_at IS NOT NULL AS blocked FROM sessions s JOIN accounts u ON u.id = s.account_id
           WHERE s.token_hash = ?1 AND s.account_id = ?2 AND s.expires_at > ?3 AND (s.machine_label IS NULL OR s.machine_label = ?4)`,
        )
        .bind(tag.session, tag.account, now, tag.label),
      db.prepare("SELECT account_id FROM machines WHERE label = ?").bind(tag.label),
    ]);
    const row = session!.results[0] as { blocked: number } | undefined;
    if (!row) return "signed_out";
    if (row.blocked) return "blocked";
    const owner = (machine!.results[0] as { account_id: number } | undefined)?.account_id ?? null;
    return owner !== null && owner !== tag.account ? "label_taken" : "machines_full";
  }

  // ---- devices -------------------------------------------------------------

  /// A device's socket. The Worker checked its session against the machine's
  /// account in D1, but a sign-out or a block may have landed since, and its
  /// recheck found no socket here yet to close. So the session, the block and
  /// the machine's account are read again here, and the account the claim
  /// stored is checked too. A device that cannot come in is told why and
  /// closed; a refused upgrade would look like a dead network to the page.
  private async acceptPhone(admitted: Admitted): Promise<Response> {
    const [client, server] = pair();
    const owner = await this.ctx.storage.get<number>("account");
    const refused = owner !== undefined && owner !== admitted.account ? "not_yours" : await this.standing(admitted);
    let full: "phones" | "daily" | null = null;
    if (!refused) {
      // The Worker read the plan from D1 just now: the freshest word on it.
      this.setPlan(admitted.plan);
      // No alarm watches devices while the computer is away; look now.
      if (!this.computer()) this.closeSilent(Date.now());
      if ((await this.full()) === "phones") this.makeRoom(admitted);
      full = await this.full();
    }
    if (refused || full) {
      this.ctx.acceptWebSocket(server, ["phone"]);
      server.serializeAttachment({ role: "phone", channel: 0, ...admitted } satisfies Tag);
      if (full) this.limited(server, full);
      else this.bye(server, refused!, closeCode(refused!));
      return new Response(null, { status: 101, webSocket: client });
    }
    const channel = ((await this.ctx.storage.get<number>("next_channel")) ?? 0) + 1;
    await this.ctx.storage.put("next_channel", channel);
    this.ctx.acceptWebSocket(server, ["phone", `ch:${channel}`]);
    const tag = { role: "phone", channel, ...admitted, since: Date.now() } satisfies Tag;
    server.serializeAttachment(tag);
    server.send(phoneControl({ type: "open", channel }));
    if (!this.introduce(tag)) {
      server.send(phoneControl({ type: "pc_offline", since: (await this.ctx.storage.get<number>("last_seen")) ?? null }));
    }
    await this.schedule();
    return new Response(null, { status: 101, webSocket: client });
  }

  /// Why a device may not come in, by what D1 says now, or null while it may:
  /// one read of its session, its account's block and the machine's account.
  /// D1 lets other events in while it answers. A recheck among them was asked
  /// for by a sign-out or a block that this read may have missed, and it did
  /// not see this socket, so the read is made again. A cloud session is never
  /// a device's, whatever the Worker let through.
  private async standing(admitted: Admitted): Promise<string | null> {
    for (;;) {
      const before = this.rechecks;
      const row = await this.env.DB.prepare(
        `SELECT s.token_hash, s.account_id, u.blocked_at IS NOT NULL AS blocked, u.plan,
                (SELECT account_id FROM machines WHERE label = ?2) AS owner
         FROM sessions s JOIN accounts u ON u.id = s.account_id
         WHERE s.token_hash = ?1 AND s.expires_at > ?3 AND s.machine_label IS NULL`,
      )
        .bind(admitted.session, admitted.label, seconds(Date.now()))
        .first<SessionRow & { owner: number | null }>();
      if (this.rechecks !== before) continue;
      return refusal({ role: "phone", channel: 1, ...admitted }, row ?? undefined, row?.owner ?? null);
    }
  }

  /// When the device places are taken, one of them is given up if it is the
  /// newcomer's own old socket, or else a socket that never pinged and has
  /// been open longer than a pinging one may stay silent. A socket like that
  /// is left behind or a client from before pings, which comes back on its
  /// own; while there is room, it stays.
  private makeRoom(admitted: Admitted): void {
    if (this.replaceStale(admitted)) return;
    const now = Date.now();
    const dead = this.phones()
      .filter((ws) => this.silentAt(ws) === null)
      .map((ws) => ({ ws, tag: ws.deserializeAttachment() as Tag & { role: "phone" } }))
      // Sockets from before `since` was kept are as old as can be.
      .filter(({ tag }) => (tag.since ?? 0) + this.silentMs <= now)
      .sort((a, b) => a.tag.channel - b.tag.channel);
    if (dead[0]) this.bye(dead[0].ws, "silent", CLOSE_GONE);
  }

  /// A device that moves between networks reconnects before the relay sees
  /// its old socket die. When that would leave it out, the oldest socket of
  /// the same page (the same session and the page's own id) makes room.
  /// Another tab is another page, so live tabs never replace each other.
  private replaceStale(admitted: Admitted): boolean {
    if (!admitted.page) return false;
    const same = this.phones()
      .map((ws) => ({ ws, tag: ws.deserializeAttachment() as Tag & { role: "phone" } }))
      .filter(({ tag }) => tag.device === admitted.device && tag.session === admitted.session && tag.page === admitted.page)
      .sort((a, b) => a.tag.channel - b.tag.channel);
    if (!same[0]) return false;
    this.bye(same[0].ws, "replaced", CLOSE_REPLACED);
    return true;
  }

  /// Why one more device cannot come in right now, if it cannot.
  private async full(): Promise<"phones" | "daily" | null> {
    const plan = await this.planNow();
    if (this.phones().length >= plan.phones) return "phones";
    if ((await this.todayCount(Date.now())).n >= plan.daily) return "daily";
    return null;
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

  /// The account's plan, asked of KV once a minute. Only the first frame
  /// after a wake waits for the answer; later reads happen behind the
  /// frames, which go by the plan read before.
  private async planNow(): Promise<Plan> {
    if (this.plan && Date.now() - this.plan.at <= PLAN_CACHE_MS) return this.plan.value;
    if (this.planName === null) {
      const stored = await this.ctx.storage.get<string>("plan");
      // A socket may have named the plan while storage was read.
      this.planName ??= stored ?? "free";
    }
    if (!this.planRead) {
      // A read for a name that changed meanwhile must not stand for the new one.
      const name = this.planName;
      const read: Promise<Plan> = planFor(this.env.CONFIG, name).then((value) => {
        if (this.planName === name) this.plan = { value, at: Date.now() };
        if (this.planRead === read) this.planRead = null;
        return value;
      });
      this.planRead = read;
    }
    if (this.plan) {
      this.ctx.waitUntil(this.planRead);
      return this.plan.value;
    }
    return this.planRead;
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
    // The count may lose a few frames, so the frame need not wait for the
    // write to be confirmed before it goes out.
    if (today.n % COUNT_FLUSH_EVERY === 0) await this.ctx.storage.put("today", today, { allowUnconfirmed: true });
    return null;
  }

  /// The plan the Worker or D1 names now. A new name is read from KV at the
  /// next frame rather than after the minute.
  private setPlan(name: string): void {
    if (name === this.planName) return;
    this.planName = name;
    this.plan = null;
    this.planRead = null;
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

  /// Closes computer sockets that did not claim in time, and empties the
  /// object when it is due. Alarms can run early or twice, so both are checked
  /// against the clock.
  override async alarm(): Promise<void> {
    const now = Date.now();
    for (const ws of this.unclaimed()) {
      if ((ws.deserializeAttachment() as Tag & { role: "pc" }).until <= now) this.bye(ws, "claim_timeout", CLOSE_UNAUTHORIZED);
    }
    this.closeSilent(now);
    const recheckAt = this.recheckAt();
    if (recheckAt !== null && recheckAt <= now) await this.tryRecheck();
    await this.expireDeliveries(now);
    if (!this.computer()) {
      const due = await this.purgeAt();
      if (due !== null && due <= now) await this.purge();
    }
    await this.schedule();
  }

  /// Ninety days without the computer: forget everything about it. Devices
  /// still waiting are sent away; the computer rebuilds the state on its next
  /// claim.
  private async purge(): Promise<void> {
    if (this.computer()) return;
    for (const phone of this.phones()) this.bye(phone, "gone", CLOSE_GONE);
    this.today = null;
    this.planName = null;
    this.plan = null;
    await this.ctx.storage.deleteAll();
  }

  // ---- the recheck ---------------------------------------------------------

  /// The recheck, kept from taking the alarm down with it: an alarm that
  /// throws is retried a few times and then dropped, and with it the claim
  /// timeouts, the silent devices and every later recheck. A failure is
  /// tried again soon instead: every socket's last check is set so that the
  /// next one is due then. That lives on the sockets, so it holds when
  /// the object is evicted before the retry.
  private async tryRecheck(): Promise<boolean> {
    try {
      await this.recheck();
      return true;
    } catch (e) {
      console.error("relay recheck failed:", e);
      const checked = Date.now() + RECHECK_RETRY_MS - RECHECK_MS;
      for (const ws of this.ctx.getWebSockets()) {
        const tag = ws.deserializeAttachment() as Tag | null;
        if (!tag || ws.readyState !== WebSocket.READY_STATE_OPEN) continue;
        ws.serializeAttachment({ ...tag, checked } satisfies Tag);
      }
      return false;
    }
  }

  /// Checks every open socket's session and account in D1, in one round
  /// trip for all of them, and closes the ones that no longer hold: a session
  /// signed out or expired (`signed_out`), a blocked account (`blocked`), a
  /// machine forgotten (`no_machine`) or now another account's. The plan
  /// name is taken from D1 too, so a change reaches open sockets.
  private async recheck(): Promise<void> {
    this.rechecks++;
    const sockets = this.ctx
      .getWebSockets()
      .filter((ws) => ws.readyState === WebSocket.READY_STATE_OPEN)
      .map((ws) => ({ ws, tag: ws.deserializeAttachment() as Tag | null }))
      .filter((s): s is { ws: WebSocket; tag: Tag } => s.tag !== null && (s.tag.role === "pc" || s.tag.channel > 0));
    if (sockets.length === 0) return;
    const db = this.env.DB;
    const [sessions, machine] = await db.batch([
      db
        .prepare(
          `SELECT s.token_hash, s.account_id, u.blocked_at IS NOT NULL AS blocked, u.plan
           FROM sessions s JOIN accounts u ON u.id = s.account_id
           WHERE s.token_hash IN (SELECT value FROM json_each(?1)) AND s.expires_at > ?2`,
        )
        .bind(JSON.stringify([...new Set(sockets.map((s) => s.tag.session))]), seconds(Date.now())),
      db.prepare("SELECT account_id FROM machines WHERE label = ?").bind(sockets[0]!.tag.label),
    ]);
    const rows = new Map((sessions!.results as SessionRow[]).map((r) => [r.token_hash, r]));
    const owner = (machine!.results[0] as { account_id: number } | undefined)?.account_id ?? null;
    const now = Date.now();
    // The computer first, so the one thing it hears is why, not its devices leaving.
    sockets.sort((a, b) => (a.tag.role === "pc" ? 0 : 1) - (b.tag.role === "pc" ? 0 : 1));
    let computerClosed = false;
    let plan: string | null = null;
    for (const { ws, tag: before } of sockets) {
      // Read again: a claim may have landed while D1 answered. Its insert may
      // be newer than the owner read, so only its session is judged now.
      const tag = ws.deserializeAttachment() as Tag;
      const row = rows.get(tag.session);
      const claimedMeanwhile = before.role === "pc" && !before.claimed && tag.role === "pc" && tag.claimed;
      const why = refusal(tag, row, owner, !claimedMeanwhile);
      if (why) {
        if (ws === this.computer()) computerClosed = true;
        this.bye(ws, why, closeCode(why));
      } else {
        ws.serializeAttachment({ ...tag, checked: now } satisfies Tag);
        plan = row!.plan;
      }
    }
    if (plan !== null && plan !== this.planName) {
      this.setPlan(plan);
      // Kept for after a wake, but only by a claimed object: one without a
      // claim stores nothing.
      if (this.computer()) await this.ctx.storage.put("plan", plan);
    }
    // A close we start never reaches webSocketClose: the devices are told here.
    if (computerClosed && !this.computer()) await this.computerLeft(now);
  }

  /// When the sockets are next due for their recheck: the oldest check, plus
  /// the period. Null while there are none.
  private recheckAt(): number | null {
    let oldest: number | null = null;
    for (const ws of this.ctx.getWebSockets()) {
      const tag = ws.deserializeAttachment() as Tag | null;
      if (!tag || ws.readyState !== WebSocket.READY_STATE_OPEN || (tag.role === "phone" && !tag.channel)) continue;
      if (oldest === null || tag.checked < oldest) oldest = tag.checked;
    }
    return oldest === null ? null : oldest + RECHECK_MS;
  }

  /// When the object empties itself, while no computer is connected: ninety
  /// days after it was last seen. State without a claim behind it (a label an
  /// older relay stored for anyone who opened /pc) goes at once. The channel
  /// counter alone is not such state: a device that waits for a computer the
  /// purge forgot stores it, and must not be sent away for it.
  private async purgeAt(): Promise<number | null> {
    const seen = await this.ctx.storage.get<number>("last_seen");
    if (seen !== undefined) return seen + PURGE_AFTER_MS;
    // Deliveries are not such state either: they may wait for a computer
    // that has not claimed yet, and go when they expire.
    const keys = await this.ctx.storage.list({ limit: LINEAR_QUEUE_MAX + 2 });
    return [...keys.keys()].some((k) => k !== "next_channel" && !k.startsWith(LINEAR_KEY)) ? 0 : null;
  }

  /// Closes the devices that pinged and then went quiet for too long.
  private closeSilent(now: number): void {
    for (const phone of this.phones()) {
      const silentAt = this.silentAt(phone);
      if (silentAt !== null && silentAt <= now) this.bye(phone, "silent", CLOSE_GONE);
    }
  }

  /// When a phone counts as gone if it pings no more; null until its first ping.
  private silentAt(phone: WebSocket): number | null {
    const last = this.ctx.getWebSocketAutoResponseTimestamp(phone);
    return last ? last.getTime() + this.silentMs : null;
  }

  /// Sets the alarm for the next thing it has to do, so every object that
  /// keeps a socket or state wakes again: a claim running out, a device gone
  /// quiet while the computer is there to hear it, a recheck, a delivery
  /// expiring, or the purge.
  private async schedule(): Promise<void> {
    const times = this.unclaimed().map((ws) => (ws.deserializeAttachment() as Tag & { role: "pc" }).until);
    const recheckAt = this.recheckAt();
    if (recheckAt !== null) times.push(recheckAt);
    const oldest = (await this.deliveries())[0];
    if (oldest) times.push(expiresAt(oldest));
    if (this.computer()) {
      // A phone that has not pinged yet (a page from before pings) is looked at again later.
      for (const phone of this.phones()) times.push(this.silentAt(phone) ?? Date.now() + this.silentMs);
    } else {
      const due = await this.purgeAt();
      if (due !== null) times.push(due);
    }
    if (times.length === 0) return this.ctx.storage.deleteAlarm();
    const at = Math.max(Math.min(...times), Date.now());
    if ((await this.ctx.storage.getAlarm()) !== at) await this.ctx.storage.setAlarm(at);
  }

  // ---- Linear deliveries ---------------------------------------------------

  /// `POST /_linear {delivery, body, guidance}`: keeps the delivery and sends
  /// it to the computer if it is here. Linear may send the same event twice,
  /// so a delivery already kept is neither kept again nor sent again. `body`
  /// and `guidance` go in that one frame and are never stored.
  private async linearDeliver(request: Request): Promise<Response> {
    const msg = await request.json().catch(() => null);
    const d = isObj(msg) ? parseDelivery(msg["delivery"]) : null;
    const body = isObj(msg) ? msg["body"] : undefined;
    const guidance = isObj(msg) ? parseGuidance(msg["guidance"]) : undefined;
    if (!d || (body !== null && typeof body !== "string") || guidance === undefined) return new Response("bad delivery", { status: 400 });
    const key = LINEAR_KEY + d.id;
    if ((await this.ctx.storage.get(key)) === undefined) {
      const queued = await this.ctx.storage.list({ prefix: LINEAR_KEY, limit: LINEAR_QUEUE_MAX });
      if (queued.size >= LINEAR_QUEUE_MAX) return new Response("queue full", { status: 429 });
      await this.ctx.storage.put(key, d);
      await this.schedule();
      const pc = this.computer();
      if (pc) {
        try {
          pc.send(pcControl(linearMessage(d, body, guidance)));
        } catch {
          // closing: it is sent again on the next claim
        }
      }
    }
    return Response.json({ online: this.computer() !== null });
  }

  /// `POST /_linear_drop {id}`: the work is done or stopped; forget it.
  private async linearDrop(request: Request): Promise<Response> {
    const msg = await request.json().catch(() => null);
    const id = isObj(msg) ? msg["id"] : undefined;
    if (typeof id !== "string" || !DELIVERY_ID.test(id)) return new Response("bad id", { status: 400 });
    await this.ctx.storage.delete(LINEAR_KEY + id);
    await this.schedule();
    return new Response(null, { status: 204 });
  }

  /// The deliveries kept, oldest first.
  private async deliveries(): Promise<Delivery[]> {
    const kept = await this.ctx.storage.list<Delivery>({ prefix: LINEAR_KEY });
    return [...kept.values()].sort((a, b) => a.created - b.created || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  }

  /// Forgets the deliveries a week old. Telling linear.ts must not take the
  /// alarm down: a failure is logged, and the delivery is gone either way.
  private async expireDeliveries(now: number): Promise<void> {
    for (const d of await this.deliveries()) {
      if (expiresAt(d) > now) break;
      // The computer may have accepted it while an earlier one was told:
      // then it was not given up on.
      if ((await this.ctx.storage.get(LINEAR_KEY + d.id)) === undefined) continue;
      await this.ctx.storage.delete(LINEAR_KEY + d.id);
      try {
        await expired(this.env, d.id);
      } catch (e) {
        console.error(`expiring delivery ${d.id} failed:`, e);
      }
    }
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

  override async webSocketClose(ws: WebSocket, code: number, reason: string, _wasClean: boolean): Promise<void> {
    // Without this the other side sees an abnormal close (1006). The runtime
    // answers a close itself from compatibility date 2026-04-07, but not for
    // these sockets: test/relay/forward.test.ts fails without this line.
    replyClose(ws, code, reason);
    const tag = ws.deserializeAttachment() as Tag | null;
    if (!tag) return;
    if (tag.role === "pc") return this.pcGone(tag, reason);
    if (tag.channel && !tag.told) this.channelClosed(tag.channel, reason);
  }

  override async webSocketError(ws: WebSocket, _error: unknown): Promise<void> {
    try {
      ws.close(1011, "error");
    } catch {
      // already gone
    }
    const tag = ws.deserializeAttachment() as Tag | null;
    if (tag?.role === "pc") return this.pcGone(tag, "error");
    // A device's socket that fails may never see a close event: the computer
    // is told here, once, as it would be on a close.
    if (tag?.role === "phone" && tag.channel && !tag.told) {
      try {
        ws.serializeAttachment({ ...tag, told: true } satisfies Tag);
      } catch {
        // already gone: no close event will follow
      }
      this.channelClosed(tag.channel, "error");
    }
  }

  /// A computer socket closed. When it was the computer, the phones hear it
  /// is offline; either way the alarm moves to what is next.
  private async pcGone(tag: Tag & { role: "pc" }, reason: string): Promise<void> {
    // Replaced means another socket of the computer is already there; a late
    // error on a replaced socket says so by the replacement still being open.
    if (tag.claimed && reason !== "replaced" && !this.computer()) await this.computerLeft(Date.now());
    await this.schedule();
  }

  /// The computer is gone: the devices hear it, and D1 keeps when it was
  /// last seen for the account's list of machines.
  private async computerLeft(now: number): Promise<void> {
    await this.ctx.storage.put("last_seen", now);
    for (const phone of this.phones()) this.tell(phone, { type: "pc_offline", since: now });
    const [label, account] = await Promise.all([this.ctx.storage.get<string>("label"), this.ctx.storage.get<number>("account")]);
    if (!label || account === undefined) return;
    try {
      await this.env.DB.prepare("UPDATE machines SET last_seen_at = ? WHERE label = ? AND account_id = ?").bind(seconds(now), label, account).run();
    } catch (e) {
      // The list shows an older time until the next claim; nothing else depends on it.
      console.error(`last_seen_at of ${label}:`, e);
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

  /// Computer sockets still open that have not claimed yet.
  private unclaimed(): WebSocket[] {
    return this.ctx.getWebSockets("pc").filter((ws) => {
      const tag = ws.deserializeAttachment() as Tag | null;
      return tag?.role === "pc" && !tag.claimed && ws.readyState === WebSocket.READY_STATE_OPEN;
    });
  }

  /// Tells the computer a device's channel is open; false when no computer is
  /// there to hear it, a socket going away included.
  private introduce(tag: Tag & { role: "phone" }): boolean {
    const pc = this.computer();
    if (!pc) return false;
    try {
      pc.send(pcControl(openMessage(tag)));
      return true;
    } catch {
      return false;
    }
  }

  /// The phone on a channel, while it is connected.
  private phone(channel: number): WebSocket | null {
    return this.ctx.getWebSockets(`ch:${channel}`)[0] ?? null;
  }

  /// The phones admitted, on a channel. One we are closing is listed until
  /// its close completes, and no longer counts.
  private phones(): WebSocket[] {
    return this.ctx.getWebSockets("phone").filter((ws) => {
      const tag = ws.deserializeAttachment() as Tag | null;
      return tag?.role === "phone" && tag.channel > 0 && ws.readyState === WebSocket.READY_STATE_OPEN;
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
  /// The computer is told here when the device had a channel, and the socket
  /// is marked so the device's answering close does not tell it again.
  private bye(ws: WebSocket, reason: string, code: number, msg: object = { type: "bye", reason }): void {
    const tag = ws.deserializeAttachment() as Tag | null;
    if (tag?.role === "phone" && tag.channel && !tag.told) {
      try {
        ws.serializeAttachment({ ...tag, told: true } satisfies Tag);
      } catch {
        // already gone: no close event will follow
      }
    }
    try {
      ws.send(tag?.role === "phone" ? phoneControl(msg) : pcControl(msg));
      ws.close(code, reason);
    } catch {
      // already gone
    }
    if (tag?.role === "phone" && tag.channel && !tag.told) this.channelClosed(tag.channel, reason);
  }

  private channelClosed(channel: number, reason: string): void {
    try {
      this.computer()?.send(pcControl({ type: "close", channel, reason: reason || "closed" }));
    } catch {
      // the computer is going too
    }
  }
}

/// Answers a close the other side started, with its code when close() takes it
/// (1000 or 3000-4999, as in browsers); any other code goes back as 1000.
export function replyClose(ws: Pick<WebSocket, "close">, code: number, reason: string): void {
  const sendable = code === 1000 || (code >= 3000 && code <= 4999);
  try {
    ws.close(sendable ? code : 1000, reason);
  } catch {
    // already closed
  }
}

/// A socket's session as the recheck reads it; only valid, unexpired ones.
export interface SessionRow {
  token_hash: string;
  account_id: number;
  blocked: number;
  plan: string;
}

/// The close code that goes with a refusal's reason.
function closeCode(why: string): number {
  return why === "signed_out" ? CLOSE_UNAUTHORIZED : why === "no_machine" ? CLOSE_NOT_FOUND : CLOSE_FORBIDDEN;
}

/// Why a socket may stay no longer, or null while it may. `owner` is the
/// account D1 has the label under, null when it has none; `checkOwner` is
/// false when that read may predate the socket's own registration.
export function refusal(tag: Tag, row: SessionRow | undefined, owner: number | null, checkOwner = true): string | null {
  if (!row || row.account_id !== tag.account) return "signed_out";
  if (row.blocked) return "blocked";
  // A computer that has not claimed yet has registered nothing.
  if (!checkOwner || (tag.role === "pc" && !tag.claimed)) return null;
  if (owner === null) return "no_machine";
  if (owner !== tag.account) return tag.role === "pc" ? "label_taken" : "not_yours";
  return null;
}

/// A delivery's id: 32 hex characters.
const DELIVERY_ID = /^[0-9a-f]{32}$/;

/// The ids Linear uses are UUIDs. linear.ts has the same; importing it from
/// here would make a cycle.
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

const DELIVERY_KINDS: readonly string[] = ["created", "prompt", "stop"] satisfies Delivery["kind"][];

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/// A delivery as the Worker sent it, rebuilt field by field so nothing else
/// is stored with it; null when any field is off.
function parseDelivery(v: unknown): Delivery | null {
  if (!isObj(v)) return null;
  const { id, kind, agent_session, issue, organization, activity, created } = v;
  if (typeof id !== "string" || !DELIVERY_ID.test(id)) return null;
  if (typeof kind !== "string" || !DELIVERY_KINDS.includes(kind)) return null;
  for (const u of [agent_session, issue, organization]) if (typeof u !== "string" || !UUID.test(u)) return null;
  if (activity !== null && (typeof activity !== "string" || !UUID.test(activity))) return null;
  if (typeof created !== "number" || !Number.isSafeInteger(created) || created <= 0) return null;
  return {
    id,
    kind: kind as Delivery["kind"],
    agent_session: agent_session as string,
    issue: issue as string,
    organization: organization as string,
    activity,
    created,
  };
}

/// The guidance the Worker passed: null when absent, undefined when it is not
/// what the Worker sends. Only the two known fields are copied on.
function parseGuidance(v: unknown): GuidanceRule[] | null | undefined {
  if (v === undefined || v === null) return null;
  if (!Array.isArray(v)) return undefined;
  const rules: GuidanceRule[] = [];
  for (const r of v) {
    if (!isObj(r) || typeof r["body"] !== "string") return undefined;
    const team = r["team"];
    if (team !== null && typeof team !== "string") return undefined;
    rules.push({ team, body: r["body"] });
  }
  return rules;
}

/// When a delivery is given up.
function expiresAt(d: Delivery): number {
  return d.created * 1000 + LINEAR_KEEP_MS;
}

/// The control frame for a delivery. `body` and `guidance` are only ever the
/// live one's.
function linearMessage(d: Delivery, body: string | null, guidance: GuidanceRule[] | null): object {
  return {
    type: "linear",
    id: d.id,
    kind: d.kind,
    agent_session: d.agent_session,
    issue: d.issue,
    organization: d.organization,
    activity: d.activity,
    body,
    guidance,
  };
}

/// What the computer hears when a device's channel opens: who the device is.
function openMessage(tag: Tag & { role: "phone" }): object {
  return { type: "open", channel: tag.channel, device: tag.device, name: tag.name, kind: tag.kind, account: tag.account };
}

/// A name the Worker URI-encoded for its header.
function decoded(header: string | null): string {
  try {
    return decodeURIComponent(header ?? "");
  } catch {
    return "";
  }
}

/// Unix seconds, the unit of every time in D1.
function seconds(ms: number): number {
  return Math.floor(ms / 1000);
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
