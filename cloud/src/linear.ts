// Linear delegation: Linear hands an issue to Arakun and the Worker passes it
// to the user's computer. Every path is under /linear.
//
//   GET /linear/link      → the caller's links: [{ organization_id, linear_user_id, installed }]
//   POST /linear/link     → proves a Linear user with the user's own token
//   DELETE /linear/link/<organization_id>
//   PUT /linear/routes    → what a computer takes: Linear teams and projects
//   GET /linear/install   → a redirect to Linear's consent page
//   GET /linear/callback  → where Linear sends the admin back to
//   POST /linear/webhook  → Linear's events (linear_webhook.ts)
//
// Without all four LINEAR_* secrets every /linear path is a 404. All routes
// but the webhook and the OAuth callback (which Linear calls) need a session
// with the `linear` flag.

import { cookie, session, type Session } from "./auth";
import { exchangeCode, LinearRefused, saveWorkspace, viewer } from "./linear_api";
import { webhook } from "./linear_webhook";
import { b64url, error, isObject, jsonBody, now, sha256, type Env } from "./util";

export const FLAG = "linear";

/// The ids Linear uses are UUIDs.
export const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/// Whether the app is set up: a Worker without its secrets does not pretend
/// to take part.
export function configured(env: Env): boolean {
  return Boolean(env.LINEAR_CLIENT_ID && env.LINEAR_CLIENT_SECRET && env.LINEAR_WEBHOOK_SECRET && env.LINEAR_TOKEN_KEY);
}

/// The session, or the 401 or 403 to send.
export async function signedIn(request: Request, env: Env): Promise<Session | Response> {
  const s = await session(request, env, { flag: FLAG });
  if (!s) return error(401, "not signed in");
  if (s.blocked || !s.flagged) return error(403, "Linear is not available for this account");
  return s;
}

/// The answer for a /linear route, or null when the path is not one.
export async function linear(request: Request, env: Env, url: URL): Promise<Response | null> {
  const { pathname } = url;
  if (pathname !== "/linear" && !pathname.startsWith("/linear/")) return null;
  if (!configured(env)) return error(404, "not found");
  const key = `${request.method} ${pathname}`;
  // Linear's own calls carry no session: the webhook's proof is its
  // signature. The callback is a browser's, and does its own check of
  // session and flag.
  if (key === "POST /linear/webhook") return webhook(request, env);
  if (key === "GET /linear/callback") return callback(request, env, url);
  const who = await signedIn(request, env);
  if (who instanceof Response) return who;
  switch (key) {
    case "GET /linear/link":
      return listLinks(env, who);
    case "POST /linear/link":
      return addLink(request, env, who);
    case "PUT /linear/routes":
      return putRoutes(request, env, who);
    case "GET /linear/install":
      return install(env, url, who);
    default: {
      const unlink = request.method === "DELETE" ? /^\/linear\/link\/([^/]+)$/.exec(pathname) : null;
      if (unlink && UUID.test(unlink[1]!)) return removeLink(env, who, unlink[1]!);
      return error(404, "not found");
    }
  }
}

/// `__Host-`, so a sibling host cannot plant a state of its own.
const STATE_COOKIE = "__Host-arakun_linear_state";
const STATE_SECONDS = 600;
const MAX_CODE = 512;
const MAX_NAME = 100;

const callbackUri = (url: URL) => `https://${url.host}/linear/callback`;

function install(env: Env, url: URL, who: Session): Response {
  const state = b64url(crypto.getRandomValues(new Uint8Array(16)));
  const to = new URL("https://linear.app/oauth/authorize");
  to.searchParams.set("client_id", env.LINEAR_CLIENT_ID!);
  to.searchParams.set("redirect_uri", callbackUri(url));
  to.searchParams.set("response_type", "code");
  to.searchParams.set("scope", "read,write,app:assignable,app:mentionable");
  to.searchParams.set("actor", "app");
  to.searchParams.set("state", state);
  const res = new Response(null, { status: 302, headers: { Location: to.toString() } });
  // Lax, so the cookie comes back with Linear's redirect, a top-level GET.
  res.headers.append("Set-Cookie", `${STATE_COOKIE}=${state}.${who.githubId}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${STATE_SECONDS}`);
  return res;
}

/// Equal as SHA-256 hashes, which are all one length.
async function same(a: string, b: string): Promise<boolean> {
  const [x, y] = await Promise.all([sha256(a), sha256(b)]);
  return crypto.subtle.timingSafeEqual(x, y);
}

async function callback(request: Request, env: Env, url: URL): Promise<Response> {
  // A state is used once: every outcome clears it.
  const done = (res: Response): Response => {
    res.headers.append("Set-Cookie", `${STATE_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0`);
    return res;
  };
  const saved = cookie(request, STATE_COOKIE);
  const state = url.searchParams.get("state");
  const dot = saved?.lastIndexOf(".") ?? -1;
  if (!saved || !state || dot < 1 || !(await same(saved.slice(0, dot), state))) return done(error(400, "bad state"));
  const who = await signedIn(request, env);
  if (who instanceof Response) return done(who);
  if (saved.slice(dot + 1) !== String(who.githubId)) return done(error(400, "bad state"));
  const code = url.searchParams.get("code");
  if (url.searchParams.has("error") || !code || code.length > MAX_CODE) return done(error(400, "linear refused the install"));
  try {
    const tokens = await exchangeCode(env, code, callbackUri(url));
    const app = await viewer(tokens.access);
    await saveWorkspace(env, {
      organizationId: app.organizationId,
      name: app.organizationName,
      appUserId: app.userId,
      access: tokens.access,
      refresh: tokens.refresh,
      expiresAt: tokens.expiresAt,
      installedBy: who.githubId,
    });
    const text = `Arakun is installed in ${app.organizationName.slice(0, MAX_NAME)}. You can close this page.`;
    return done(new Response(text, { headers: { "Content-Type": "text/plain; charset=utf-8" } }));
  } catch (e) {
    if (e instanceof LinearRefused) return done(error(502, "linear refused the install"));
    throw e;
  }
}

const MAX_TOKEN = 512;
const MAX_ROUTES_BODY = 32 * 1024;
const MAX_IDS = 200;
const LABEL = /^[a-z2-7]{16}$/;

async function listLinks(env: Env, who: Session): Promise<Response> {
  const rows = await env.DB.prepare(
    `SELECT l.organization_id, l.linear_user_id, EXISTS (SELECT 1 FROM linear_workspaces w WHERE w.organization_id = l.organization_id) AS installed
     FROM linear_links l WHERE l.github_id = ? ORDER BY l.linked_at, l.organization_id`,
  )
    .bind(who.githubId)
    .all<{ organization_id: string; linear_user_id: string; installed: number }>();
  return Response.json({ links: rows.results.map((r) => ({ ...r, installed: r.installed === 1 })) });
}

/// The token is the user's own, used for this one call and then dropped.
async function addLink(request: Request, env: Env, who: Session): Promise<Response> {
  const body = await jsonBody(request, MAX_TOKEN + 64);
  const token = isObject(body) ? body["token"] : undefined;
  if (typeof token !== "string" || token.length === 0 || new TextEncoder().encode(token).length > MAX_TOKEN) return error(400, "bad token");
  let limited: boolean;
  try {
    limited = !(await env.LINEAR_LIMIT.limit({ key: String(who.githubId) })).success;
  } catch (e) {
    console.error("linear limit failed:", e);
    return error(503, "the server could not check the request: try again");
  }
  if (limited) return error(429, "too many requests");
  let seen: Awaited<ReturnType<typeof viewer>>;
  try {
    seen = await viewer(token);
  } catch (e) {
    if (e instanceof LinearRefused) return error(400, "linear refused that token");
    throw e;
  }
  // Whoever proves the Linear user holds the link. The routes of the account
  // it leaves go too: they no longer have a link to stand on.
  const [, , installed] = await env.DB.batch<{ n: number }>([
    env.DB.prepare(
      `DELETE FROM linear_routes WHERE organization_id = ?1 AND github_id IN
         (SELECT github_id FROM linear_links WHERE organization_id = ?1 AND linear_user_id = ?2 AND github_id != ?3)`,
    ).bind(seen.organizationId, seen.userId, who.githubId),
    env.DB.prepare(
      `INSERT INTO linear_links (organization_id, linear_user_id, github_id, linked_at) VALUES (?1, ?2, ?3, ?4)
       ON CONFLICT (organization_id, linear_user_id) DO UPDATE SET github_id = excluded.github_id, linked_at = excluded.linked_at`,
    ).bind(seen.organizationId, seen.userId, who.githubId, now()),
    env.DB.prepare("SELECT COUNT(*) AS n FROM linear_workspaces WHERE organization_id = ?").bind(seen.organizationId),
  ]);
  return Response.json({
    organization_id: seen.organizationId,
    linear_user_id: seen.userId,
    installed: installed!.results[0]!.n > 0,
  });
}

async function removeLink(env: Env, who: Session, organizationId: string): Promise<Response> {
  await env.DB.batch([
    env.DB.prepare("DELETE FROM linear_routes WHERE organization_id = ? AND github_id = ?").bind(organizationId, who.githubId),
    env.DB.prepare("DELETE FROM linear_links WHERE organization_id = ? AND github_id = ?").bind(organizationId, who.githubId),
  ]);
  return new Response(null, { status: 204 });
}

/// A list of at most MAX_IDS UUIDs, without repeats, or null.
function ids(v: unknown): string[] | null {
  if (!Array.isArray(v) || v.length > MAX_IDS) return null;
  if (!v.every((x) => typeof x === "string" && UUID.test(x))) return null;
  return [...new Set(v as string[])];
}

async function putRoutes(request: Request, env: Env, who: Session): Promise<Response> {
  const body = await jsonBody(request, MAX_ROUTES_BODY);
  const bad = () => error(400, "bad routes");
  if (!isObject(body)) return bad();
  const { label, organization_id: org } = body;
  const teams = ids(body["teams"]);
  const projects = ids(body["projects"]);
  if (typeof label !== "string" || typeof org !== "string" || !UUID.test(org) || !teams || !projects) return bad();
  if (teams.length + projects.length > MAX_IDS) return bad();
  if (!LABEL.test(label)) return error(404, "no such computer");
  const [machine, link] = await env.DB.batch<{ n: number }>([
    env.DB.prepare("SELECT COUNT(*) AS n FROM machines WHERE label = ? AND github_id = ?").bind(label, who.githubId),
    env.DB.prepare("SELECT COUNT(*) AS n FROM linear_links WHERE organization_id = ? AND github_id = ?").bind(org, who.githubId),
  ]);
  if (machine!.results[0]!.n === 0) return error(404, "no such computer");
  if (link!.results[0]!.n === 0) return error(409, "link linear first");
  const at = now();
  // Each insert re-checks the machine and the link, in case either went
  // between the check above and the batch.
  const insert = (kind: string, target: string) =>
    env.DB.prepare(
      `INSERT INTO linear_routes (label, github_id, organization_id, kind, target_id, updated_at)
       SELECT ?1, ?2, ?3, ?4, ?5, ?6
       WHERE EXISTS (SELECT 1 FROM machines WHERE label = ?1 AND github_id = ?2)
         AND EXISTS (SELECT 1 FROM linear_links WHERE organization_id = ?3 AND github_id = ?2)`,
    ).bind(label, who.githubId, org, kind, target, at);
  await env.DB.batch([
    env.DB.prepare("DELETE FROM linear_routes WHERE label = ? AND organization_id = ?").bind(label, org),
    ...teams.map((t) => insert("team", t)),
    ...projects.map((p) => insert("project", p)),
  ]);
  return new Response(null, { status: 204 });
}

/// The computer that takes an issue: a project row beats a team row, and of
/// equals the computer seen last. Only a machine the account still owns.
export async function routeFor(
  env: Env,
  githubId: number,
  organizationId: string,
  place: { teamId: string; projectId: string | null },
): Promise<{ label: string; machine: string } | null> {
  const row = await env.DB.prepare(
    `SELECT r.label AS label, m.name AS machine FROM linear_routes r
     JOIN machines m ON m.label = r.label AND m.github_id = r.github_id
     WHERE r.github_id = ?1 AND r.organization_id = ?2
       AND ((r.kind = 'team' AND r.target_id = ?3) OR (r.kind = 'project' AND r.target_id = ?4))
     ORDER BY (r.kind = 'project') DESC, m.last_seen_at DESC, r.label
     LIMIT 1`,
  )
    .bind(githubId, organizationId, place.teamId, place.projectId)
    .first<{ label: string; machine: string }>();
  return row;
}
