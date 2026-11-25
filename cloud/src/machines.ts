// The account's machines, and the two doors to the relay. A computer comes in
// at /machines/<label>/pc with its account's session and registers the label
// to the account when it claims; a device comes in at /machines/<label>/ws
// with a session of the same account. Everything the label's object needs to
// know about who is calling, it hears from here in headers built from the
// session, never from the client.
//
//   GET    /machines              → [{ label, pub, name, kind, online, last_seen_at }]
//   DELETE /machines/<label>      → forgets the machine and closes its sockets
//   GET    /machines/<label>/pc   → the computer's socket
//   GET    /machines/<label>/ws   → a device's socket

import { session, type Session } from "./auth";
import { PAGE_ID, phoneControl, pcControl } from "./relay/frames";
import { disabledReason, planFor } from "./relay/limits";
import { CLOSE_FORBIDDEN, CLOSE_GONE, CLOSE_LIMIT, CLOSE_NOT_FOUND, CLOSE_UNAUTHORIZED, CLOSE_UNAVAILABLE } from "./relay/relay";
import { discard, error, type Env } from "./util";

const MACHINE = /^\/machines\/([a-z2-7]{16})(?:\/(pc|ws))?$/;
const MACHINE_KINDS = ["app", "server"];
const MAX_NAME = 100;

/// Why a socket is sent away before it reaches the object, and the close code.
const REFUSALS = {
  signed_out: CLOSE_UNAUTHORIZED,
  blocked: CLOSE_FORBIDDEN,
  label_taken: CLOSE_FORBIDDEN,
  not_yours: CLOSE_FORBIDDEN,
  machines_full: CLOSE_LIMIT,
  no_machine: CLOSE_NOT_FOUND,
  update: CLOSE_GONE,
  disabled: CLOSE_UNAVAILABLE,
} as const;

export type Refusal = keyof typeof REFUSALS;

/// The answer for a /machines route, or null when the path is not one.
export async function machines(request: Request, env: Env, url: URL): Promise<Response | null> {
  if (url.pathname === "/machines") return request.method === "GET" ? list(request, env) : null;
  const match = MACHINE.exec(url.pathname);
  if (!match) return null;
  const label = match[1]!;
  const door = match[2];
  if (!door) return request.method === "DELETE" ? forget(request, env, label) : null;
  if (request.method !== "GET") return null;
  return door === "pc" ? computerDoor(request, env, url, label) : deviceDoor(request, env, url, label);
}

async function list(request: Request, env: Env): Promise<Response> {
  const s = await session(request, env);
  if (!s) return error(401, "not signed in");
  if (s.blocked) return blockedAnswer(env, s.githubId);
  const rows = await env.DB.prepare("SELECT label, pub, name, kind, last_seen_at FROM machines WHERE github_id = ? ORDER BY created_at, label")
    .bind(s.githubId)
    .all<{ label: string; pub: string; name: string; kind: string; last_seen_at: number }>();
  const online = await Promise.all(rows.results.map((r) => isOnline(env, r.label)));
  return Response.json(rows.results.map((r, i) => ({ label: r.label, pub: r.pub, name: r.name, kind: r.kind, online: online[i], last_seen_at: r.last_seen_at })));
}

/// Forgets one of the account's machines. Its sockets are closed; a computer
/// still running registers the label again when it reconnects.
async function forget(request: Request, env: Env, label: string): Promise<Response> {
  const s = await session(request, env, { renew: false });
  if (!s) return error(401, "not signed in");
  if (s.blocked) return blockedAnswer(env, s.githubId);
  const result = await env.DB.prepare("DELETE FROM machines WHERE label = ? AND github_id = ?").bind(label, s.githubId).run();
  if (result.meta.changes === 0) return error(404, "no such machine");
  await recheckMachine(env, label);
  return new Response(null, { status: 204 });
}

/// The computer's door. The object registers the label when the computer
/// proves it holds the key; these checks only turn a hopeless claim away early.
async function computerDoor(request: Request, env: Env, url: URL, label: string): Promise<Response> {
  if (!isUpgrade(request)) return notASocket(env);
  const name = (url.searchParams.get("name") ?? "").trim();
  const kind = url.searchParams.get("kind") ?? "app";
  if (name.length === 0 || name.length > MAX_NAME) return error(400, `name must be 1 to ${MAX_NAME} characters`);
  if (!MACHINE_KINDS.includes(kind)) return error(400, `kind must be one of ${MACHINE_KINDS.join(", ")}`);
  // The one place a cloud session is good for: the door of its own label.
  const [disabled, s] = await Promise.all([disabledReason(env.CONFIG), session(request, env, { machine: label })]);
  if (disabled) return refuseSocket("pc", "disabled", disabled);
  if (!s) return refuseSocket("pc", "signed_out");
  if (s.blocked) return refuseSocket("pc", "blocked");
  const [owner, count] = await env.DB.batch<{ n: number }>([
    env.DB.prepare("SELECT github_id AS n FROM machines WHERE label = ?").bind(label),
    env.DB.prepare("SELECT COUNT(*) AS n FROM machines WHERE github_id = ?").bind(s.githubId),
  ]);
  const ownerId = owner!.results[0]?.n;
  if (ownerId !== undefined && ownerId !== s.githubId) return refuseSocket("pc", "label_taken");
  if (ownerId === undefined && count!.results[0]!.n >= (await planFor(env.CONFIG, s.plan)).machines) return refuseSocket("pc", "machines_full");
  return toObject(env, label, "pc", null, s, {
    "x-arakun-machine-name": encodeURIComponent(name),
    "x-arakun-machine-kind": kind,
  });
}

/// A device's door: only a session of the account the machine is registered to.
async function deviceDoor(request: Request, env: Env, url: URL, label: string): Promise<Response> {
  if (!isUpgrade(request)) return notASocket(env);
  const [disabled, s] = await Promise.all([disabledReason(env.CONFIG), session(request, env)]);
  if (disabled) return refuseSocket("ws", "disabled", disabled);
  if (!s) return refuseSocket("ws", "signed_out");
  if (s.blocked) return refuseSocket("ws", "blocked");
  const owner = await env.DB.prepare("SELECT github_id FROM machines WHERE label = ?").bind(label).first<number>("github_id");
  if (owner === null) return refuseSocket("ws", "no_machine");
  if (owner !== s.githubId) return refuseSocket("ws", "not_yours");
  const page = url.searchParams.get("page");
  return toObject(env, label, "ws", page !== null && PAGE_ID.test(page) ? page : null, s, {
    "x-arakun-device": s.id,
    "x-arakun-device-name": encodeURIComponent(s.name),
    "x-arakun-device-kind": s.kind,
  });
}

/// Opens the socket on the label's object. The request is made here, not
/// passed on: no header the client sent, an `x-arakun-*` one least of all,
/// nor its session token, reaches the object. Names are URI-encoded, since a
/// header carries no characters outside Latin-1.
function toObject(env: Env, label: string, door: "pc" | "ws", page: string | null, s: Session, extra: Record<string, string>): Promise<Response> {
  const url = new URL(`https://relay/${door}`);
  if (page) url.searchParams.set("page", page);
  const headers = new Headers({
    Upgrade: "websocket",
    "x-arakun-label": label,
    "x-arakun-account": String(s.githubId),
    "x-arakun-plan": s.plan,
    "x-arakun-session": s.hash,
    ...extra,
  });
  return relayStub(env, label).fetch(url, { headers });
}

/// Accepts the socket only to say why it is refused and close it: a refused
/// upgrade would look like a dead network to a page. `message` is the
/// operator's text, for `disabled`.
export function refuseSocket(door: "pc" | "ws", reason: Refusal, message?: string): Response {
  const pair = new WebSocketPair();
  const [client, server] = [pair[0], pair[1]];
  server.accept();
  const bye = message === undefined ? { type: "bye", reason } : { type: "bye", reason, message };
  server.send(door === "pc" ? pcControl(bye) : phoneControl(bye));
  server.close(REFUSALS[reason], reason);
  return new Response(null, { status: 101, webSocket: client });
}

/// Tells the account's machine objects to recheck their sockets, after a
/// sign-out or a block, so those sockets end now rather than at the alarm.
export async function recheckAccount(env: Env, githubId: number): Promise<void> {
  const rows = await env.DB.prepare("SELECT label FROM machines WHERE github_id = ?").bind(githubId).all<{ label: string }>();
  await Promise.all(rows.results.map((r) => recheckMachine(env, r.label)));
}

/// A recheck that fails is not the caller's failure: the object's alarm
/// rechecks within minutes anyway.
async function recheckMachine(env: Env, label: string): Promise<void> {
  try {
    const res = await relayStub(env, label).fetch("https://relay/_recheck", { method: "POST", headers: { "x-arakun-label": label } });
    if (!res.ok) console.error(`recheck of ${label} answered ${res.status}`);
    await discard(res);
  } catch (e) {
    console.error(`recheck of ${label} failed:`, e);
  }
}

async function isOnline(env: Env, label: string): Promise<boolean> {
  try {
    const res = await relayStub(env, label).fetch("https://relay/_state", { headers: { "x-arakun-label": label } });
    if (!res.ok) {
      await discard(res);
      return false;
    }
    return ((await res.json()) as { online?: unknown }).online === true;
  } catch {
    return false;
  }
}

/// A blocked account may still see why: the reason the operator gave.
async function blockedAnswer(env: Env, githubId: number): Promise<Response> {
  const reason = await env.DB.prepare("SELECT blocked_reason FROM users WHERE github_id = ?").bind(githubId).first<string | null>("blocked_reason");
  return Response.json({ error: "blocked", reason: reason ?? null }, { status: 403 });
}

/// A door asked without an upgrade: plain HTTP can read a 503, so a closed
/// relay says so there too.
async function notASocket(env: Env): Promise<Response> {
  const disabled = await disabledReason(env.CONFIG);
  return disabled ? error(503, disabled) : error(426, "expected a WebSocket upgrade");
}

function isUpgrade(request: Request): boolean {
  return request.headers.get("Upgrade")?.toLowerCase() === "websocket";
}

function relayStub(env: Env, label: string) {
  return env.RELAY.get(env.RELAY.idFromName(label));
}
