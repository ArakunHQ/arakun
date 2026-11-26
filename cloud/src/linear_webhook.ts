// POST /linear/webhook: Linear tells Arakun that someone delegated an issue to
// it, mentioned it, or wrote in its session. It carries no session: the
// signature is its proof. Linear wants an HTTP answer within 5 seconds and an
// activity in the session within 10.
//
// The body holds the issue's text (promptContext, the title, comments). None
// of it is kept, stored, logged or put in an error: only ids go on.

import { routeFor, FLAG, UUID } from "./linear";
import { issuePlace, LinearRefused, postActivity } from "./linear_api";
import { deliver, QueueFull, type Delivery } from "./machines";
import { bodyText, error, hex, isObject, now, sha256, type Env } from "./util";

const MAX_BODY = 1024 * 1024;
/// How far Linear's clock may be from ours, so a captured webhook cannot be
/// replayed later.
const MAX_SKEW_MS = 60_000;
const SIGNATURE = /^[0-9a-f]{64}$/i;

const NO_CREATOR = "Arakun starts work only for a person. Delegate this issue yourself.";
const NO_ISSUE = "Arakun works on issues. Mention it on an issue instead.";
const NOT_LINKED =
  'The person who delegated this has not connected Linear in Arakun. In Arakun, open the Linear tab and turn on "Let Linear start sessions on this computer".';
const NO_ROUTE = "No computer is set up for this team. In Arakun's Linear tab, map the team or its project to a project on your computer.";
const queueFull = (machine: string) => `Too many requests are waiting for ${machine}. Try again when it has caught up.`;
const sent = (machine: string) => `Sent to ${machine}. A session starts there now.`;
const offline = (machine: string) => `${machine} is offline. The session starts when it is back, for up to 7 days.`;

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
  webhookId: string;
  creatorId: string | null;
  issueId: string | null;
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
  const action = parsed["action"];
  if (request.headers.get("Linear-Event") !== "AgentSessionEvent" || (action !== "created" && action !== "prompted")) return ok();
  const session = parsed["agentSession"];
  const { organizationId, webhookId } = parsed;
  if (!isObject(session) || !isUuid(organizationId) || !isUuid(session["id"]) || !isUuid(webhookId)) return error(400, "bad webhook");
  const hook: Hook = {
    organizationId,
    agentSessionId: session["id"],
    webhookId,
    creatorId: typeof session["creatorId"] === "string" && session["creatorId"] !== "" ? session["creatorId"] : null,
    issueId: isUuid(session["issueId"]) ? session["issueId"] : null,
  };
  try {
    return action === "created" ? await delegated(env, hook) : await followUp(env, hook, parsed);
  } catch (e) {
    // Linear sends it again after a 500; the retry lands on the same row.
    // The status or the error's name only: a message could quote Linear.
    const why = e instanceof LinearRefused ? e.status : e instanceof Error ? e.name : "unknown";
    console.error(`POST /linear/webhook: ${why}`);
    return error(500, "internal error");
  }
}

function isUuid(v: unknown): v is string {
  return typeof v === "string" && UUID.test(v);
}

/// A follow-up in a session. Task 7 hands it to the computer that has the
/// session; until then it is taken and dropped.
async function followUp(_env: Env, _hook: Hook, _body: Record<string, unknown>): Promise<Response> {
  return ok();
}

interface Row {
  id: string;
  agent_session_id: string;
  organization_id: string;
  issue_id: string;
  label: string;
  created_at: number;
  announced_at: number | null;
  accepted_at: number | null;
  expired_at: number | null;
  machine: string | null;
}

/// An issue delegated to Arakun, or Arakun mentioned on one: find the
/// delegator's computer, hand it the ids, and say so in the session.
async function delegated(env: Env, hook: Hook): Promise<Response> {
  const say = (type: "thought" | "error", body: string) => postActivity(env, hook.organizationId, hook.agentSessionId, type, body);
  if (!hook.creatorId) return (await say("error", NO_CREATOR), ok());
  if (!hook.issueId) return (await say("error", NO_ISSUE), ok());
  // A blocked account, or one that lost the flag, is as good as unlinked.
  const link = await env.DB.prepare(
    `SELECT l.github_id AS github_id FROM linear_links l
     JOIN users u ON u.github_id = l.github_id AND u.blocked_at IS NULL
     JOIN user_flags f ON f.github_id = l.github_id AND f.flag = ?3
     WHERE l.organization_id = ?1 AND l.linear_user_id = ?2`,
  )
    .bind(hook.organizationId, hook.creatorId, FLAG)
    .first<{ github_id: number }>();
  if (!link) return (await say("error", NOT_LINKED), ok());
  let place: Awaited<ReturnType<typeof issuePlace>>;
  try {
    place = await issuePlace(env, hook.organizationId, hook.issueId);
  } catch (e) {
    // An issue the app cannot see will not appear on a retry.
    if (e instanceof LinearRefused && e.status === 404) return (await say("error", NO_ROUTE), ok());
    throw e;
  }
  const route = await routeFor(env, link.github_id, hook.organizationId, place);
  if (!route) return (await say("error", NO_ROUTE), ok());

  // The row first, keyed by the webhook: a retry finds it and finishes the
  // same delivery, under the same id, which the relay takes only once while
  // it still keeps it.
  const id = hex(crypto.getRandomValues(new Uint8Array(16)));
  const [, read] = await env.DB.batch<Row>([
    env.DB.prepare(
      `INSERT INTO linear_deliveries (id, webhook_id, agent_session_id, organization_id, issue_id, kind, activity_id, github_id, label, created_at)
       VALUES (?, ?, ?, ?, ?, 'created', NULL, ?, ?, ?) ON CONFLICT (webhook_id) DO NOTHING`,
    ).bind(id, hook.webhookId, hook.agentSessionId, hook.organizationId, hook.issueId, link.github_id, route.label, now()),
    env.DB.prepare(
      `SELECT d.id, d.agent_session_id, d.organization_id, d.issue_id, d.label, d.created_at, d.announced_at, d.accepted_at, d.expired_at,
         m.name AS machine
       FROM linear_deliveries d LEFT JOIN machines m ON m.label = d.label AND m.github_id = d.github_id
       WHERE d.webhook_id = ?`,
    ).bind(hook.webhookId),
  ]);
  const row = read!.results[0]!;
  if (row.announced_at !== null) return ok();
  // A retry may find its computer renamed or gone since; it still goes to
  // the label the row holds.
  const machine = row.machine ?? route.machine;
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
    created: row.created_at,
  };
  let online: boolean;
  try {
    ({ online } = await deliver(env, row.label, delivery, null));
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
