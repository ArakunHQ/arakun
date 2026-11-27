// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The operator's door, with no accounts to log into: a bearer secret set with
// `wrangler secret put ADMIN_SECRET`.
//
//   PUT    /admin/users/<account_id>/flags/<flag>  → grants the flag
//   DELETE /admin/users/<account_id>/flags/<flag>  → takes it away
//   DELETE /admin/users/<account_id>/sessions      → signs the user out everywhere
//   PUT    /admin/users/<account_id>/block         → blocks the account, { reason? }
//   DELETE /admin/users/<account_id>/block         → lets it back
//   PUT    /admin/users/<account_id>/plan          → puts it on a plan, { plan }
//   GET    /admin/users                           → accounts, last seen first (admin_lists.ts)
//   GET    /admin/users/<account_id>               → one account in full (admin_lists.ts)
//   PUT    /admin/github/<github_id>              → the account of a GitHub user, made if it has none: { account_id, github_id, created }
//   GET    /admin/notices                         → every notice with its read count, newest first (admin_lists.ts)
//   GET    /admin/plans                           → the plan names, built-in and from KV, free first (admin_lists.ts)
//   GET    /admin/machines                        → every registered machine
//   POST   /admin/notices                         → a notice for one account or all, { account_id?, text, url?, expires_at? }
//   DELETE /admin/notices/<id>                    → takes it back
//   GET    /admin/cloud/requests                  → requests for cloud access, pending first
//   PUT    /admin/cloud/requests/<account_id>/grant   → grants one and the `cloud` flag, { by? }
//   PUT    /admin/cloud/requests/<account_id>/decline → declines a pending one, { by? }
//   GET    /admin/cloud/machines                  → every cloud machine, its owner and whether it runs (cloud_machines.ts)
//   POST   /admin/cloud/machines/<label>/sleep    → stops one now, after a snapshot
//   PUT    /admin/cloud/stop                      → stops every cloud machine until DELETE, { reason? } (cloud_machines.ts)
//   DELETE /admin/cloud/stop                      → lets them run again
//   PUT    /admin/users/<account_id>/cloud/stop   → stops one account's cloud machines until DELETE, { reason? }
//   DELETE /admin/users/<account_id>/cloud/stop   → lets them run again
//   GET    /admin/log                             → what was changed and by whom, newest first (admin_log.ts)
//   GET    /admin/beta/signups                    → sign-ups for the phone app betas, pending first, ?platform= ?status=
//   PUT    /admin/beta/signups/<account_id>/<platform>/admit   → marks one admitted, { by? }
//   PUT    /admin/beta/signups/<account_id>/<platform>/decline → declines one, { by? }
//   …      /admin/operators/<github_id>/totp[/use] → the panel operators' authenticator keys (admin_totp.ts)
//
// Accounts are named by their account ID, not a GitHub ID. The flag, block
// and plan routes answer 404 for an ID with no account; to set one up before
// the person first signs in, make their account with PUT /admin/github first.
//
// The flag routes answer { account_id, flags } with the user's flags after the
// change; the sessions route answers { account_id, closed } with how many ended.
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
import { holdAccount, listAllMachines, releaseAccount, resumeEveryone, sleepMachine, stopEveryone } from "./cloud_machines";
import { accountFor, GITHUB_ACCOUNT } from "./auth";
import { recheckAccount } from "./machines";
import { createNotice, deleteNotice } from "./notices";
import { GITHUB_ID_OF, NUMERIC_ID, bearer, error, flagList, flagsQuery, isObject, jsonBody, now, sha256, type Env } from "./util";

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
  const github = /^\/admin\/github\/([^/]+)$/.exec(path);
  if (github && request.method === "PUT") return githubAccount(env, op, github[1]!);
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
  if (path === "/admin/cloud/stop" && request.method === "PUT") return stopEveryone(request, env, op);
  if (path === "/admin/cloud/stop" && request.method === "DELETE") return resumeEveryone(env, op);
  const hold = /^\/admin\/users\/([^/]+)\/cloud\/stop$/.exec(path);
  if (hold && (request.method === "PUT" || request.method === "DELETE")) {
    const id = numericId(hold[1]!);
    if (id === null) return error(400, NO_ID);
    return request.method === "PUT" ? holdAccount(request, env, op, id) : releaseAccount(env, op, id);
  }
  if (path === "/admin/beta/signups" && request.method === "GET") return listSignups(env, url);
  const signup = /^\/admin\/beta\/signups\/([^/]+)\/([^/]+)\/(admit|decline)$/.exec(path);
  if (signup && request.method === "PUT") return decideSignup(request, env, signup[1]!, signup[2]!, signup[3] as "admit" | "decline", op);
  const totp = /^\/admin\/operators\/([^/]+)\/totp(\/use)?$/.exec(path);
  if (totp) return (await operatorTotp(request, env, op, totp[1]!, !!totp[2])) ?? error(404, "not found");
  return error(404, "not found");
}

/// The account or GitHub ID in a path, or null.
function numericId(text: string): number | null {
  const id = NUMERIC_ID.test(text) ? Number(text) : NaN;
  return Number.isSafeInteger(id) ? id : null;
}

const NO_ID = "the account ID must be a positive integer";
const NO_ACCOUNT = "no such account";

/// Whether an account has this ID.
async function exists(env: Env, id: number): Promise<boolean> {
  return (await env.DB.prepare("SELECT 1 FROM accounts WHERE id = ?").bind(id).first()) !== null;
}

/// The account linked to a GitHub user, made when there is none, so the
/// operator can give it a flag, a plan or a block before its first sign-in. The
/// first sign-in fills in the login.
async function githubAccount(env: Env, op: Operator, idText: string): Promise<Response> {
  const githubId = numericId(idText);
  if (githubId === null) return error(400, "the GitHub ID must be a positive integer");
  const db = env.DB;
  const results = await db.batch([...accountFor(db, githubId, now()), db.prepare(`SELECT ${GITHUB_ACCOUNT} AS id`).bind(String(githubId))]);
  const created = results[0]!.meta.changes > 0;
  const accountId = (results[2]!.results[0] as { id: number }).id;
  if (created) await record(env, op, "account.create", accountId, { github_id: githubId });
  return Response.json({ account_id: accountId, github_id: githubId, created });
}

async function setFlag(method: "PUT" | "DELETE", env: Env, op: Operator, idText: string, flag: string): Promise<Response> {
  const id = numericId(idText);
  if (id === null) return error(400, NO_ID);
  if (!FLAG.test(flag)) return error(400, "a flag is a lower-case letter, then up to 31 of a-z, 0-9 and _");
  if (!(await exists(env, id))) return error(404, NO_ACCOUNT);

  const db = env.DB;
  const t = now();
  const results =
    method === "PUT"
      ? await db.batch([
          db.prepare("INSERT INTO user_flags (account_id, flag, granted_at) VALUES (?, ?, ?) ON CONFLICT DO NOTHING").bind(id, flag, t),
          flagsQuery(db, id),
        ])
      : await db.batch([db.prepare("DELETE FROM user_flags WHERE account_id = ? AND flag = ?").bind(id, flag), flagsQuery(db, id)]);
  await record(env, op, method === "PUT" ? "flag.grant" : "flag.revoke", id, { flag });
  return Response.json({ account_id: id, flags: flagList(results[results.length - 1]!) });
}

/// Ends every session of the user, as when a session token may have leaked.
/// The user can sign in again; take their flags away to keep them out.
async function closeSessions(env: Env, op: Operator, idText: string): Promise<Response> {
  const id = numericId(idText);
  if (id === null) return error(400, NO_ID);
  const result = await env.DB.prepare("DELETE FROM sessions WHERE account_id = ?").bind(id).run();
  await record(env, op, "sessions.close", id, { closed: result.meta.changes });
  await recheckAccount(env, id);
  return Response.json({ account_id: id, closed: result.meta.changes });
}

/// Keeps the account off the relay and out of Teams: its relay sockets close
/// now and new ones are refused, the teams API answers it 403, and a channel
/// closes its socket at the next message or presence check, as for a lost
/// flag. It can still sign in; `GET /machines` answers it 403 with
/// `{ error: "blocked", reason }`, which is how it learns why.
async function setBlock(request: Request, env: Env, op: Operator, idText: string): Promise<Response> {
  const id = numericId(idText);
  if (id === null) return error(400, NO_ID);
  const body = request.headers.get("Content-Length") === "0" || !request.body ? {} : await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, 'expected {"reason": "..."} or no body');
  const reason = body["reason"];
  if (reason !== undefined && (typeof reason !== "string" || reason.length > MAX_REASON)) {
    return error(400, `reason must be text of at most ${MAX_REASON} characters`);
  }
  const result = await env.DB.prepare("UPDATE accounts SET blocked_at = ?2, blocked_reason = ?3 WHERE id = ?1")
    .bind(id, now(), reason ?? null)
    .run();
  if (result.meta.changes === 0) return error(404, NO_ACCOUNT);
  await record(env, op, "block", id, { reason: reason ?? null });
  await recheckAccount(env, id);
  return new Response(null, { status: 204 });
}

async function unblock(env: Env, op: Operator, idText: string): Promise<Response> {
  const id = numericId(idText);
  if (id === null) return error(400, NO_ID);
  const result = await env.DB.prepare("UPDATE accounts SET blocked_at = NULL, blocked_reason = NULL WHERE id = ?").bind(id).run();
  if (result.meta.changes === 0) return error(404, NO_ACCOUNT);
  await record(env, op, "unblock", id);
  return new Response(null, { status: 204 });
}

/// The plan names a built-in plan or a key of the KV `plans`; any other name
/// is the free plan. Open sockets go by the new plan after the recheck.
async function setPlan(request: Request, env: Env, op: Operator, idText: string): Promise<Response> {
  const id = numericId(idText);
  if (id === null) return error(400, NO_ID);
  const body = await jsonBody(request, MAX_BODY);
  const plan = isObject(body) ? body["plan"] : undefined;
  if (typeof plan !== "string" || !PLAN.test(plan)) return error(400, "a plan is a lower-case letter, then up to 31 of a-z, 0-9 and _");
  const result = await env.DB.prepare("UPDATE accounts SET plan = ? WHERE id = ?").bind(plan, id).run();
  if (result.meta.changes === 0) return error(404, NO_ACCOUNT);
  await record(env, op, "plan.set", id, { plan });
  await recheckAccount(env, id);
  return new Response(null, { status: 204 });
}

/// Every registered machine, the one seen last first: what the operator
/// looks at before blocking or changing a plan.
async function listMachines(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    `SELECT m.label, m.account_id, ${GITHUB_ID_OF("m.account_id")} AS github_id, u.login, m.name, m.kind, m.created_at, m.last_seen_at
     FROM machines m JOIN accounts u ON u.id = m.account_id
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
