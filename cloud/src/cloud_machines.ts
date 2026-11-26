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
//
// The operator's side is in admin.ts: GET /admin/cloud/machines and
// POST /admin/cloud/machines/<label>/sleep, which call the functions at the
// bottom. Without the CLOUD_MACHINE binding (production, until it has been
// tried on the test Worker) every route answers 503.

import { record, type Operator } from "./admin_log";
import { session } from "./auth";
import { CLOUD_FLAG } from "./cloud_access";
import { newMachineKey, type Refused } from "./cloud_machine";
import { recheckMachine } from "./machines";
import { planFor } from "./relay/limits";
import { error, isObject, jsonBody, now, type Env } from "./util";

/// Cloud machines an account may have. One while the metering (#784) is not in.
export const MAX_CLOUD_MACHINES = 1;
const MAX_NAME = 100;
const MAX_BODY = 4096;
const DEFAULT_NAME = "Cloud";
const ROUTE = /^\/cloud\/machines\/([a-z2-7]{16})(?:\/(wake|sleep))?$/;

/// The answer for a /cloud/machines route, or null when the path is not one.
export async function cloudMachines(request: Request, env: Env, url: URL, ctx: ExecutionContext): Promise<Response | null> {
  const path = url.pathname;
  const one = ROUTE.exec(path);
  if (path !== "/cloud/machines" && !one) return null;
  const ns = env.CLOUD_MACHINE;
  if (!ns) return error(503, "cloud machines are not set up here");
  const s = await session(request, env, { flag: CLOUD_FLAG });
  if (!s) return error(401, "not signed in");
  if (s.blocked) return error(403, "blocked");
  if (!s.flagged) return error(403, "this account has no access to cloud machines; ask for it at /cloud/access");
  if (!one) {
    if (request.method === "GET") return list(env, ns, s.accountId);
    if (request.method === "POST") return create(request, env, ns, url, s.accountId, s.plan, ctx);
    return error(404, "not found");
  }
  const label = one[1]!;
  const action = one[2];
  const owner = await env.DB.prepare("SELECT account_id FROM cloud_machines WHERE label = ?").bind(label).first<number>("account_id");
  if (owner !== s.accountId) return error(404, "no such cloud machine");
  const stub = ns.get(ns.idFromName(label));
  if (action === "wake" && request.method === "POST") {
    const woke = await stub.wake();
    if (woke !== "running") return refusal(woke);
    return Response.json(await stub.state());
  }
  if (action === "sleep" && request.method === "POST") {
    await stub.sleep("asked by the owner");
    return new Response(null, { status: 204 });
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
    stub.wake().then(
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

function refusal(why: Refused): Response {
  switch (why) {
    case "not_allowed":
      return error(403, "this account may not run a cloud machine now");
    case "no_machine":
      return error(404, "no such cloud machine");
    case "no_container":
      return error(503, "cloud machines are not set up here");
  }
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

/// GET /admin/cloud/machines: every cloud machine, with its owner and state.
export async function listAllMachines(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    `SELECT c.label, c.account_id, u.login, c.name, c.created_at
     FROM cloud_machines c JOIN accounts u ON u.id = c.account_id
     ORDER BY c.created_at DESC, c.label`,
  ).all<{ label: string; account_id: number; login: string; name: string; created_at: number }>();
  const ns = env.CLOUD_MACHINE;
  const states = await Promise.all(rows.results.map((r) => (ns ? ns.get(ns.idFromName(r.label)).state() : null)));
  return Response.json(rows.results.map((r, i) => ({ ...r, running: states[i]?.running ?? false, snapshot: states[i]?.snapshot ?? false })));
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
