// Calls to Linear as the installed Arakun app: the OAuth token endpoint and
// the GraphQL API. Nothing here logs or reports a token; an error carries the
// HTTP status and Linear's error code only.

import { open, seal } from "./linear_seal";
import { now, type Env } from "./util";

/// The ids Linear uses are UUIDs.
export const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

const TOKEN_URL = "https://api.linear.app/oauth/token";
const GRAPHQL_URL = "https://api.linear.app/graphql";
const TIMEOUT_MS = 3000;
/// A token this close to its end is refreshed before use.
const REFRESH_MARGIN = 300;

/// Linear gave an answer we cannot use, or none.
export class LinearRefused extends Error {
  constructor(
    message: string,
    public status: number,
  ) {
    super(message);
    this.name = "LinearRefused";
  }
}

/// A stored token that does not open under LINEAR_TOKEN_KEY: the key changed
/// or the row was damaged. Only installing again mends it.
export class TokenUnopened extends LinearRefused {
  constructor() {
    super("the stored Linear token cannot be opened", 500);
    this.name = "TokenUnopened";
  }
}

interface Tokens {
  access: string;
  refresh: string;
  expiresAt: number;
}

async function tokenCall(env: Env, form: Record<string, string>): Promise<Tokens> {
  const body = new URLSearchParams({ ...form, client_id: env.LINEAR_CLIENT_ID ?? "", client_secret: env.LINEAR_CLIENT_SECRET ?? "" });
  let res: Response;
  try {
    res = await fetch(TOKEN_URL, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body,
      signal: AbortSignal.timeout(TIMEOUT_MS),
    });
  } catch {
    throw new LinearRefused("Linear did not answer", 502);
  }
  // Linear answers a refresh token it no longer takes (the app was removed,
  // or it went unused for 30 days) with 400 invalid_grant: a refusal, like a 401.
  if (!res.ok) throw new LinearRefused(`Linear refused the token request: ${res.status}`, res.status === 400 || res.status === 401 ? 401 : 502);
  const j = (await res.json().catch(() => null)) as { access_token?: unknown; refresh_token?: unknown; expires_in?: unknown } | null;
  if (!j || typeof j.access_token !== "string" || typeof j.refresh_token !== "string" || typeof j.expires_in !== "number")
    throw new LinearRefused("Linear sent an unusable token answer", 502);
  return { access: j.access_token, refresh: j.refresh_token, expiresAt: now() + j.expires_in };
}

export function exchangeCode(env: Env, code: string, redirectUri: string): Promise<Tokens> {
  return tokenCall(env, { grant_type: "authorization_code", code, redirect_uri: redirectUri });
}

interface GraphQLAnswer {
  data?: unknown;
  errors?: { extensions?: { code?: unknown } }[];
}

/// One GraphQL call. An expired token is a LinearRefused with status 401,
/// however Linear says it.
async function graphql<T>(token: string, query: string, variables: Record<string, unknown>): Promise<T> {
  let res: Response;
  try {
    res = await fetch(GRAPHQL_URL, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        // An API key goes bare; an OAuth token as Bearer.
        Authorization: token.startsWith("lin_api_") ? token : `Bearer ${token}`,
      },
      body: JSON.stringify({ query, variables }),
      signal: AbortSignal.timeout(TIMEOUT_MS),
    });
  } catch {
    throw new LinearRefused("Linear did not answer", 502);
  }
  const j = (await res.json().catch(() => null)) as GraphQLAnswer | null;
  const code = j?.errors?.map((e) => e.extensions?.code).find((c) => typeof c === "string") as string | undefined;
  if (res.status === 401 || code === "AUTHENTICATION_ERROR") throw new LinearRefused("Linear did not accept the token", 401);
  if (!res.ok) throw new LinearRefused(`Linear answered ${res.status}${code ? ` ${code}` : ""}`, res.status);
  if (!j || j.errors?.length || j.data === undefined || j.data === null)
    throw new LinearRefused(`Linear refused the request${code ? `: ${code}` : ""}`, 502);
  return j.data as T;
}

/// Who a token belongs to. `app` is true for an app's own token (`actor=app`)
/// and false for a person's. Ids that are not UUIDs are refused: they are
/// stored and later matched against webhooks.
export async function viewer(token: string): Promise<{ userId: string; app: boolean; organizationId: string; organizationName: string }> {
  const data = await graphql<{ viewer?: { id?: unknown; app?: unknown; organization?: { id?: unknown; name?: unknown } | null } | null }>(
    token,
    "query { viewer { id app organization { id name } } }",
    {},
  );
  const v = data?.viewer;
  if (
    typeof v?.id !== "string" ||
    !UUID.test(v.id) ||
    typeof v.app !== "boolean" ||
    typeof v.organization?.id !== "string" ||
    !UUID.test(v.organization.id) ||
    typeof v.organization.name !== "string"
  )
    throw new LinearRefused("Linear sent an unusable viewer answer", 502);
  return { userId: v.id, app: v.app, organizationId: v.organization.id, organizationName: v.organization.name };
}

export async function saveWorkspace(
  env: Env,
  w: { organizationId: string; name: string; appUserId: string; access: string; refresh: string; expiresAt: number; installedBy: number },
): Promise<void> {
  const access = await seal(env, w.organizationId, w.access);
  const refresh = await seal(env, w.organizationId, w.refresh);
  await env.DB.prepare(
    `INSERT INTO linear_workspaces (organization_id, name, app_user_id, access_token, refresh_token, expires_at, installed_by, installed_at)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?)
     ON CONFLICT (organization_id) DO UPDATE SET name = excluded.name, app_user_id = excluded.app_user_id,
       access_token = excluded.access_token, refresh_token = excluded.refresh_token, expires_at = excluded.expires_at,
       installed_by = excluded.installed_by, installed_at = excluded.installed_at`,
  )
    .bind(w.organizationId, w.name, w.appUserId, access, refresh, w.expiresAt, w.installedBy, now())
    .run();
}

interface Row {
  access_token: string;
  refresh_token: string;
  expires_at: number;
}

async function readRow(env: Env, organizationId: string): Promise<Row> {
  const row = await env.DB.prepare("SELECT access_token, refresh_token, expires_at FROM linear_workspaces WHERE organization_id = ?")
    .bind(organizationId)
    .first<Row>();
  if (!row) throw new LinearRefused("Linear is not set up for this workspace", 404);
  return row;
}

async function unseal(env: Env, organizationId: string, sealed: string): Promise<string> {
  const plain = await open(env, organizationId, sealed);
  if (plain === null) throw new TokenUnopened();
  return plain;
}

/// A usable access token after refreshing `row`'s. Linear rotates the refresh
/// token, so the new pair is written only where the row still holds the
/// refresh token we used. If it does not, another request refreshed first and
/// its pair is the one to use; Linear takes the old refresh token again for
/// 30 minutes, so our call was harmless.
async function refreshed(env: Env, organizationId: string, row: Row): Promise<string> {
  let fresh: Tokens;
  try {
    fresh = await tokenCall(env, { grant_type: "refresh_token", refresh_token: await unseal(env, organizationId, row.refresh_token) });
  } catch (e) {
    // The other request may have used the token up already.
    const again = await readRow(env, organizationId);
    if (again.refresh_token !== row.refresh_token) return unseal(env, organizationId, again.access_token);
    throw e;
  }
  const done = await env.DB.prepare(
    "UPDATE linear_workspaces SET access_token = ?, refresh_token = ?, expires_at = ? WHERE organization_id = ? AND refresh_token = ?",
  )
    .bind(await seal(env, organizationId, fresh.access), await seal(env, organizationId, fresh.refresh), fresh.expiresAt, organizationId, row.refresh_token)
    .run();
  if (done.meta.changes > 0) return fresh.access;
  return unseal(env, organizationId, (await readRow(env, organizationId)).access_token);
}

/// Runs a query as the app in a workspace: refreshes a token that is close to
/// its end, and once after a 401.
export async function asApp<T>(env: Env, organizationId: string, query: string, variables: Record<string, unknown>): Promise<T> {
  const row = await readRow(env, organizationId);
  let token =
    row.expires_at - now() < REFRESH_MARGIN ? await refreshed(env, organizationId, row) : await unseal(env, organizationId, row.access_token);
  try {
    return await graphql<T>(token, query, variables);
  } catch (e) {
    if (!(e instanceof LinearRefused) || e.status !== 401) throw e;
  }
  token = await refreshed(env, organizationId, await readRow(env, organizationId));
  return graphql<T>(token, query, variables);
}

export async function postActivity(
  env: Env,
  organizationId: string,
  agentSessionId: string,
  type: "thought" | "elicitation" | "response" | "error",
  body: string,
  ephemeral = false,
  options: string[] = [],
): Promise<void> {
  // An ephemeral thought is replaced by the next activity in Linear.
  // Linear shows the choices of a `select` to pick from; a pick comes back
  // as an ordinary reply with the choice as its text.
  const select = options.length > 0 ? { signal: "select", signalMetadata: { options: options.map((o) => ({ label: o, value: o })) } } : {};
  const input = { agentSessionId, content: { type, body }, ...(ephemeral ? { ephemeral: true } : {}), ...select };
  await asApp(
    env,
    organizationId,
    "mutation($input: AgentActivityCreateInput!) { agentActivityCreate(input: $input) { success } }",
    { input },
  );
}

export async function addLinks(env: Env, organizationId: string, agentSessionId: string, links: { label: string; url: string }[]): Promise<void> {
  await asApp(
    env,
    organizationId,
    "mutation($id: String!, $input: AgentSessionUpdateInput!) { agentSessionUpdate(id: $id, input: $input) { success } }",
    { id: agentSessionId, input: { addedExternalUrls: links } },
  );
}

export async function issuePlace(env: Env, organizationId: string, issueId: string): Promise<{ teamId: string; projectId: string | null }> {
  const data = await asApp<{ issue?: { team?: { id?: unknown } | null; project?: { id?: unknown } | null } | null }>(
    env,
    organizationId,
    "query($id: String!) { issue(id: $id) { team { id } project { id } } }",
    { id: issueId },
  );
  // Linear answers null for an issue that does not exist or the app cannot see.
  if (data.issue === null) throw new LinearRefused("Linear has no such issue", 404);
  const teamId = data.issue?.team?.id;
  const projectId = data.issue?.project?.id ?? null;
  if (typeof teamId !== "string" || (projectId !== null && typeof projectId !== "string"))
    throw new LinearRefused("Linear sent an unusable issue answer", 502);
  return { teamId, projectId };
}
