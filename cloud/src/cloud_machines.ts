// The account's cloud machines (cloud_machine.ts): `arakun-server` in a
// container we run. Only an account with the `cloud` flag may have one, and one
// each for now. The app and the phone reach a running machine through the
// usual /machines/<label> doors; these routes only make, wake, put to sleep and
// remove it. There is no app or phone screen for them yet.
//
//   GET    /cloud/machines                → [MachineState]
//   POST   /cloud/machines                → 201 MachineState, { name? }; makes and starts one
//   POST   /cloud/machines/<label>/wake   → MachineState, after a start if it slept
//   POST   /cloud/machines/<label>/sleep  → 204, after a snapshot
//   DELETE /cloud/machines/<label>        → 204; the container, its snapshot and key go
//   GET    /cloud/usage                   → Usage: this month's awake time and limits
//
// The owner's model credentials for the agents there (cloud_model.ts). Only
// which providers are set ever comes back, never a key:
//
//   GET    /cloud/machines/<label>/model            → [{provider, set_at}]
//   PUT    /cloud/machines/<label>/model/<provider> → { model, applied }
//   DELETE /cloud/machines/<label>/model/<provider> → { model, applied }
//
// A wake past a limit (cloud_meter.ts) answers 403 with `{ error, limit }`,
// `limit` one of `stopped_all`, `held`, `allowance_used` or `task_cap`, and
// `error` the sentence to show. A machine stopped at the task cap wakes again
// only by the wake route here, not by a device at its door.
//
// The operator's side is in admin.ts: GET /admin/cloud/machines,
// POST /admin/cloud/machines/<label>/sleep, PUT and DELETE /admin/cloud/stop
// for every machine and /admin/users/<account_id>/cloud/stop for one account's,
// which call the functions at the bottom. Without the CLOUD_MACHINE binding (production, until it has been
// tried on the test Worker) every route answers 503.

import { record, type Operator } from "./admin_log";
import { session } from "./auth";
import { CLOUD_FLAG } from "./cloud_access";
import { allowedOwner, newMachineKey, type Refused } from "./cloud_machine";
import { STOP_ALL_KEY, budgetOf, limitOf, limitText, type Limit } from "./cloud_meter";
import { isProvider, keyUsable, parseModel } from "./cloud_model";
import { recheckMachine } from "./machines";
import { planFor } from "./relay/limits";
import { error, isObject, jsonBody, now, type Env } from "./util";

/// Cloud machines an account may have: one for now, so the allowance is one
/// machine's hours.
export const MAX_CLOUD_MACHINES = 1;
const MAX_NAME = 100;
const MAX_BODY = 4096;
const DEFAULT_NAME = "Cloud";
/// A Google credentials file is a few kilobytes.
const MAX_MODEL_BODY = 32 * 1024;
const ROUTE = /^\/cloud\/machines\/([a-z2-7]{16})(?:\/(wake|sleep|model)(?:\/([a-z]+))?)?$/;

/// The answer for a /cloud/machines route, or null when the path is not one.
export async function cloudMachines(request: Request, env: Env, url: URL, ctx: ExecutionContext): Promise<Response | null> {
  const path = url.pathname;
  const one = ROUTE.exec(path);
  if (path !== "/cloud/machines" && path !== "/cloud/usage" && !one) return null;
  const ns = env.CLOUD_MACHINE;
  if (!ns) return error(503, "cloud machines are not set up here");
  const s = await session(request, env, { flag: CLOUD_FLAG });
  if (!s) return error(401, "not signed in");
  if (s.blocked) return error(403, "blocked");
  if (!s.flagged) return error(403, "this account has no access to cloud machines; ask for it at /cloud/access");
  if (path === "/cloud/usage") return request.method === "GET" ? usage(env, s.accountId) : error(404, "not found");
  if (!one) {
    if (request.method === "GET") return list(env, ns, s.accountId);
    if (request.method === "POST") return create(request, env, ns, url, s.accountId, s.plan, ctx);
    return error(404, "not found");
  }
  const label = one[1]!;
  const action = one[2];
  const provider = one[3];
  if (provider !== undefined && action !== "model") return error(404, "not found");
  const owner = await env.DB.prepare("SELECT account_id FROM cloud_machines WHERE label = ?").bind(label).first<number>("account_id");
  if (owner !== s.accountId) return error(404, "no such cloud machine");
  const stub = ns.get(ns.idFromName(label));
  if (action === "wake" && request.method === "POST") {
    const woke = await stub.wake("owner");
    if (woke !== "running") return refusal(env, s.accountId, woke);
    return Response.json(await stub.state());
  }
  if (action === "sleep" && request.method === "POST") {
    await stub.sleep("asked by the owner");
    return new Response(null, { status: 204 });
  }
  if (action === "model") {
    if (provider === undefined) return request.method === "GET" ? Response.json(await stub.model()) : error(404, "not found");
    if (!isProvider(provider)) return error(404, "no such provider");
    if (request.method === "DELETE") {
      const cleared = await stub.clearModel(provider);
      return cleared ? Response.json(cleared) : error(404, "no such cloud machine");
    }
    if (request.method !== "PUT") return error(404, "not found");
    if (!keyUsable(env)) return error(503, "cloud machines take no API key here");
    const parsed = parseModel(provider, await jsonBody(request, MAX_MODEL_BODY));
    if (typeof parsed === "string") return error(400, parsed);
    const set = await stub.setModel(provider, parsed);
    return set ? Response.json(set) : error(404, "no such cloud machine");
  }
  if (!action && request.method === "DELETE") {
    await removeMachine(env, ns, label, s.accountId);
    return new Response(null, { status: 204 });
  }
  return error(404, "not found");
}

async function list(env: Env, ns: DurableObjectNamespace<import("./cloud_machine").CloudMachine>, accountId: number): Promise<Response> {
  const rows = await env.DB.prepare("SELECT label FROM cloud_machines WHERE account_id = ? ORDER BY created_at, label").bind(accountId).all<{ label: string }>();
  const states = await Promise.all(rows.results.map((r) => ns.get(ns.idFromName(r.label)).state()));
  return Response.json(states.filter((s) => s !== null));
}

async function create(
  request: Request,
  env: Env,
  ns: DurableObjectNamespace<import("./cloud_machine").CloudMachine>,
  url: URL,
  accountId: number,
  plan: string,
  ctx: ExecutionContext,
): Promise<Response> {
  const body = request.headers.get("Content-Length") === "0" || !request.body ? {} : await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, 'expected {"name": "..."} or no body');
  const given = body["name"];
  if (given !== undefined && (typeof given !== "string" || given.trim().length === 0 || given.trim().length > MAX_NAME)) {
    return error(400, `name must be 1 to ${MAX_NAME} characters`);
  }
  const name = typeof given === "string" ? given.trim() : DEFAULT_NAME;
  // The machine registers like any computer of the account once it runs, so
  // it needs room under the plan's limit.
  const count = await env.DB.prepare("SELECT COUNT(*) AS n FROM machines WHERE account_id = ?").bind(accountId).first<number>("n");
  if ((count ?? 0) >= (await planFor(env.CONFIG, plan)).machines) return error(409, "the account has as many machines as its plan allows");
  const key = await newMachineKey();
  // The count and the insert in one statement, so two requests at once
  // cannot both get under the limit.
  const made = await env.DB.prepare(
    `INSERT INTO cloud_machines (label, account_id, name, created_at)
     SELECT ?1, ?2, ?3, ?4 WHERE (SELECT COUNT(*) FROM cloud_machines WHERE account_id = ?2) < ?5`,
  )
    .bind(key.label, accountId, name, now(), MAX_CLOUD_MACHINES)
    .run();
  if (made.meta.changes === 0) return error(409, `an account has at most ${MAX_CLOUD_MACHINES} cloud machine`);
  const stub = ns.get(ns.idFromName(key.label));
  await stub.create({ label: key.label, githubId: accountId, name, base: accountBase(env, url), identity: key.identity, identityPublic: key.identityPublic });
  // The first start pulls the image, which takes a while: the answer does not
  // wait for it. GET /cloud/machines shows when it runs.
  ctx.waitUntil(
    stub.wake("owner").then(
      (woke) => {
        if (woke !== "running") console.error(`cloud machine ${key.label} did not start: ${woke}`);
      },
      (e) => console.error(`cloud machine ${key.label} did not start:`, e),
    ),
  );
  return Response.json(await stub.state(), { status: 201 });
}

/// The account host the machine's server signs in to and dials.
function accountBase(env: Env, url: URL): string {
  return env.ACCOUNT_HOST ? `https://${env.ACCOUNT_HOST.toLowerCase()}` : url.origin;
}

async function refusal(env: Env, accountId: number, why: Refused): Promise<Response> {
  switch (why) {
    case "not_allowed":
      return error(403, "this account may not run a cloud machine now");
    case "no_machine":
      return error(404, "no such cloud machine");
    case "no_container":
      return error(503, "cloud machines are not set up here");
    default: {
      const owner = await allowedOwner(env, accountId);
      const budget = await budgetOf(env, accountId, owner?.plan ?? "free", Date.now());
      return Response.json({ error: limitText(why, budget), limit: why }, { status: 403 });
    }
  }
}

/// GET /cloud/usage: what the account's cloud machines used this UTC month,
/// what it may use, and the account-wide limit that stops them, if any. The
/// task cap is per machine: its `stopped` in GET /cloud/machines.
export interface Usage {
  month: string;
  used_ms: number;
  allowance_ms: number;
  left_ms: number;
  task_cap_ms: number;
  stopped: Exclude<Limit, "task_cap"> | null;
  /// The sentence to show for `stopped`, or null.
  message: string | null;
}

async function usage(env: Env, accountId: number): Promise<Response> {
  const owner = await allowedOwner(env, accountId);
  const b = await budgetOf(env, accountId, owner?.plan ?? "free", Date.now());
  const limit = limitOf(b, 0);
  const stopped = limit === "task_cap" ? null : limit;
  return Response.json({
    month: b.month,
    used_ms: b.usedMs,
    allowance_ms: b.allowanceMs,
    left_ms: Math.max(0, b.allowanceMs - b.usedMs),
    task_cap_ms: b.taskCapMs,
    stopped,
    message: stopped ? limitText(stopped, b) : null,
  } satisfies Usage);
}

/// Stops the machine and forgets it: its object's key and snapshot, its row,
/// its registration on the relay and its cloud session.
async function removeMachine(env: Env, ns: DurableObjectNamespace<import("./cloud_machine").CloudMachine>, label: string, accountId: number): Promise<void> {
  await ns.get(ns.idFromName(label)).remove();
  await env.DB.batch([
    env.DB.prepare("DELETE FROM cloud_machines WHERE label = ? AND account_id = ?").bind(label, accountId),
    env.DB.prepare("DELETE FROM machines WHERE label = ? AND account_id = ?").bind(label, accountId),
  ]);
  await recheckMachine(env, label);
}

/// Wakes the label's machine if it is a cloud machine, for a device that
/// came to its door. Errors are logged: the device's socket goes on either way,
/// and the relay tells it the computer is offline until the server dials in.
export async function wakeIfCloud(env: Env, label: string): Promise<void> {
  const ns = env.CLOUD_MACHINE;
  if (!ns) return;
  try {
    const woke = await ns.get(ns.idFromName(label)).wake();
    if (woke !== "running") console.error(`cloud machine ${label} did not wake: ${woke}`);
  } catch (e) {
    console.error(`cloud machine ${label} did not wake:`, e);
  }
}

/// GET /admin/cloud/machines: every cloud machine, with its owner, its
/// state, why it is stopped and the owner's awake time this UTC month.
export async function listAllMachines(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    `SELECT c.label, c.account_id, u.login, c.name, c.created_at, COALESCE(g.awake_ms, 0) AS month_ms
     FROM cloud_machines c JOIN accounts u ON u.id = c.account_id
     LEFT JOIN cloud_usage g ON g.account_id = c.account_id AND g.month = ?
     ORDER BY c.created_at DESC, c.label`,
  )
    .bind(new Date().toISOString().slice(0, 7))
    .all<{ label: string; account_id: number; login: string; name: string; created_at: number; month_ms: number }>();
  const ns = env.CLOUD_MACHINE;
  const states = await Promise.all(rows.results.map((r) => (ns ? ns.get(ns.idFromName(r.label)).state() : null)));
  return Response.json(
    rows.results.map((r, i) => ({
      ...r,
      running: states[i]?.running ?? false,
      snapshot: states[i]?.snapshot ?? false,
      stopped: states[i]?.stopped ?? null,
    })),
  );
}

/// POST /admin/cloud/machines/<label>/sleep: the operator stops one machine
/// now, with a snapshot. To keep it down, take the account's `cloud` flag or
/// block it: the next wake is refused then.
export async function sleepMachine(env: Env, op: Operator, label: string): Promise<Response> {
  const ns = env.CLOUD_MACHINE;
  if (!ns) return error(503, "cloud machines are not set up here");
  const owner = await env.DB.prepare("SELECT account_id FROM cloud_machines WHERE label = ?").bind(label).first<number>("account_id");
  if (owner === null) return error(404, "no such cloud machine");
  await ns.get(ns.idFromName(label)).sleep("stopped by the operator");
  await record(env, op, "cloud.machine.sleep", owner, { label });
  return new Response(null, { status: 204 });
}

const MAX_REASON = 500;
const MAX_BODY_STOP = 4096;

/// `{ reason? }` of a stop, or a Response for a bad one.
async function stopReason(request: Request): Promise<string | null | Response> {
  const body = request.headers.get("Content-Length") === "0" || !request.body ? {} : await jsonBody(request, MAX_BODY_STOP);
  if (!isObject(body)) return error(400, 'expected {"reason": "..."} or no body');
  const reason = body["reason"];
  if (reason !== undefined && reason !== null && (typeof reason !== "string" || reason.length > MAX_REASON)) {
    return error(400, `reason must be text of at most ${MAX_REASON} characters`);
  }
  return typeof reason === "string" && reason.trim() !== "" ? reason.trim() : null;
}

/// Puts the machines to sleep with a snapshot, all at once. One that fails is
/// logged; its next check sees the limit and stops it then.
async function stopAll(env: Env, labels: string[], limit: Limit): Promise<number> {
  const ns = env.CLOUD_MACHINE;
  if (!ns) return 0;
  const done = await Promise.allSettled(labels.map((label) => ns.get(ns.idFromName(label)).stop(limit)));
  done.forEach((d, i) => {
    if (d.status === "rejected") console.error(`cloud machine ${labels[i]} did not stop (${limit}):`, d.reason);
  });
  return done.filter((d) => d.status === "fulfilled").length;
}

/// PUT /admin/cloud/stop: the switch for everyone. Every cloud machine sleeps
/// now and none wakes until DELETE. Answers `{ machines }`, how many stopped
/// here; the rest stop at their next check, within CHECK_MS.
export async function stopEveryone(request: Request, env: Env, op: Operator): Promise<Response> {
  const reason = await stopReason(request);
  if (reason instanceof Response) return reason;
  // KV first, so a machine that wakes while the others stop is refused.
  await env.CONFIG.put(STOP_ALL_KEY, reason ?? "true");
  const rows = await env.DB.prepare("SELECT label FROM cloud_machines").all<{ label: string }>();
  const machines = await stopAll(
    env,
    rows.results.map((r) => r.label),
    "stopped_all",
  );
  await record(env, op, "cloud.stop_all", null, { reason, machines });
  return Response.json({ machines });
}

/// DELETE /admin/cloud/stop: lifts the switch. Machines wake as before, at a
/// device's knock or their owner's wake.
export async function resumeEveryone(env: Env, op: Operator): Promise<Response> {
  await env.CONFIG.delete(STOP_ALL_KEY);
  await record(env, op, "cloud.resume_all", null);
  return new Response(null, { status: 204 });
}

/// PUT /admin/users/<account_id>/cloud/stop: the switch for one account. Its
/// cloud machines sleep now and do not wake until DELETE; its flag stays.
export async function holdAccount(request: Request, env: Env, op: Operator, accountId: number): Promise<Response> {
  const reason = await stopReason(request);
  if (reason instanceof Response) return reason;
  if (!(await env.DB.prepare("SELECT 1 FROM accounts WHERE id = ?").bind(accountId).first())) return error(404, "no such account");
  await env.DB.prepare(
    `INSERT INTO cloud_holds (account_id, reason, held_at) VALUES (?1, ?2, ?3)
     ON CONFLICT (account_id) DO UPDATE SET reason = ?2, held_at = ?3`,
  )
    .bind(accountId, reason, now())
    .run();
  const rows = await env.DB.prepare("SELECT label FROM cloud_machines WHERE account_id = ?").bind(accountId).all<{ label: string }>();
  const machines = await stopAll(
    env,
    rows.results.map((r) => r.label),
    "held",
  );
  await record(env, op, "cloud.hold", accountId, { reason, machines });
  return Response.json({ machines });
}

/// DELETE /admin/users/<account_id>/cloud/stop: lifts it.
export async function releaseAccount(env: Env, op: Operator, accountId: number): Promise<Response> {
  const result = await env.DB.prepare("DELETE FROM cloud_holds WHERE account_id = ?").bind(accountId).run();
  if (result.meta.changes === 0) return error(404, "this account's cloud machines are not stopped");
  await record(env, op, "cloud.release", accountId);
  return new Response(null, { status: 204 });
}
