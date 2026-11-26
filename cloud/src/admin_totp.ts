// The admin panel operators' authenticator keys (migrations/0014). The admin
// Worker seals each key before it sends it, and these routes only keep it:
// they never see a key or a code.
//
//   GET    /admin/operators/<github_id>/totp      → { sealed, last_step }, 404 without one
//   POST   /admin/operators/<github_id>/totp      → keeps the first key, { sealed, step }; 409 if there is one
//   PUT    /admin/operators/<github_id>/totp      → replaces the key, { sealed, step }
//   POST   /admin/operators/<github_id>/totp/use  → takes a code's step, { step }; 409 if it is not newer than the last
//   DELETE /admin/operators/<github_id>/totp      → forgets the key: the operator sets up a new one at the next sign-in
//
// The admin Worker never forwards the browser to these paths. DELETE is the
// maintainer's, with curl, for an operator who lost their phone.

import { record, type Operator } from "./admin_log";
import { ACCOUNT_OF_GITHUB, NUMERIC_ID, error, isObject, jsonBody, now, type Env } from "./util";

const MAX_BODY = 1024;
/// base64url of the IV and the sealed key: about 70 characters.
const SEALED = /^[A-Za-z0-9_-]{16,200}$/;

export async function operatorTotp(request: Request, env: Env, op: Operator, idText: string, use: boolean): Promise<Response | null> {
  const id = NUMERIC_ID.test(idText) ? Number(idText) : NaN;
  if (!Number.isSafeInteger(id)) return error(400, "the GitHub ID must be a positive integer");
  if (use) return request.method === "POST" ? useStep(request, env, id) : null;
  switch (request.method) {
    case "GET":
      return getKey(env, id);
    case "POST":
    case "PUT":
      return putKey(request, env, op, id, request.method === "POST");
    case "DELETE":
      return forget(env, op, id);
  }
  return null;
}

async function getKey(env: Env, id: number): Promise<Response> {
  const row = await env.DB.prepare("SELECT sealed, last_step FROM operator_totp WHERE github_id = ?")
    .bind(id)
    .first<{ sealed: string; last_step: number }>();
  return row ? Response.json(row) : error(404, "no key");
}

async function putKey(request: Request, env: Env, op: Operator, id: number, first: boolean): Promise<Response> {
  const body = await jsonBody(request, MAX_BODY);
  const sealed = isObject(body) ? body["sealed"] : undefined;
  const step = isObject(body) ? body["step"] : undefined;
  if (typeof sealed !== "string" || !SEALED.test(sealed) || !validStep(step)) return error(400, 'expected {"sealed": "...", "step": n}');
  if (first) {
    // Two first sign-ins at once: the one that comes second gets 409, not
    // the other's key replaced.
    const result = await env.DB.prepare(
      "INSERT INTO operator_totp (github_id, sealed, last_step, created_at) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
      .bind(id, sealed, step, now())
      .run();
    if (result.meta.changes === 0) return error(409, "a key exists");
    await record(env, op, "operator.totp.add", await accountOf(env, id), { github_id: id });
    return new Response(null, { status: 201 });
  }
  await env.DB.prepare(
    `INSERT INTO operator_totp (github_id, sealed, last_step, created_at) VALUES (?1, ?2, ?3, ?4)
     ON CONFLICT (github_id) DO UPDATE SET sealed = excluded.sealed, last_step = excluded.last_step, created_at = excluded.created_at`,
  )
    .bind(id, sealed, step, now())
    .run();
  await record(env, op, "operator.totp.replace", await accountOf(env, id), { github_id: id });
  return new Response(null, { status: 204 });
}

/// One statement, so two requests with the same code cannot both pass.
async function useStep(request: Request, env: Env, id: number): Promise<Response> {
  const body = await jsonBody(request, MAX_BODY);
  const step = isObject(body) ? body["step"] : undefined;
  if (!validStep(step)) return error(400, 'expected {"step": n}');
  const result = await env.DB.prepare("UPDATE operator_totp SET last_step = ? WHERE github_id = ? AND last_step < ?").bind(step, id, step).run();
  return result.meta.changes === 1 ? new Response(null, { status: 204 }) : error(409, "code used");
}

async function forget(env: Env, op: Operator, id: number): Promise<Response> {
  const result = await env.DB.prepare("DELETE FROM operator_totp WHERE github_id = ?").bind(id).run();
  if (result.meta.changes > 0) await record(env, op, "operator.totp.reset", await accountOf(env, id), { github_id: id });
  return new Response(null, { status: 204 });
}

/// The operator's account, if they have one, for the log: operators are
/// GitHub users, and need no account to use the panel.
async function accountOf(env: Env, githubId: number): Promise<number | null> {
  return env.DB.prepare(`SELECT ${ACCOUNT_OF_GITHUB("?1")} AS id`).bind(githubId).first<number | null>("id");
}

function validStep(step: unknown): step is number {
  return typeof step === "number" && Number.isSafeInteger(step) && step > 0;
}
