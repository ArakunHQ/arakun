// A fake GitHub and short ways to call the Worker under test.
import { SELF } from "cloudflare:test";
import { vi } from "vitest";

export const ADMIN = { Authorization: "Bearer test-secret" };

/// What the fake GitHub knows and what it was asked. The Worker runs in the
/// same isolate as the tests, so replacing the global fetch reaches it.
export const github = {
  /// Access token → the user GitHub would answer with.
  users: new Map<string, { id: number; login: string }>(),
  /// Lower-case login → who has it now, for GET /users/<login>. Signing in
  /// adds the user; a test can give a login to someone else.
  accounts: new Map<string, { id: number; login: string; type?: string }>(),
  /// "throw": the network fails; a number: GitHub answers with that status.
  outage: null as "throw" | number | null,
  /// The same, for revoking a token only.
  revokeOutage: null as "throw" | number | null,
  /// What GET /user/emails answers; "throw" or a status makes it fail.
  emails: null as { email: string; primary: boolean; verified: boolean }[] | "throw" | number | null,
  /// Code → the access token GitHub's web flow would give for it. A code is
  /// good once.
  codes: new Map<string, string>(),
  /// "throw", a status, or "error" (a 200 with `{ error }`, as GitHub refuses a code).
  codeOutage: null as "throw" | "error" | number | null,
  /// The bodies the code exchange was sent.
  exchanges: [] as string[],
  calls: [] as Request[],
  /// The requests to GET /user/emails, apart from `calls`.
  emailCalls: [] as Request[],
  /// The body of each call, read once by the fake.
  bodies: [] as string[],
  /// GitHub ID → the public profile GET /user/<id> answers with; 404 without one.
  profiles: new Map<number, Record<string, unknown>>(),
  /// The requests to GET /user/<id>, apart from `calls`.
  profileCalls: [] as Request[],
};

/// The Discord webhook the cloud access tests post to, and what it was sent.
export const webhook = {
  url: "https://discord.test/api/webhooks/1/secret",
  bodies: [] as string[],
  /// "throw": the network fails; a number: the webhook answers with it.
  outage: null as "throw" | number | null,
};

export function fakeGitHub(): void {
  github.users.clear();
  github.accounts.clear();
  github.outage = null;
  github.revokeOutage = null;
  github.emails = null;
  github.codes.clear();
  github.codeOutage = null;
  github.exchanges = [];
  github.calls = [];
  github.emailCalls = [];
  github.bodies = [];
  github.profiles.clear();
  github.profileCalls = [];
  webhook.bodies = [];
  webhook.outage = null;
  resetLinear();
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const req = new Request(input, init);
    const lookup = /^https:\/\/api\.github\.com\/users\/([^/]+)$/.exec(req.url);
    if (lookup && req.method === "GET") return lookupUser(req, decodeURIComponent(lookup[1]!));
    const profile = /^https:\/\/api\.github\.com\/user\/([0-9]+)$/.exec(req.url);
    if (profile && req.method === "GET") {
      github.profileCalls.push(req);
      if (github.outage === "throw") throw new TypeError("Network connection lost.");
      if (github.outage !== null) return new Response("unavailable", { status: github.outage });
      const found = github.profiles.get(Number(profile[1]));
      return found ? Response.json(found) : Response.json({ message: "Not Found" }, { status: 404 });
    }
    if (req.url.startsWith("https://api.linear.app/")) return linearReply(req);
    if (req.url === webhook.url && req.method === "POST") {
      webhook.bodies.push(await req.text());
      if (webhook.outage === "throw") throw new TypeError("Network connection lost.");
      return new Response(null, { status: webhook.outage ?? 204 });
    }
    if (req.url === "https://api.github.com/user/emails" && req.method === "GET") {
      github.emailCalls.push(req);
      if (github.emails === "throw") throw new TypeError("Network connection lost.");
      if (typeof github.emails === "number") return new Response("unavailable", { status: github.emails });
      return Response.json(github.emails ?? []);
    }
    if (req.url === "https://github.com/login/oauth/access_token" && req.method === "POST") return exchangeCode(req);
    if (req.url !== "https://api.github.com/applications/Iv-test/token" || (req.method !== "POST" && req.method !== "DELETE"))
      throw new Error(`unexpected fetch: ${req.method} ${req.url}`);
    const body = await req.text();
    github.calls.push(req);
    github.bodies.push(body);
    const outage = req.method === "DELETE" ? (github.revokeOutage ?? github.outage) : github.outage;
    if (outage === "throw") throw new TypeError("Network connection lost.");
    if (outage !== null) return new Response("unavailable", { status: outage });
    // What api.github.com answers to a wrong client ID or secret.
    if (req.headers.get("Authorization") !== `Basic ${btoa("Iv-test:client-secret")}`)
      return Response.json({ message: "Bad credentials" }, { status: 401 });
    const token = (JSON.parse(body) as { access_token?: string }).access_token;
    const user = token ? github.users.get(token) : undefined;
    // GitHub answers 404 for a token this app did not issue, or revoked.
    if (!user) return Response.json({ message: "Not Found" }, { status: 404 });
    if (req.method === "DELETE") {
      github.users.delete(token!);
      return new Response(null, { status: 204 });
    }
    // Real answers carry much more; the Worker must keep none of it.
    return Response.json({ id: 1, token: null, user: { ...user, email: "someone@example.com", name: "Some One" } });
  });
}

/// POST /login/oauth/access_token: GitHub answers 200 even for a bad code.
async function exchangeCode(req: Request): Promise<Response> {
  const body = await req.text();
  github.exchanges.push(body);
  if (github.codeOutage === "throw") throw new TypeError("Network connection lost.");
  if (typeof github.codeOutage === "number") return new Response("unavailable", { status: github.codeOutage });
  const sent = JSON.parse(body) as { client_id?: string; client_secret?: string; code?: string };
  const token = sent.code ? github.codes.get(sent.code) : undefined;
  if (github.codeOutage === "error" || sent.client_id !== "Iv-test" || sent.client_secret !== "client-secret" || !token)
    return Response.json({ error: "bad_verification_code", error_description: "The code passed is incorrect or expired." });
  github.codes.delete(sent.code!);
  return Response.json({ access_token: token, token_type: "bearer", scope: "user:email" });
}

/// GET /users/<login>: public data, asked with the app's credentials.
function lookupUser(req: Request, login: string): Response {
  github.calls.push(req);
  github.bodies.push("");
  if (github.outage === "throw") throw new TypeError("Network connection lost.");
  if (github.outage !== null) return new Response("unavailable", { status: github.outage });
  if (req.headers.get("Authorization") !== `Basic ${btoa("Iv-test:client-secret")}`)
    return Response.json({ message: "Bad credentials" }, { status: 401 });
  const account = github.accounts.get(login.toLowerCase());
  if (!account) return Response.json({ message: "Not Found" }, { status: 404 });
  return Response.json({ type: "User", ...account, name: "Some One" });
}

/// Tells the fake GitHub that this account has the login now.
export function onGitHub(id: number, login: string, type = "User"): void {
  github.accounts.set(login.toLowerCase(), { id, login, type });
}

let nextIp = 1;

/// A different client IP per call, so tests don't share a rate limit.
export function freshIp(): string {
  const n = nextIp++;
  return `10.${(n >> 16) & 255}.${(n >> 8) & 255}.${n & 255}`;
}

export function signInRequest(accessToken: unknown, ip = freshIp(), extra: object = {}): Promise<Response> {
  return SELF.fetch("https://teams.test/auth/github", {
    method: "POST",
    headers: { "Content-Type": "application/json", "CF-Connecting-IP": ip },
    body: JSON.stringify({ access_token: accessToken, ...extra }),
  });
}

export interface SignedIn {
  session: string;
  session_id: string;
  plan: string;
  expires_at: number;
  account_id: number;
  /// The account ID too, under the name older clients read.
  github_id: number;
  login: string;
  flags: string[];
  /// Not the Worker's: the GitHub ID the test signed in with, to sign in as
  /// the same GitHub user again.
  githubId: number;
}

/// Signs in as a user GitHub knows, and returns the Worker's answer.
export async function signIn(
  id: number,
  login: string,
  accessToken = `gho_${id}_${login}`,
  extra: { name?: string; kind?: string } = {},
): Promise<SignedIn> {
  github.users.set(accessToken, { id, login });
  onGitHub(id, login);
  const res = await signInRequest(accessToken, freshIp(), extra);
  if (res.status !== 200) throw new Error(`sign-in failed: ${res.status} ${await res.text()}`);
  return { ...(await res.json<Omit<SignedIn, "githubId">>()), githubId: id };
}

export function call(method: string, path: string, headers: Record<string, string> = {}, body?: BodyInit): Promise<Response> {
  return SELF.fetch(`https://teams.test${path}`, { method, headers, body });
}

export function as(session: string): Record<string, string> {
  return { Authorization: `Bearer ${session}` };
}

export function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

/// Waits for the next rate-limit window when this one has less than `roomMs`
/// left. The simulated limiter counts in windows fixed to the clock, so a
/// test that sends its burst across the end of one sees every call pass.
export async function roomInWindow(periodSeconds: 10 | 60, roomMs = 3_000): Promise<void> {
  const period = periodSeconds * 1000;
  const left = period - (Date.now() % period);
  if (left < roomMs) await new Promise((r) => setTimeout(r, left + 50));
}

/// Signs in and gives the user the `linear` flag, as `user()` does for `teams`.
/// `id` is the account's, not the GitHub ID signed in with.
export async function flagged(githubId: number, login: string): Promise<{ id: number; login: string; session: string }> {
  const got = await signIn(githubId, login);
  await call("PUT", `/admin/users/${got.account_id}/flags/linear`, ADMIN);
  return { id: got.account_id, login, session: got.session };
}

// A fake Linear: its token endpoint and GraphQL API. It issues the tokens it
// then accepts, and takes an old refresh token again, as Linear does for 30
// minutes after a refresh.

/// Who a token is. `app` says it is an app's own token (`actor=app`); left
/// out, it is a person's.
export interface LinearWho {
  userId: string;
  organizationId: string;
  organizationName: string;
  app?: boolean;
}

/// The workspace and app user an exchanged code belongs to.
export const FAKE_ORG = "0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f";
export const FAKE_APP_USER = "0a0a0a0a-0a0a-4a0a-8a0a-0a0a0a0a0a0a";

export const linearFake = {
  /// Access token → who Linear says it belongs to. A test adds the tokens it
  /// saves; an exchange or refresh adds the pair it issues.
  tokens: new Map<string, LinearWho>(),
  /// What an exchange or refresh issues a token as, over the app in FAKE_ORG.
  issueAs: {} as Partial<LinearWho>,
  /// Refresh tokens Linear still takes → the access token issued with each.
  refreshTokens: new Map<string, string>(),
  /// Refresh tokens already rotated → the pair that answered them. Linear
  /// answers the same pair again for 30 minutes; with `grace` off it refuses.
  rotated: new Map<string, { access: string; refresh: string }>(),
  grace: true,
  /// Issue id → its team and project.
  issues: new Map<string, { teamId: string; projectId: string | null }>(),
  /// Every GraphQL call: the query, its variables and the access token.
  calls: [] as { query: string; variables: unknown; token: string }[],
  /// The calls Linear accepted, apart from `calls`: what `activities()` and `links()` read.
  accepted: [] as { query: string; variables: unknown; token: string }[],
  /// The Authorization header of every GraphQL call, as sent.
  auth: [] as string[],
  /// The forms POSTed to /oauth/token.
  exchanges: [] as URLSearchParams[],
  /// Every call fails as the network does, as a timeout would.
  down: false,
  /// Every call answers this status.
  refuse: null as number | null,
  /// With `refuse`, only GraphQL calls whose query contains this text are refused.
  refuseOnly: null as string | null,
  /// The next GraphQL call answers 401, once.
  expireNext: false,
  /// Every GraphQL call answers 401, whatever the token.
  expireAlways: false,
  /// Runs after a refresh issued its pair and before the answer goes out: a
  /// test uses it to let another request refresh in between.
  afterRefresh: null as ((pair: { access: string; refresh: string }) => Promise<void>) | null,
  /// Makes a query answer a 200 with data of the wrong shape.
  malformed: null as "viewer" | "issue-null" | "no-team" | null,
  /// The agent activities created, in order.
  /// `ephemeral` is there only when it was sent.
  activities(): { agentSessionId: string; type: string; body: string; ephemeral?: boolean; signal?: string; options?: string[] }[] {
    return this.accepted
      .filter((c) => c.query.includes("agentActivityCreate"))
      .map((c) => {
        const input = (
          c.variables as {
            input: {
              agentSessionId: string;
              content: { type: string; body: string };
              ephemeral?: boolean;
              signal?: string;
              signalMetadata?: { options: { label: string; value: string }[] };
            };
          }
        ).input;
        const sent = { agentSessionId: input.agentSessionId, type: input.content.type, body: input.content.body };
        const select = input.signal === undefined ? {} : { signal: input.signal, options: input.signalMetadata?.options.map((o) => o.value) };
        return { ...sent, ...(input.ephemeral === undefined ? {} : { ephemeral: input.ephemeral }), ...select };
      });
  },
  /// The links added to agent sessions, in order.
  links(): { agentSessionId: string; label: string; url: string }[] {
    return this.accepted
      .filter((c) => c.query.includes("agentSessionUpdate"))
      .flatMap((c) => {
        const v = c.variables as { id: string; input: { addedExternalUrls: { label: string; url: string }[] } };
        return v.input.addedExternalUrls.map((l) => ({ agentSessionId: v.id, label: l.label, url: l.url }));
      });
  },
};

let linearIssued = 0;

function resetLinear(): void {
  linearFake.tokens.clear();
  linearFake.issueAs = {};
  linearFake.refreshTokens.clear();
  linearFake.rotated.clear();
  linearFake.grace = true;
  linearFake.issues.clear();
  linearFake.calls = [];
  linearFake.accepted = [];
  linearFake.auth = [];
  linearFake.exchanges = [];
  linearFake.down = false;
  linearFake.refuse = null;
  linearFake.refuseOnly = null;
  linearFake.expireNext = false;
  linearFake.expireAlways = false;
  linearFake.malformed = null;
  linearFake.afterRefresh = null;
  linearIssued = 0;
}

async function linearReply(req: Request): Promise<Response> {
  const url = new URL(req.url);
  if (req.method !== "POST" || (url.pathname !== "/oauth/token" && url.pathname !== "/graphql"))
    throw new Error(`unexpected fetch: ${req.method} ${req.url}`);
  if (linearFake.down) throw new TypeError("Network connection lost.");
  if (linearFake.refuse !== null && linearFake.refuseOnly === null) return new Response("refused", { status: linearFake.refuse });
  if (url.pathname === "/oauth/token") {
    const form = new URLSearchParams([...(await req.formData()).entries()] as [string, string][]);
    linearFake.exchanges.push(form);
    if (form.get("client_id") !== "lin-test" || form.get("client_secret") !== "lin-secret")
      return Response.json({ error: "invalid_client" }, { status: 401 });
    const grant = form.get("grant_type");
    const used = form.get("refresh_token") ?? "";
    if (grant === "refresh_token") {
      const again = linearFake.rotated.get(used);
      if (again && linearFake.grace)
        return Response.json({ access_token: again.access, refresh_token: again.refresh, expires_in: 86400, token_type: "Bearer", scope: "read" });
      if (!linearFake.refreshTokens.has(used)) return Response.json({ error: "invalid_grant" }, { status: 400 });
    } else if (!form.get("code")) return Response.json({ error: "invalid_grant" }, { status: 400 });
    linearIssued++;
    const access = `lin_oauth_issued${linearIssued}`;
    const refresh = `lin_refresh_issued${linearIssued}`;
    if (grant === "refresh_token") {
      // The pair it replaces stops working.
      linearFake.tokens.delete(linearFake.refreshTokens.get(used)!);
      linearFake.refreshTokens.delete(used);
      linearFake.rotated.set(used, { access, refresh });
    }
    linearFake.tokens.set(access, { userId: FAKE_APP_USER, organizationId: FAKE_ORG, organizationName: "Fake", app: true, ...linearFake.issueAs });
    linearFake.refreshTokens.set(refresh, access);
    if (grant === "refresh_token") await linearFake.afterRefresh?.({ access, refresh });
    return Response.json({ access_token: access, refresh_token: refresh, expires_in: 86400, token_type: "Bearer", scope: "read" });
  }
  const header = req.headers.get("Authorization") ?? "";
  const token = header.replace(/^Bearer /, "");
  const { query, variables } = (await req.json()) as { query: string; variables: Record<string, unknown> };
  linearFake.auth.push(header);
  linearFake.calls.push({ query, variables, token });
  if (linearFake.refuse !== null && linearFake.refuseOnly !== null && query.includes(linearFake.refuseOnly))
    return new Response("refused", { status: linearFake.refuse });
  const who = linearFake.tokens.get(token);
  if (linearFake.expireNext || linearFake.expireAlways || !who) {
    linearFake.expireNext = false;
    return Response.json({ errors: [{ message: "expired", extensions: { code: "AUTHENTICATION_ERROR" } }] }, { status: 401 });
  }
  linearFake.accepted.push({ query, variables, token });
  if (linearFake.malformed === "viewer") return Response.json({ data: { viewer: null } });
  if (linearFake.malformed === "issue-null") return Response.json({ data: { issue: null } });
  if (linearFake.malformed === "no-team") return Response.json({ data: { issue: { team: null, project: null } } });
  if (query.includes("viewer"))
    return Response.json({
      data: { viewer: { id: who.userId, app: who.app ?? false, organization: { id: who.organizationId, name: who.organizationName } } },
    });
  if (query.includes("agentActivityCreate") || query.includes("agentSessionUpdate"))
    return Response.json({ data: { agentActivityCreate: { success: true }, agentSessionUpdate: { success: true } } });
  if (query.includes("issue(")) {
    const issue = linearFake.issues.get((variables as { id: string }).id);
    if (!issue) return Response.json({ errors: [{ message: "Entity not found", extensions: { code: "INVALID_INPUT" } }] });
    return Response.json({ data: { issue: { team: { id: issue.teamId }, project: issue.projectId ? { id: issue.projectId } : null } } });
  }
  throw new Error(`unexpected Linear query: ${query}`);
}

/// Every row of every table the app owns, as one JSON string, to look for
/// what must not be stored.
export async function dump(db: D1Database): Promise<string> {
  const tables = await db
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '\\_cf\\_%' ESCAPE '\\' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' AND name NOT LIKE 'd1\\_%' ESCAPE '\\' ORDER BY name")
    .all<{ name: string }>();
  const out: Record<string, unknown[]> = {};
  for (const { name } of tables.results) out[name] = (await db.prepare(`SELECT * FROM "${name}"`).all()).results;
  return JSON.stringify(out);
}

// Linear's webhooks, as Linear sends them: the body signed with the webhook
// secret the tests run with.

export const HOOK_ORG = "11111111-1111-4111-8111-111111111111";
export const HOOK_SESSION = "55555555-5555-4555-8555-555555555555";
/// The id of the webhook itself. Linear sends it on every event, so it is one
/// value here too.
export const HOOK_WEBHOOK = "99999999-9999-4999-8999-999999999999";
const HOOK_SECRET = "lin-hook";

/// A signed webhook: the exact text sent and its headers. `at` is the
/// webhook's time in milliseconds; the text is serialized once and that text
/// is what is signed.
export async function signedWebhook(
  body: Record<string, unknown>,
  { secret = HOOK_SECRET, at = Date.now() }: { secret?: string; at?: number } = {},
): Promise<{ text: string; headers: Record<string, string> }> {
  const text = JSON.stringify({ ...body, webhookTimestamp: at });
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(text)));
  const signature = Array.from(mac, (b) => b.toString(16).padStart(2, "0")).join("");
  return { text, headers: { "Content-Type": "application/json", "Linear-Signature": signature, "Linear-Event": "AgentSessionEvent" } };
}

/// The body of a `created` AgentSessionEvent: someone delegated or mentioned
/// Arakun. Each call has a fresh agent session id unless one is given.
export function created({
  creatorId,
  issueId,
  promptContext = "<issue identifier=\"ENG-1\"><title>Make it blue</title></issue>",
  webhookId = HOOK_WEBHOOK,
  organizationId = HOOK_ORG,
  agentSessionId = crypto.randomUUID(),
}: {
  creatorId: string | null;
  issueId: string | null;
  promptContext?: string;
  webhookId?: string;
  organizationId?: string;
  agentSessionId?: string;
}): Record<string, unknown> {
  return {
    type: "AgentSessionEvent",
    action: "created",
    organizationId,
    webhookId,
    appUserId: "app-user",
    agentSession: {
      id: agentSessionId,
      issueId,
      creatorId,
      issue: issueId ? { id: issueId, identifier: "ENG-1", title: "Make it blue", description: "The button is grey.", teamId: null } : null,
      comment: null,
    },
    promptContext,
    previousComments: [],
    guidance: [],
  };
}

/// The body of a `prompted` AgentSessionEvent: a follow-up in the session.
/// Linear's schema has the author twice, as `userId` and `user.id`; a null
/// `userId` leaves both out. Each call has a fresh agent session id and
/// activity id unless they are given.
export function prompted({
  userId,
  body,
  activityId = crypto.randomUUID(),
  signal = null,
  issueId = null,
  webhookId = HOOK_WEBHOOK,
  organizationId = HOOK_ORG,
  agentSessionId = crypto.randomUUID(),
}: {
  userId: string | null;
  body: string;
  activityId?: string;
  signal?: string | null;
  issueId?: string | null;
  webhookId?: string;
  organizationId?: string;
  agentSessionId?: string;
}): Record<string, unknown> {
  const author = userId === null ? {} : { userId, user: { id: userId, name: "Some One", email: "someone@example.com" } };
  return {
    type: "AgentSessionEvent",
    action: "prompted",
    organizationId,
    webhookId,
    appUserId: "app-user",
    agentSession: { id: agentSessionId, issueId, creatorId: null, comment: null },
    agentActivity: { id: activityId, ...author, body, signal, content: { type: "prompt", body } },
  };
}
