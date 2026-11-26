# Linear Delegation, Worker Side: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The cloud Worker lets a Linear workspace install Arakun as an agent, hears when an issue is delegated, and hands the issue's id to the right person's computer over the relay, queued while the computer is off. It posts to Linear what the computer reports.

**Architecture:** Routes under `/linear` in the account Worker, with three D1 tables for workspaces, links and routes and one for deliveries. The Relay Durable Object keeps undelivered deliveries in its own storage and sends them on channel 0 until the computer accepts. The Worker stores ids and encrypted tokens only; issue text, prompts and progress text pass through or are dropped.

**Tech Stack:** TypeScript on Cloudflare Workers, D1, Durable Objects, Web Crypto, vitest with `@cloudflare/vitest-plugin`. Linear's OAuth and GraphQL APIs.

**Spec:** `docs/superpowers/specs/2026-11-25-linear-integration-design.md`, "Part 2: delegation", "On the Worker". Issue: ARAKUN-123 (GitHub #942).

This is plan 2 of 3. Plan 3 (the app side, ARAKUN-124) is stacked on this one.

## Global Constraints

- Nothing with text is stored: no issue title, description, comment, prompt, activity body or `promptContext` in D1, Durable Object storage, KV or logs. Ids, tokens, times and machine labels only.
- Linear tokens are stored encrypted (AES-GCM) under the `LINEAR_TOKEN_KEY` secret. No token, signing secret or webhook body is logged.
- Without all of `LINEAR_CLIENT_ID`, `LINEAR_CLIENT_SECRET`, `LINEAR_WEBHOOK_SECRET` and `LINEAR_TOKEN_KEY`, every `/linear` path answers 404.
- Every `/linear` route except `POST /linear/webhook` and `GET /linear/callback` needs a signed-in session with the user flag `linear` (`session(request, env, { flag: "linear" })`, 401 when not signed in, 403 when blocked or not flagged).
- Errors are `{ "error": "..." }` through `error(status, message)` from `src/util.ts`.
- The webhook answers within Linear's 5 seconds: every call to Linear from it has a 3 second timeout (`AbortSignal.timeout(3000)`).
- Phone traffic is untouched: only channel 0 control frames are added, and they do not count toward a plan's limits.
- Times in D1 are Unix seconds. Migrations are `NNNN_snake_name.sql`; this one is `0013_linear.sql`. A new table is added to the DELETE list in `cloud/test/setup.ts`.
- Tests run with `(cd cloud && npm test -- <file>)`; types with `(cd cloud && npm run check)`. Outbound calls are faked in `cloud/test/helpers.ts`; an unknown URL must still throw.
- Comments say why. Docs use plain, short sentences. Commits follow Conventional Commits, scope `cloud`.
- Do not change code behind the `team` or `coderlabs` features, and do not change `cloud/PROTOCOL.md` (it is the teams protocol).
- Repeats are dropped by the agent session's id (a delegation) or the activity's id (a follow-up), not by `webhookId`, which names the webhook and is the same on every event. The `webhook_id` column in the task text below became `event_key` (`created:<agent session id>` or `activity:<activity id>`).

## Wire shapes (shared by all tasks)

Control frame to the computer, channel 0:

```json
{ "type": "linear", "id": "<32 hex>", "kind": "created" | "prompt" | "stop",
  "agent_session": "<uuid>", "issue": "<uuid>", "organization": "<uuid>",
  "activity": "<uuid>" | null, "body": "<text>" | null }
```

`body` is the follow-up's text. It is present only on the frame sent the moment a `prompt` arrives while the computer is connected. It is never stored, so a frame sent again after a reconnect has `"body": null`.

Activities the Worker posts itself, with this exact copy (`{machine}` is the computer's name from `machines.name`):

| When | Type | Body |
|---|---|---|
| No `creatorId` | `error` | `Arakun starts work only for a person. Delegate this issue yourself.` |
| No issue on the session | `error` | `Arakun works on issues. Mention it on an issue instead.` |
| Creator not linked | `error` | `The person who delegated this has not connected Linear in Arakun. In Arakun, open the Linear tab and turn on "Let Linear start sessions on this computer".` |
| No route matches | `error` | `No computer is set up for this team. In Arakun's Linear tab, map the team or its project to a project on your computer.` |
| Delivered, computer connected | `thought` | `Sent to {machine}. A session starts there now.` |
| Delivered, computer away | `thought` | `{machine} is offline. The session starts when it is back, for up to 7 days.` |
| Dropped after 7 days | `error` | `{machine} stayed offline for 7 days, so this was dropped. Delegate it again when the computer is on.` |
| Follow-up from someone else | `thought` | `Only the person who delegated this can steer it.` |
| Follow-up for an unknown session | `error` | `Arakun has no session for this. Delegate the issue again.` |

## Review Focus

1. **The same webhook twice** (Linear retries, or a replay inside the 60 seconds). One delivery row, one frame id, one activity. Test in Task 6.
2. **A forged, altered or stale webhook** (wrong signature, one byte of the body changed, timestamp 61 seconds old, no signature header). 401, nothing stored, no call to Linear. Test in Task 6.
3. **A computer that reconnects, accepts twice, or never accepts.** A delivery is sent again after every claim until accepted; a second accept is 204; an accept from another account is 404; after 7 days it is dropped with the error activity. Tests in Tasks 5 and 7.
4. **A Linear token that expired, or a refresh that loses a race.** The call refreshes once and retries; two refreshes at once leave one valid token pair in D1; a refresh Linear refuses surfaces as a 502 to the computer and a 500 to the webhook (so Linear retries), never as a crash. Test in Task 2.
5. **Text never rests on the server.** After a `created` webhook carrying `promptContext` and a `prompt` carrying a body, no D1 row and no value in the Relay object's storage contains either string. Test in Task 7.

---

### Task 1: Tables, bindings, the gate and token sealing

**Files:**
- Create: `cloud/migrations/0013_linear.sql`, `cloud/src/linear.ts`, `cloud/src/linear_seal.ts`, `cloud/test/linear.test.ts`
- Modify: `cloud/src/env.d.ts`, `cloud/test/env.d.ts`, `cloud/vitest.config.ts` (test bindings), `cloud/wrangler.toml` (`LINEAR_LIMIT` rate limit under both environments, ids 1015 and 1016, 30 per 60 seconds; update the comment that lists the ids), `cloud/src/teams-router.ts` (`"linear"` in `API`; `linear(request, env, url)` in the handler chain after `push`), `cloud/test/setup.ts` (DELETE the four tables, children first)

**Interfaces:**
- Produces:
  - Tables:
    - `linear_workspaces(organization_id TEXT PRIMARY KEY, name TEXT NOT NULL, app_user_id TEXT NOT NULL, access_token TEXT NOT NULL, refresh_token TEXT NOT NULL, expires_at INTEGER NOT NULL, installed_by INTEGER NOT NULL REFERENCES users(github_id), installed_at INTEGER NOT NULL)`
    - `linear_links(organization_id TEXT NOT NULL, linear_user_id TEXT NOT NULL, github_id INTEGER NOT NULL REFERENCES users(github_id) ON DELETE CASCADE, linked_at INTEGER NOT NULL, PRIMARY KEY (organization_id, linear_user_id))` plus an index on `github_id`
    - `linear_routes(label TEXT NOT NULL REFERENCES machines(label) ON DELETE CASCADE, github_id INTEGER NOT NULL, organization_id TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('team','project')), target_id TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (label, organization_id, kind, target_id))` plus an index on `(organization_id, github_id, target_id)`
    - `linear_deliveries(id TEXT PRIMARY KEY, webhook_id TEXT NOT NULL UNIQUE, agent_session_id TEXT NOT NULL, organization_id TEXT NOT NULL, issue_id TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('created','prompt','stop')), activity_id TEXT, github_id INTEGER NOT NULL, label TEXT NOT NULL, created_at INTEGER NOT NULL, announced_at INTEGER, accepted_at INTEGER, expired_at INTEGER)` plus an index on `(agent_session_id, kind)`
  - `export async function linear(request: Request, env: Env, url: URL): Promise<Response | null>` in `src/linear.ts`: null when the path is not under `/linear`; 404 when not configured.
  - `export function configured(env: Env): boolean`
  - `export async function signedIn(request: Request, env: Env): Promise<Session | Response>` (the 401/403 rule above)
  - `export const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/`
  - In `src/linear_seal.ts`: `export async function seal(env: Env, organizationId: string, plain: string): Promise<string>` and `export async function open(env: Env, organizationId: string, sealed: string): Promise<string | null>`. Format `v1.<b64url iv>.<b64url ciphertext>`, a random 12-byte IV, the organization id as additional data. `LINEAR_TOKEN_KEY` is 32 bytes in base64url.
  - Env: `LINEAR_CLIENT_ID?`, `LINEAR_CLIENT_SECRET?`, `LINEAR_WEBHOOK_SECRET?`, `LINEAR_TOKEN_KEY?`, `LINEAR_LIMIT: RateLimit`. Test bindings: `LINEAR_CLIENT_ID: "lin-test"`, `LINEAR_CLIENT_SECRET: "lin-secret"`, `LINEAR_WEBHOOK_SECRET: "lin-hook"`, `LINEAR_TOKEN_KEY: <32 zero bytes in base64url>`.

- [ ] **Step 1: Write the failing tests** in `cloud/test/linear.test.ts`

```ts
it("is not a page: an unknown /linear path is a JSON 404", async () => {
  const res = await call("GET", "/linear/nope");
  expect(res.status).toBe(404);
  expect(await res.json()).toEqual({ error: "not found" });
});
it("needs a session and the linear flag", async () => {
  expect((await call("GET", "/linear/link")).status).toBe(401);
  const u = await signIn(1, "ada");
  expect((await call("GET", "/linear/link", as(u.session))).status).toBe(403);
});
it("seals a token so only the same workspace opens it", async () => {
  const sealed = await seal(env, "org-1", "lin_oauth_abc");
  expect(sealed).toMatch(/^v1\.[\w-]+\.[\w-]+$/);
  expect(sealed).not.toContain("lin_oauth_abc");
  expect(await open(env, "org-1", sealed)).toBe("lin_oauth_abc");
  expect(await open(env, "org-2", sealed)).toBeNull();
  expect(await open(env, "org-1", sealed.slice(0, -2) + "AA")).toBeNull();
  expect(await seal(env, "org-1", "lin_oauth_abc")).not.toBe(sealed); // a fresh IV each time
});
```

Add a helper `flagged(id, login)` to `cloud/test/helpers.ts`: signs in and gives the user the `linear` flag through the admin route, as the teams tests do for `teams`.

- [ ] **Step 2: Run and see them fail.** `(cd cloud && npm test -- test/linear.test.ts)`. Expected: the module does not exist.
- [ ] **Step 3: Implement** the migration, bindings, `linear()` with only the gate and a `GET /linear/link` that answers `{ "links": [] }` for now, and `linear_seal.ts`.
- [ ] **Step 4: Run** `(cd cloud && npm test -- test/linear.test.ts test/router.test.ts && npm run check)`. Expected: pass.
- [ ] **Step 5: Commit** `feat(cloud): add the Linear tables, bindings and token sealing`

---

### Task 2: Talking to Linear

**Files:**
- Create: `cloud/src/linear_api.ts`, `cloud/test/linear_api.test.ts`
- Modify: `cloud/test/helpers.ts` (a fake Linear inside the existing fetch mock, before the "unexpected fetch" throw)

**Interfaces:**
- Consumes: `seal`, `open`.
- Produces, in `src/linear_api.ts`:
  - `export class LinearRefused extends Error { status: number }` for any answer that is not usable.
  - `export async function exchangeCode(env: Env, code: string, redirectUri: string): Promise<{ access: string; refresh: string; expiresAt: number }>` (`POST https://api.linear.app/oauth/token`, form-encoded, `grant_type=authorization_code`)
  - `export async function viewer(token: string): Promise<{ userId: string; organizationId: string; organizationName: string }>`. A token starting `lin_api_` goes as the bare `Authorization` header; any other as `Bearer`.
  - `export async function saveWorkspace(env: Env, w: { organizationId: string; name: string; appUserId: string; access: string; refresh: string; expiresAt: number; installedBy: number }): Promise<void>` (upsert; tokens sealed)
  - `export async function asApp<T>(env: Env, organizationId: string, query: string, variables: Record<string, unknown>): Promise<T>`: reads the workspace, refreshes when `expires_at` is under 5 minutes away, sends the query as `Bearer`, and on a 401 refreshes once and retries. Throws `LinearRefused` when there is no workspace (status 404) or Linear refuses.
  - `export async function postActivity(env: Env, organizationId: string, agentSessionId: string, type: "thought" | "elicitation" | "response" | "error", body: string): Promise<void>` (`agentActivityCreate` with `content: { type, body }`)
  - `export async function addLinks(env: Env, organizationId: string, agentSessionId: string, links: { label: string; url: string }[]): Promise<void>` (`agentSessionUpdate` with `addedExternalUrls`)
  - `export async function issuePlace(env: Env, organizationId: string, issueId: string): Promise<{ teamId: string; projectId: string | null }>`
- Produces, in `test/helpers.ts`: `export const linearFake` with `tokens: Map<string, { userId; organizationId; organizationName }>`, `issues: Map<string, { teamId; projectId }>`, `calls: { query: string; variables: unknown; token: string }[]`, `exchanges: URLSearchParams[]`, `refuse: number | null`, `expireNext: boolean`, and `activities(): { agentSessionId: string; type: string; body: string }[]`. Reset in `fakeGitHub()`.

Refresh rule: `POST /oauth/token` with `grant_type=refresh_token`. Write the new pair with `UPDATE linear_workspaces SET ... WHERE organization_id = ? AND refresh_token = ?` naming the sealed value that was read. If no row changed, another request refreshed first: read the row again and use its token. Linear accepts the same refresh token again for 30 minutes, so the loser's call is harmless.

- [ ] **Step 1: Write the failing tests**

```ts
it("sends an API key bare and an OAuth token as Bearer", async () => {
  linearFake.tokens.set("lin_api_k", who); linearFake.tokens.set("lin_oauth_t", who);
  await viewer("lin_api_k"); await viewer("lin_oauth_t");
  expect(authHeaders()).toEqual(["lin_api_k", "Bearer lin_oauth_t"]);
});
it("stores a workspace's tokens sealed", async () => {
  await saveWorkspace(env, workspace({ access: "A1", refresh: "R1" }));
  const row = await env.DB.prepare("SELECT access_token, refresh_token FROM linear_workspaces").first<Record<string, string>>();
  expect(row!.access_token.startsWith("v1.")).toBe(true);
  expect(JSON.stringify(row)).not.toMatch(/A1|R1/);
});
it("refreshes a token that is about to expire, once", async () => {
  await saveWorkspace(env, workspace({ expiresAt: nowSeconds() + 60 }));
  await postActivity(env, ORG, SESSION, "thought", "hi");
  expect(linearFake.exchanges.map((e) => e.get("grant_type"))).toEqual(["refresh_token"]);
  expect(linearFake.activities()).toEqual([{ agentSessionId: SESSION, type: "thought", body: "hi" }]);
});
it("refreshes and retries after a 401", async () => {
  await saveWorkspace(env, workspace({}));
  linearFake.expireNext = true;
  await postActivity(env, ORG, SESSION, "thought", "hi");
  expect(linearFake.activities()).toHaveLength(1);
});
it("two refreshes at once leave one valid pair", async () => {
  await saveWorkspace(env, workspace({ expiresAt: nowSeconds() + 60 }));
  await Promise.all([postActivity(env, ORG, SESSION, "thought", "a"), postActivity(env, ORG, SESSION, "thought", "b")]);
  expect(linearFake.activities()).toHaveLength(2);
  await postActivity(env, ORG, SESSION, "thought", "c"); // the stored pair still works
  expect(linearFake.activities()).toHaveLength(3);
});
it("a refusal is LinearRefused, not a crash", async () => {
  await saveWorkspace(env, workspace({}));
  linearFake.refuse = 500;
  await expect(postActivity(env, ORG, SESSION, "thought", "hi")).rejects.toBeInstanceOf(LinearRefused);
  await expect(postActivity(env, "no-such-org", SESSION, "thought", "hi")).rejects.toMatchObject({ status: 404 });
});
it("reads an issue's team and project", async () => {
  await saveWorkspace(env, workspace({}));
  linearFake.issues.set(ISSUE, { teamId: TEAM, projectId: null });
  expect(await issuePlace(env, ORG, ISSUE)).toEqual({ teamId: TEAM, projectId: null });
});
```

- [ ] **Step 2: Run and see them fail.** `(cd cloud && npm test -- test/linear_api.test.ts)`
- [ ] **Step 3: Implement** `linear_api.ts` and the fake. Every fetch carries `signal: AbortSignal.timeout(3000)`. Errors never include a token.
- [ ] **Step 4: Run** the file and `npm run check`. Expected: pass.
- [ ] **Step 5: Commit** `feat(cloud): call Linear as the Arakun app with refreshed tokens`

---

### Task 3: Installing Arakun in a workspace

**Files:**
- Modify: `cloud/src/linear.ts`, `cloud/test/linear.test.ts`

**Interfaces:**
- Consumes: `exchangeCode`, `viewer`, `saveWorkspace`, `signedIn`.
- Produces:
  - `GET /linear/install` → 302 to `https://linear.app/oauth/authorize?client_id=…&redirect_uri=https://<host>/linear/callback&response_type=code&scope=read,write,app:assignable,app:mentionable&actor=app&state=<state>`, and sets the cookie `__Host-arakun_linear_state=<state>.<github id>` (`HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=600`). `state` is 16 random bytes in base64url.
  - `GET /linear/callback?code&state` → checks the cookie's state and that the signed-in account is the one in it, exchanges the code, reads `viewer`, saves the workspace, clears the cookie, and answers 200 `text/plain`: `Arakun is installed in <workspace name>. You can close this page.` A missing or wrong state is 400 `{ "error": "bad state" }`. `?error=` from Linear is 400 `{ "error": "linear refused the install" }`.

- [ ] **Step 1: Write the failing tests**: the redirect has every parameter above and `actor=app`; the callback with the matching cookie stores one sealed workspace with `installed_by` the caller and answers the sentence; a callback with another state, with no cookie, or signed in as another account is 400 and stores nothing; `?error=access_denied` is 400; installing the same workspace twice leaves one row with the new tokens.
- [ ] **Step 2: Run and see them fail.** `(cd cloud && npm test -- test/linear.test.ts)`
- [ ] **Step 3: Implement.** The callback is a GET with a cookie session; `session()` already allows that for GET.
- [ ] **Step 4: Run** the file and `npm run check`.
- [ ] **Step 5: Commit** `feat(cloud): install Arakun as an agent in a Linear workspace`

---

### Task 4: Linking a person and saying what a computer takes

**Files:**
- Modify: `cloud/src/linear.ts`, `cloud/test/linear.test.ts`

**Interfaces:**
- Consumes: `viewer`, `signedIn`, `UUID`.
- Produces:
  - `POST /linear/link` body `{ "token": "<the user's Linear token or API key>" }` (at most 512 bytes) → calls `viewer(token)` once, upserts `linear_links`, answers `{ "organization_id", "linear_user_id", "installed": boolean }`. The token is not stored or logged. A token Linear refuses is 400 `{ "error": "linear refused that token" }`. Counted by `LINEAR_LIMIT` per account (429 `{ "error": "too many requests" }`).
  - `GET /linear/link` → `{ "links": [{ "organization_id", "linear_user_id", "installed" }] }` for the caller.
  - `DELETE /linear/link/<organization_id>` → 204; also deletes the caller's routes for that workspace.
  - `PUT /linear/routes` body `{ "label", "organization_id", "teams": [uuid…], "projects": [uuid…] }` → replaces the rows for that label and workspace in one batch, 204. The label must be a machine of the caller (404 `{ "error": "no such computer" }`), the caller must have a link for the workspace (409 `{ "error": "link linear first" }`), every id must match `UUID` and there are at most 200 ids (400 `{ "error": "bad routes" }`).
  - `export async function routeFor(env: Env, githubId: number, organizationId: string, place: { teamId: string; projectId: string | null }): Promise<{ label: string; machine: string } | null>`: a project row wins over a team row; among equals, the machine with the newest `last_seen_at`.

- [ ] **Step 1: Write the failing tests**

```ts
it("links the Linear user the token belongs to and forgets the token", async () => {
  const u = await flagged(1, "ada"); linearFake.tokens.set("lin_api_ada", { userId: LU, organizationId: ORG, organizationName: "Acme" });
  const res = await call("POST", "/linear/link", as(u.session), JSON.stringify({ token: "lin_api_ada" }));
  expect(await res.json()).toEqual({ organization_id: ORG, linear_user_id: LU, installed: false });
  expect(await dump(env.DB)).not.toContain("lin_api_ada");   // dump: every row of every table as JSON
});
it("a second account proving the same Linear user takes the link over", …);   // one row, the new github_id
it("refuses a token Linear refuses", …);                                      // 400, no row
it("replaces a computer's routes", …);            // PUT twice; the second list is all that remains
it("refuses routes for a computer of another account, an unlinked workspace, or a bad id", …); // 404, 409, 400
it("picks the project route over the team route, then the computer seen last", async () => {
  // two computers of one account: A maps the team, B maps the project
  expect(await routeFor(env, 1, ORG, { teamId: TEAM, projectId: PROJECT })).toMatchObject({ label: B });
  expect(await routeFor(env, 1, ORG, { teamId: TEAM, projectId: null })).toMatchObject({ label: A });
  expect(await routeFor(env, 2, ORG, { teamId: TEAM, projectId: PROJECT })).toBeNull(); // another account's routes never match
});
it("forgetting a computer drops its routes", …);  // DELETE /machines/<label>, then routeFor is null
it("unlinking drops that workspace's routes", …);
```

- [ ] **Step 2: Run and see them fail.**
- [ ] **Step 3: Implement.** Machines for tests come from `connectedComputer(owner)` in `test/relay/helpers.ts`.
- [ ] **Step 4: Run** the file and `npm run check`.
- [ ] **Step 5: Commit** `feat(cloud): link a Linear user to an account and register a computer's routes`

---

### Task 5: The relay's queue for a computer

**Files:**
- Modify: `cloud/src/relay/relay.ts`, `cloud/src/machines.ts`
- Create: `cloud/test/relay/linear.test.ts`

**Interfaces:**
- Produces, in `src/machines.ts`:
  - `export interface Delivery { id: string; kind: "created" | "prompt" | "stop"; agent_session: string; issue: string; organization: string; activity: string | null; created: number }`
  - `export async function deliver(env: Env, label: string, d: Delivery, body: string | null): Promise<{ online: boolean }>` (`POST https://relay/_linear`)
  - `export async function dropDelivery(env: Env, label: string, id: string): Promise<void>` (`POST https://relay/_linear_drop`)
- Produces, in the Relay object:
  - `POST /_linear`: stores the delivery under the storage key `linear:<id>` (without `body`), sends the frame of "Wire shapes" (with `body`) to a claimed computer, answers `{ online }`. Storing the same id again changes nothing and sends nothing.
  - `POST /_linear_drop`: deletes `linear:<id>`.
  - After `claimed` is sent in `claim()`, every stored delivery is sent, oldest first, with `"body": null`.
  - `alarm()`: a stored delivery older than `LINEAR_KEEP_MS = 7 * 24 * 60 * 60 * 1000` is deleted and `expired(env, id)` from `src/linear.ts` is called for it (Task 7 writes it; until then a stub that does nothing). `schedule()` counts the next expiry among its times. The 90-day purge stays as it is.
  - At most `LINEAR_QUEUE_MAX = 50` stored deliveries per computer; `/_linear` answers 429 beyond that and stores nothing.

- [ ] **Step 1: Write the failing tests**

```ts
it("sends a delivery to a connected computer and keeps it until dropped", async () => {
  const { me, pc } = await connectedComputer();
  expect(await deliver(env, me.label, D1, null)).toEqual({ online: true });
  expect(await pc.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
  expect(await stored(me.label, `linear:${D1.id}`)).toBeDefined();
  await dropDelivery(env, me.label, D1.id);
  expect(await stored(me.label, `linear:${D1.id}`)).toBeUndefined();
});
it("keeps a delivery for a computer that is away and sends it after the claim", async () => {
  const owner = await account(); const me = await computer();
  // the label must be registered first: connect once, then close
  …
  expect(await deliver(env, me.label, D1, null)).toEqual({ online: false });
  const pc = await openPc(me.label, owner.session); await claim(pc, me);
  expect(await pc.inbox.control()).toEqual({ type: "linear", ...frame(D1), body: null });
});
it("sends it again after every claim until it is dropped, oldest first", …);
it("sends a prompt's body live and never stores it", async () => {
  await deliver(env, me.label, PROMPT, "make it blue");
  expect((await pc.inbox.control()).body).toBe("make it blue");
  expect(JSON.stringify(await stored(me.label, `linear:${PROMPT.id}`))).not.toContain("make it blue");
  // reconnect: the same frame, body null
});
it("the same id twice is one frame", …);
it("drops a delivery after 7 days and tells linear.ts", …);   // runDurableObjectAlarm with the clock moved; spy on `expired`
it("refuses the 51st queued delivery", …);
it("a linear frame does not count toward the plan's frames", …); // as limits.test.ts measures them
it("still closes on a control frame from the computer on channel 0", …); // unchanged behaviour
```

- [ ] **Step 2: Run and see them fail.** `(cd cloud && npm test -- test/relay/linear.test.ts)`
- [ ] **Step 3: Implement.** Follow `/_recheck` for the internal routes (they need `x-arakun-label`). A frame is only sent to a socket whose attachment has `claimed: true`.
- [ ] **Step 4: Run** `(cd cloud && npm test -- test/relay && npm run check)`. Expected: the whole relay folder passes.
- [ ] **Step 5: Commit** `feat(cloud): queue Linear deliveries in the relay until the computer takes them`

---

### Task 6: The webhook, for a delegated issue

**Files:**
- Create: `cloud/src/linear_webhook.ts`, `cloud/test/linear_webhook.test.ts`
- Modify: `cloud/src/linear.ts` (route `POST /linear/webhook`), `cloud/test/helpers.ts` (`signedWebhook(body, { secret?, at? })`)

**Interfaces:**
- Consumes: `asApp`, `postActivity`, `issuePlace`, `routeFor`, `deliver`, `UUID`.
- Produces: `export async function webhook(request: Request, env: Env): Promise<Response>`.

Rules, in order:

1. Read at most 1 MB as text (413 beyond). Verify `Linear-Signature`: hex HMAC-SHA256 of the raw text with `LINEAR_WEBHOOK_SECRET`, compared as `admin.ts` compares secrets (hash both sides, `timingSafeEqual`). Wrong or missing: 401.
2. Parse. `webhookTimestamp` (milliseconds) must be within 60 seconds of now: else 401.
3. `Linear-Event` other than `AgentSessionEvent`, or an `action` other than `created` and `prompted`: 200 and nothing else.
4. `organizationId`, `agentSession.id` and `webhookId` must match `UUID`: else 400.
5. For `created`:
   - no `agentSession.creatorId` → the "no creator" activity, 200.
   - no `agentSession.issueId` → the "no issue" activity, 200.
   - no link for (`organizationId`, `creatorId`) → the "not linked" activity, 200.
   - `issuePlace`, then `routeFor`; none → the "no route" activity, 200.
   - `INSERT INTO linear_deliveries … ON CONFLICT (webhook_id) DO NOTHING`, then read the row by `webhook_id`. If it has `announced_at`, answer 200 and stop: this webhook was handled.
   - `deliver(env, label, delivery, null)`, post the "delivered" thought for `online`, set `announced_at`, 200.
6. Any `LinearRefused` or thrown error after step 4: log the status only and answer 500, so Linear tries again. The retry lands on the same delivery row.

Nothing from `promptContext`, `agentSession.issue.title`, `description`, `comment` or `previousComments` is read into a variable that outlives the request, stored or logged.

- [ ] **Step 1: Write the failing tests**

```ts
it("refuses a webhook that is unsigned, wrongly signed, altered or old", async () => {
  for (const bad of [unsigned(body), signedWebhook(body, { secret: "other" }), altered(signedWebhook(body)), signedWebhook(body, { at: Date.now() - 61_000 })])
    expect((await send(bad)).status).toBe(401);
  expect(await count("linear_deliveries")).toBe(0);
  expect(linearFake.calls).toEqual([]);
});
it("sends a delegated issue to the delegator's computer and says so", async () => {
  const { me, pc } = await linkedComputerWithRoute({ team: TEAM });     // helper: flagged account, workspace, link, route
  expect((await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE })))).status).toBe(200);
  expect(await pc.inbox.control()).toMatchObject({ type: "linear", kind: "created", agent_session: SESSION, issue: ISSUE, organization: ORG });
  expect(linearFake.activities()).toEqual([{ agentSessionId: SESSION, type: "thought", body: "Sent to Desk. A session starts there now." }]);
});
it("says the computer is offline and keeps the delivery", …);  // body "Desk is offline. The session starts when it is back, for up to 7 days."
it("the same webhook twice is one delivery and one activity", async () => {
  const hook = signedWebhook(created({ creatorId: LU, issueId: ISSUE }));
  await send(hook); await send(hook);
  expect(await count("linear_deliveries")).toBe(1);
  expect(linearFake.activities()).toHaveLength(1);
});
it("a retry after Linear refused the activity finishes the same delivery", async () => {
  linearFake.refuse = 500; expect((await send(hook)).status).toBe(500);
  linearFake.refuse = null; expect((await send(hook)).status).toBe(200);
  expect(await count("linear_deliveries")).toBe(1);
  expect(linearFake.activities()).toHaveLength(1);
});
it.each([
  ["no creator", created({ creatorId: null, issueId: ISSUE }), "Arakun starts work only for a person. Delegate this issue yourself."],
  ["no issue", created({ creatorId: LU, issueId: null }), "Arakun works on issues. Mention it on an issue instead."],
  ["an unlinked creator", created({ creatorId: OTHER, issueId: ISSUE }), 'The person who delegated this has not connected Linear in Arakun. In Arakun, open the Linear tab and turn on "Let Linear start sessions on this computer".'],
  ["no route", created({ creatorId: LU, issueId: UNMAPPED }), "No computer is set up for this team. In Arakun's Linear tab, map the team or its project to a project on your computer."],
])("answers %s with an error in the issue and stores nothing", async (_n, body, text) => {
  expect((await send(signedWebhook(body))).status).toBe(200);
  expect(linearFake.activities()).toEqual([{ agentSessionId: SESSION, type: "error", body: text }]);
  expect(await count("linear_deliveries")).toBe(0);
});
it("ignores other events and actions", …);          // 200, no calls
it("never delivers to another account's computer", …); // two accounts map the same team; only the creator's computer hears
```

- [ ] **Step 2: Run and see them fail.** `(cd cloud && npm test -- test/linear_webhook.test.ts)`
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the file and `npm run check`.
- [ ] **Step 5: Commit** `feat(cloud): take Linear's webhook and deliver a delegated issue`

---

### Task 7: Follow-ups, and what the computer sends back

**Files:**
- Modify: `cloud/src/linear_webhook.ts`, `cloud/src/linear.ts`, `cloud/test/linear_webhook.test.ts`, `cloud/test/linear.test.ts`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - In the webhook, for `prompted`: find the `created` delivery for `agentSession.id` (none → the "unknown session" activity, 200). The author is `agentActivity.userId` (check the name against Linear's SDK schema, `packages/sdk/src/schema.graphql` in `linear/linear`; an author that cannot be read counts as someone else). Author's link is not the delivery's `github_id` → the "someone else" thought, 200. Otherwise insert a delivery of kind `stop` when `agentActivity.signal` is `"stop"`, else `prompt`, with `activity_id`, to the same label, and `deliver(…, body)` where `body` is `agentActivity.body` for a prompt and null for a stop. No activity is posted. The same `ON CONFLICT (webhook_id)` rule applies.
  - `POST /linear/deliveries/<id>/accept` → the delivery must be the caller's (404 `{ "error": "no such delivery" }`), sets `accepted_at` once, calls `dropDelivery`, 204. A second accept is 204.
  - `POST /linear/sessions/<agent_session>/activity` body `{ "type": "thought" | "elicitation" | "response" | "error", "body": string (1 to 8000 chars), "links"?: [{ "label": string (1 to 80), "url": "https://…" (at most 2048) }] (at most 5) }` → the caller must own a `created` delivery for that session (404 `{ "error": "no such session" }`); `postActivity`, then `addLinks` when there are links; 204. Bad input is 400 `{ "error": "bad activity" }`; `LinearRefused` is 502 `{ "error": "linear refused" }`; counted by `LINEAR_LIMIT`.
  - `export async function expired(env: Env, id: string): Promise<void>` in `src/linear.ts`: sets `expired_at` on the delivery and, for a `created` one, posts the "dropped" error with the machine's name (`this computer` if the machine is gone). Errors are swallowed and logged by status: an alarm must not fail.

- [ ] **Step 1: Write the failing tests**

```ts
it("sends the delegator's follow-up to the same computer with its text, and stores no text", async () => {
  await send(signedWebhook(created({ creatorId: LU, issueId: ISSUE, promptContext: "SECRET-CONTEXT" }))); await pc.inbox.control();
  await send(signedWebhook(prompted({ userId: LU, body: "SECRET-FOLLOW-UP", activityId: ACT })));
  expect(await pc.inbox.control()).toMatchObject({ type: "linear", kind: "prompt", activity: ACT, body: "SECRET-FOLLOW-UP" });
  const everything = (await dump(env.DB)) + JSON.stringify(await storedAll(me.label));
  expect(everything).not.toMatch(/SECRET-CONTEXT|SECRET-FOLLOW-UP/);
});
it("a stop arrives as a stop with no text", …);                 // kind "stop", body null
it("someone else's follow-up never reaches the computer", async () => {
  await send(signedWebhook(prompted({ userId: OTHER_LINKED, body: "do this instead" })));
  expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "thought", body: "Only the person who delegated this can steer it." });
  await expect(pc.inbox.nothingFor(200)).resolves.toBeUndefined();
});
it("a follow-up with no readable author is treated as someone else's", …);
it("a follow-up for a session we never saw gets an error", …);  // "Arakun has no session for this. Delegate the issue again."
it("accepting removes the delivery from the relay, once or twice, and only for its owner", async () => {
  expect((await call("POST", `/linear/deliveries/${id}/accept`, as(owner.session))).status).toBe(204);
  expect((await call("POST", `/linear/deliveries/${id}/accept`, as(owner.session))).status).toBe(204);
  expect((await call("POST", `/linear/deliveries/${id}/accept`, as(stranger.session))).status).toBe(404);
  expect(await stored(me.label, `linear:${id}`)).toBeUndefined();
});
it("posts the computer's activity as Arakun, with links", async () => {
  const res = await call("POST", `/linear/sessions/${SESSION}/activity`, as(owner.session),
    JSON.stringify({ type: "response", body: "Done.", links: [{ label: "Pull request", url: "https://github.com/a/b/pull/1" }] }));
  expect(res.status).toBe(204);
  expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "response", body: "Done." });
  expect(linearFake.links()).toEqual([{ agentSessionId: SESSION, label: "Pull request", url: "https://github.com/a/b/pull/1" }]);
});
it("refuses an activity for a session of another account, a bad type, an empty or oversized body, or a link that is not https", …); // 404, 400 ×4
it("a refusal from Linear is a 502 for the computer", …);
it("tells Linear when a delivery was dropped after 7 days", async () => {
  await expired(env, id);
  expect(linearFake.activities().at(-1)).toEqual({ agentSessionId: SESSION, type: "error",
    body: "Desk stayed offline for 7 days, so this was dropped. Delegate it again when the computer is on." });
});
it("expired() never throws", …);   // Linear refusing, a missing delivery, a deleted machine
```

- [ ] **Step 2: Run and see them fail.**
- [ ] **Step 3: Implement.** Replace Task 5's `expired` stub with the real import.
- [ ] **Step 4: Run** `(cd cloud && npm test && npm run check)`. Expected: the whole cloud suite passes.
- [ ] **Step 5: Commit** `feat(cloud): pass follow-ups to the computer and its progress to Linear`

---

### Task 8: Docs and the pull request

- [ ] **Step 1: `cloud/README.md`.** Add to the file map `src/linear.ts`, `src/linear_api.ts`, `src/linear_seal.ts`, `src/linear_webhook.ts`; a "Linear" section with every route, what each table holds, the `linear` control frame and its `body` rule, the copy of the Worker's own activities, and that no text is stored; the four secrets and `LINEAR_LIMIT` in the deploy section with the `wrangler secret put` lines, how to make `LINEAR_TOKEN_KEY` (`openssl rand -base64 32 | tr '+/' '-_' | tr -d '='`), and the migration command for production.
- [ ] **Step 2: Known gap, written down.** If Linear's payload cannot tell a person's delegation from an automation's (the docs say an automation's `creator` is the assignee), say so in the README section and in a comment on ARAKUN-123, with what the spec wanted.
- [ ] **Step 3: Checks.** `(cd cloud && npm ci && npm run check && npm test)` and `(cd phone && npm ci && npm test && npm run build)`; `git status` must show no change in `phone/dist`.
- [ ] **Step 4: Open the pull request** with the `ship-pr` skill: draft, base `feat/linear-integration-both-sessions` (or `main` if that branch has merged), title `feat(cloud): deliver delegated Linear issues to a computer through the relay`, `Refs #940` and `Closes #942`, and "Stacked on #1001." at the end of Changes while it is.
- [ ] **Step 5: Update ARAKUN-123** with what shipped, and ARAKUN-126 with the exact secrets and migration steps.
