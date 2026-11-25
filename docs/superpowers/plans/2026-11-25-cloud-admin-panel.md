# Cloud admin panel: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The operator does everything `cloud/src/admin.ts` offers from a browser panel on its own host, signed in with GitHub, and every change is recorded with the operator who made it.

**Architecture:** The cloud Worker (`cloud/`) gains list routes and an append-only `admin_log`. A second Worker (`cloud/admin/`) signs operators in with its own GitHub OAuth App, keeps a 12-hour signed cookie, and forwards `/api/*` to the cloud Worker's `/admin/*` over a service binding with `ADMIN_SECRET`. The same Worker serves a React panel as static assets.

**Tech Stack:** TypeScript, Cloudflare Workers (D1, KV, service bindings, static assets), Vitest in workerd, Vite with `@cloudflare/vite-plugin`, React 19, TanStack Router (file-based) and TanStack Query, plain CSS.

**Spec:** `docs/superpowers/specs/2026-11-25-cloud-admin-panel-design.md`

## Global Constraints

- Read `AGENTS.md` first. Issue #807 tracks this work; comment there on decisions and blockers.
- Conventional Commits, scope `cloud`. One logical change per commit. End each commit message with `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.
- Three stacked branches with `gh stack` (the `stack-pr` skill): `pr/admin-lists` (Tasks 1-3), `pr/admin-worker` (Tasks 4-6), `pr/admin-panel` (Tasks 7-11). Pull requests are opened as drafts with the template; merging is a person's job.
- The browser never holds `ADMIN_SECRET` or a GitHub token. No session, secret or token in a log.
- No way in without GitHub in any deployed build. No dev bypass in the Worker's code.
- No GitHub IDs of real people in the repository. Tests use IDs such as 1001.
- Users are keyed by numeric GitHub ID, never by login.
- A schema change is a new file in `cloud/migrations`. A new route needs a test in `cloud/test`.
- The existing curl routes keep their paths, bodies and answers.
- Times are Unix seconds on the wire.
- Do not use the latest-looking API from memory for TanStack Router, TanStack Query, `@cloudflare/vite-plugin` or wrangler: check current docs with the context7 tools first.
- Checks: `(cd cloud && npm run check && npx vitest run <file>)` and `(cd cloud/admin && npm run check && npm test && npm run build)`.
- Comments say why, in plain short sentences. Interface text follows the glossary in `CONTRIBUTING.md` and the `arakun-brand` skill's voice.

## Review Focus

1. The operator's 12-hour session ends while the panel is open and they press Grant: the panel goes to sign-in and says the change was not made. Test in Task 7 (`api.test.ts`).
2. A search for `%` or `_` in Users: it must not match every login. Test in Task 2.
3. A browser sends its own `X-Arakun-Operator-Id` through `/api/*`: the record must still name the signed-in operator. Test in Task 6.
4. Grant pressed twice quickly: one grant, one log row for the first, the button disabled while the call runs. Tests in Task 1 (second grant) and Task 8 (button).
5. The cloud Worker answers 500 or the binding throws: the panel shows an error with the server's text or "the cloud did not answer", never an empty table. Tests in Task 6 (502) and Task 7.

## File Structure

```
cloud/migrations/0011_admin_log.sql      the admin_log table
cloud/src/admin_log.ts                   operator(), record(), listLog()
cloud/src/admin_lists.ts                 listUsers(), getUser(), listNotices(), listPlans()
cloud/src/admin.ts                       routes wired; each change calls record()
cloud/src/cloud_access.ts                decideRequest records; requestFor() exported
cloud/src/notices.ts                     create and delete record
cloud/test/admin_log.test.ts, cloud/test/admin_lists.test.ts

cloud/admin/package.json, wrangler.toml, vite.config.ts, vitest.config.ts, tsconfig.json, index.html
cloud/admin/worker/index.ts              the router, the assets with their policy
cloud/admin/worker/session.ts            the signed cookie and the allow-list
cloud/admin/worker/auth.ts               GitHub's web flow
cloud/admin/worker/proxy.ts              /api/* → the cloud Worker
cloud/admin/worker/util.ts               the few helpers, copied, not imported
cloud/admin/worker/env.d.ts
cloud/admin/test/*.test.ts               Worker tests (workerd)
cloud/admin/scripts/dev-cookie.mjs       signs a local session for screenshots
cloud/admin/src/main.tsx, api.ts, types.ts, queries.ts, format.ts
cloud/admin/src/routes/__root.tsx, sign-in.tsx, index.tsx, users.index.tsx,
  users.$githubId.tsx, machines.tsx, notices.tsx, activity.tsx
cloud/admin/src/components/Confirm.tsx, ActivityList.tsx, NoticeForm.tsx, Time.tsx
cloud/admin/src/styles/tokens.css, app.css
cloud/admin/src/**/*.test.ts(x)          panel tests (jsdom)
```

---

## Part 1: the cloud Worker (`pr/admin-lists`)

Start: `gh stack init pr/admin-lists` from `main`. First commit: the spec and this plan, `docs(cloud): add the admin panel design and plan`.

### Task 1: the record of who did what

**Files:**
- Create: `cloud/migrations/0011_admin_log.sql`, `cloud/src/admin_log.ts`, `cloud/test/admin_log.test.ts`
- Modify: `cloud/src/admin.ts`, `cloud/src/cloud_access.ts` (`decideRequest`), `cloud/src/notices.ts` (`createNotice`, `deleteNotice`), `cloud/test/setup.ts` (empty the new table)

**Interfaces:**
- Produces:
  ```ts
  export type LogAction = "flag.grant" | "flag.revoke" | "sessions.close" | "block" | "unblock"
    | "plan.set" | "notice.create" | "notice.delete" | "cloud.grant" | "cloud.decline";
  export interface Operator { id: number | null; login: string | null }
  /// From X-Arakun-Operator-Id (GITHUB_ID) and X-Arakun-Operator-Login (GITHUB_LOGIN). Both or neither: one bad or missing makes both null.
  export function operator(request: Request): Operator;
  export async function record(env: Env, op: Operator, action: LogAction, githubId: number | null,
    detail?: Record<string, string | number | null>): Promise<void>;
  /// GET /admin/log?github_id=&before=&limit=
  export async function listLog(env: Env, url: URL): Promise<Response>;
  ```
- `GET /admin/log` answers `{ entries: LogEntry[], next: number | null }`, newest first; `LogEntry = { id, at, operator_id, operator_login, action, github_id, login, detail }` with `login` joined from `users` (null if none) and `detail` an object or null. `limit` defaults to 50, at most 100; `before` is an entry `id`; `next` is the last `id` when more remain.
- Detail per action: `flag.*` `{flag}`; `sessions.close` `{closed}`; `block` `{reason}`; `plan.set` `{plan}`; `notice.create` `{notice_id}` (github_id from the body, null for everyone); `notice.delete` `{notice_id}`; `cloud.*` none.

Table:

```sql
CREATE TABLE admin_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  -- Null for a call made with the secret alone (curl).
  operator_id INTEGER,
  operator_login TEXT,
  action TEXT NOT NULL,
  -- The account the change is about. No foreign key: the record outlives the account.
  github_id INTEGER,
  detail TEXT
);
CREATE INDEX admin_log_by_account ON admin_log (github_id, id);
```

- [ ] **Step 1: Write the failing tests** in `cloud/test/admin_log.test.ts`, with `call` and `ADMIN` from `helpers.ts` and `OP = { ...ADMIN, "X-Arakun-Operator-Id": "1001", "X-Arakun-Operator-Login": "octocat" }`:
  - `records each change with the operator`: one call per action with `OP` on account 2002; `GET /admin/log` lists ten entries newest first, each with `operator_id: 1001`, `operator_login: "octocat"` and the detail above (`flag.grant` has `{ flag: "teams" }`, `plan.set` has `{ plan: "pro" }`).
  - `records a call without operator headers with a null operator`.
  - `does not take one operator header without the other`, and not `X-Arakun-Operator-Id: abc`: both null.
  - `records nothing for a refused change`: a decline of a request that is not pending, a 400 plan, a 404 notice delete; the log stays empty.
  - `a second grant records nothing new` (Review Focus 4): grant twice; one `cloud.grant` entry.
  - `filters by account and pages`: 60 entries; default page has 50 and `next`; `before=next` gives the other 10 and `next: null`; `github_id=2002` gives only that account's; `limit=101` and `before=x` answer 400.
  - `needs the secret`: 401 without `ADMIN`, even with the operator headers.
  - `has no route that changes it`: `DELETE /admin/log` and `PUT /admin/log/1` answer 404.
- [ ] **Step 2: Run** `(cd cloud && npx vitest run test/admin_log.test.ts)`. Expected: FAIL, no such table or route.
- [ ] **Step 3: Implement.** Add the migration and `admin_log.ts`. In `admin()`, read `const op = operator(request)` after the bearer check and pass it to each handler that changes something; each calls `record()` after its write succeeded and before it answers. `closeSessions` records even when `closed` is 0. A second grant is told from a first by the `UPDATE`'s result: record only when the status changed. `decideRequest`, `createNotice` and `deleteNotice` take `op` as a new last parameter.
- [ ] **Step 4: Run** `(cd cloud && npm run check && npx vitest run test/admin_log.test.ts test/admin.test.ts test/cloud_access.test.ts test/notices.test.ts)`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(cloud): record the operator's changes`.

### Task 2: list users and read one

**Files:**
- Create: `cloud/src/admin_lists.ts`, `cloud/test/admin_lists.test.ts`
- Modify: `cloud/src/admin.ts` (routes), `cloud/src/cloud_access.ts` (export `requestFor`)

**Interfaces:**
- Consumes: `listLog` is not used here; the panel asks `/admin/log?github_id=` itself.
- Produces:
  ```ts
  export async function listUsers(env: Env, url: URL): Promise<Response>;   // GET /admin/users?q=&after=&limit=
  export async function getUser(env: Env, idText: string): Promise<Response>; // GET /admin/users/<github_id>
  /// In cloud_access.ts: the request as GET /admin/cloud/requests lists it, or null.
  export async function requestFor(env: Env, githubId: number): Promise<RequestJson | null>;
  ```
- `GET /admin/users` → `{ users: UserRow[], next: string | null }`. `UserRow = { github_id, login, plan, blocked_at, blocked_reason, created_at, last_seen_at, flags: string[], machines: number }`. Order `last_seen_at DESC, github_id DESC`. `limit` defaults to 50, at most 100. `after` and `next` are `<last_seen_at>_<github_id>` of the last row.
- `q`: 1 to 39 characters of `[A-Za-z0-9_-]`, else 400. It matches logins that start with it, without case, and also the account whose ID it is when it is all digits. `_` and `%` are escaped in the `LIKE`.
- `GET /admin/users/<id>` → `{ user: UserRow & { email: string | null }, sessions: { id, name, kind, created_at, last_used_at, expires_at, machine_label }[], machines: { label, name, kind, created_at, last_seen_at }[], cloud_request: RequestJson | null, notices: NoticeRow[] }`; 400 for a bad ID, 404 `{ error: "no such user" }`. `NoticeRow` is Task 3's; here define and export it.

- [ ] **Step 1: Write the failing tests** in `cloud/test/admin_lists.test.ts`:
  - `lists accounts, last seen first, with flags and machine counts`.
  - `pages with the cursor`: 120 accounts, three pages of 50, 50, 20, no account twice, last `next` null.
  - `finds by login prefix without case, and by exact ID`: `q=Octo` finds `octocat`; `q=1001` finds account 1001 and a login `1001abc`.
  - `does not treat _ as a wildcard` (Review Focus 2): logins `a_b` and `axb`; `q=a_` finds only `a_b`; `q=%25` answers 400.
  - `refuses a bad limit or cursor`: `limit=0`, `limit=101`, `after=x` → 400.
  - `reads one account in full`: sessions without `token_hash` (assert the JSON text has no `token_hash` key and none of the session tokens), machines, the cloud request, the notices for it and not those for everyone.
  - `answers 404 for an account it does not have` and `400 for a bad ID`.
  - `needs the secret` for both routes.
- [ ] **Step 2: Run** `npx vitest run test/admin_lists.test.ts`. Expected: FAIL with 404s.
- [ ] **Step 3: Implement** both functions and wire them in `admin()`. Place the `GET /admin/users/<id>` match so the existing `/flags`, `/sessions`, `/block` and `/plan` routes still win.
- [ ] **Step 4: Run** `npm run check && npx vitest run test/admin_lists.test.ts test/admin.test.ts`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(cloud): list users for the operator`.

### Task 3: list notices and plans, document, open the pull request

**Files:**
- Modify: `cloud/src/admin_lists.ts`, `cloud/src/admin.ts` (routes and the header comment), `cloud/test/admin_lists.test.ts`, `cloud/README.md` (route table and the operator section), `cubic.yaml` (cloud rule)

**Interfaces:**
- Produces:
  ```ts
  export interface NoticeRow { id: string; github_id: number | null; login: string | null; text: string;
    url: string | null; created_at: number; expires_at: number | null; reads: number }
  export async function listNotices(env: Env): Promise<Response>; // GET /admin/notices → NoticeRow[], newest first, at most 500
  export async function listPlans(env: Env): Promise<Response>;   // GET /admin/plans → { plans: string[] }
  ```
- `plans`: the keys of the KV `plans` that match `PLAN` in `admin.ts`, sorted, with `free` first and always present, also when KV has no `plans` or cannot be read.

- [ ] **Step 1: Write the failing tests**: `lists notices newest first with read counts` (one for everyone read by two accounts, one for an account, an expired one still listed); `lists plan names with free first` (KV `{ pro: {}, free: {}, "Bad Name": {} }` → `["free", "pro"]`); `answers free alone without the KV key`; `needs the secret`.
- [ ] **Step 2: Run** them. Expected: FAIL with 404s.
- [ ] **Step 3: Implement**, wire the routes, list every new route in the comment at the top of `admin.ts`.
- [ ] **Step 4: Update the docs.** `cloud/README.md`: the new routes with curl examples, and what `admin_log` keeps. `cubic.yaml`: add to the teams API rule that `admin_log` is only appended to, that every admin route that changes something calls `record()`, and that the `X-Arakun-Operator-*` headers are read only after the bearer check.
- [ ] **Step 5: Run** `(cd cloud && npm run check && npx vitest run)`. Expected: PASS.
- [ ] **Step 6: Commit** `feat(cloud): list notices and plans for the operator`, then `gh stack push` and open the draft with the `ship-pr` skill: title `feat(cloud): list users, notices and plans and record the operator's changes`, `Refs #807`.

---

## Part 2: the admin Worker (`pr/admin-worker`)

Start: `gh stack add pr/admin-worker`.

### Task 4: the package and the session cookie

**Files:**
- Create: `cloud/admin/package.json` (name `arakun-admin`, private, scripts `dev`, `build`, `check` = `tsc --noEmit`, `test` = `vitest run`, `deploy`), `package-lock.json`, `wrangler.toml`, `vite.config.ts`, `vitest.config.ts`, `tsconfig.json`, `index.html`, `src/main.tsx` (a placeholder that renders "Arakun admin"), `worker/env.d.ts`, `worker/util.ts`, `worker/session.ts`, `worker/index.ts`, `test/session.test.ts`, `.gitignore` (`dist`, `.dev.vars`, `.wrangler`)

**Interfaces:**
- Produces, in `worker/env.d.ts`:
  ```ts
  interface Env { ASSETS: Fetcher; CLOUD: Fetcher; CLOUD_ORIGIN: string; GITHUB_CLIENT_ID: string;
    GITHUB_CLIENT_SECRET?: string; ADMIN_SECRET?: string; ADMIN_GITHUB_IDS?: string; SESSION_SECRET?: string;
    SIGN_IN_LIMIT: RateLimit }
  ```
- Produces, in `worker/session.ts`:
  ```ts
  export const SESSION_COOKIE = "__Host-arakun_admin";
  export const SESSION_SECONDS = 12 * 60 * 60;
  export interface Operator { githubId: number; login: string }
  /// Whether the ID is in the comma-separated list. Unset, empty or all junk: nobody.
  export function allowed(ids: string | undefined, githubId: number): boolean;
  /// The cookie's value: b64url(JSON { github_id, login, exp }) + "." + b64url(HMAC-SHA256 of the first part).
  export async function signSession(op: Operator, secret: string, now: number): Promise<string>;
  /// The operator of a request, or null: no cookie, a bad signature, expired, not on the list, or no SESSION_SECRET.
  export async function readSession(request: Request, env: Env, now: number): Promise<Operator | null>;
  export function sessionCookie(value: string): string; // HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=43200
  export function clearedCookie(): string;
  ```
- `wrangler.toml`: top level `arakun-admin-dev` with `[[services]] binding = "CLOUD", service = "arakun-cloud-dev"`, `CLOUD_ORIGIN = "http://localhost:8787"`, assets with `not_found_handling = "single-page-application"` and the Worker first for `/api/*` and `/auth/*`, a `SIGN_IN_LIMIT` of 10 per 60 seconds with a namespace ID not used in `cloud/wrangler.toml`; `[env.production]` named `arakun-admin`, `workers_dev = false`, `routes = [{ pattern = "admin.arakun.dev/*", zone_name = "arakun.dev" }]`, service `arakun-cloud`, `CLOUD_ORIGIN = "https://app.arakun.dev"`, `GITHUB_CLIENT_ID = "PLACEHOLDER"`. Check in Cloudflare's docs that a more specific route wins over `*.arakun.dev/*`, and note the source in a comment.
- `vitest.config.ts`: two projects, `worker` (`test/**`, the Cloudflare pool, bindings `ADMIN_SECRET: "test-secret"`, `SESSION_SECRET`, `ADMIN_GITHUB_IDS: "1001,1002"`, `GITHUB_CLIENT_SECRET`) and `panel` (`src/**`, jsdom).

- [ ] **Step 1: Scaffold** the package so `npm run check` and `npm run build` pass with the placeholder page and a Worker that answers 404 for `/api/*` and `/auth/*`.
- [ ] **Step 2: Write the failing tests** in `test/session.test.ts`:
  - `reads back what it signed`.
  - `refuses a changed payload, a changed signature, another secret, and a value without a dot`.
  - `refuses at and after exp`: signed at `t`, read at `t + 43199` is the operator, at `t + 43200` null.
  - `refuses an operator taken off the list`: signed for 1002, `ADMIN_GITHUB_IDS: "1001"` → null.
  - `lets nobody in without a list or a secret`: `allowed(undefined, 1001)`, `allowed("", 1001)`, `allowed("abc, ,", 1001)` false; `allowed(" 1001 ,1002", 1002)` true; `allowed("10011", 1001)` false.
  - `sets the cookie HttpOnly, Secure, SameSite=Lax, Path=/ and Max-Age=43200`.
- [ ] **Step 3: Run** `npm test`. Expected: FAIL, functions missing.
- [ ] **Step 4: Implement** `session.ts` with Web Crypto HMAC (`crypto.subtle.verify` for the compare) and `util.ts` (`b64url`, `fromB64url`, `cookie`, `error`).
- [ ] **Step 5: Run** `npm run check && npm test`. Expected: PASS.
- [ ] **Step 6: Commit** `feat(cloud): add the admin worker package and its session cookie`.

### Task 5: sign in with GitHub

**Files:**
- Create: `cloud/admin/worker/auth.ts`, `cloud/admin/test/auth.test.ts`, `cloud/admin/test/helpers.ts` (a fake GitHub, after `cloud/test/helpers.ts`)
- Modify: `cloud/admin/worker/index.ts`

**Interfaces:**
- Consumes: `signSession`, `sessionCookie`, `clearedCookie`, `allowed`, `readSession`.
- Produces:
  ```ts
  export async function authStart(request: Request, env: Env): Promise<Response>;    // GET /auth/github/start
  export async function authCallback(request: Request, env: Env): Promise<Response>; // GET /auth/github/callback
  export function logout(request: Request): Response;                                // POST /auth/logout → 204
  ```
- Routes in `index.ts` also: `GET /api/me` → `{ github_id, login }` or 401 `{ error: "not signed in" }`.
- The flow follows `cloud/src/web_auth.ts`: state cookie `__Host-arakun_admin_oauth`, 600 seconds, single use; no `scope`. The token is checked and the user read with GitHub's Applications API and then revoked, as `verifyAndRevoke` in `cloud/src/auth.ts` does, without the email call.
- Outcomes: a listed account → 302 `/` with the session cookie; an account not listed → 302 `/?sign_in=denied`, no cookie; anything else (state mismatch, refused code, GitHub down, rate limited, no client secret, no `SESSION_SECRET`) → 302 `/?sign_in=failed`. `logout` needs `Origin` equal to the request's origin, else 403.

- [ ] **Step 1: Write the failing tests** in `test/auth.test.ts`:
  - `start sends the browser to GitHub with a state and no scope`.
  - `signs a listed account in`: cookie set; `GET /api/me` with it answers `{ github_id: 1001, login: "octocat" }`; the token was revoked.
  - `turns away an account not on the list`: 302 `/?sign_in=denied`, no session cookie, token still revoked.
  - `fails without a matching state`, `with a state used twice`, `when GitHub refuses the code`, `when GitHub is down`.
  - `never puts GitHub's token in a cookie, a redirect or a body`.
  - `me answers 401 without a session`.
  - `logout clears the cookie` and `refuses another origin`.
- [ ] **Step 2: Run** `npm test -- test/auth.test.ts`. Expected: FAIL with 404s.
- [ ] **Step 3: Implement** `auth.ts` and the routes.
- [ ] **Step 4: Run** `npm run check && npm test`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(cloud): sign operators in to the admin worker with github`.

### Task 6: forward to the cloud Worker, serve the panel, CI

**Files:**
- Create: `cloud/admin/worker/proxy.ts`, `cloud/admin/test/proxy.test.ts`
- Modify: `cloud/admin/worker/index.ts`, `.github/workflows/ci.yml` (cloud job), `AGENTS.md` and `CONTRIBUTING.md` (the quick check for `cloud/admin`), `cubic.yaml`

**Interfaces:**
- Consumes: `readSession`, `Operator`.
- Produces:
  ```ts
  /// /api/<rest> → the cloud Worker's /admin/<rest>, as the operator.
  export async function proxy(request: Request, env: Env, op: Operator): Promise<Response>;
  /// The policy of the panel's pages.
  export function pagePolicy(): string;
  ```
- Rules of `proxy`:
  - No valid session → 401 `{ error: "not signed in" }` (checked in `index.ts` before the call). No `ADMIN_SECRET` → 503.
  - A method other than GET or HEAD needs `Origin` equal to the request's origin → else 403 `{ error: "the request comes from another origin" }`.
  - A body over 4096 bytes → 413.
  - The forwarded request goes to `env.CLOUD.fetch(CLOUD_ORIGIN + "/admin/" + rest + search)` with only these headers: `Authorization: Bearer <ADMIN_SECRET>`, `Content-Type` when there is a body, `X-Arakun-Operator-Id`, `X-Arakun-Operator-Login`.
  - For `PUT /api/cloud/requests/<id>/(grant|decline)` the body sent on is `{ "by": "<login>" }`.
  - The answer keeps the status, the body and `Content-Type`, and gets `Cache-Control: no-store`. If the binding throws → 502 `{ error: "the cloud did not answer" }`.
- Pages: every other GET goes to `env.ASSETS` and gets `Content-Security-Policy` from `pagePolicy()` (`default-src 'self'; script-src 'self'; connect-src 'self'; img-src 'self' https://avatars.githubusercontent.com; style-src 'self' 'unsafe-inline'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`), `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`.

- [ ] **Step 1: Write the failing tests** in `test/proxy.test.ts`, with `CLOUD` a fake that keeps the requests it got:
  - `forwards a signed-in operator's call with the secret and their name`: `GET /api/users?q=octo` reaches `/admin/users?q=octo` with the bearer and both operator headers.
  - `does not pass on the cookie or the client's headers` (Review Focus 3): the client sends `Cookie`, `X-Arakun-Operator-Id: 9`, `X-Arakun-Operator-Login: evil`, `Authorization: Bearer x`; the cloud sees the session's operator, the real secret and no `Cookie`.
  - `puts the operator's login in by, whatever the body said`: body `{ "by": "someone" }` on grant → `{ "by": "octocat" }`.
  - `answers 401 without a session and asks the cloud nothing`.
  - `refuses a change from another origin, and one without Origin`; a GET without `Origin` passes.
  - `refuses a body over 4096 bytes`.
  - `hands back the cloud's status and error text`: a 404 `{ error: "no such user" }` arrives unchanged, with `no-store`.
  - `answers 502 when the cloud does not answer` (Review Focus 5).
  - `answers 503 without the admin secret`.
  - `never sends the secret to the browser`: no response header or body holds `test-secret`.
  - `serves the page with its policy`, also for `/users/1001` (the single-page fallback).
- [ ] **Step 2: Run** `npm test -- test/proxy.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement** `proxy.ts` and finish the router in `index.ts`.
- [ ] **Step 4: Wire CI.** In the cloud job of `ci.yml`, after the cloud tests, four steps with `working-directory: cloud/admin`: `npm ci`, `npm run check`, `npm test`, `npm run build`. Add the same line to the quick checks in `AGENTS.md` and `CONTRIBUTING.md`. Add a `cubic.yaml` rule for `cloud/admin/**`: the session and the allow-list are checked on every `/api` request; `Origin` on every change; only the listed headers are forwarded; `ADMIN_SECRET` and GitHub tokens never reach a response or a log; no sign-in bypass; no real GitHub IDs.
- [ ] **Step 5: Run** `(cd cloud/admin && npm run check && npm test && npm run build)` and `zizmor .github` if installed. Expected: PASS.
- [ ] **Step 6: Commit** `feat(cloud): forward the admin worker's api to the cloud worker`, then `gh stack push` and open the draft: title `feat(cloud): add the admin worker with github sign-in`, `Refs #807`, `Stacked on #<Part 1>.`

---

## Part 3: the panel (`pr/admin-panel`)

Start: `gh stack add pr/admin-panel`. Before Task 7, load the `arakun-brand` and `frontend-design:frontend-design` skills and read TanStack Router's and Query's current docs with context7.

### Task 7: the shell: sign-in, the API client, the look

**Files:**
- Create: `src/api.ts`, `src/api.test.ts`, `src/types.ts`, `src/queries.ts`, `src/format.ts`, `src/format.test.ts`, `src/routes/__root.tsx`, `src/routes/sign-in.tsx`, `src/components/Confirm.tsx`, `src/components/Time.tsx`, `src/styles/tokens.css`, `src/styles/app.css`, `scripts/dev-cookie.mjs`
- Modify: `src/main.tsx`, `vite.config.ts` (the router plugin), `package.json`

**Interfaces:**
- Produces, in `api.ts`:
  ```ts
  export class ApiError extends Error { constructor(public status: number, message: string) }
  /// Calls /api/<path>. Throws ApiError with the server's `error` text; for a 401 it also calls onSignedOut.
  export async function api<T>(method: "GET" | "PUT" | "POST" | "DELETE", path: string, body?: unknown): Promise<T>;
  export function setOnSignedOut(handler: () => void): void;
  ```
- Produces, in `types.ts`: `Me`, `UserRow`, `UserDetail`, `SessionRow`, `MachineRow`, `CloudRequest`, `NoticeRow`, `LogEntry`, `LogAction`, matching Tasks 1-3 and `requestJson` in `cloud/src/cloud_access.ts` field for field.
- Produces, in `queries.ts`: one `queryOptions` factory per read (`meQuery`, `requestsQuery(status)`, `usersQuery({ q, after })`, `userQuery(id)`, `machinesQuery`, `noticesQuery`, `plansQuery`, `logQuery({ githubId, before })`) and one `useMutation` hook per change. Each hook invalidates the queries its change touches (the lists it appears in, `userQuery(id)`, and always `logQuery`); none edits the cache by hand.
- Produces: `<Confirm title body confirmLabel onConfirm>` (a `<dialog>`; focus starts on Cancel), `<Time at>` (relative text, the exact time in `title`), `format.ts` `age(seconds, now)` → "3 years", "5 days".
- `__root.tsx`: `beforeLoad` ensures `meQuery`; a 401 redirects to `/sign-in`. The layout: the navigation (Cloud access, Users, Machines, Notices, Activity), the operator's login and Sign out. `sign-in.tsx`: one link to `/auth/github/start`; `?sign_in=denied` shows "This GitHub account is not an operator."; `?sign_in=failed` shows "Sign-in failed. Try again."; `?signed_out=1` shows "You were signed out. The last change was not made."
- Look: tokens from the brand book (black and white, one amber accent, Geist and Geist Mono), light and dark by `prefers-color-scheme`. The fonts are loaded from `phone/fonts` through Vite, not copied. No colour or size outside `tokens.css`.
- `scripts/dev-cookie.mjs <github_id> <login>`: prints a session cookie signed with `SESSION_SECRET` from `.dev.vars`, for local screenshots. It is not part of the Worker.

- [ ] **Step 1: Write the failing tests.** `api.test.ts` with a mocked `fetch`: `sends JSON with the same-origin cookie`; `throws the server's error text`; `falls back to the status when the body is not JSON`; `calls onSignedOut on a 401 and still throws` (Review Focus 1); `shows "the cloud did not answer" from a 502` (Review Focus 5); `answers undefined for a 204`. `format.test.ts`: `age` at 0, 59 days, 13 months, 3 years.
- [ ] **Step 2: Run** `npm test -- src`. Expected: FAIL.
- [ ] **Step 3: Implement** the client, the query layer, the root route, sign-in and the styles. `main.tsx` sets `setOnSignedOut` to clear the query cache and go to `/sign-in?signed_out=1`.
- [ ] **Step 4: Run** `npm run check && npm test && npm run build`. Expected: PASS.
- [ ] **Step 5: Look at it.** Run both Workers locally (`wrangler dev` in `cloud/` with its local D1 migrated and a few seeded rows, `npm run dev` in `cloud/admin/`), set the cookie from `dev-cookie.mjs`, and screenshot sign-in and the empty shell in light and dark. Fix what looks wrong.
- [ ] **Step 6: Commit** `feat(cloud): add the admin panel's shell and sign-in`.

### Task 8: Cloud access

**Files:**
- Create: `src/routes/index.tsx`, `src/routes/index.test.tsx`

**Interfaces:**
- Consumes: `requestsQuery`, `useGrant`, `useDecline`, `Confirm`, `Time`, `age`.
- The tab is the search param `status` (`pending` by default, `granted`, `declined`), validated by the route.

- [ ] **Step 1: Write the failing tests** (Testing Library, `fetch` mocked): `shows pending requests with the GitHub signals and the reason`; `shows "not known" for a signal GitHub did not give`; `grant calls PUT /api/cloud/requests/<id>/grant and the row leaves the list`; `the button is disabled while the call runs` (Review Focus 4); `decline asks to confirm first, and Cancel calls nothing`; `the Declined tab offers Grant and shows who declined and when`; `a failed grant shows the server's text`; `says so when nothing is pending`.
- [ ] **Step 2: Run** them. Expected: FAIL.
- [ ] **Step 3: Implement** the screen. Each row: avatar (`https://avatars.githubusercontent.com/u/<id>?s=64`), login linked to `https://github.com/<login>` with `rel="noreferrer"`, account age, public repos, followers, following, the reason as plain text, when it was asked, and a link to the user's screen.
- [ ] **Step 4: Run** `npm run check && npm test`, then screenshot the three tabs. Expected: PASS.
- [ ] **Step 5: Commit** `feat(cloud): review cloud access requests in the admin panel`.

### Task 9: Users and one user

**Files:**
- Create: `src/routes/users.index.tsx`, `src/routes/users.$githubId.tsx`, `src/components/ActivityList.tsx`, `src/components/NoticeForm.tsx`, tests beside each route

**Interfaces:**
- Consumes: `usersQuery`, `userQuery`, `plansQuery`, `logQuery`, the mutation hooks, `Confirm`.
- Produces: `<ActivityList githubId? />` (the log, newest first, "Load more" with `next`; one line per entry: time, operator or "the secret", the action in words, the account linked) and `<NoticeForm githubId? />` (text up to 500, an optional `https://` link, an optional expiry as a date; without `githubId` it offers "Everyone" or a GitHub ID). Task 10 uses both.
- Users: search params `q` and `after`. The search box writes `q` after 300 ms. A `q` of digits also shows "Open account <id>", which goes to that user's screen whether or not it is listed.
- User, for an account the database has: flags as chips with a remove button and a field to add one (`^[a-z][a-z0-9_]{0,31}$`, checked before the call); a plan select from `plansQuery`; Block with a reason up to 500, or Unblock; Sign out everywhere; machines; signed-in devices; the cloud access request; notices for it and `NoticeForm`; `ActivityList`.
- User, for a 404: "This account has not signed in yet." with the flag, plan and block forms only.
- Confirm before: removing a flag, Block, Sign out everywhere.

- [ ] **Step 1: Write the failing tests**: `search writes q to the URL and asks the server with it`; `Next follows the cursor`; `offers to open a numeric ID`; `adds a flag, and refuses "Bad Flag" without calling`; `removing a flag, blocking and signing out ask to confirm`; `sets the plan`; `shows the forms for an account that never signed in`; `shows the server's error under the form that failed`.
- [ ] **Step 2: Run** them. Expected: FAIL.
- [ ] **Step 3: Implement** both screens and the two components.
- [ ] **Step 4: Run** `npm run check && npm test`, then screenshot the list, a full user and an unknown one. Expected: PASS.
- [ ] **Step 5: Commit** `feat(cloud): manage users in the admin panel`.

### Task 10: Machines, Notices, Activity

**Files:**
- Create: `src/routes/machines.tsx`, `src/routes/notices.tsx`, `src/routes/activity.tsx`, tests beside each

**Interfaces:**
- Consumes: `machinesQuery`, `noticesQuery`, `useDeleteNotice`, `ActivityList`, `NoticeForm`, `Confirm`.
- Machines: search param `q` filters in the browser by login, name or label; each row links to the user.
- Notices: the list with who it is for ("Everyone" or the login), the read count and the expiry; "Take back" asks to confirm; a notice for everyone asks to confirm before it is sent.
- Activity: `<ActivityList />` for all accounts.

- [ ] **Step 1: Write the failing tests**: `filters machines by login, name and label`; `lists notices with read counts`; `asks before sending to everyone, not before sending to one account`; `refuses a link that is not https before calling`; `takes a notice back after a confirm`; `activity shows the operator, the action in words and the account` for a `flag.grant` and a `cloud.decline` entry, and "the secret" for a null operator; `Load more asks with before`.
- [ ] **Step 2: Run** them. Expected: FAIL.
- [ ] **Step 3: Implement** the three screens.
- [ ] **Step 4: Run** `npm run check && npm test && npm run build`, then screenshot each. Expected: PASS.
- [ ] **Step 5: Commit** `feat(cloud): add machines, notices and activity to the admin panel`.

### Task 11: deploy, docs, pull request

**Files:**
- Modify: `.github/workflows/cloud-deploy.yml`, `cloud/README.md`, `cloud/admin/wrangler.toml` (comments only)
- Create: `cloud/admin/README.md` (short: what it is, how to run it, a pointer to the setup in `cloud/README.md`)

- [ ] **Step 1: Add the deploy steps** to the `deploy` job, after the cloud Worker's deploy: read `GITHUB_CLIENT_ID` under `[env.production.vars]` of `cloud/admin/wrangler.toml`; if it is `PLACEHOLDER`, print `::notice::` saying the admin Worker is not set up and skip; otherwise `npm ci`, `npm run build` and `npx wrangler deploy --env production` in `cloud/admin`, with the same deploy message as the cloud Worker. The hotfix branch check already allows `cloud/`. Run `zizmor .github` if installed.
- [ ] **Step 2: Write the docs.** `cloud/README.md`, a section "The admin panel": running both Workers locally with a personal OAuth App for `localhost` and `.dev.vars`; the one-time production setup (create the "Arakun Admin" OAuth App with the callback `https://admin.arakun.dev/auth/github/callback`, replace `PLACEHOLDER`, `wrangler secret put` for `GITHUB_CLIENT_SECRET`, `ADMIN_SECRET`, `ADMIN_GITHUB_IDS` and `SESSION_SECRET`); how to add or remove an operator; that changing `SESSION_SECRET` signs everyone out.
- [ ] **Step 3: Run** the checks of both packages once more. Expected: PASS.
- [ ] **Step 4: Commit** `ci(cloud): deploy the admin worker with a release` and `docs(cloud): explain how to set up the admin panel`, then `gh stack push`, open the draft with the screenshots: title `feat(cloud): add the admin panel`, `Closes #807`, `Stacked on #<Part 2>.` Run `gh stack submit --auto`, wait with `tools/wait-checks.sh` for each pull request, answer cubic, and mark each ready from the bottom up.
