# Cloud admin panel: design

Date: 2026-11-25

## Intent

The operator runs Arakun's cloud by curl with one shared secret: granting cloud
access, setting flags and plans, blocking accounts, sending notices. Since #795
there is a queue of cloud access requests to review, and reviewing it by curl is
slow and easy to get wrong. The operator wants a web panel for the whole admin
door, signed in with GitHub, so each operator acts under their own name.

## Goals

- Everything `cloud/src/admin.ts` can do today, from a browser.
- Sign-in with GitHub. Only the accounts the operator lists may enter.
- Each decision records which operator made it.
- The panel shares no origin, cookie or session with the phone page, which
  shows session transcripts.

## Non-goals

- Editing plans and their limits in KV. The panel only reads the plan names.
- Operator roles. An operator can do everything.
- Changing how the desktop app, the phone page or the curl routes work. The
  curl routes keep working as documented.
- Teams administration.

## Anti-goals

- The browser must never hold `ADMIN_SECRET` or a GitHub token.
- No way in without GitHub in any deployed build: no dev bypass that a
  production setting could turn on.
- No GitHub IDs of real people in the repository (`cubic.yaml` already asks
  this), so the list of operators is a secret, not a `[vars]` entry.

## Approaches

1. Same host as the phone page, operators listed in config. Least work, but
   the panel would share an origin and a 180-day session cookie with a page
   that renders transcripts.
2. Same host, an `admin` flag in the database. Same weakness, and any
   operator session could make more operators.
3. **Chosen by Rik: its own host and its own GitHub OAuth App.** Strongest
   isolation. It costs a second OAuth App, a second Worker and its secrets.

Within 3, how the panel reaches the data (my call):

- **Chosen: a small Worker that signs operators in and forwards to the cloud
  Worker's `/admin/*` over a service binding, adding `ADMIN_SECRET` itself.**
  The cloud Worker stays the only owner of the database and of the relay
  rechecks, and its admin door keeps one way in.
- Rejected: binding D1 and the relay objects into the admin Worker. It
  would copy or share the admin logic across two Workers.
- Rejected: teaching the cloud Worker a second kind of session for a foreign
  origin. That needs CORS with credentials on the account host, which the
  cloud Worker says it does not have, on purpose.

## The parts

```
browser ── admin.<domain> ── arakun-admin Worker ──(service binding, Bearer ADMIN_SECRET)── arakun-cloud Worker ── D1, KV, relay objects
                              │ static panel (React)
                              │ /auth/github/*  (its own OAuth App)
                              └ /api/*  → /admin/*
```

### 1. Cloud Worker (`cloud/src`): the lists the panel needs

New routes behind the same bearer check, each with tests in `cloud/test`:

- `GET /admin/users?q=&after=&limit=`: accounts, last seen first. `q` matches
  a login prefix (case-insensitive) or an exact GitHub ID. Each row:
  `github_id, login, plan, blocked_at, blocked_reason, created_at,
  last_seen_at, flags, machines` (a count). Paged by a cursor; at most 100 a
  page.
- `GET /admin/users/<github_id>`: that row plus `email`, the sessions (`id,
  name, kind, created_at, last_seen_at`, `machine_label` for a cloud one;
  never a hash), the machines, the cloud access request if any, and the
  notices addressed to the account. 404 for an account the database does not
  have. The flag, plan and block routes already make the row for an account
  that never signed in, and the panel keeps that: see the User screen.
- `GET /admin/notices`: every notice, newest first, with `github_id` (null for
  everyone), the login, `text, url, created_at, expires_at` and how many
  accounts read it. At most 500.
- `GET /admin/plans`: the plan names in the KV `plans`, always with `free`.

- `GET /admin/log?github_id=&before=&limit=`: the record below, newest first,
  optionally for one account. At most 100 a page.

**The record of who did what** (approved by Rik). A new migration adds
`admin_log`: `id, at, operator_id, operator_login, action, github_id`
(the account the change is about, null for a notice to everyone) and `detail`
(a short JSON of what was set: the flag, the plan, the block reason, the
notice's id; never a secret). `action` is a fixed name such as `flag.grant`
or `cloud.decline`, which the panel can show without parsing a path (my
call). Every admin route that changes something writes one row right after
its change succeeds; if the row cannot be written the route answers 500, so
the operator knows and can repeat the call. The operator comes from two headers the admin Worker
sets, `X-Arakun-Operator-Id` and `X-Arakun-Operator-Login`; they are trusted
because the bearer was checked first. A call without them (curl) is recorded
with a null operator. Grant and decline keep filling `decided_by` as today.
The table is only appended to; nothing in the API edits or deletes a row.

### 2. Admin Worker (`cloud/admin/`, package `arakun-admin`)

A new npm package with its own `wrangler.toml`, built with Vite and
`@cloudflare/vite-plugin`: the Worker in `cloud/admin/worker/`, the panel in
`cloud/admin/src/`. It has no database and no KV.

Bindings and settings:

- `CLOUD`: service binding to the cloud Worker (dev to dev, production to
  production).
- `CLOUD_ORIGIN` (var): the account host's origin, used to build the URL of
  forwarded requests, since the cloud Worker routes by host.
- `GITHUB_CLIENT_ID` (var), `GITHUB_CLIENT_SECRET` (secret): the second OAuth
  App, "Arakun Admin", whose callback is `https://<admin host>/auth/github/callback`.
- `ADMIN_SECRET` (secret): the same value the cloud Worker has.
- `ADMIN_GITHUB_IDS` (secret): numeric GitHub IDs, comma-separated. Unset or
  empty means nobody gets in.
- `SESSION_SECRET` (secret): 32 random bytes that sign the session cookie.
- A rate limit on the callback per client IP, as the cloud Worker's sign-in has.

Routes:

- `GET /auth/github/start`, `GET /auth/github/callback`: GitHub's web flow with
  a single-use `state` cookie, as `cloud/src/web_auth.ts` does it. No scope is
  asked. The callback trades the code for a token, asks GitHub `/user` for the
  numeric ID and login, revokes the token, and checks the ID against
  `ADMIN_GITHUB_IDS`. A listed account gets the session cookie and goes to
  `/`; anyone else goes to `/?sign_in=denied` and gets no cookie.
- The session is a signed cookie, `__Host-arakun_admin`: `{ github_id, login,
  exp }` with an HMAC-SHA256 under `SESSION_SECRET`, `HttpOnly; Secure;
  SameSite=Lax; Path=/`, good for 12 hours and never renewed. Every request
  checks the signature, the expiry and that the ID is still in
  `ADMIN_GITHUB_IDS`, so taking an operator off the list ends their access at
  once. Changing `SESSION_SECRET` ends every session.
- `GET /api/me`: `{ github_id, login }`, or 401.
- `POST /auth/logout`: clears the cookie.
- `/api/<rest>` → the cloud Worker's `/admin/<rest>` with the same method,
  query and body and `Authorization: Bearer <ADMIN_SECRET>`. Only for a valid
  session; 401 otherwise. Any request other than GET or HEAD must carry an
  `Origin` equal to the Worker's own, or it is refused with 403. The client's
  cookies and other headers are not passed on. The Worker sets
  `X-Arakun-Operator-Id` and `X-Arakun-Operator-Login` from the session,
  dropping any the client sent, and for the grant and decline routes it sets
  `by` to the operator's login, whatever the body said.
- Everything else is the panel: static assets with single-page fallback, under
  a content security policy as strict as the phone page's (`script-src 'self'`,
  `connect-src 'self'`, `frame-ancestors 'none'`; images also from
  `https://avatars.githubusercontent.com`).

The Worker keeps its own copies of the few helpers it needs (hashing, cookie
parsing). It does not import from `cloud/src`.

Tests (`@cloudflare/vitest-plugin`, GitHub and the `CLOUD` binding faked):
state mismatch, an account not on the list, a forged or expired cookie, an
operator removed from the list, the `Origin` check, `by` overwritten, the
secret added and the cookie not forwarded.

### 3. The panel (`cloud/admin/src`)

React 19, TanStack Router with file-based routes, TanStack Query for every
read and change, strict TypeScript. Plain CSS with the brand's tokens (black
and white, one amber accent, Geist) and no component library. Light and dark
follow the system.

Screens:

- **Sign in**: one button. Says so plainly when the account is not an operator.
- **Cloud access** (`/`, the first screen): tabs Pending, Granted, Declined.
  Each request shows the login (linked to GitHub) and avatar, account age,
  public repos, followers and following, the reason, and when it was asked.
  Grant and Decline on pending ones; Grant on declined ones. Decline asks to
  confirm. Decided rows show who decided and when.
- **Users** (`/users`): search by login or ID, a paged table with plan, flags,
  blocked state, machines and last seen.
- **User** (`/users/$githubId`): flags as chips to remove and a field to add
  one; a plan picker from `GET /admin/plans`; block with a reason, or unblock;
  sign out everywhere; the machines, sessions and cloud access request; the
  notices for the account and a form to send one. For an ID the database
  does not have (the 404), the screen says so and still offers the flag, plan
  and block forms, so an account can be set up or blocked before its first
  sign-in, as with curl. The Users search opens any numeric ID (my call).
- **Machines** (`/machines`): every machine with its account, kind and last
  seen; a filter box; each row links to the user.
- **Notices** (`/notices`): the list with read counts, a form for a new one
  (everyone or one account, text, link, expiry) and a button to take one back.

- **Activity** (`/activity`): the record of changes, newest first: when, which
  operator, what, and the account, linked to its User screen. The User screen
  shows the same list for that account.

Rules for every screen: search, tab and page live in the URL; a change
invalidates the queries it touches instead of editing the cache by hand; a
change that removes access or reaches everyone (block, sign out everywhere,
decline, a notice for everyone, taking a flag away) asks to confirm; a failed
call shows the server's `error` text; a 401 sends the operator to sign in.

### 4. CI, deploy and docs

- `ci.yml`: the cloud job also installs, type-checks, tests and builds
  `cloud/admin`. The `cloud/*` path filter already covers it.
- `cloud-deploy.yml`: after the cloud Worker, build and deploy the admin
  Worker to production (approved by Rik). Its `[env.production]` has the
  route `admin.arakun.dev/*`: the more specific route wins over the cloud
  Worker's `*.arakun.dev/*`, and the wildcard DNS record already covers the
  host, so the CI token needs no DNS permission (my call, instead of a
  custom domain). `GITHUB_CLIENT_ID` there is a
  placeholder until a person creates the OAuth App; while it is the
  placeholder the step skips with a notice, so a release cannot fail on it.
  The hotfix rules stay as they are.
- `cloud/README.md`: how to run both Workers locally, and the one-time setup:
  create the OAuth App, set the five secrets, give the Worker its host.
- `cubic.yaml`: rules for the admin Worker (the allow-list on every request,
  the `Origin` check, the secret never sent to the browser, no real IDs).

## How it ships

One issue, three stacked pull requests:

1. `feat(cloud): list users, notices and plans and record the operator's changes`
2. `feat(cloud): add the admin worker with github sign-in` (Worker, tests, CI)
3. `feat(cloud): add the admin panel` (the React app, deploy step, README)

## Done by a person, not in the code

- Create the "Arakun Admin" OAuth App with the callback
  `https://admin.arakun.dev/auth/github/callback`, and put its client ID in
  `cloud/admin/wrangler.toml`.
- Set the secrets on the production Worker.

## Left to the builder

- The shape of the cursor for `GET /admin/users`.
- How the Worker is laid out in files, and the exact cookie encoding.
- How local development signs in (a personal OAuth App for `localhost` is the
  expected way; a bypass, if any, must not exist in a deployed build).
- Table and form details, empty and loading states, and the panel's tests
  beyond type-checking (the Worker's tests are required; the screens' are the
  builder's call).
