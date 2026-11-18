// The operator's door, with no accounts to log into: a bearer secret set with
// `wrangler secret put ADMIN_SECRET`.
//
//   PUT    /admin/users/<github_id>/flags/<flag>  → grants the flag
//   DELETE /admin/users/<github_id>/flags/<flag>  → takes it away
//
// Both answer { github_id, flags } with the user's flags after the change.

import { error, flagList, flagsQuery, now, sha256, type Env } from "./util";

const FLAG = /^[a-z][a-z0-9_]{0,31}$/;
/// A positive integer that fits in a JavaScript number without rounding.
const GITHUB_ID = /^[1-9][0-9]{0,15}$/;

export async function admin(request: Request, env: Env, path: string): Promise<Response> {
  if (!(await authorized(request, env))) return error(401, "unauthorized");
  const match = /^\/admin\/users\/([^/]+)\/flags\/([^/]+)$/.exec(path);
  if (!match || (request.method !== "PUT" && request.method !== "DELETE")) return error(404, "not found");
  const id = GITHUB_ID.test(match[1]!) ? Number(match[1]) : NaN;
  if (!Number.isSafeInteger(id)) return error(400, "the GitHub ID must be a positive integer");
  const flag = match[2]!;
  if (!FLAG.test(flag)) return error(400, "a flag is a lower-case letter, then up to 31 of a-z, 0-9 and _");

  const db = env.DB;
  const t = now();
  const results =
    request.method === "PUT"
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

/// `Authorization: Bearer <ADMIN_SECRET>`. Both sides are hashed first, so the
/// constant-time compare sees equal lengths and the secret's length does not
/// leak either. No secret configured means no admin at all.
async function authorized(request: Request, env: Env): Promise<boolean> {
  const secret = env.ADMIN_SECRET;
  const given = /^Bearer +(.+)$/i.exec(request.headers.get("Authorization") ?? "")?.[1];
  if (!secret || !given) return false;
  const [a, b] = await Promise.all([sha256(secret), sha256(given)]);
  return crypto.subtle.timingSafeEqual(a, b);
}
