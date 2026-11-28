// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// POST /linear/webhook: Linear tells Arakun that someone delegated an issue to
// it, mentioned it, or wrote in its session; or that it was unassigned, its
// issue closed, or its access taken away, which stops the session. It carries
// no session: the signature is its proof. Linear wants an HTTP answer within
// 5 seconds and an activity in the session within 10.
//
// The body holds the issue's text (promptContext, the title, comments). None
// of it is kept, stored, logged or put in an error: only ids are kept. Two
// texts go on to a connected computer in the live frame and nowhere else: a
// follow-up's, and the guidance a workspace or team set for agents.

import { offeredFor, routeFor, FLAG, UUID } from "./linear";
import { issuePlace, issueStateType, LinearRefused, postActivity, rankRepositories, TokenUnopened } from "./linear_api";
import { deliver, QueueFull, type Delivery, type GuidanceRule, type StopReason } from "./machines";
import { bodyText, error, GITHUB_ID_OF, hex, isObject, now, sha256, type Env } from "./util";

const MAX_BODY = 1024 * 1024;
/// How far Linear's clock may be from ours, so a captured webhook cannot be
/// replayed later.
const MAX_SKEW_MS = 60_000;
const SIGNATURE = /^[0-9a-f]{64}$/i;

const NO_CREATOR = "Arakun starts work only for a person. Delegate this issue yourself.";
const NO_ISSUE = "Arakun works on issues. Mention it on an issue instead.";
const NOT_LINKED =
  'The person who delegated this has not connected Linear in Arakun. In Arakun, open the Linear tab and turn on "Let Linear start sessions on this computer".';
const NO_GITHUB =
  "The person who delegated this has no GitHub account linked to their Arakun account, which Linear delegation needs. In Arakun, open Settings › Account and link GitHub.";
const NO_ROUTE = "No computer is set up for this team. In Arakun's Linear tab, map the team or its project to a project on your computer.";
const NO_SESSION = "Arakun has no session for this. Delegate the issue again.";
const NOT_DELEGATOR = "Only the person who delegated this can steer it.";
const ASK_PROJECT = "No computer is set up for this team yet. Which project should Arakun work in?";
const PICK_AGAIN = "Pick one of the choices, or delegate the issue again.";
const PICK_STOPPED = "Stopped. Nothing was started.";
const PICK_GONE = "That choice is no longer offered: the computer stopped taking work from Linear. Delegate the issue again.";
/// The most choices offered in one question.
export const MAX_CHOICES = 5;
/// The most characters of a computer's name in a choice: Linear shows 80.
const MAX_CHOICE_MACHINE = 30;
/// How long a question waits for its pick, as a delivery waits for its computer.
const PICK_SECONDS = 7 * 24 * 60 * 60;
/// A computer's name when its row is gone.
const THIS_COMPUTER = "this computer";
const queueFull = (machine: string) => `Too many requests are waiting for ${machine}. Try again when it has caught up.`;
const sent = (machine: string) => `Sent to ${machine}. A session starts there now.`;
const offline = (machine: string) => `${machine} is offline. The session starts when it is back, for up to 7 days.`;
const replyWaits = (machine: string) => `${machine} is offline. Your message reaches the session when it is back.`;
const stopWaits = (machine: string) => `${machine} is offline. The session there is stopped as soon as it is back.`;

/// Whether `signature` is the hex HMAC-SHA256 of `text` under `secret`. The
/// two are compared as SHA-256 hashes in constant time, as admin.ts compares
/// its secret, so the compare leaks nothing about where they differ.
export async function verified(text: string, signature: string | null, secret: string): Promise<boolean> {
  if (!signature || !SIGNATURE.test(signature)) return false;
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const mac = hex(new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(text))));
  const [a, b] = await Promise.all([sha256(mac), sha256(signature.toLowerCase())]);
  return crypto.subtle.timingSafeEqual(a, b);
}

/// The ids of a webhook, checked. Nothing with text is copied out.
interface Hook {
  organizationId: string;
  agentSessionId: string;
  creatorId: string | null;
  issueId: string | null;
  /// The comment that mentioned Arakun, when a mention started the session.
  commentId: string | null;
}

const ok = () => new Response(null, { status: 200 });

export async function webhook(request: Request, env: Env): Promise<Response> {
  const text = await bodyText(request, MAX_BODY);
  if (text === null) return error(413, "too large");
  if (!(await verified(text, request.headers.get("Linear-Signature"), env.LINEAR_WEBHOOK_SECRET!))) return error(401, "bad signature");
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    // The parser's message quotes the text, so it is dropped here.
    return error(400, "bad webhook");
  }
  if (!isObject(parsed)) return error(400, "bad webhook");
  const at = parsed["webhookTimestamp"];
  if (typeof at !== "number" || !(Math.abs(Date.now() - at) <= MAX_SKEW_MS)) return error(401, "bad signature");
  try {
    return await handle(env, request.headers.get("Linear-Event"), parsed);
  } catch (e) {
    // The status or the error's name only: a message could quote Linear.
    const why = e instanceof TokenUnopened ? e.name : e instanceof LinearRefused ? e.status : e instanceof Error ? e.name : "unknown";
    console.error(`POST /linear/webhook: ${why}`);
    // Linear sends a webhook again after a 500, in 1 minute, 1 hour and 6
    // hours, and may turn off one that keeps failing. So a 500 is only for
    // what a retry can mend: a timeout, Linear's 5xx or 429, a D1 error. The
    // retry lands on the same row. No workspace, a token Linear refuses even
    // after the refresh, or one that will not open stay so until someone
    // installs again: a 200.
    return lasting(e) ? ok() : error(500, "internal error");
  }
}

/// The event, by the signed body's `type` for the events that stop a session:
/// the header is not signed. The agent session events keep to the header,
/// as before.
async function handle(env: Env, event: string | null, parsed: Record<string, unknown>): Promise<Response> {
  const { type, action, organizationId } = parsed;
  if (type === "AppUserNotification" && (action === "issueUnassignedFromYou" || action === "issueStatusChanged")) {
    if (!isUuid(organizationId)) return error(400, "bad webhook");
    return notified(env, organizationId, action, parsed);
  }
  if (type === "PermissionChange" && action === "teamAccessChanged") {
    const removed = parsed["removedTeamIds"];
    if (!isUuid(organizationId) || !Array.isArray(removed) || removed.length > MAX_TEAMS || !removed.every(isUuid)) return error(400, "bad webhook");
    if (removed.length > 0) await stopAll(env, organizationId, "team_access", removed);
    return ok();
  }
  if (type === "OAuthApp" && action === "revoked") {
    if (!isUuid(organizationId)) return error(400, "bad webhook");
    await revoked(env, organizationId);
    return ok();
  }
  if (event !== "AgentSessionEvent" || (action !== "created" && action !== "prompted")) return ok();
  const session = parsed["agentSession"];
  if (!isObject(session) || !isUuid(organizationId) || !isUuid(session["id"])) return error(400, "bad webhook");
  const hook: Hook = {
    organizationId,
    agentSessionId: session["id"],
    creatorId: typeof session["creatorId"] === "string" && session["creatorId"] !== "" ? session["creatorId"] : null,
    issueId: isUuid(session["issueId"]) ? session["issueId"] : null,
    commentId: commentOf(session),
  };
  return action === "created" ? await delegated(env, hook, guidanceOf(parsed)) : await followUp(env, hook, parsed);
}

/// Whether a retry of the webhook would fail the same way.
function lasting(e: unknown): boolean {
  if (e instanceof TokenUnopened) return true;
  return e instanceof LinearRefused && (e.status === 401 || e.status === 403 || e.status === 404);
}

function isUuid(v: unknown): v is string {
  return typeof v === "string" && UUID.test(v);
}

/// The id of the comment that mentioned Arakun, or null when a delegation
/// started the session. Linear's schema has it as `commentId` and again as
/// `comment.id`. Only the id goes on: the computer reads the text from Linear.
function commentOf(session: Record<string, unknown>): string | null {
  const comment = session["comment"];
  const id = session["commentId"] ?? (isObject(comment) ? comment["id"] : undefined);
  return isUuid(id) ? id : null;
}

/// A follow-up's text goes to the computer only up to this length. A longer
/// one is not cut: the computer reads it from Linear by the activity id.
const MAX_PROMPT = 16_000;

/// The author of a follow-up. Linear's schema has it as `userId` and again as
/// `user.id`; anything else counts as someone else.
function authorOf(activity: Record<string, unknown>): string | null {
  const user = activity["user"];
  const id = activity["userId"] ?? (isObject(user) ? user["id"] : undefined);
  return typeof id === "string" && id !== "" ? id : null;
}

/// The follow-up's text, or null when there is none to pass on. Linear's
/// schema puts it in `content.body`; `body` is read too, in case.
function promptText(activity: Record<string, unknown>): string | null {
  const content = activity["content"];
  const text = isObject(content) && typeof content["body"] === "string" ? content["body"] : activity["body"];
  return typeof text === "string" && text.length <= MAX_PROMPT ? text : null;
}

/// The most characters of guidance passed on, all rules together. More is
/// not cut, as a cut rule could say the opposite: none is passed.
export const MAX_GUIDANCE = 16_000;

/// The guidance a workspace or team set for agents, from a `created`
/// webhook: each rule's text and the team that set it (null for the
/// workspace). Null when there is none, or too much to pass on.
export function guidanceOf(body: Record<string, unknown>): GuidanceRule[] | null {
  const list = body["guidance"];
  if (!Array.isArray(list)) return null;
  const rules: GuidanceRule[] = [];
  let size = 0;
  for (const r of list) {
    if (!isObject(r) || typeof r["body"] !== "string" || r["body"].trim() === "") continue;
    const origin = r["origin"];
    const team = isObject(origin) && origin["type"] === "Team" && isObject(origin["team"]) ? teamName(origin["team"]) : null;
    size += r["body"].length + (team?.length ?? 0);
    if (size > MAX_GUIDANCE) return null;
    rules.push({ team, body: r["body"] });
  }
  return rules.length > 0 ? rules : null;
}

/// A team's name with its parents' names, as Linear shows it.
function teamName(team: Record<string, unknown>): string | null {
  const name = [team["displayName"], team["name"]].find((n) => typeof n === "string" && n.trim() !== "");
  return typeof name === "string" ? name : null;
}

/// A follow-up in a session: a message or a stop. It goes to the computer
/// that has the session, and only from the person who delegated it there.
async function followUp(env: Env, hook: Hook, body: Record<string, unknown>): Promise<Response> {
  const activity = body["agentActivity"];
  if (!isObject(activity) || !isUuid(activity["id"])) return error(400, "bad webhook");
  const activityId = activity["id"];
  const author = authorOf(activity);
  const say = (type: "thought" | "error", text: string) => postActivity(env, hook.organizationId, hook.agentSessionId, type, text);
  // The newest delegation still waiting or taken. One that ran out of time
  // left nothing on the computer to steer.
  const session = await env.DB.prepare(
    `SELECT account_id, label, issue_id FROM linear_deliveries
     WHERE agent_session_id = ? AND organization_id = ? AND kind = 'created' AND expired_at IS NULL
     ORDER BY created_at DESC, rowid DESC LIMIT 1`,
  )
    .bind(hook.agentSessionId, hook.organizationId)
    .first<{ account_id: number; label: string; issue_id: string }>();
  // Anyone in the workspace can write in the session. Only the delegator's
  // words reach their computer.
  const isDelegator = async (accountId: number) =>
    author !== null &&
    Boolean(
      await env.DB.prepare("SELECT 1 AS yes FROM linear_links WHERE organization_id = ? AND linear_user_id = ? AND account_id = ?")
        .bind(hook.organizationId, author, accountId)
        .first(),
    );
  if (!session) {
    const pick = await openPick(env, hook);
    if (!pick) return (await say("error", NO_SESSION), ok());
    if (!(await isDelegator(pick.account_id))) return (await say("thought", NOT_DELEGATOR), ok());
    if (activity["signal"] === "stop") {
      await env.DB.prepare("DELETE FROM linear_picks WHERE agent_session_id = ?").bind(hook.agentSessionId).run();
      await postActivity(env, hook.organizationId, hook.agentSessionId, "response", PICK_STOPPED);
      return ok();
    }
    return picked(env, hook, pick, promptText(activity));
  }
  if (!(await isDelegator(session.account_id))) return (await say("thought", NOT_DELEGATOR), ok());

  const id = hex(crypto.getRandomValues(new Uint8Array(16)));
  const kind = activity["signal"] === "stop" ? "stop" : "prompt";
  const key = `activity:${activityId}`;
  const [, read] = await env.DB.batch<Row>([
    env.DB.prepare(
      `INSERT INTO linear_deliveries (id, event_key, agent_session_id, organization_id, issue_id, kind, activity_id, account_id, label, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (event_key) DO NOTHING`,
    ).bind(id, key, hook.agentSessionId, hook.organizationId, session.issue_id, kind, activityId, session.account_id, session.label, now()),
    readBack(env, key),
  ]);
  const row = read!.results[0]!;
  if (!belongs(row, hook, kind)) return mismatch();
  if (row.announced_at !== null) return ok();
  const done = () => env.DB.prepare("UPDATE linear_deliveries SET announced_at = ? WHERE id = ?").bind(now(), row.id).run();
  // As for a delegation: the relay forgot it, so it would arrive twice.
  if (row.accepted_at !== null || row.expired_at !== null) return (await done(), ok());
  const delivery: Delivery = {
    id: row.id,
    kind: row.kind,
    agent_session: row.agent_session_id,
    issue: row.issue_id,
    organization: row.organization_id,
    activity: row.activity_id,
    comment: null,
    repository: null,
    created: row.created_at,
  };
  let online: boolean;
  try {
    // The text goes in the live frame and nowhere else.
    ({ online } = await deliver(env, row.label, delivery, row.kind === "prompt" ? promptText(activity) : null));
  } catch (e) {
    if (!(e instanceof QueueFull)) throw e;
    await env.DB.prepare("DELETE FROM linear_deliveries WHERE id = ?").bind(row.id).run();
    await say("error", queueFull(row.machine ?? THIS_COMPUTER));
    return ok();
  }
  // A connected computer answers for itself. An offline one cannot, and
  // Linear would show Arakun as starting until it is back. A stop ends the
  // turn in Linear: nothing works on it meanwhile.
  if (!online) {
    const machine = row.machine ?? THIS_COMPUTER;
    if (row.kind === "stop") await postActivity(env, hook.organizationId, hook.agentSessionId, "response", stopWaits(machine));
    else await say("thought", replyWaits(machine));
  }
  await done();
  return ok();
}

/// The delivery an event made, with its computer's name now.
function readBack(env: Env, eventKey: string): D1PreparedStatement {
  return env.DB.prepare(
    `SELECT d.id, d.agent_session_id, d.organization_id, d.issue_id, d.kind, d.activity_id, d.reason, d.label, d.repository, d.created_at, d.announced_at,
       d.accepted_at, d.expired_at, m.name AS machine
     FROM linear_deliveries d LEFT JOIN machines m ON m.label = d.label AND m.account_id = d.account_id
     WHERE d.event_key = ?`,
  ).bind(eventKey);
}

/// Whether the row read back was made for this event. The key holds ids from
/// the signed body, so it always should be; a row that is not must never be
/// finished for someone else's event.
function belongs(row: Row, hook: Hook, kind: Row["kind"]): boolean {
  return row.organization_id === hook.organizationId && row.agent_session_id === hook.agentSessionId && row.kind === kind;
}

/// A retry finds the same row, so a 500 would not help.
function mismatch(): Response {
  console.error("POST /linear/webhook: delivery mismatch");
  return ok();
}

interface Row {
  id: string;
  agent_session_id: string;
  organization_id: string;
  issue_id: string;
  kind: Delivery["kind"];
  activity_id: string | null;
  reason: StopReason | null;
  label: string;
  repository: string | null;
  created_at: number;
  announced_at: number | null;
  accepted_at: number | null;
  expired_at: number | null;
  machine: string | null;
}

/// An issue delegated to Arakun, or Arakun mentioned on one: find the
/// delegator's computer, hand it the ids, and say so in the session.
async function delegated(env: Env, hook: Hook, guidance: GuidanceRule[] | null): Promise<Response> {
  const say = (type: "thought" | "error", body: string) => postActivity(env, hook.organizationId, hook.agentSessionId, type, body);
  if (!hook.creatorId) return (await say("error", NO_CREATOR), ok());
  if (!hook.issueId) return (await say("error", NO_ISSUE), ok());
  // A blocked account, or one that lost the flag, is as good as unlinked.
  const link = await env.DB.prepare(
    `SELECT l.account_id AS account_id, ${GITHUB_ID_OF("l.account_id")} IS NOT NULL AS github FROM linear_links l
     JOIN accounts u ON u.id = l.account_id AND u.blocked_at IS NULL
     JOIN user_flags f ON f.account_id = l.account_id AND f.flag = ?3
     WHERE l.organization_id = ?1 AND l.linear_user_id = ?2`,
  )
    .bind(hook.organizationId, hook.creatorId, FLAG)
    .first<{ account_id: number; github: number }>();
  if (!link) return (await say("error", NOT_LINKED), ok());
  // Like asking for cloud access (cloud_access.ts): an account made with an
  // address alone costs nothing to make again.
  if (link.github !== 1) return (await say("error", NO_GITHUB), ok());
  let place: Awaited<ReturnType<typeof issuePlace>>;
  try {
    place = await issuePlace(env, hook.organizationId, hook.issueId);
  } catch (e) {
    // An issue the app cannot see will not appear on a retry.
    if (e instanceof LinearRefused && e.status === 404) return (await say("error", NO_ROUTE), ok());
    throw e;
  }
  const route = await routeFor(env, link.account_id, hook.organizationId, place);
  if (!route) return ask(env, hook, hook.issueId, link.account_id, place.teamId);
  return send(env, hook, { issueId: hook.issueId, accountId: link.account_id, label: route.label, machine: route.machine, teamId: place.teamId, repository: null }, guidance);
}

/// Where a delegation goes: a computer, and the repository picked for it
/// when no route took the issue.
interface Target {
  issueId: string;
  accountId: number;
  label: string;
  machine: string;
  teamId: string;
  repository: string | null;
}

/// Hands a delegation to its computer and says so in the session.
async function send(env: Env, hook: Hook, to: Target, guidance: GuidanceRule[] | null): Promise<Response> {
  const say = (type: "thought" | "error", body: string) => postActivity(env, hook.organizationId, hook.agentSessionId, type, body);
  // The row first, keyed by the agent session: a retry finds it and finishes
  // the same delivery, under the same id, which the relay takes only once
  // while it still keeps it. A second `created` for the same session is the
  // same event to us.
  const id = hex(crypto.getRandomValues(new Uint8Array(16)));
  const key = `created:${hook.agentSessionId}`;
  const [, read] = await env.DB.batch<Row>([
    env.DB.prepare(
      `INSERT INTO linear_deliveries (id, event_key, agent_session_id, organization_id, issue_id, kind, activity_id, account_id, label, team_id, repository, created_at)
       VALUES (?, ?, ?, ?, ?, 'created', NULL, ?, ?, ?, ?, ?) ON CONFLICT (event_key) DO NOTHING`,
    ).bind(id, key, hook.agentSessionId, hook.organizationId, to.issueId, to.accountId, to.label, to.teamId, to.repository, now()),
    readBack(env, key),
  ]);
  const row = read!.results[0]!;
  if (!belongs(row, hook, "created")) return mismatch();
  if (row.announced_at !== null) return ok();
  // A retry may find its computer renamed or gone since; it still goes to
  // the label the row holds.
  const machine = row.machine ?? to.machine;
  const done = () => env.DB.prepare("UPDATE linear_deliveries SET announced_at = ? WHERE id = ?").bind(now(), row.id).run();
  // The relay forgets a delivery once the computer took it or the week ran
  // out, so sending it again would start a second session. For an expired
  // one, linear_expire.ts speaks in the session.
  if (row.accepted_at !== null) return (await say("thought", sent(machine)), await done(), ok());
  if (row.expired_at !== null) return (await done(), ok());
  const delivery: Delivery = {
    id: row.id,
    kind: "created",
    agent_session: row.agent_session_id,
    issue: row.issue_id,
    organization: row.organization_id,
    activity: null,
    // From the signed body, as a retry carries the same one.
    comment: hook.commentId,
    repository: row.repository,
    created: row.created_at,
  };
  let online: boolean;
  try {
    // The guidance goes in the live frame and nowhere else: an offline
    // computer starts without it.
    ({ online } = await deliver(env, row.label, delivery, null, guidance));
  } catch (e) {
    if (!(e instanceof QueueFull)) throw e;
    // Nothing waits anywhere for it: the person tries again later.
    await env.DB.prepare("DELETE FROM linear_deliveries WHERE id = ?").bind(row.id).run();
    await say("error", queueFull(machine));
    return ok();
  }
  await say("thought", online ? sent(machine) : offline(machine));
  await done();
  return ok();
}

// ---- Asking which project to use ---------------------------------------------
//
// No route takes the issue, but the delegator's computers offer the GitHub
// repositories of their projects. Linear ranks them for the issue, and the
// session asks the delegator to pick one. The pick goes to that computer,
// which checks for itself that the repository is one of its projects.

/// One choice of a question: a repository on a computer, and the text it
/// was shown as, which is what a pick comes back as.
interface Choice {
  label: string;
  repository: string;
  text: string;
}

/// What a choice reads as in Linear.
function choiceText(repository: string, machine: string): string {
  const name = [...machine.trim()];
  const short = name.length > MAX_CHOICE_MACHINE ? `${name.slice(0, MAX_CHOICE_MACHINE - 1).join("")}…` : name.join("");
  return `${repository} on ${short || THIS_COMPUTER}`;
}

/// The best choices: Linear's ranking first, then the computer seen last.
/// Two that would read the same keep the first.
export function choose(offered: { label: string; machine: string; repository: string }[], ranked: string[]): Choice[] {
  const rank = (r: string) => {
    const i = ranked.findIndex((n) => n.toLowerCase() === r.toLowerCase());
    return i < 0 ? ranked.length : i;
  };
  const sorted = offered.map((o, i) => ({ ...o, i })).sort((a, b) => rank(a.repository) - rank(b.repository) || a.i - b.i);
  const out: Choice[] = [];
  for (const o of sorted) {
    const text = choiceText(o.repository, o.machine);
    if (out.some((c) => c.text === text)) continue;
    out.push({ label: o.label, repository: o.repository, text });
    if (out.length === MAX_CHOICES) break;
  }
  return out;
}

/// Asks the delegator which project to use, or says no computer is set up
/// when none offers a repository.
async function ask(env: Env, hook: Hook, issueId: string, accountId: number, teamId: string): Promise<Response> {
  const say = (type: "thought" | "error" | "elicitation", body: string, options: string[] = []) =>
    postActivity(env, hook.organizationId, hook.agentSessionId, type, body, false, options);
  const offered = await offeredFor(env, accountId, hook.organizationId);
  if (offered.length === 0) return (await say("error", NO_ROUTE), ok());
  const names = [...new Map(offered.map((o) => [o.repository.toLowerCase(), o.repository])).values()];
  let ranked: string[];
  try {
    ranked = await rankRepositories(env, hook.organizationId, issueId, hook.agentSessionId, names);
  } catch (e) {
    // Without Linear's ranking the choices still stand, in the order the
    // computers were seen. A refusal that lasts is the webhook's to handle.
    if (!(e instanceof LinearRefused) || lasting(e)) throw e;
    ranked = names;
  }
  const choices = choose(offered, ranked);
  const at = now();
  await env.DB.batch([
    // Questions nobody answered in time go here, as there is no sweep for them.
    env.DB.prepare("DELETE FROM linear_picks WHERE created_at < ?").bind(at - PICK_SECONDS),
    env.DB.prepare(
      `INSERT INTO linear_picks (agent_session_id, organization_id, issue_id, account_id, team_id, choices, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT (agent_session_id) DO UPDATE SET choices = excluded.choices, created_at = excluded.created_at`,
    ).bind(hook.agentSessionId, hook.organizationId, issueId, accountId, teamId, JSON.stringify(choices), at),
  ]);
  // Linear shows two choices or more to pick from. One is asked in words.
  if (choices.length === 1) await say("elicitation", `${ASK_PROJECT} Reply "${choices[0]!.text}" to start there.`);
  else await say("elicitation", ASK_PROJECT, choices.map((c) => c.text));
  return ok();
}

interface Pick {
  organization_id: string;
  issue_id: string;
  account_id: number;
  team_id: string;
  choices: string;
}

/// The question waiting in this session, while it may still be answered.
async function openPick(env: Env, hook: Hook): Promise<Pick | null> {
  return env.DB.prepare(
    `SELECT organization_id, issue_id, account_id, team_id, choices FROM linear_picks
     WHERE agent_session_id = ? AND organization_id = ? AND created_at >= ?`,
  )
    .bind(hook.agentSessionId, hook.organizationId, now() - PICK_SECONDS)
    .first<Pick>();
}

/// The delegator answered the question: the choice it names goes to its
/// computer, if that computer still offers it. Anything else asks again.
async function picked(env: Env, hook: Hook, pick: Pick, text: string | null): Promise<Response> {
  const say = (type: "thought" | "error", body: string) => postActivity(env, hook.organizationId, hook.agentSessionId, type, body);
  let choices: Choice[];
  try {
    choices = JSON.parse(pick.choices) as Choice[];
  } catch {
    choices = [];
  }
  const answer = text?.trim();
  const chosen = choices.find((c) => c.text === answer);
  if (!chosen) return (await say("thought", PICK_AGAIN), ok());
  const offered = await offeredFor(env, pick.account_id, hook.organizationId);
  const still = offered.find((o) => o.label === chosen.label && o.repository === chosen.repository);
  if (!still) return (await say("error", PICK_GONE), ok());
  // Gone first, so a second reply cannot send it twice.
  const taken = await env.DB.prepare("DELETE FROM linear_picks WHERE agent_session_id = ?").bind(hook.agentSessionId).run();
  if (taken.meta.changes === 0) return ok();
  const to: Target = {
    issueId: pick.issue_id,
    accountId: pick.account_id,
    label: still.label,
    machine: still.machine,
    teamId: pick.team_id,
    repository: still.repository,
  };
  // The guidance came with the delegation, which is long gone: the computer
  // starts without it, as an offline one does.
  return send(env, hook, to, null);
}

// ---- Stops Linear did not ask for --------------------------------------------
//
// Nobody pressed stop, but the work is no longer Arakun's: it was
// unassigned, its issue was closed, or it lost the team or the workspace.
// Each session still at work gets a stop with the reason, and its computer
// says why in the session. Anyone who may unassign Arakun in Linear stops it
// this way, not only the delegator: the issue is no longer Arakun's to work
// on. Nothing is said in Linear from here: the run may have ended long ago,
// and the computer knows whether it did.

/// The most teams one access change may remove.
const MAX_TEAMS = 1000;
/// The most sessions one event stops, newest first.
const MAX_STOPS = 20;
/// How far back a session may have started and still be stopped. Its
/// delegation is given up after 7 days, and a run rarely lasts longer.
const STOP_WINDOW_SECONDS = 7 * 24 * 60 * 60;

/// A delegation that may still be at work on a computer.
interface Started {
  agent_session_id: string;
  issue_id: string;
  account_id: number;
  label: string;
}

/// Arakun was unassigned, or the status of an issue it is subscribed to
/// changed. The notification carries no state, so it is read from Linear.
async function notified(env: Env, organizationId: string, action: string, parsed: Record<string, unknown>): Promise<Response> {
  const n = parsed["notification"];
  if (!isObject(n) || !isUuid(n["id"]) || !isUuid(n["issueId"])) return error(400, "bad webhook");
  const issueId = n["issueId"];
  let reason: StopReason;
  if (action === "issueUnassignedFromYou") reason = "unassigned";
  else {
    let state: string;
    try {
      state = await issueStateType(env, organizationId, issueId);
    } catch (e) {
      // An issue the app can no longer see: the access change says so.
      if (e instanceof LinearRefused && e.status === 404) return ok();
      throw e;
    }
    // Done by a person: the person called it finished. Done by an
    // integration, such as a merged pull request, has no person as actor:
    // the session may still be answering its review.
    const actor = n["actorId"];
    const byPerson = typeof actor === "string" && actor !== "" && actor !== parsed["appUserId"];
    if (state === "canceled") reason = "canceled";
    else if (state === "completed" && byPerson) reason = "completed";
    else return ok();
  }
  const newest = await env.DB.prepare(
    `SELECT agent_session_id, issue_id, account_id, label FROM linear_deliveries
     WHERE organization_id = ? AND issue_id = ? AND kind = 'created' AND expired_at IS NULL
     ORDER BY created_at DESC, rowid DESC LIMIT 1`,
  )
    .bind(organizationId, issueId)
    .first<Started>();
  if (newest) await stopOne(env, organizationId, newest, reason, `notification:${n["id"]}`);
  return ok();
}

/// The app was removed from the workspace: its sessions stop, and its tokens
/// are dropped, as Linear will not take them again. The Linear tab then
/// shows the workspace as not installed.
async function revoked(env: Env, organizationId: string): Promise<void> {
  await stopAll(env, organizationId, "revoked", null);
  await env.DB.prepare("DELETE FROM linear_workspaces WHERE organization_id = ?").bind(organizationId).run();
}

/// Stops the recent sessions of a workspace, or of its `teams`. A session
/// delegated before its team was stored is not found by team.
async function stopAll(env: Env, organizationId: string, reason: StopReason, teams: string[] | null): Promise<void> {
  const byTeam = teams === null ? "" : `AND team_id IN (SELECT value FROM json_each(?3))`;
  const binds: unknown[] = [organizationId, now() - STOP_WINDOW_SECONDS];
  if (teams !== null) binds.push(JSON.stringify(teams));
  const rows = await env.DB.prepare(
    `SELECT agent_session_id, issue_id, account_id, label FROM linear_deliveries
     WHERE organization_id = ?1 AND kind = 'created' AND expired_at IS NULL AND created_at >= ?2 ${byTeam}
     ORDER BY created_at DESC, rowid DESC LIMIT ${MAX_STOPS}`,
  )
    .bind(...binds)
    .all<Started>();
  for (const r of rows.results) await stopOne(env, organizationId, r, reason, `${reason}:${r.agent_session_id}`);
}

/// One stop, sent once: a retry of the same event finds its row.
async function stopOne(env: Env, organizationId: string, s: Started, reason: StopReason, key: string): Promise<void> {
  const id = hex(crypto.getRandomValues(new Uint8Array(16)));
  const [, read] = await env.DB.batch<Row>([
    env.DB.prepare(
      `INSERT INTO linear_deliveries (id, event_key, agent_session_id, organization_id, issue_id, kind, activity_id, account_id, label, reason, created_at)
       VALUES (?, ?, ?, ?, ?, 'stop', NULL, ?, ?, ?, ?) ON CONFLICT (event_key) DO NOTHING`,
    ).bind(id, key, s.agent_session_id, organizationId, s.issue_id, s.account_id, s.label, reason, now()),
    readBack(env, key),
  ]);
  const row = read!.results[0]!;
  if (row.organization_id !== organizationId || row.agent_session_id !== s.agent_session_id || row.kind !== "stop") return void mismatch();
  if (row.announced_at !== null || row.accepted_at !== null || row.expired_at !== null) return;
  const delivery: Delivery = {
    id: row.id,
    kind: "stop",
    agent_session: row.agent_session_id,
    issue: row.issue_id,
    organization: row.organization_id,
    activity: null,
    reason: row.reason,
    created: row.created_at,
  };
  try {
    await deliver(env, row.label, delivery, null);
  } catch (e) {
    if (!(e instanceof QueueFull)) throw e;
    // Nobody to tell: the computer has more waiting than it keeps.
    await env.DB.prepare("DELETE FROM linear_deliveries WHERE id = ?").bind(row.id).run();
    console.error(`POST /linear/webhook: stop for ${row.label} dropped, queue full`);
    return;
  }
  await env.DB.prepare("UPDATE linear_deliveries SET announced_at = ? WHERE id = ?").bind(now(), row.id).run();
}
