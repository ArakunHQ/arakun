// The operator's door, with no accounts to log into: a bearer secret set with
// `wrangler secret put ADMIN_SECRET`.
//
//   PUT    /admin/users/<github_id>/flags/<flag>  → grants the flag
//   DELETE /admin/users/<github_id>/flags/<flag>  → takes it away
//   DELETE /admin/users/<github_id>/sessions      → signs the user out everywhere
//
// The flag routes answer { github_id, flags } with the user's flags after the
// change; the sessions route answers { github_id, closed } with how many ended.

import { GITHUB_ID, bearer, error, flagList, flagsQuery, now, sha256, type Env } from "./util";

const FLAG = /^[a-z][a-z0-9_]{0,31}$/;

export async function admin(request: Request, env: Env, path: string): Promise<Response> {
  if (!(await authorized(request, env))) return error(401, "unauthorized");
  const flag = /^\/admin\/users\/([^/]+)\/flags\/([^/]+)$/.exec(path);
  if (flag && (request.method === "PUT" || request.method === "DELETE")) return setFlag(request.method, env, flag[1]!, flag[2]!);
  const sessions = /^\/admin\/users\/([^/]+)\/sessions$/.exec(path);
  if (sessions && request.method === "DELETE") return closeSessions(env, sessions[1]!);
  return error(404, "not found");
}

/// The GitHub ID in a path, or null.
function githubId(text: string): number | null {
  const id = GITHUB_ID.test(text) ? Number(text) : NaN;
  return Number.isSafeInteger(id) ? id : null;
}

async function setFlag(method: "PUT" | "DELETE", env: Env, idText: string, flag: string): Promise<Response> {
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
  return Response.json({ github_id: id, flags: flagList(results[results.length - 1]!) });
}

/// Ends every session of the user, as when a session token may have leaked.
/// The user can sign in again; take their flags away to keep them out.
async function closeSessions(env: Env, idText: string): Promise<Response> {
  const id = githubId(idText);
  if (id === null) return error(400, "the GitHub ID must be a positive integer");
  const result = await env.DB.prepare("DELETE FROM sessions WHERE github_id = ?").bind(id).run();
  return Response.json({ github_id: id, closed: result.meta.changes });
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
