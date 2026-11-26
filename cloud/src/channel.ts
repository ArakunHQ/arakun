// One channel's messages, `seq` and live sockets: a SQLite-backed Durable
// Object per channel, named `<team_id>/<channel_id>`.
//
// Only the Worker reaches it. The Worker checks the session, the flag, the
// membership and the device, then opens the socket here with the trusted
// headers below, which it builds itself. Sockets hibernate between frames, so
// who is on a socket lives in its attachment.
//
// On every `send` this reads the team's epochs, and whether each socket's
// session is still live, its user still a member and still flagged, from D1
// rather than trusting what the Worker saw at upgrade: a socket stays open for
// hours, and a removal, a logout, a lost flag or a rotation must apply to the
// next message, for the sender and for every recipient. That is one D1 read
// per message, and another when sockets were admitted during it.

import { DurableObject } from "cloudflare:workers";
import { TEAMS_FLAG, error, isObject, now, type Env } from "./util";
import { messageSigned, parseMessage, sigKey, verify, type WireMessage } from "./wire";

/// Headers only the Worker sets, on a request it builds from scratch.
export const TRUSTED = {
  accountId: "X-Teams-Account-Id",
  deviceId: "X-Teams-Device-Id",
  sigPub: "X-Teams-Sig-Pub",
  teamId: "X-Teams-Team-Id",
  channelId: "X-Teams-Channel-Id",
  /// The SHA-256 the sessions table keys on, never the session token itself.
  session: "X-Teams-Session-Hash",
  /// "0" when the socket opens without counting toward presence.
  presence: "X-Teams-Presence",
} as const;

/// Who is on a socket, kept on it across hibernation.
interface Who {
  accountId: number;
  deviceId: string;
  sigPub: string;
  teamId: string;
  channelId: string;
  session: string;
  /// A socket that does not count toward presence: it is not listed, gets no
  /// presence frames, and its opening and closing tell nobody. A client reads
  /// channels it does not show this way. Absent on sockets from before the
  /// field, which all count.
  quiet?: boolean;
}

export interface Stored {
  seq: number;
  at: number;
  message: WireMessage;
}

/// The JSON of a ct of 64 KiB is about 87 KiB; the rest of a frame is small.
const MAX_FRAME = 96 * 1024;
/// The most messages a channel keeps: the object has a storage quota a
/// channel would otherwise fill, after which every send fails. Old ones are
/// dropped in runs: once there are more than MAX_MESSAGES, the newest TRIM_TO
/// stay, so about one send in a thousand deletes. seq is contiguous, so a
/// client that asked for history from before the oldest one kept sees the gap
/// and knows to resync.
const MAX_MESSAGES = 10_000;
const TRIM_TO = 9_000;
/// In the 4000s so they never collide with the protocol's own codes.
const CLOSE_BAD_FRAME = 4400;
const CLOSE_FORBIDDEN = 4403;
/// A socket that may have missed a message: the client opens a new one and
/// reads the history from the last `seq` it has.
const CLOSE_RECONNECT = 4503;
/// How many more D1 reads a send makes for sockets admitted while it read.
/// More than one only when sockets keep arriving during each read.
const RECHECKS = 3;
/// When a check a send needs could not run. Nothing was stored.
const CHECK_FAILED = "the server could not check the message: send it again";

/// Whether a message or channel name sealed with this epoch may be stored:
/// only the current epoch, and only once it has envelopes. While a rotation
/// is pending nothing is: the newest key with envelopes is one a removed
/// member holds, and the current epoch's may never be uploaded (a second
/// removal skips it, and what was sealed with it could not be read). A
/// device that has not read the team since the removal hears so on its next
/// send, not up to a refresh later.
export function epochAllowed(epoch: number, current: number, keyed: number): boolean {
  return keyed >= 1 && keyed === current && epoch === current;
}

/// A refusal of an epoch: `code` is for clients, `error` for people.
export interface EpochRefusal {
  code: "stale_epoch" | "rotation_pending" | "unknown_epoch";
  error: string;
}

/// Why an epoch was refused, for the sender. Each `error` names the epoch,
/// so a client from before `code` that reads the team again on "epoch"
/// still does so.
export function epochRefused(epoch: number, current: number, keyed: number): EpochRefusal {
  if (epoch < keyed) return { code: "stale_epoch", error: "stale epoch: fetch the new team key" };
  if (keyed < current && epoch <= current) {
    return { code: "rotation_pending", error: `a key rotation is pending: epoch ${epoch} is retired, so wait for the new team key and send again` };
  }
  return { code: "unknown_epoch", error: "unknown epoch" };
}

/// Whether `allowed` says this session may still hear a message, checked
/// now, not when the read ran.
function live(allowed: Map<string, number>, session: string): boolean {
  return (allowed.get(session) ?? 0) > now();
}

/// The session a socket was opened with; "" for none, which no check allows.
function sessionOf(ws: WebSocket): string {
  return (ws.deserializeAttachment() as Who | null)?.session ?? "";
}

/// Whether a socket counts toward presence.
function counted(ws: WebSocket): boolean {
  const who = ws.deserializeAttachment() as Who | null;
  return !!who && !who.quiet;
}

export class Channel extends DurableObject<Env> {
  private sql: SqlStorage;
  /// Imported verify keys by sig_pub; lost on hibernation, which is fine.
  private keys = new Map<string, CryptoKey>();
  /// Kicks per user, so a socket admitted while its user was being removed is
  /// refused. Only read during an admission, which keeps the object awake.
  private kicks = new Map<number, number>();
  /// Sessions whose socket is being admitted, by count. A send checks them
  /// with the open sockets: one admitted while that check runs was itself
  /// checked earlier, and a logout in between must not let it hear the send.
  private admitting = new Map<string, number>();
  /// How many presence lists were started, so one that a newer one overtook
  /// sends nothing: the newer one has the current sockets.
  private presenceRuns = 0;


  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(`CREATE TABLE IF NOT EXISTS messages (
      seq INTEGER PRIMARY KEY AUTOINCREMENT,
      id TEXT NOT NULL UNIQUE,
      at INTEGER NOT NULL,
      device_id TEXT NOT NULL,
      message TEXT NOT NULL
    )`);
    // For sent(): a channel keeps up to 10,000 messages.
    this.sql.exec("CREATE INDEX IF NOT EXISTS messages_by_device ON messages (device_id)");
    // Retention drops ciphertext, not the channel's record of accepted IDs.
    // A trigger makes the ID and the delete part of the same transaction. The
    // epoch lets a trim forget IDs that no send can reuse any more.
    this.sql.exec("CREATE TABLE IF NOT EXISTS retired_ids (id TEXT PRIMARY KEY, epoch INTEGER) WITHOUT ROWID");
    if (this.sql.exec("SELECT 1 FROM pragma_table_info('retired_ids') WHERE name = 'epoch'").toArray().length === 0) {
      // A channel made before the column: its IDs keep a null epoch, which no
      // trim forgets, and the trigger is made again to fill the column.
      this.sql.exec("ALTER TABLE retired_ids ADD COLUMN epoch INTEGER");
      this.sql.exec("DROP TRIGGER IF EXISTS retire_message");
    }
    this.sql.exec(`CREATE TRIGGER IF NOT EXISTS retire_message BEFORE DELETE ON messages
      BEGIN INSERT OR IGNORE INTO retired_ids (id, epoch) VALUES (OLD.id, json_extract(OLD.message, '$.epoch')); END`);
  }

  /// Opens a socket. Only the Worker calls this, after its checks.
  override async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return error(426, "expected a WebSocket upgrade");
    const h = request.headers;
    const who: Who = {
      accountId: Number(h.get(TRUSTED.accountId)),
      deviceId: h.get(TRUSTED.deviceId) ?? "",
      sigPub: h.get(TRUSTED.sigPub) ?? "",
      teamId: h.get(TRUSTED.teamId) ?? "",
      channelId: h.get(TRUSTED.channelId) ?? "",
      session: h.get(TRUSTED.session) ?? "",
      quiet: h.get(TRUSTED.presence) === "0",
    };
    if (!Number.isSafeInteger(who.accountId) || who.accountId <= 0 || !who.deviceId || !who.sigPub || !who.teamId || !who.channelId || !who.session) {
      return error(500, "internal error");
    }
    // The Worker checked membership before it called here, but a removal can
    // commit and kick in between, before this socket exists. So check again,
    // and refuse if a kick for this user ran while we checked.
    const kicks = this.kicks.get(who.accountId) ?? 0;
    this.admitting.set(who.session, (this.admitting.get(who.session) ?? 0) + 1);
    let allowed: Map<string, number>;
    try {
      allowed = await this.allowed(who.teamId, [who.session]);
    } finally {
      const n = (this.admitting.get(who.session) ?? 1) - 1;
      if (n > 0) this.admitting.set(who.session, n);
      else this.admitting.delete(who.session);
    }
    if (!live(allowed, who.session) || (this.kicks.get(who.accountId) ?? 0) !== kicks) return error(404, "not found");
    const pair = new WebSocketPair();
    const [client, server] = [pair[0], pair[1]];
    // The tag lets a removal find one user's sockets.
    this.ctx.acceptWebSocket(server, [`u:${who.accountId}`]);
    server.serializeAttachment(who);
    // Everyone, the new socket included, hears who is on the channel now. A
    // quiet socket changes nothing anyone hears.
    if (!who.quiet) this.presenceSoon();
    return new Response(null, { status: 101, webSocket: client });
  }

  /// Messages after `after`, oldest first.
  history(after: number, limit: number, before?: number): Stored[] {
    const query = before === undefined
      ? "SELECT seq, at, message FROM messages WHERE seq > ? ORDER BY seq LIMIT ?"
      : "SELECT seq, at, message FROM messages WHERE seq < ? ORDER BY seq DESC LIMIT ?";
    const rows = this.sql.exec<{ seq: number; at: number; message: string }>(query, before ?? after, limit).toArray();
    if (before !== undefined) rows.reverse();
    return rows.map((row) => ({ seq: row.seq, at: row.at, message: JSON.parse(row.message) as WireMessage }));
  }

  /// Whether the channel still keeps a message from this device.
  sent(deviceId: string): boolean {
    return this.sql.exec("SELECT 1 FROM messages WHERE device_id = ? LIMIT 1", deviceId).toArray().length > 0;
  }

  /// Closes a removed member's sockets; answers how many there were.
  kick(accountId: number): number {
    this.kicks.set(accountId, (this.kicks.get(accountId) ?? 0) + 1);
    const sockets = this.ctx.getWebSockets(`u:${accountId}`);
    for (const ws of sockets) this.close(ws, CLOSE_FORBIDDEN, "removed from the team");
    if (sockets.some(counted)) this.presenceSoon();
    return sockets.length;
  }

  override async webSocketMessage(ws: WebSocket, data: ArrayBuffer | string): Promise<void> {
    const who = ws.deserializeAttachment() as Who | null;
    if (!who) return this.close(ws, CLOSE_BAD_FRAME, "unknown socket");
    // length counts UTF-16 units; the limit is for the bytes on the wire.
    // One unit takes up to three, so only near the cap are the bytes counted.
    if (
      typeof data !== "string" ||
      data.length > MAX_FRAME ||
      (data.length > MAX_FRAME / 3 && new TextEncoder().encode(data).length > MAX_FRAME)
    )
      return this.refuse(ws, null, "frames are JSON text of at most 96 KiB");
    let frame: unknown;
    try {
      frame = JSON.parse(data);
    } catch {
      return this.refuse(ws, null, "frames are JSON text of at most 96 KiB");
    }
    if (isObject(frame) && frame["type"] === "presence") return this.setPresence(ws, who, frame["on"]);
    if (!isObject(frame) || frame["type"] !== "send") return this.refuse(ws, null, 'a client sends {"type": "send"} or {"type": "presence"}');
    const raw = frame["message"];
    const rawId = isObject(raw) && typeof raw["id"] === "string" ? raw["id"].slice(0, 26) : null;
    const msg = parseMessage(raw);
    if (typeof msg === "string") return this.refuse(ws, rawId, msg);
    if (msg.device_id !== who.deviceId) return this.refuse(ws, msg.id, "device_id is not this socket's device");

    let limited: boolean;
    try {
      limited = !(await this.env.SEND_LIMIT.limit({ key: who.deviceId })).success;
    } catch (e) {
      console.error("send limit failed:", e);
      return this.refuse(ws, msg.id, CHECK_FAILED, "retry");
    }
    if (limited) return this.refuse(ws, msg.id, "too many messages, slow down");

    let key = this.keys.get(who.sigPub);
    if (!key) {
      const imported = await sigKey(who.sigPub);
      if (!imported) return this.refuse(ws, msg.id, "the device has no usable key");
      key = imported;
      this.keys.set(who.sigPub, key);
    }
    if (!(await verify(key, msg.sig, messageSigned(who.teamId, who.channelId, msg)))) return this.refuse(ws, msg.id, "bad signature");

    // Every socket's session, the sender's included, checked in the same read,
    // and those of sockets being admitted.
    const checked = new Set([who.session, ...this.sessions()]);
    let team: { epoch: number; keyed_epoch: number } | null;
    let allowed: Map<string, number>;
    try {
      [team, allowed] = await Promise.all([
        this.env.DB.prepare("SELECT epoch, keyed_epoch FROM teams WHERE team_id = ?").bind(who.teamId).first<{ epoch: number; keyed_epoch: number }>(),
        this.allowed(who.teamId, [...checked]),
      ]);
    } catch (e) {
      // D1 can refuse a read, for one when its queue is full. Nothing is
      // stored without the checks, and the sender hears it can try again.
      console.error("send check failed:", e);
      return this.refuse(ws, msg.id, CHECK_FAILED, "retry");
    }
    if (!team || !live(allowed, who.session)) return this.forbidSend(ws, msg.id);
    if (!epochAllowed(msg.epoch, team.epoch, team.keyed_epoch)) return this.refuseEpoch(ws, msg.id, epochRefused(msg.epoch, team.epoch, team.keyed_epoch));
    // A socket admitted while that read ran had its own check, but D1 does not
    // order two separate reads, so a logout between its check and ours could
    // reach neither. Check those sessions too, until no new one came in.
    try {
      for (let round = 0; round < RECHECKS; round++) {
        const fresh = this.sessions().filter((s) => !checked.has(s));
        if (fresh.length === 0) break;
        for (const s of fresh) checked.add(s);
        for (const [s, at] of await this.allowed(who.teamId, fresh)) allowed.set(s, at);
      }
      // The rechecks above only ask about the new sessions: an admission
      // still running can have its user log out just after the read that
      // filled `allowed`, so a logout would reach neither. With one still in
      // flight, ask about everyone once more.
      if (this.admitting.size > 0) {
        allowed = await this.allowed(who.teamId, [...checked]);
      }
    } catch (e) {
      console.error("send recheck failed:", e);
      return this.refuse(ws, msg.id, CHECK_FAILED, "retry");
    }

    // The epochs were read before the awaits above: a removal in between
    // starts a rotation, and one that ended would let this send store a
    // message under the retired key, on a channel with no newer message for
    // the `last` check below to see. Read them once more, at the insertion
    // boundary.
    let teamNow: { epoch: number; keyed_epoch: number } | null;
    try {
      teamNow = await this.env.DB.prepare("SELECT epoch, keyed_epoch FROM teams WHERE team_id = ?").bind(who.teamId).first<{ epoch: number; keyed_epoch: number }>();
    } catch (e) {
      console.error("send epoch check failed:", e);
      return this.refuse(ws, msg.id, CHECK_FAILED, "retry");
    }
    // A full recheck can withdraw the sender's access, and the session may
    // have expired during the last read. Nothing awaits after this check.
    if (!live(allowed, who.session) || !teamNow) return this.forbidSend(ws, msg.id);
    if (!epochAllowed(msg.epoch, teamNow.epoch, teamNow.keyed_epoch))
      return this.refuseEpoch(ws, msg.id, epochRefused(msg.epoch, teamNow.epoch, teamNow.keyed_epoch));

    // From here to the send nothing awaits, so no other frame runs in between:
    // the checks below and the insert are one step.
    if (this.sql.exec("SELECT 1 FROM messages WHERE id = ?1 UNION ALL SELECT 1 FROM retired_ids WHERE id = ?1 LIMIT 1", msg.id).toArray().length > 0) {
      return this.refuse(ws, msg.id, "a message with this id exists", "exists");
    }
    // The epoch read above may be old by now: a rotation can end, and a send
    // with the new key be stored, while this one awaited its checks. Never
    // store an older epoch after a newer one. Each insert keeps that true, so
    // the last message holds the newest epoch.
    const last = this.sql.exec<{ epoch: number }>("SELECT json_extract(message, '$.epoch') AS epoch FROM messages ORDER BY seq DESC LIMIT 1").toArray()[0];
    if (last && msg.epoch < last.epoch) return this.refuseEpoch(ws, msg.id, epochRefused(msg.epoch, last.epoch, last.epoch));
    const at = now();
    let seq: number;
    try {
      seq = this.ctx.storage.transactionSync(() => {
        const n = this.sql
          .exec<{ seq: number }>("INSERT INTO messages (id, at, device_id, message) VALUES (?, ?, ?, ?) RETURNING seq", msg.id, at, msg.device_id, JSON.stringify(msg))
          .one().seq;
        // seq never goes down, so the count comes from the oldest kept row,
        // not from n: otherwise every send past 10,000 would delete one.
        const oldest = this.sql.exec<{ seq: number }>("SELECT MIN(seq) AS seq FROM messages").one().seq;
        if (n - oldest + 1 > MAX_MESSAGES) {
          this.sql.exec("DELETE FROM messages WHERE seq <= ?", n - TRIM_TO);
          // A send older than the newest stored epoch is refused, and this
          // message is the newest, so IDs of older epochs can never be sent
          // again. Without this the table would grow with every trim.
          this.sql.exec("DELETE FROM retired_ids WHERE epoch < ?", msg.epoch);
        }
        return n;
      });
    } catch (e) {
      // The sender hears it rather than waiting for an ack that never comes.
      console.error("channel insert failed:", e);
      return this.refuse(ws, msg.id, "the channel could not store the message");
    }
    // Everyone on the channel, the sender included: that is its ack. A socket
    // whose session ended, or whose user lost the team or the flag, is closed
    // instead. One still unchecked after the last round gets nothing, and its
    // client may have read the history before this insert, so it is closed
    // with a code that tells the client to reconnect and read it again.
    const out = JSON.stringify({ type: "message", seq, at, message: msg });
    for (const other of this.ctx.getWebSockets()) {
      const session = sessionOf(other);
      if (!checked.has(session)) {
        this.close(other, CLOSE_RECONNECT, "reconnect and read the history");
        continue;
      }
      if (!live(allowed, session)) {
        this.close(other, CLOSE_FORBIDDEN, "not a member of this team");
        continue;
      }
      try {
        other.send(out);
      } catch {
        // closing
      }
    }
  }

  /// Makes a socket count toward presence (`on`) or not, for a client that
  /// starts or stops showing the channel. Nothing is answered: when the list
  /// changes, the presence frame that follows goes to everyone counted.
  private setPresence(ws: WebSocket, who: Who, on: unknown): void {
    if (typeof on !== "boolean") return this.refuse(ws, null, "presence takes on: true or false");
    if (!who.quiet === on) return;
    ws.serializeAttachment({ ...who, quiet: !on });
    this.presenceSoon();
  }

  /// The sessions of the open sockets and of those being admitted.
  private sessions(): string[] {
    return [...this.ctx.getWebSockets().map(sessionOf), ...this.admitting.keys()];
  }

  /// Which of these sessions are of a member of the team with the `teams`
  /// flag whose account is not blocked, each with when it ends. Expiry is checked where the answer is
  /// used: `now()` bound here would be old by then, and a session can end
  /// while the read runs.
  private async allowed(teamId: string, sessions: string[]): Promise<Map<string, number>> {
    const { results } = await this.env.DB.prepare(
      `SELECT s.token_hash, s.expires_at FROM sessions s
       JOIN members m ON m.account_id = s.account_id AND m.team_id = ?1
       JOIN user_flags f ON f.account_id = s.account_id AND f.flag = ?3
       JOIN accounts u ON u.id = s.account_id AND u.blocked_at IS NULL
       WHERE s.token_hash IN (SELECT value FROM json_each(?2))`,
    )
      .bind(teamId, JSON.stringify(sessions), TEAMS_FLAG)
      .all<{ token_hash: string; expires_at: number }>();
    return new Map(results.map((r) => [r.token_hash, r.expires_at]));
  }

  // The compatibility date is after 2026-04-07, so the runtime answers a
  // client's close by itself. The others hear who is left.
  override async webSocketClose(ws: WebSocket): Promise<void> {
    if (counted(ws)) await this.presence(ws);
  }

  override async webSocketError(ws: WebSocket): Promise<void> {
    this.close(ws, 1011, "error");
    if (counted(ws)) await this.presence(ws);
  }

  /// presence() without waiting for it, for callers that answer first.
  private presenceSoon(): void {
    this.ctx.waitUntil(this.presence());
  }

  /// Sends the account IDs with a counted socket open on this channel, sorted,
  /// once each, to every open counted socket that passes the same check as a
  /// send's recipients. Only those are listed; quiet sockets are neither
  /// listed nor told. `gone` is a socket that is closing and
  /// may still be listed. The server already knows who is connected; this
  /// only tells the members, and nothing of what they write.
  /// `retried` marks the one retry a failed run may schedule, so D1 being
  /// down does not make presence try for ever.
  private async presence(gone?: WebSocket, retried = false): Promise<void> {
    const run = ++this.presenceRuns;
    const open = () => this.ctx.getWebSockets().filter((ws) => ws !== gone && ws.readyState === WebSocket.OPEN && counted(ws));
    const first = open()[0]?.deserializeAttachment() as Who | null | undefined;
    if (!first) return;
    let allowed: Map<string, number>;
    try {
      allowed = await this.allowed(first.teamId, [...new Set(open().map(sessionOf))]);
    } catch (e) {
      console.error("presence check failed:", e);
      // One bounded retry, and only while this is still the latest list: an
      // older run retrying would send after the newer one it overtook.
      if (!retried) {
        this.ctx.waitUntil(
          new Promise((r) => setTimeout(r, 1000)).then(() => {
            if (run === this.presenceRuns) void this.presence(undefined, true);
          }),
        );
      }
      return;
    }
    // A socket that opened or closed during the check started a newer list,
    // which goes out instead of this one, so the last list is never older.
    if (run !== this.presenceRuns) return;
    // A socket that failed the check is neither told nor listed; the next
    // send closes it.
    const current = open().filter((ws) => live(allowed, sessionOf(ws)));
    const here = [...new Set(current.map((ws) => (ws.deserializeAttachment() as Who | null)?.accountId ?? 0))].filter((id) => id > 0).sort((a, b) => a - b);
    const out = JSON.stringify({ type: "presence", here });
    for (const ws of current) {
      try {
        ws.send(out);
      } catch {
        // closing
      }
    }
  }

  private forbidSend(ws: WebSocket, id: string): void {
    const reason = "not a member of this team";
    this.refuse(ws, id, reason);
    this.close(ws, CLOSE_FORBIDDEN, reason);
  }

  private refuseEpoch(ws: WebSocket, id: string, why: EpochRefusal): void {
    this.refuse(ws, id, why.error, why.code);
  }

  /// `code`, when given, is what a client matches on; older clients only
  /// read `error`.
  private refuse(ws: WebSocket, id: string | null, message: string, code?: string): void {
    try {
      ws.send(JSON.stringify(code === undefined ? { type: "error", id, error: message } : { type: "error", id, error: message, code }));
    } catch {
      // closing
    }
  }

  private close(ws: WebSocket, code: number, reason: string): void {
    try {
      ws.close(code, reason);
    } catch {
      // already gone
    }
  }
}
