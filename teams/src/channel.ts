// One channel's messages, `seq` and live sockets: a SQLite-backed Durable
// Object per channel, named `<team_id>/<channel_id>`.
//
// Only the Worker reaches it. The Worker checks the session, the flag, the
// membership and the device, then opens the socket here with the trusted
// headers below, which it builds itself. Sockets hibernate between frames, so
// who is on a socket lives in its attachment.
//
// On every `send` this reads the team's epoch, and whether each socket's
// session is still live, its user still a member and still flagged, from D1
// rather than trusting what the Worker saw at upgrade: a socket stays open for
// hours, and a removal, a logout, a lost flag or a rotation must apply to the
// next message, for the sender and for every recipient. That is one D1 batch
// per message.

import { DurableObject } from "cloudflare:workers";
import { error, isObject, now, type Env } from "./util";
import { messageSigned, parseMessage, sigKey, verify, type WireMessage } from "./wire";

/// Headers only the Worker sets, on a request it builds from scratch.
export const TRUSTED = {
  githubId: "X-Teams-Github-Id",
  deviceId: "X-Teams-Device-Id",
  sigPub: "X-Teams-Sig-Pub",
  teamId: "X-Teams-Team-Id",
  channelId: "X-Teams-Channel-Id",
  /// The SHA-256 the sessions table keys on, never the session token itself.
  session: "X-Teams-Session-Hash",
} as const;

/// Who is on a socket, kept on it across hibernation.
interface Who {
  githubId: number;
  deviceId: string;
  sigPub: string;
  teamId: string;
  channelId: string;
  session: string;
}

export interface Stored {
  seq: number;
  at: number;
  message: WireMessage;
}

/// The JSON of a ct of 64 KiB is about 87 KiB; the rest of a frame is small.
const MAX_FRAME = 96 * 1024;
/// In the 4000s so they never collide with the protocol's own codes.
const CLOSE_BAD_FRAME = 4400;
const CLOSE_FORBIDDEN = 4403;

/// Whether a message's epoch may be stored. The current epoch always may.
/// While a rotation is pending (no envelopes for the current epoch yet), the
/// newest epoch that has them still may, since clients have no newer key.
export function epochAllowed(epoch: number, current: number, keyed: number): boolean {
  if (epoch === current) return true;
  return keyed < current && keyed >= 1 && epoch === keyed;
}

/// The session a socket was opened with; "" for none, which no check allows.
function sessionOf(ws: WebSocket): string {
  return (ws.deserializeAttachment() as Who | null)?.session ?? "";
}

export class Channel extends DurableObject<Env> {
  private sql: SqlStorage;
  /// Imported verify keys by sig_pub; lost on hibernation, which is fine.
  private keys = new Map<string, CryptoKey>();
  /// Kicks per user, so a socket admitted while its user was being removed is
  /// refused. Only read during an admission, which keeps the object awake.
  private kicks = new Map<number, number>();

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
  }

  /// Opens a socket. Only the Worker calls this, after its checks.
  override async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return error(426, "expected a WebSocket upgrade");
    const h = request.headers;
    const who: Who = {
      githubId: Number(h.get(TRUSTED.githubId)),
      deviceId: h.get(TRUSTED.deviceId) ?? "",
      sigPub: h.get(TRUSTED.sigPub) ?? "",
      teamId: h.get(TRUSTED.teamId) ?? "",
      channelId: h.get(TRUSTED.channelId) ?? "",
      session: h.get(TRUSTED.session) ?? "",
    };
    if (!Number.isSafeInteger(who.githubId) || who.githubId <= 0 || !who.deviceId || !who.sigPub || !who.teamId || !who.channelId || !who.session) {
      return error(500, "internal error");
    }
    // The Worker checked membership before it called here, but a removal can
    // commit and kick in between, before this socket exists. So check again,
    // and refuse if a kick for this user ran while we checked.
    const kicks = this.kicks.get(who.githubId) ?? 0;
    const allowed = await this.allowed(who.teamId, [who.session]);
    if (!allowed.has(who.session) || (this.kicks.get(who.githubId) ?? 0) !== kicks) return error(404, "not found");
    const pair = new WebSocketPair();
    const [client, server] = [pair[0], pair[1]];
    // The tag lets a removal find one user's sockets.
    this.ctx.acceptWebSocket(server, [`u:${who.githubId}`]);
    server.serializeAttachment(who);
    return new Response(null, { status: 101, webSocket: client });
  }

  /// Messages after `after`, oldest first.
  history(after: number, limit: number): Stored[] {
    return this.sql
      .exec<{ seq: number; at: number; message: string }>("SELECT seq, at, message FROM messages WHERE seq > ? ORDER BY seq LIMIT ?", after, limit)
      .toArray()
      .map((row) => ({ seq: row.seq, at: row.at, message: JSON.parse(row.message) as WireMessage }));
  }

  /// Closes a removed member's sockets; answers how many there were.
  kick(githubId: number): number {
    this.kicks.set(githubId, (this.kicks.get(githubId) ?? 0) + 1);
    const sockets = this.ctx.getWebSockets(`u:${githubId}`);
    for (const ws of sockets) this.close(ws, CLOSE_FORBIDDEN, "removed from the team");
    return sockets.length;
  }

  override async webSocketMessage(ws: WebSocket, data: ArrayBuffer | string): Promise<void> {
    const who = ws.deserializeAttachment() as Who | null;
    if (!who) return this.close(ws, CLOSE_BAD_FRAME, "unknown socket");
    if (typeof data !== "string" || data.length > MAX_FRAME) return this.refuse(ws, null, "frames are JSON text of at most 96 KiB");
    let frame: unknown;
    try {
      frame = JSON.parse(data);
    } catch {
      return this.refuse(ws, null, "frames are JSON text of at most 96 KiB");
    }
    if (!isObject(frame) || frame["type"] !== "send") return this.refuse(ws, null, 'the only frame a client sends is {"type": "send"}');
    const raw = frame["message"];
    const rawId = isObject(raw) && typeof raw["id"] === "string" ? raw["id"].slice(0, 26) : null;
    const msg = parseMessage(raw);
    if (typeof msg === "string") return this.refuse(ws, rawId, msg);
    if (msg.device_id !== who.deviceId) return this.refuse(ws, msg.id, "device_id is not this socket's device");

    const { success } = await this.env.SEND_LIMIT.limit({ key: who.deviceId });
    if (!success) return this.refuse(ws, msg.id, "too many messages, slow down");

    let key = this.keys.get(who.sigPub);
    if (!key) {
      const imported = await sigKey(who.sigPub);
      if (!imported) return this.refuse(ws, msg.id, "the device has no usable key");
      key = imported;
      this.keys.set(who.sigPub, key);
    }
    if (!(await verify(key, msg.sig, messageSigned(who.teamId, who.channelId, msg)))) return this.refuse(ws, msg.id, "bad signature");

    // Every socket's session, the sender's included, checked in the same read.
    const checked = new Set([who.session, ...this.ctx.getWebSockets().map(sessionOf)]);
    const [team, allowed] = await Promise.all([
      this.env.DB.prepare("SELECT epoch, keyed_epoch FROM teams WHERE team_id = ?").bind(who.teamId).first<{ epoch: number; keyed_epoch: number }>(),
      this.allowed(who.teamId, [...checked]),
    ]);
    if (!team || !allowed.has(who.session)) {
      this.refuse(ws, msg.id, "not a member of this team");
      return this.close(ws, CLOSE_FORBIDDEN, "not a member of this team");
    }
    if (!epochAllowed(msg.epoch, team.epoch, team.keyed_epoch)) {
      return this.refuse(ws, msg.id, msg.epoch < team.epoch ? "stale epoch: fetch the new team key" : "unknown epoch");
    }

    // From here to the send nothing awaits, so no other frame runs in between:
    // the check for a duplicate and the insert are one step.
    if (this.sql.exec("SELECT 1 FROM messages WHERE id = ?", msg.id).toArray().length > 0) {
      return this.refuse(ws, msg.id, "a message with this id exists");
    }
    const at = now();
    let seq: number;
    try {
      ({ seq } = this.sql
        .exec<{ seq: number }>("INSERT INTO messages (id, at, device_id, message) VALUES (?, ?, ?, ?) RETURNING seq", msg.id, at, msg.device_id, JSON.stringify(msg))
        .one());
    } catch (e) {
      // Most likely the object's storage is full. The sender hears it, rather
      // than waiting for an ack that never comes.
      console.error("channel insert failed:", e);
      return this.refuse(ws, msg.id, "the channel could not store the message");
    }
    // Everyone on the channel, the sender included: that is its ack. A socket
    // whose session ended, or whose user lost the team or the flag, is closed
    // instead. One opened during the read above was checked when it opened.
    const out = JSON.stringify({ type: "message", seq, at, message: msg });
    for (const other of this.ctx.getWebSockets()) {
      const session = sessionOf(other);
      if (checked.has(session) && !allowed.has(session)) {
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

  /// Which of these sessions are live, of a member of the team, with the
  /// `teams` flag.
  private async allowed(teamId: string, sessions: string[]): Promise<Set<string>> {
    const { results } = await this.env.DB.prepare(
      `SELECT s.token_hash FROM sessions s
       JOIN members m ON m.github_id = s.github_id AND m.team_id = ?1
       JOIN user_flags f ON f.github_id = s.github_id AND f.flag = 'teams'
       WHERE s.token_hash IN (SELECT value FROM json_each(?2)) AND s.expires_at > ?3`,
    )
      .bind(teamId, JSON.stringify(sessions), now())
      .all<{ token_hash: string }>();
    return new Set(results.map((r) => r.token_hash));
  }

  // The compatibility date is after 2026-04-07, so the runtime answers a
  // client's close by itself; there is nothing to clean up here.
  override async webSocketClose(): Promise<void> {}

  override async webSocketError(ws: WebSocket): Promise<void> {
    this.close(ws, 1011, "error");
  }

  private refuse(ws: WebSocket, id: string | null, message: string): void {
    try {
      ws.send(JSON.stringify({ type: "error", id, error: message }));
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
