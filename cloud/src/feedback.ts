// Feedback from the app (bug reports, feature requests, UX notes and the
// rest) as Linear customer requests. The app cannot hold a Linear key, since
// anyone could read it out of the binary, so it posts here and the Worker
// holds the key. Support questions do not come here: they stay in the
// `survey sent` event in PostHog.
//
//   POST /feedback  { kind, title, description, request, from, email? } → 202
//
// No sign-in: feedback works signed out, so each client IP gets a few a
// minute. The answer comes before Linear is called, so a slow Linear never
// holds the app; the app sends the same response to PostHog as well, so a
// lost one is not gone.
//
// Linear syncs the team's issues to the public GitHub repository, so the
// issue gets only what the app scrubbed (`title`, `description`). The fuller
// text (`request`, only its secrets taken out) goes in the customer request,
// which stays in Linear, and the email only names the customer.

import { error, isObject, jsonBody, type Env } from "./util";

const LINEAR_API = "https://api.linear.app/graphql";
const LINEAR_TIMEOUT_MS = 10_000;

export const KINDS = ["bug", "feature", "ux", "other"] as const;
export type Kind = (typeof KINDS)[number];

/// The type label of each kind. `other` gets none: a person picks it in triage.
const TYPE_LABEL: Record<Kind, string | null> = { bug: "Bug", feature: "Feature", ux: "Improvement", other: null };
/// Feedback is about the desktop app.
const AREA_LABEL = "area: app";

export const MAX_TITLE = 200;
/// The app caps its message at 16 KiB.
export const MAX_TEXT = 16 * 1024 + 16;
const MAX_FROM = 100;
const MAX_EMAIL = 254;
/// Both texts at their longest, escaped, with room for the rest.
const MAX_BODY = 4 * MAX_TEXT + 4096;
const EMAIL = /^[^\s@]{1,64}@[^\s@.]+(\.[^\s@.]+)+$/;

/// What the app sends, checked.
export interface Feedback {
  kind: Kind;
  title: string;
  description: string;
  request: string;
  from: string;
  email: string | null;
}

/// The answer for /feedback, or null when the path is not it.
export async function feedback(request: Request, env: Env, url: URL, ctx: ExecutionContext): Promise<Response | null> {
  if (url.pathname !== "/feedback") return null;
  if (request.method !== "POST") return error(404, "not found");
  if (!env.LINEAR_API_KEY || !env.LINEAR_TEAM_ID) return error(503, "feedback is not set up");
  const ip = request.headers.get("CF-Connecting-IP") ?? "unknown";
  let limited: boolean;
  try {
    limited = !(await env.FEEDBACK_LIMIT.limit({ key: ip })).success;
  } catch (e) {
    console.error("feedback limit failed:", e);
    return error(503, "the server could not check the request: try again");
  }
  if (limited) return error(429, "too many requests, try again in a minute");
  const parsed = parse(await jsonBody(request, MAX_BODY));
  if (typeof parsed === "string") return error(400, parsed);
  const config = { key: env.LINEAR_API_KEY, team: env.LINEAR_TEAM_ID };
  ctx.waitUntil(
    toLinear(parsed, config).catch((e) => {
      console.error("feedback to Linear failed:", e instanceof Error ? e.message : e);
    }),
  );
  return Response.json({ ok: true }, { status: 202 });
}

/// The checked feedback, or what is wrong with it.
export function parse(body: unknown): Feedback | string {
  if (!isObject(body)) return "expected a JSON object";
  const kind = body["kind"];
  if (typeof kind !== "string" || !(KINDS as readonly string[]).includes(kind)) return `kind must be one of ${KINDS.join(", ")}`;
  const title = text(body["title"], MAX_TITLE);
  if (title === null) return `title must be text of 1 to ${MAX_TITLE} characters`;
  const description = text(body["description"], MAX_TEXT);
  const req = text(body["request"], MAX_TEXT);
  if (description === null || req === null) return `description and request must be text of 1 to ${MAX_TEXT} characters`;
  const from = text(body["from"], MAX_FROM);
  if (from === null) return `from must be text of 1 to ${MAX_FROM} characters`;
  const given = body["email"];
  let email: string | null = null;
  if (given !== undefined && given !== null) {
    if (typeof given !== "string") return "email must be text";
    const typed = given.trim().toLowerCase();
    // A follow-up answer that is no email is dropped, as the app does.
    if (typed.length <= MAX_EMAIL && EMAIL.test(typed)) email = typed;
  }
  return { kind: kind as Kind, title: oneLine(title), description, request: req, from: oneLine(from), email };
}

function text(v: unknown, max: number): string | null {
  if (typeof v !== "string") return null;
  const t = v.trim();
  return t.length > 0 && t.length <= max ? t : null;
}

function oneLine(t: string): string {
  return t.replace(/\s+/g, " ");
}

interface LinearConfig {
  key: string;
  team: string;
}

/// Opens the issue in the team's default state, so it lands where a person
/// triages it, then adds the customer request to it.
export async function toLinear(f: Feedback, config: LinearConfig): Promise<void> {
  const labelIds = await labels(f.kind, config);
  const created = await linear<{ issueCreate: { success: boolean; issue: { id: string } | null } }>(
    config,
    `mutation($input: IssueCreateInput!) { issueCreate(input: $input) { success issue { id } } }`,
    { input: { teamId: config.team, title: f.title, description: `${f.description}\n\nFrom: ${f.from}`, labelIds } },
  );
  const issue = created.issueCreate.issue;
  if (!created.issueCreate.success || !issue) throw new Error("Linear did not create the issue");
  const customerId = f.email ? await customer(f.email, config) : null;
  const need = await linear<{ customerNeedCreate: { success: boolean } }>(
    config,
    `mutation($input: CustomerNeedCreateInput!) { customerNeedCreate(input: $input) { success } }`,
    { input: { issueId: issue.id, body: f.request, ...(customerId ? { customerId } : {}) } },
  );
  if (!need.customerNeedCreate.success) throw new Error("Linear did not create the customer request");
}

/// The ids of the kind's labels, from the team or the workspace. A label
/// that is gone is left out rather than failing the feedback.
async function labels(kind: Kind, config: LinearConfig): Promise<string[]> {
  const names = [TYPE_LABEL[kind], AREA_LABEL].filter((n): n is string => n !== null);
  const data = await linear<{ issueLabels: { nodes: { id: string; name: string; team: { id: string } | null }[] } }>(
    config,
    `query($names: [String!]!) { issueLabels(filter: { name: { in: $names } }) { nodes { id name team { id } } } }`,
    { names },
  );
  const usable = data.issueLabels.nodes.filter((l) => l.team === null || l.team.id === config.team);
  return names.flatMap((n) => {
    const found = usable.find((l) => l.name === n);
    return found ? [found.id] : [];
  });
}

/// The customer for an email, made the first time it writes. A person, not
/// a company: the email is its external id and its name.
async function customer(email: string, config: LinearConfig): Promise<string | null> {
  const data = await linear<{ customerUpsert: { success: boolean; customer: { id: string } | null } }>(
    config,
    `mutation($input: CustomerUpsertInput!) { customerUpsert(input: $input) { success customer { id } } }`,
    { input: { externalId: email, name: email } },
  );
  return data.customerUpsert.customer?.id ?? null;
}

async function linear<T>(config: LinearConfig, query: string, variables: Record<string, unknown>): Promise<T> {
  const res = await fetch(LINEAR_API, {
    method: "POST",
    redirect: "manual",
    // A personal API key goes as it is, with no Bearer.
    headers: { "Content-Type": "application/json", Authorization: config.key },
    body: JSON.stringify({ query, variables }),
    signal: AbortSignal.timeout(LINEAR_TIMEOUT_MS),
  });
  const body = (await res.json().catch(() => null)) as { data?: T; errors?: { message?: string }[] } | null;
  if (!res.ok || !body?.data || body.errors?.length) {
    const why = body?.errors?.map((e) => e.message).join("; ") ?? `status ${res.status}`;
    throw new Error(`Linear answered: ${why}`);
  }
  return body.data;
}
