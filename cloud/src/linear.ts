// Linear delegation: Linear hands an issue to Arakun and the Worker passes it
// to the user's computer. Every path is under /linear.
//
//   GET /linear/link      → the caller's links: [{ organization_id, linear_user_id, installed }],
//                           `installed` when the app is in the workspace and
//                           Linear still takes its token
//   POST /linear/link     → proves a Linear user with the user's own token
//   DELETE /linear/link/<organization_id>
//   PUT /linear/routes    → what a computer takes: Linear teams and projects
//   GET /linear/install   → a redirect to Linear's consent page
//   GET /linear/callback  → where Linear sends the admin back to
//   POST /linear/webhook  → Linear's events (linear_webhook.ts)
//   POST /linear/deliveries/<id>/accept
//                         → the computer took a delivery
//   POST /linear/sessions/<agent_session>/activity
//   POST /linear/sessions/<agent_session>/links
//   PUT /linear/sessions/<agent_session>/plan
//                         → the computer's progress, posted as Arakun. The
//                           activity that says the session started also
//                           marks the issue as being worked on. A plan
//                           replaces the session's whole checklist.
//
// Without all four LINEAR_* secrets, or with a LINEAR_TOKEN_KEY that is not 32
// bytes of base64url, every /linear path is a 404. All routes
// but the webhook and the OAuth callback (which Linear calls) need a session
// with the `linear` flag.

import { cookie, session, type Session } from "./auth";
import { addLinks, exchangeCode, LinearRefused, postAction, postActivity, saveWorkspace, setPlan, startIssue, UUID, viewer, type PlanItem } from "./linear_api";
import { keyUsable } from "./linear_seal";
import { webhook } from "./linear_webhook";
import { dropDelivery } from "./machines";
import { b64url, error, isObject, jsonBody, now, sha256, type Env } from "./util";

export const FLAG = "linear";
export { UUID };

/// Whether the app is set up: a Worker without its secrets, or with a token
/// key that cannot seal, does not pretend to take part.
export function configured(env: Env): boolean {
  return Boolean(env.LINEAR_CLIENT_ID && env.LINEAR_CLIENT_SECRET && env.LINEAR_WEBHOOK_SECRET && keyUsable(env));
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
      const taken = request.method === "POST" ? /^\/linear\/deliveries\/([^/]+)\/accept$/.exec(pathname) : null;
      if (taken) return accept(env, who, taken[1]!);
      const said = request.method === "POST" ? /^\/linear\/sessions\/([^/]+)\/activity$/.exec(pathname) : null;
      if (said) return sessionActivity(request, env, who, said[1]!);
      const linked = request.method === "POST" ? /^\/linear\/sessions\/([^/]+)\/links$/.exec(pathname) : null;
      if (linked) return sessionLinks(request, env, who, linked[1]!);
      const planned = request.method === "PUT" ? /^\/linear\/sessions\/([^/]+)\/plan$/.exec(pathname) : null;
      if (planned) return sessionPlan(request, env, who, planned[1]!);
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
  res.headers.append("Set-Cookie", `${STATE_COOKIE}=${state}.${who.accountId}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${STATE_SECONDS}`);
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
  if (saved.slice(dot + 1) !== String(who.accountId)) return done(error(400, "bad state"));
  const code = url.searchParams.get("code");
  if (url.searchParams.has("error") || !code || code.length > MAX_CODE) return done(error(400, "linear refused the install"));
  try {
    const tokens = await exchangeCode(env, code, callbackUri(url));
    const app = await viewer(tokens.access);
    // A member who authorized without actor=app gets a token of their own.
    // Saved, it would replace the app's and break delegation in the workspace.
    if (!app.app) return done(error(400, "that was not an app install"));
    await saveWorkspace(env, {
      organizationId: app.organizationId,
      name: app.organizationName,
      appUserId: app.userId,
      access: tokens.access,
      refresh: tokens.refresh,
      expiresAt: tokens.expiresAt,
      installedBy: who.accountId,
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
    `SELECT l.organization_id, l.linear_user_id, EXISTS (SELECT 1 FROM linear_workspaces w WHERE w.organization_id = l.organization_id AND w.refused_at IS NULL) AS installed
     FROM linear_links l WHERE l.account_id = ? ORDER BY l.linked_at, l.organization_id`,
  )
    .bind(who.accountId)
    .all<{ organization_id: string; linear_user_id: string; installed: number }>();
  return Response.json({ links: rows.results.map((r) => ({ ...r, installed: r.installed === 1 })) });
}

/// The token is the user's own, used for this one call and then dropped.
async function addLink(request: Request, env: Env, who: Session): Promise<Response> {
  const body = await jsonBody(request, MAX_TOKEN + 64);
  const token = isObject(body) ? body["token"] : undefined;
  if (typeof token !== "string" || token.length === 0 || new TextEncoder().encode(token).length > MAX_TOKEN) return error(400, "bad token");
  const refused = await overLimit(env, who);
  if (refused) return refused;
  let seen: Awaited<ReturnType<typeof viewer>>;
  try {
    seen = await viewer(token);
  } catch (e) {
    if (e instanceof LinearRefused) return error(400, "linear refused that token");
    throw e;
  }
  // A link stands for a person who delegates. An app's token names the app,
  // which never does.
  if (seen.app) return error(400, "linear refused that token");
  // Whoever proves the Linear user holds the link. The routes of the account
  // it leaves go too: they no longer have a link to stand on.
  const [, , installed] = await env.DB.batch<{ n: number }>([
    env.DB.prepare(
      `DELETE FROM linear_routes WHERE organization_id = ?1 AND account_id IN
         (SELECT account_id FROM linear_links WHERE organization_id = ?1 AND linear_user_id = ?2 AND account_id != ?3)`,
    ).bind(seen.organizationId, seen.userId, who.accountId),
    env.DB.prepare(
      `INSERT INTO linear_links (organization_id, linear_user_id, account_id, linked_at) VALUES (?1, ?2, ?3, ?4)
       ON CONFLICT (organization_id, linear_user_id) DO UPDATE SET account_id = excluded.account_id, linked_at = excluded.linked_at`,
    ).bind(seen.organizationId, seen.userId, who.accountId, now()),
    env.DB.prepare("SELECT COUNT(*) AS n FROM linear_workspaces WHERE organization_id = ? AND refused_at IS NULL").bind(seen.organizationId),
  ]);
  return Response.json({
    organization_id: seen.organizationId,
    linear_user_id: seen.userId,
    installed: installed!.results[0]!.n > 0,
  });
}

async function removeLink(env: Env, who: Session, organizationId: string): Promise<Response> {
  await env.DB.batch([
    env.DB.prepare("DELETE FROM linear_routes WHERE organization_id = ? AND account_id = ?").bind(organizationId, who.accountId),
    env.DB.prepare("DELETE FROM linear_links WHERE organization_id = ? AND account_id = ?").bind(organizationId, who.accountId),
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
    env.DB.prepare("SELECT COUNT(*) AS n FROM machines WHERE label = ? AND account_id = ?").bind(label, who.accountId),
    env.DB.prepare("SELECT COUNT(*) AS n FROM linear_links WHERE organization_id = ? AND account_id = ?").bind(org, who.accountId),
  ]);
  if (machine!.results[0]!.n === 0) return error(404, "no such computer");
  if (link!.results[0]!.n === 0) return error(409, "link linear first");
  const at = now();
  // Each insert re-checks the machine and the link, in case either went
  // between the check above and the batch.
  const insert = (kind: string, target: string) =>
    env.DB.prepare(
      `INSERT INTO linear_routes (label, account_id, organization_id, kind, target_id, updated_at)
       SELECT ?1, ?2, ?3, ?4, ?5, ?6
       WHERE EXISTS (SELECT 1 FROM machines WHERE label = ?1 AND account_id = ?2)
         AND EXISTS (SELECT 1 FROM linear_links WHERE organization_id = ?3 AND account_id = ?2)`,
    ).bind(label, who.accountId, org, kind, target, at);
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
  accountId: number,
  organizationId: string,
  place: { teamId: string; projectId: string | null },
): Promise<{ label: string; machine: string } | null> {
  const row = await env.DB.prepare(
    `SELECT r.label AS label, m.name AS machine FROM linear_routes r
     JOIN machines m ON m.label = r.label AND m.account_id = r.account_id
     WHERE r.account_id = ?1 AND r.organization_id = ?2
       AND ((r.kind = 'team' AND r.target_id = ?3) OR (r.kind = 'project' AND r.target_id = ?4))
     ORDER BY (r.kind = 'project') DESC, m.last_seen_at DESC, r.label
     LIMIT 1`,
  )
    .bind(accountId, organizationId, place.teamId, place.projectId)
    .first<{ label: string; machine: string }>();
  return row;
}

/// The 429 or 503 to send when the account has asked Linear too often, or
/// null to go on. Counted per account, as everything here asks Linear.
async function overLimit(env: Env, who: Session): Promise<Response | null> {
  let limited: boolean;
  try {
    limited = !(await env.LINEAR_LIMIT.limit({ key: String(who.accountId) })).success;
  } catch (e) {
    console.error("linear limit failed:", e);
    return error(503, "the server could not check the request: try again");
  }
  return limited ? error(429, "too many requests") : null;
}

const DELIVERY_ID = /^[0-9a-f]{32}$/;

/// The computer took a delivery: the relay need not keep it any longer.
async function accept(env: Env, who: Session, id: string): Promise<Response> {
  if (!DELIVERY_ID.test(id)) return error(404, "no such delivery");
  // Owner and id in one statement, so another account's id looks unknown.
  const row = await env.DB.prepare(
    "UPDATE linear_deliveries SET accepted_at = COALESCE(accepted_at, ?) WHERE id = ? AND account_id = ? RETURNING label",
  )
    .bind(now(), id, who.accountId)
    .first<{ label: string }>();
  if (!row) return error(404, "no such delivery");
  // Every time: a first accept may have failed after the update.
  await dropDelivery(env, row.label, id);
  return new Response(null, { status: 204 });
}

const ACTIVITY_TYPES = ["thought", "elicitation", "response", "error"] as const;
const MAX_ACTIVITY_BODY = 64 * 1024;
const MAX_ACTIVITY_TEXT = 8000;
const MAX_LINKS = 5;
const MAX_LINK_LABEL = 80;
const MAX_URL = 2048;
const MAX_OPTIONS = 10;
const MAX_OPTION = 80;

/// Length in characters, not UTF-16 units, so an emoji counts once.
const chars = (s: string) => [...s].length;

interface Activity {
  type: (typeof ACTIVITY_TYPES)[number];
  body: string;
  /// Linear replaces it with the next activity: what a session does right now.
  ephemeral: boolean;
  links: { label: string; url: string }[];
  /// The choices of a question, for Linear to show as ones to pick from.
  options: string[];
  /// The session started on the computer: the thought that says so.
  started: boolean;
}

/// A step the session works on, as Linear shows a tool call: what it does
/// (`Editing`) and on what (`src/main.rs`, or `a command`).
interface Step {
  type: "action";
  action: string;
  parameter: string;
  ephemeral: boolean;
}

const MAX_ACTION = 80;
const MAX_PARAMETER = 200;

/// A step, checked, or null. It carries no body, links or choices.
function parseStep(v: Record<string, unknown>): Step | null {
  const { type, action, parameter, ephemeral = false, ...rest } = v;
  if (type !== "action" || Object.keys(rest).length > 0 || typeof ephemeral !== "boolean") return null;
  const text = (s: unknown, max: number) => typeof s === "string" && s.trim().length > 0 && chars(s) <= max && !/[\p{Cc}]/u.test(s);
  if (!text(action, MAX_ACTION) || !text(parameter, MAX_PARAMETER)) return null;
  return { type, action: action as string, parameter: parameter as string, ephemeral };
}

/// The activity, checked, or null. Only the known fields are copied out.
function parseActivity(v: unknown): Activity | Step | null {
  if (!isObject(v)) return null;
  if (v["type"] === "action") return parseStep(v);
  const { type, body, ephemeral = false, links = [], options = [], started = false } = v;
  if (!ACTIVITY_TYPES.includes(type as Activity["type"])) return null;
  // Linear takes it for a thought or a step only.
  if (typeof ephemeral !== "boolean" || (ephemeral && type !== "thought")) return null;
  if (typeof started !== "boolean" || (started && (type !== "thought" || ephemeral))) return null;
  if (typeof body !== "string" || body.length === 0 || chars(body) > MAX_ACTIVITY_TEXT) return null;
  const out = parseLinks(links);
  if (!out) return null;
  // Only a question has choices, and one choice is none.
  if (!Array.isArray(options) || options.length > MAX_OPTIONS || options.length === 1) return null;
  if (options.length > 0 && type !== "elicitation") return null;
  if (!options.every((o) => typeof o === "string" && o.trim().length > 0 && chars(o) <= MAX_OPTION)) return null;
  return { type: type as Activity["type"], body, ephemeral, links: out, options: options as string[], started };
}

/// At most MAX_LINKS links, each a label and a plain https address, or null.
function parseLinks(links: unknown): Activity["links"] | null {
  if (!Array.isArray(links) || links.length > MAX_LINKS) return null;
  const out: Activity["links"] = [];
  for (const l of links) {
    if (!isObject(l)) return null;
    const { label, url } = l;
    if (typeof label !== "string" || label.length === 0 || chars(label) > MAX_LINK_LABEL) return null;
    if (typeof url !== "string" || !url.startsWith("https://") || /\s/.test(url) || url.length > MAX_URL) return null;
    out.push({ label, url });
  }
  return out;
}

/// The workspace and issue of a session the caller was given. They come from
/// the delivery, not the caller.
async function ownedSession(env: Env, who: Session, agentSessionId: string): Promise<{ organizationId: string; issueId: string } | null> {
  const owned = await env.DB.prepare(
    `SELECT organization_id, issue_id FROM linear_deliveries WHERE agent_session_id = ? AND account_id = ? AND kind = 'created'
     ORDER BY created_at DESC LIMIT 1`,
  )
    .bind(agentSessionId, who.accountId)
    .first<{ organization_id: string; issue_id: string }>();
  return owned ? { organizationId: owned.organization_id, issueId: owned.issue_id } : null;
}

/// The computer's progress in a session it was given, posted as Arakun. The
/// text goes to Linear and nowhere else.
async function sessionActivity(request: Request, env: Env, who: Session, agentSessionId: string): Promise<Response> {
  if (!UUID.test(agentSessionId)) return error(404, "no such session");
  const activity = parseActivity(await jsonBody(request, MAX_ACTIVITY_BODY));
  if (!activity) return error(400, "bad activity");
  const refused = await overLimit(env, who);
  if (refused) return refused;
  const owned = await ownedSession(env, who, agentSessionId);
  if (!owned) return error(404, "no such session");
  const { organizationId, issueId } = owned;
  try {
    if (activity.type === "action") {
      await postAction(env, organizationId, agentSessionId, activity.action, activity.parameter, activity.ephemeral);
    } else {
      await postActivity(env, organizationId, agentSessionId, activity.type, activity.body, activity.ephemeral, activity.options);
      if (activity.links.length > 0) await addLinks(env, organizationId, agentSessionId, activity.links);
    }
  } catch (e) {
    if (e instanceof LinearRefused) return error(502, "linear refused");
    throw e;
  }
  if (activity.type !== "action" && activity.started) await markStarted(env, organizationId, issueId);
  return new Response(null, { status: 204 });
}

/// The issue as being worked on, after the activity that says the session
/// started. Only after it, so an offline computer or a refusal leaves the
/// issue as it was. A failure here is logged and goes no further: the
/// activity is in, and a retry would post it twice.
async function markStarted(env: Env, organizationId: string, issueId: string): Promise<void> {
  try {
    await startIssue(env, organizationId, issueId);
  } catch (e) {
    // The status or the error's name only: a message could quote Linear.
    const why = e instanceof LinearRefused ? e.status : e instanceof Error ? e.name : "unknown";
    console.error(`linear start issue: ${why}`);
  }
}

/// Links for a session the computer was given, such as its pull request,
/// without a new activity: a link can turn up at any time, also after the
/// last reply.
async function sessionLinks(request: Request, env: Env, who: Session, agentSessionId: string): Promise<Response> {
  if (!UUID.test(agentSessionId)) return error(404, "no such session");
  const body = await jsonBody(request, MAX_ACTIVITY_BODY);
  const links = isObject(body) ? parseLinks(body["links"]) : null;
  if (!links || links.length === 0) return error(400, "bad links");
  const refused = await overLimit(env, who);
  if (refused) return refused;
  const owned = await ownedSession(env, who, agentSessionId);
  if (!owned) return error(404, "no such session");
  const { organizationId } = owned;
  try {
    await addLinks(env, organizationId, agentSessionId, links);
  } catch (e) {
    if (e instanceof LinearRefused) return error(502, "linear refused");
    throw e;
  }
  return new Response(null, { status: 204 });
}

const PLAN_STATUSES = ["pending", "inProgress", "completed", "canceled"] as const;
const MAX_PLAN_ITEMS = 50;
const MAX_PLAN_ITEM = 300;
const MAX_PLAN_BODY = 128 * 1024;

/// A plan, checked, or null: at most MAX_PLAN_ITEMS items of one short line
/// and one of Linear's statuses. Only the known fields are copied out.
function parsePlan(v: unknown): PlanItem[] | null {
  if (!Array.isArray(v) || v.length > MAX_PLAN_ITEMS) return null;
  const out: PlanItem[] = [];
  for (const item of v) {
    if (!isObject(item)) return null;
    const { content, status } = item;
    if (typeof content !== "string" || content.trim().length === 0 || chars(content) > MAX_PLAN_ITEM || /[\p{Cc}]/u.test(content)) return null;
    if (!PLAN_STATUSES.includes(status as PlanItem["status"])) return null;
    out.push({ content, status: status as PlanItem["status"] });
  }
  return out;
}

/// The plan of a session the computer was given: its checklist in Linear.
/// Linear replaces the whole plan each time, so the body is all of it.
async function sessionPlan(request: Request, env: Env, who: Session, agentSessionId: string): Promise<Response> {
  if (!UUID.test(agentSessionId)) return error(404, "no such session");
  const body = await jsonBody(request, MAX_PLAN_BODY);
  const plan = isObject(body) ? parsePlan(body["plan"]) : null;
  if (!plan) return error(400, "bad plan");
  const refused = await overLimit(env, who);
  if (refused) return refused;
  const owned = await ownedSession(env, who, agentSessionId);
  if (!owned) return error(404, "no such session");
  try {
    await setPlan(env, owned.organizationId, agentSessionId, plan);
  } catch (e) {
    if (e instanceof LinearRefused) return error(502, "linear refused");
    throw e;
  }
  return new Response(null, { status: 204 });
}
