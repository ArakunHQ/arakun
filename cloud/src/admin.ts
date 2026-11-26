// The operator's door, with no accounts to log into: a bearer secret set with
// `wrangler secret put ADMIN_SECRET`.
//
//   PUT    /admin/users/<github_id>/flags/<flag>  → grants the flag
//   DELETE /admin/users/<github_id>/flags/<flag>  → takes it away
//   DELETE /admin/users/<github_id>/sessions      → signs the user out everywhere
//   PUT    /admin/users/<github_id>/block         → blocks the account, { reason? }
//   DELETE /admin/users/<github_id>/block         → lets it back
//   PUT    /admin/users/<github_id>/plan          → puts it on a plan, { plan }
//   GET    /admin/users                           → accounts, last seen first (admin_lists.ts)
//   GET    /admin/users/<github_id>               → one account in full (admin_lists.ts)
//   GET    /admin/notices                         → every notice with its read count, newest first (admin_lists.ts)
//   GET    /admin/plans                           → the plan names, built-in and from KV, free first (admin_lists.ts)
//   GET    /admin/machines                        → every registered machine
//   POST   /admin/notices                         → a notice for one account or all, { github_id?, text, url?, expires_at? }
//   DELETE /admin/notices/<id>                    → takes it back
//   GET    /admin/cloud/requests                  → requests for cloud access, pending first
//   PUT    /admin/cloud/requests/<github_id>/grant   → grants one and the `cloud` flag, { by? }
//   PUT    /admin/cloud/requests/<github_id>/decline → declines a pending one, { by? }
//   GET    /admin/cloud/machines                  → every cloud machine, its owner and whether it runs (cloud_machines.ts)
//   POST   /admin/cloud/machines/<label>/sleep    → stops one now, after a snapshot
//   GET    /admin/log                             → what was changed and by whom, newest first (admin_log.ts)
//   GET    /admin/beta/signups                    → sign-ups for the phone app betas, pending first, ?platform= ?status=
//   PUT    /admin/beta/signups/<github_id>/<platform>/admit   → marks one admitted, { by? }
//   PUT    /admin/beta/signups/<github_id>/<platform>/decline → declines one, { by? }
//   …      /admin/operators/<github_id>/totp[/use] → the panel operators' authenticator keys (admin_totp.ts)
//
// The flag routes answer { github_id, flags } with the user's flags after the
// change; the sessions route answers { github_id, closed } with how many ended.
// Block, plan and notice delete answer 204; a new notice answers { id }. Each of them, and the sessions route, makes the
// account's machine objects recheck their sockets at once. The cloud request
// routes answer with the request (cloud_access.ts), the beta routes with the
// sign-up (beta.ts).
//
// Every route that changes something writes one row to the log, with the
// operator named by the X-Arakun-Operator-* headers (null for curl).

import { listLog, operator, record, type Operator } from "./admin_log";
import { getUser, listNotices, listPlans, listUsers } from "./admin_lists";
import { operatorTotp } from "./admin_totp";
import { decideSignup, listSignups } from "./beta";
import { decideRequest, listRequests } from "./cloud_access";
import { listAllMachines, sleepMachine } from "./cloud_machines";
import { recheckAccount } from "./machines";
import { createNotice, deleteNotice } from "./notices";
import { GITHUB_ID, bearer, error, flagList, flagsQuery, isObject, jsonBody, now, sha256, type Env } from "./util";

const FLAG = /^[a-z][a-z0-9_]{0,31}$/;
/// A plan's name: a built-in one or a key of the KV `plans`.
const PLAN = /^[a-z][a-z0-9_]{0,31}$/;
const MAX_REASON = 500;
const MAX_BODY = 4096;

export async function admin(request: Request, env: Env, url: URL): Promise<Response> {
  const path = url.pathname;
  if (!(await authorized(request, env))) return error(401, "unauthorized");
  const op = operator(request);
  const flag = /^\/admin\/users\/([^/]+)\/flags\/([^/]+)$/.exec(path);
  if (flag && (request.method === "PUT" || request.method === "DELETE")) return setFlag(request.method, env, op, flag[1]!, flag[2]!);
  const sessions = /^\/admin\/users\/([^/]+)\/sessions$/.exec(path);
  if (sessions && request.method === "DELETE") return closeSessions(env, op, sessions[1]!);
  const block = /^\/admin\/users\/([^/]+)\/block$/.exec(path);
  if (block && request.method === "PUT") return setBlock(request, env, op, block[1]!);
  if (block && request.method === "DELETE") return unblock(env, op, block[1]!);
  const plan = /^\/admin\/users\/([^/]+)\/plan$/.exec(path);
  if (plan && request.method === "PUT") return setPlan(request, env, op, plan[1]!);
  if (path === "/admin/users" && request.method === "GET") return listUsers(env, url);
  const user = /^\/admin\/users\/([^/]+)$/.exec(path);
  if (user && request.method === "GET") return getUser(env, user[1]!);
  if (path === "/admin/machines" && request.method === "GET") return listMachines(env);
  if (path === "/admin/notices" && request.method === "POST") return createNotice(request, env, op);
  const notice = /^\/admin\/notices\/([^/]+)$/.exec(path);
  if (notice && request.method === "DELETE") return deleteNotice(env, notice[1]!, op);
  if (path === "/admin/notices" && request.method === "GET") return listNotices(env);
  if (path === "/admin/plans" && request.method === "GET") return listPlans(env);
  if (path === "/admin/log" && request.method === "GET") return listLog(env, url);
  if (path === "/admin/cloud/requests" && request.method === "GET") return listRequests(env, url);
  const decide = /^\/admin\/cloud\/requests\/([^/]+)\/(grant|decline)$/.exec(path);
  if (decide && request.method === "PUT") return decideRequest(request, env, decide[1]!, decide[2] as "grant" | "decline", op);
  if (path === "/admin/cloud/machines" && request.method === "GET") return listAllMachines(env);
  const cloudMachine = /^\/admin\/cloud\/machines\/([a-z2-7]{16})\/sleep$/.exec(path);
  if (cloudMachine && request.method === "POST") return sleepMachine(env, op, cloudMachine[1]!);
  if (path === "/admin/beta/signups" && request.method === "GET") return listSignups(env, url);
  const signup = /^\/admin\/beta\/signups\/([^/]+)\/([^/]+)\/(admit|decline)$/.exec(path);
  if (signup && request.method === "PUT") return decideSignup(request, env, signup[1]!, signup[2]!, signup[3] as "admit" | "decline", op);
  const totp = /^\/admin\/operators\/([^/]+)\/totp(\/use)?$/.exec(path);
  if (totp) return (await operatorTotp(request, env, op, totp[1]!, !!totp[2])) ?? error(404, "not found");
  return error(404, "not found");
}

/// The GitHub ID in a path, or null.
function githubId(text: string): number | null {
  const id = GITHUB_ID.test(text) ? Number(text) : NaN;
  return Number.isSafeInteger(id) ? id : null;
}

async function setFlag(method: "PUT" | "DELETE", env: Env, op: Operator, idText: string, flag: string): Promise<Response> {
  const id = githubId(idText);
  if (id === null) return error(400, "the GitHub ID must be a positive integer");
  if (!FLAG.test(flag)) return error(400, "a flag is a lower-case letter, then up to 31 of a-z, 0-9 and _");

  const db = env.DB;
  const t = now();
  const results =
    method === "PUT"
      ? await db.batch([
          // A user can get a flag before signing in: the login and
          // last_seen_at are filled in on the first sign-in.
          db
            .prepare("INSERT INTO users (github_id, login, created_at, last_seen_at) VALUES (?, '', ?, 0) ON CONFLICT DO NOTHING")
            .bind(id, t),
          db.prepare("INSERT INTO user_flags (github_id, flag, granted_at) VALUES (?, ?, ?) ON CONFLICT DO NOTHING").bind(id, flag, t),
          flagsQuery(db, id),
        ])
      : await db.batch([db.prepare("DELETE FROM user_flags WHERE github_id = ? AND flag = ?").bind(id, flag), flagsQuery(db, id)]);
  await record(env, op, method === "PUT" ? "flag.grant" : "flag.revoke", id, { flag });
  return Response.json({ github_id: id, flags: flagList(results[results.length - 1]!) });
}

/// Ends every session of the user, as when a session token may have leaked.
/// The user can sign in again; take their flags away to keep them out.
async function closeSessions(env: Env, op: Operator, idText: string): Promise<Response> {
  const id = githubId(idText);
  if (id === null) return error(400, "the GitHub ID must be a positive integer");
  const result = await env.DB.prepare("DELETE FROM sessions WHERE github_id = ?").bind(id).run();
  await record(env, op, "sessions.close", id, { closed: result.meta.changes });
  await recheckAccount(env, id);
  return Response.json({ github_id: id, closed: result.meta.changes });
}

/// Keeps the account off the relay and out of Teams: its relay sockets close
/// now and new ones are refused, the teams API answers it 403, and a channel
/// closes its socket at the next message or presence check, as for a lost
/// flag. It can still sign in; `GET /machines` answers it 403 with
/// `{ error: "blocked", reason }`, which is how it learns why.
async function setBlock(request: Request, env: Env, op: Operator, idText: string): Promise<Response> {
  const id = githubId(idText);
  if (id === null) return error(400, "the GitHub ID must be a positive integer");
  const body = request.headers.get("Content-Length") === "0" || !request.body ? {} : await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, 'expected {"reason": "..."} or no body');
  const reason = body["reason"];
  if (reason !== undefined && (typeof reason !== "string" || reason.length > MAX_REASON)) {
    return error(400, `reason must be text of at most ${MAX_REASON} characters`);
  }
  // An account can be blocked before it first signs in, as it can get a flag.
  await env.DB.prepare(
    `INSERT INTO users (github_id, login, created_at, last_seen_at, blocked_at, blocked_reason) VALUES (?1, '', ?2, 0, ?2, ?3)
     ON CONFLICT (github_id) DO UPDATE SET blocked_at = excluded.blocked_at, blocked_reason = excluded.blocked_reason`,
  )
    .bind(id, now(), reason ?? null)
    .run();
  await record(env, op, "block", id, { reason: reason ?? null });
  await recheckAccount(env, id);
  return new Response(null, { status: 204 });
}

async function unblock(env: Env, op: Operator, idText: string): Promise<Response> {
  const id = githubId(idText);
  if (id === null) return error(400, "the GitHub ID must be a positive integer");
  await env.DB.prepare("UPDATE users SET blocked_at = NULL, blocked_reason = NULL WHERE github_id = ?").bind(id).run();
  await record(env, op, "unblock", id);
  return new Response(null, { status: 204 });
}

/// The plan names a built-in plan or a key of the KV `plans`; any other name
/// is the free plan. Open sockets go by the new plan after the recheck.
async function setPlan(request: Request, env: Env, op: Operator, idText: string): Promise<Response> {
  const id = githubId(idText);
  if (id === null) return error(400, "the GitHub ID must be a positive integer");
  const body = await jsonBody(request, MAX_BODY);
  const plan = isObject(body) ? body["plan"] : undefined;
  if (typeof plan !== "string" || !PLAN.test(plan)) return error(400, "a plan is a lower-case letter, then up to 31 of a-z, 0-9 and _");
  await env.DB.prepare(
    `INSERT INTO users (github_id, login, created_at, last_seen_at, plan) VALUES (?1, '', ?2, 0, ?3)
     ON CONFLICT (github_id) DO UPDATE SET plan = excluded.plan`,
  )
    .bind(id, now(), plan)
    .run();
  await record(env, op, "plan.set", id, { plan });
  await recheckAccount(env, id);
  return new Response(null, { status: 204 });
}

/// Every registered machine, the one seen last first: what the operator
/// looks at before blocking or changing a plan.
async function listMachines(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    `SELECT m.label, m.github_id, u.login, m.name, m.kind, m.created_at, m.last_seen_at
     FROM machines m JOIN users u ON u.github_id = m.github_id
     ORDER BY m.last_seen_at DESC, m.label`,
  ).all();
  return Response.json(rows.results);
}

/// `Authorization: Bearer <ADMIN_SECRET>`. Both sides are hashed first, so the
/// constant-time compare sees equal lengths and the secret's length does not
/// leak either. No secret configured means no admin at all.
async function authorized(request: Request, env: Env): Promise<boolean> {
  const secret = env.ADMIN_SECRET;
  const given = bearer(request);
  if (!secret || !given) return false;
  const [a, b] = await Promise.all([sha256(secret), sha256(given)]);
  return crypto.subtle.timingSafeEqual(a, b);
}
