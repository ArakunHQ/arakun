# Account sign-in for the relay: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Through the relay, a device reaches a computer because both are signed in to the same GitHub-backed Arakun account; pairing links through the relay go away, and the relay and teams Workers become one.

**Architecture:** One Cloudflare Worker (`cloud/`) holds sign-in, teams, the machine registry (D1) and the relay Durable Object. The Worker checks the session before a socket reaches a machine's object and tells the object who is calling in headers it builds itself. Computers and devices keep the end-to-end handshake; the device takes the machine's public key from the registry.

**Tech Stack:** TypeScript on Cloudflare Workers (D1, KV, Durable Objects, Vitest in workerd), Rust (`arakun-core`, the GPUI app), plain JS (`phone/`), Swift (`ios/`), Kotlin (`android/`).

**Spec:** `docs/superpowers/specs/2026-11-23-account-relay-pairing-design.md`

## Global Constraints

- Read `AGENTS.md` first. Conventional Commits; one logical change per commit; end each commit message with `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.
- Do not push, and do not open pull requests.
- Do not touch code behind the `team` or `coderlabs` features, or `PROTOCOL.md`.
- Pairing on the local network and over Tailscale (code, QR code, token) must work exactly as before. No account is needed for it.
- End-to-end encryption stays: the relay never reads a `hello` or `data` payload.
- Rust: `tools/rustfmt.sh --check`, `cargo clippy --locked --all-targets --no-default-features -- -D warnings`, `cargo test --locked --no-default-features <module>` for touched modules only. Tests in `#[cfg(test)] mod tests` at the bottom of the file.
- Worker: `(cd cloud && npx tsc --noEmit && npx vitest run)`.
- UI uses `theme` and `ui` tokens only; interface words follow the glossary in `CONTRIBUTING.md` (environment, session, notice…). An agent "session" and a sign-in session are different things: in the interface call the latter "signed-in device".
- Comments say why, in plain short sentences. Update `README.md` (and `docs/windows.md`) when a command, flag or storage path changes.
- Session tokens never go in logs, `UserDefaults`, plain `SharedPreferences` or `localStorage`.

## Review Focus

1. A session that is signed out or whose account is blocked while its socket is open: the socket must close within the recheck period, and at once when the Worker says so.
2. A label already registered to account A, claimed with account B's session: refused with `label_taken`, and A's machine is untouched.
3. A device whose session belongs to another account opening `/machines/<label>/ws`: refused with `not_yours`, and the computer never hears `open`.
4. A computer whose session expired while it was offline: the Phone tab and `arakun-server` say "sign in again", and do not retry in a loop.
5. A cookie session used from another origin (CSRF): a `POST`/`DELETE` or a socket upgrade with a foreign or missing `Origin` is refused.

Each has a test in the task that owns the code (Tasks 2, 4, 4, 7, 2).

## Wire contract

Every task below uses these names. Do not change one without changing every side.

**HTTP (JSON, errors are `{ "error": "..." }`):**

| Route | Request | Answer |
|---|---|---|
| `GET /auth/config` | | `{ github_client_id }` |
| `POST /auth/github` | `{ access_token, name?, kind? }` | `{ session, session_id, expires_at, github_id, login, flags, plan }` |
| `GET /auth/github/start` | | `302` to GitHub with `state`; sets cookie `__Host-arakun_oauth` |
| `GET /auth/github/callback?code&state` | | sets cookie `__Host-arakun_session`, `302 /` |
| `GET /me` | | `{ github_id, login, flags, plan, notices: [{ id, text, url }] }` |
| `POST /auth/logout` | | `204`; clears the cookie |
| `GET /sessions` | | `[{ id, name, kind, created_at, last_used_at, current }]` |
| `DELETE /sessions/<id>` | | `204` |
| `GET /machines` | | `[{ label, pub, name, kind, online, last_seen_at }]`; a blocked account gets `403 { error: "blocked", reason }` (`reason` null when the operator gave none) |
| `DELETE /machines/<label>` | | `204` |
| `POST /notices/<id>/read` | | `204` |
| `PUT\|DELETE /admin/users/<github_id>/block` | `{ reason? }` | `204` |
| `PUT /admin/users/<github_id>/plan` | `{ plan }` | `204` |
| `GET /admin/machines` | | `[{ label, github_id, login, name, kind, created_at, last_seen_at }]` |
| `POST /admin/notices` | `{ github_id?, text, url?, expires_at? }` | `{ id }` |
| `DELETE /admin/notices/<id>` | | `204` |

- `kind` of a session: `app`, `server`, `phone` (the web page), `ios`, `android`. Default `app`. `name`: 1 to 100 characters, default the login.
- `kind` of a machine: `app` or `server`.
- A session comes as `Authorization: Bearer <token>` or as the cookie `__Host-arakun_session` (`HttpOnly; Secure; SameSite=Lax; Path=/`, no `Domain`). A request with two cookies of that name is signed out. With the cookie, any method other than `GET`/`HEAD`, and any socket upgrade, needs an `Origin` equal to the request's own origin, else `403`.
- Sign-in asks GitHub for the scope `user:email`. Before it revokes the token, the Worker reads `GET https://api.github.com/user/emails` and keeps the primary verified address in `users.email` (null if there is none; if the call fails the stored address stays; sign-in does not fail for it).

**Sockets:**

- Computer: `GET /machines/<label>/pc?name=<name>&kind=<app|server>` with the bearer session. Then the challenge and `claim` exactly as today.
- Device: `GET /machines/<label>/ws?page=<page id>` with the bearer session or the cookie.
- Refusals before the object: the socket is accepted and closed with a `bye` control message, as today, so a page can read why. Reasons: `signed_out` (no valid session), `blocked`, `label_taken`, `machines_full`, `not_yours`, `no_machine`, `update` (an old `<label>.<base>` host), `disabled` (the operator closed the relay; the `bye` also carries `message`, the operator's text).
- Headers the Worker sets for the object (it strips any the client sent): `x-arakun-label`, `x-arakun-account` (github_id), `x-arakun-plan`, `x-arakun-session` (SHA-256 hex of the token), `x-arakun-device` (the session's `id`), `x-arakun-device-name`, `x-arakun-device-kind`, `x-arakun-machine-name`, `x-arakun-machine-kind`.
- Control to the computer on channel 0: `{ type: "open", channel, device, name, kind, account }`. `tickets`, `pair_open`, `pair_close` no longer exist; the object answers them with `bye` `unknown control`.
- Inside the encrypted channel through the relay there is no `{pair}` or `{auth}`: once the handshake is done the channel is a client. Everything else (`req`/`res`/`part`, `ClientMsg`) is unchanged.
- The object rechecks every socket's session and account every 5 minutes on its alarm, and on `POST /_recheck` from the Worker (sent after a sign-out, a block or a forgotten machine). A socket that fails closes with `signed_out` or `blocked`; the computer's socket closes with `no_machine` once its machine was forgotten (`DELETE /machines/<label>`), and its `bye` says so.

**Plans:** KV `plans` → `{ free: { machines, phones, burst, daily, frame }, … }`. Free: `machines: 5`, the rest as today.

## File structure

- `cloud/` (was `teams/` plus `relay/`): `src/index.ts` routes; `src/auth.ts` sign-in and sessions; `src/web_auth.ts` the browser flow; `src/machines.ts` registry and socket admission; `src/notices.ts`; `src/admin.ts`; `src/relay/{relay,frames,label,limits}.ts` the object; `migrations/0008_accounts_relay.sql`; `scripts/peers.mjs`; tests beside the existing ones.
- `core/src/cloud.rs`: the account client every Rust side shares (device flow, sign-in, `/me`, `/machines`, `/sessions`), and the server's stored session.
- `core/src/phone/relay.rs`, `core/src/phone/mod.rs`: the computer's side.
- `core/src/server.rs` (and `core/src/server/*`): `login`, `logout`, the gate on `serve`.
- `src/arakun_teams/api.rs`: uses `core::cloud` for the device flow and sign-in.
- `src/shell/phone.rs`, `src/phone_view.rs`: the Phone tab. `src/servers.rs`, `src/servers/client.rs`, `src/servers_view.rs`, `src/shell/servers.rs`: the Servers tab.
- `phone/app.js`, `phone/relay.js`, `phone/index.html`, `phone/app.css`: the page.
- `ios/ArakunKit`, `ios/Arakun`; `android/kit`, `android/app`.

Branches: each task group is one future pull request. When a group's last task is committed, mark it: `git branch -f <name> HEAD`. Names: `pr/cloud-worker` (Task 1), `pr/account-relay-worker` (2–5), `pr/account-relay-computer` (6–8), `pr/account-relay-server-login` (9), `pr/account-relay-phone-page` (10), `pr/account-relay-ios` (11), `pr/account-relay-android` (12).

---

### Task 1: One Worker, no change in behavior

**Files:** `git mv teams cloud`; move `relay/src/{relay,frames,label,limits,admin}.ts` to `cloud/src/relay/`; `relay/test/*` to `cloud/test/relay/`; `relay/scripts/peers.mjs` to `cloud/scripts/`; merge `relay/wrangler.toml` into `cloud/wrangler.toml` (assets `../phone`, `RELAY` object, `CONFIG` KV, `BASE_DOMAIN`, a new migration tag for `Relay`, the newer compatibility date); merge `relay/README.md` into `cloud/README.md`; delete `relay/`. Update every reference: `.github/workflows/ci.yml`, `relay-deploy.yml` (rename to `cloud-deploy.yml`; the hotfix rule becomes `cloud/` and `phone/`), `codeql.yml`, `.github/labeler.yml`, `labels.yml`, dependabot config if any, `lefthook.yml`, `cubic.yaml`, `AGENTS.md`, `CONTRIBUTING.md`, `README.md`, and any Rust comment or test that names `relay/` or `teams/` as a path.

**Interfaces:** Produces `cloud/` with `npm run check` and `npm test`. `cloud/src/index.ts` routes: a host whose first level is a label under `BASE_DOMAIN` (or `LABEL_OVERRIDE`) → the relay router as it was; `/_admin/*` → relay admin; everything else → the teams router. Worker names: `arakun-cloud-dev`, production `arakun-cloud`.

- [ ] Move the files and merge the config. Keep both test suites passing unchanged apart from import paths and the shared `Env`.
- [ ] Add `cloud/test/router.test.ts` cases: `GET https://<label>.arakun.dev/_relay` answers `{ label }`; `GET https://app.arakun.dev/healthz` answers `{ ok: true }`; `GET https://app.arakun.dev/_relay` is `404`.
- [ ] Run `(cd cloud && npm ci && npx tsc --noEmit && npx vitest run)`: all pass. Run `npx wrangler deploy --dry-run` and `--env production --dry-run`: both bundle.
- [ ] Grep for leftover `relay/` and `teams/` paths: none outside history, release notes and the spec.
- [ ] Commit: `refactor(cloud): serve the relay and teams from one worker`. Then `git branch -f pr/cloud-worker HEAD`.

### Task 2: Sessions with a name, a kind and a cookie

**Files:** `cloud/migrations/0008_accounts_relay.sql` (all the columns and tables of the spec's "Data" in this one migration: `sessions.id/name/kind`, `users.plan/blocked_at/blocked_reason/email`, `machines`, `notices`, `notice_reads`); `cloud/src/auth.ts`; `cloud/src/index.ts`; `cloud/test/auth.test.ts`; the fake GitHub in `cloud/test/`.

**Interfaces:** Produces `session(request, env, opts): Promise<Session | null>` where `Session` gains `id: string`, `name: string`, `kind: string`, `plan: string`, `blocked: boolean`, and accepts the bearer or the cookie under the `Origin` rule. Produces `newSession(env, user: GitHubUser, name: string, kind: string): Promise<{ session, session_id, expires_at }>` for Task 3. Routes: `GET /auth/config`, `POST /auth/github` (new fields), `GET /sessions`, `DELETE /sessions/<id>`, `/me` with `plan` and `notices: []` (Task 5 fills it), `POST /auth/logout` clearing the cookie. `DELETE /sessions/<id>` and logout call `recheckAccount(env, githubId)` (a no-op exported from `cloud/src/machines.ts` until Task 4).

- [ ] Tests first, in `auth.test.ts`: sign-in with `{ name: "Rik's Mac", kind: "app" }` lists that session in `GET /sessions` with `current: true`; an unknown kind is `400`; `DELETE /sessions/<id>` of another session makes its token answer `401`; deleting another account's session id is `404`; a cookie session `GET /me` works; a cookie session `DELETE /sessions/<id>` with no `Origin` or a foreign one is `403`, with the right one `204`; sign-in stores the primary verified email and still succeeds when the emails call fails; `GET /auth/config` answers the client ID.
- [ ] Run them: they fail. Implement. Run: they pass, with the whole suite.
- [ ] Commit: `feat(cloud): name sessions, list them and sign one out`.

### Task 3: Browser sign-in for the phone page

**Files:** `cloud/src/web_auth.ts`, `cloud/src/index.ts`, `cloud/test/web_auth.test.ts`.

**Interfaces:** Consumes `newSession`. Produces `GET /auth/github/start` and `GET /auth/github/callback`. Start: random `state` in the cookie `__Host-arakun_oauth` (`HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=600`), `302` to `https://github.com/login/oauth/authorize?client_id&state&scope=user:email&redirect_uri=<origin>/auth/github/callback`. Callback: `state` must equal the cookie (else `302 /?sign_in=failed`, as for every other failure); exchange `code` at `https://github.com/login/oauth/access_token` with the client secret; then the same verify, email and revoke steps as `signIn`; session kind `phone`, name from the `User-Agent` (`iPhone`, `iPad`, `Android`, `Mac`, `Windows`, `Linux`, else `Browser`); set `__Host-arakun_session` (`Max-Age` = 180 days, the most a session lasts); `302 /`. The callback shares `SIGN_IN_LIMIT` with `POST /auth/github`; the start is not counted.

- [ ] Tests first: the happy path sets the cookie and `GET /me` with it answers the login; a wrong `state` redirects to `/?sign_in=failed` and sets no session; a GitHub refusal of the code redirects to `/?sign_in=failed` with no session.
- [ ] Fail, implement, pass. Commit: `feat(cloud): sign in from a browser with github's web flow`.

### Task 4: Machines, and admission by session

**Files:** `cloud/src/machines.ts`; `cloud/src/relay/relay.ts`, `limits.ts`, `admin.ts`; `cloud/src/admin.ts`; `cloud/src/index.ts`; `cloud/scripts/peers.mjs`; `cloud/test/relay/*` (rewrite `admit`, `claim`, `admin`, `limits`, `router` for the new doors); `cloud/test/machines.test.ts`; `cloud/README.md`.

**Interfaces:** Everything under "Sockets", "Plans" and the `machines` and admin rows of the wire contract. Produces `recheckAccount(env, githubId): Promise<void>` (posts `/_recheck` to each of the account's machine objects). The object writes the `machines` row on a successful claim (`INSERT … ON CONFLICT (label) DO UPDATE` only where `github_id` matches; a mismatch closes with `label_taken`) and keeps `account` in its storage. `GET /machines` asks each object `GET /_state` → `{ online: boolean }`. Remove: tickets, pairing codes, `spendCode`, the KV `blocked` list and label cards, `/_admin/block|unblock|labels`. Hosts `<label>.<BASE_DOMAIN>`: `/pc` and `/ws` accept the socket and close with `bye` `update`; `/_relay` answers `410 { update: true, account_host }` (`account_host` only when `ACCOUNT_HOST` is set); other paths serve the assets.

- [ ] Tests first. Admission: a computer with a session claims and `GET /machines` lists it `online: true` with its `pub`; a sixth machine on the free plan closes `machines_full`; account B claiming A's label closes `label_taken` and `GET /machines` for A is unchanged; a device of A opens `/ws` and the computer hears `open` with `device` = the session id, the name, the kind and A's `github_id`; a device of B closes `not_yours` and the computer hears nothing; no session closes `signed_out`; an unknown label closes `no_machine`; a `tickets` control message closes the computer with `unknown control`.
- [ ] Tests first. Ending: `DELETE /sessions/<id>` closes that device's open socket with `signed_out` without waiting for the alarm; `PUT /admin/users/<id>/block` closes the computer and its devices with `blocked`, and a new socket is refused `blocked`; `DELETE` of the block lets them back; `DELETE /machines/<label>` closes its sockets and frees a place; the alarm closes a socket whose session row was deleted directly in D1; a cookie device socket with a foreign `Origin` is `403`.
- [ ] Tests first. Plans: `PUT /admin/users/<id>/plan { plan: "pro" }` with KV `plans.pro.phones = 1` limits that account's machine to one device; `GET /admin/machines` lists label, owner and times. Old host: `/ws` on `<label>.arakun.dev` closes `update`.
- [ ] Fail, implement, pass. Frames still cross unread: keep `forward.test.ts` and `close.test.ts` green with only their setup changed.
- [ ] Rewrite `peers.mjs` for sessions (it takes `--session-pc` and `--session-phone`, or signs in against a local Worker's fake) and the README sections on self-hosting and operating.
- [ ] Commit: `feat(cloud)!: admit computers and devices to the relay by account`, with a `BREAKING CHANGE:` line saying relay pairing links stop working and both sides sign in.

### Task 5: Notices

**Files:** `cloud/src/notices.ts`, `cloud/src/admin.ts`, `cloud/src/auth.ts` (`/me`), `cloud/test/notices.test.ts`.

**Interfaces:** `POST /admin/notices`, `DELETE /admin/notices/<id>`, `POST /notices/<id>/read`, and `/me`'s `notices`: unread, unexpired, for this account or for everyone, oldest first, at most 5. `text` 1 to 500 characters; `url` must be `https://`.

- [ ] Tests first: a notice for everyone shows in two accounts' `/me`; read by one, it is gone for that one only; a notice for account A is not in B's; an expired one is gone; a non-https `url` is `400`.
- [ ] Fail, implement, pass. Commit: `feat(cloud): show an operator's notice to an account or to everyone`. Then `git branch -f pr/account-relay-worker HEAD`.

### Task 6: The account client in core

**Files:** create `core/src/cloud.rs`; `core/src/lib.rs`; modify `src/arakun_teams/api.rs` (and `mod.rs` where it calls the moved functions).

**Interfaces:** Move the GitHub device flow and the `POST /auth/github` call out of `src/arakun_teams/api.rs` into core, keeping the app's behavior. Produces, all `pub` in `arakun_core::cloud`:

```rust
pub const DEFAULT_URL: &str = "https://app.arakun.dev";
pub const URL_ENV: &str = "ARAKUN_TEAMS_URL"; // kept: people have it set
pub fn base_url() -> Result<String, String>;
pub struct DeviceCode { pub user_code: String, pub verification_uri: String, pub device_code: String, pub interval: Duration, pub expires_in: Duration }
pub struct SignedIn { pub session: String, pub session_id: String, pub github_id: u64, pub login: String, pub flags: Vec<String>, pub plan: String }
pub struct Me { pub github_id: u64, pub login: String, pub flags: Vec<String>, pub plan: String, pub notices: Vec<Notice> }
pub struct Notice { pub id: String, pub text: String, pub url: Option<String> }
pub struct Machine { pub label: String, pub public: String, pub name: String, pub kind: String, pub online: bool, pub last_seen_at: u64 }
pub struct SessionRow { pub id: String, pub name: String, pub kind: String, pub created_at: u64, pub last_used_at: u64, pub current: bool }
pub enum Error { SignedOut, Blocked(String), Unreachable(String), Refused(String) }
pub fn client_id(base: &str) -> Result<String, Error>;                 // GET /auth/config
pub fn start_device_flow(client_id: &str) -> Result<DeviceCode, Error>; // scope user:email
pub fn poll_device_flow(client_id: &str, code: &DeviceCode) -> Result<Option<String>, Error>; // None: keep waiting
pub fn sign_in(base: &str, github_token: &str, name: &str, kind: &str) -> Result<SignedIn, Error>;
pub fn me(base: &str, session: &str) -> Result<Me, Error>;
pub fn machines(base: &str, session: &str) -> Result<Vec<Machine>, Error>;
pub fn forget_machine(base: &str, session: &str, label: &str) -> Result<(), Error>;
pub fn sessions(base: &str, session: &str) -> Result<Vec<SessionRow>, Error>;
pub fn sign_out_session(base: &str, session: &str, id: &str) -> Result<(), Error>;
pub fn read_notice(base: &str, session: &str, id: &str) -> Result<(), Error>;
pub fn logout(base: &str, session: &str) -> Result<(), Error>;
```

Blocking calls with the HTTP client core already uses. `401` → `Error::SignedOut`. A session is refused over plain `http://` unless the host is local, as the app does today.

- [ ] Tests first, against a small local HTTP server in the test (as other core tests do): `me` parses `plan` and `notices`; a `401` is `SignedOut`; `machines` parses a row; `sign_in` sends `name` and `kind`; `base_url` keeps the trailing-slash and scheme rules of the app's current tests.
- [ ] Fail, implement, pass. The app's teams sign-in now passes its machine's name and `kind: "app"`, and its existing tests pass (`cargo test --locked --no-default-features arakun_teams cloud`).
- [ ] Commit: `refactor(core): share the account client between the app and the server`.

### Task 7: The computer's side of the relay

**Files:** `core/src/phone/relay.rs`, `core/src/phone/mod.rs`, `core/src/phone/wire.rs` if a message shape changes; `core/src/host.rs` if the session must be asked for there.

**Interfaces:** Consumes `cloud::base_url`, the wire contract. `RelayClient` is started with the session token and the machine's name and kind, and dials `wss://<relay_base host>/machines/<label>/pc?name&kind` with the bearer header. `Settings.relay_base` default becomes `https://app.arakun.dev`; a stored `https://arakun.dev` is read as the new default. Produces:

- `relay::Status::SignedOut` (no session, or the relay said `signed_out`): no retry until a new session is given through `RelayClient::set_session(&self, session: Option<String>)`. `blocked` maps to `Refused`, `label_taken` and `machines_full` to `Rejected` with a sentence a person can act on.
- `open` with an `account` other than the computer's own (`RelayClient` is given `github_id`) closes that channel with `not_yours`.
- A relay channel is a client as soon as the handshake is done. Its `Device` is found by `id` = the relay's `device`, or made with the relay's `name` (`token_hash` and `ticket_hash` empty). The first time a device id is seen, the shell hears `Incoming::NewDevice { name: String, kind: String }`.
- Removed: `Command::Tickets`, `PairOpen`, `PairClose`, relay tickets on `Device` for new devices (keep reading `ticket_hash` from old files; ignore it), and `{pair}`/`{auth}` handling on relay channels. The local server's pairing is untouched.
- `Transport::Relay` unpair (`disconnect_device` and forgetting a device) only closes the channel and drops the record; signing the device out is the account's job.

- [ ] Tests first, with the file's existing fake relay: a claim sends the bearer and the query; `open` for the right account then a handshake makes a client and a `Device` with the relay's name, and emits `NewDevice` once (a second connect of the same id does not); `open` with another account is closed `not_yours` and makes no client; a `bye` `signed_out` gives `Status::SignedOut` and no further dial until `set_session(Some(..))`; a `{pair}` message on a relay channel is ignored; a local-network pairing test still passes unchanged.
- [ ] Fail, implement, pass: `cargo test --locked --no-default-features phone`.
- [ ] Commit: `feat(phone)!: reach the relay with the account's session`, with a `BREAKING CHANGE:` line.

### Task 8: The Phone tab and the Servers tab

**Files:** `src/shell/phone.rs`, `src/phone_view.rs`, `src/servers.rs`, `src/servers/client.rs`, `src/servers_view.rs`, `src/shell/servers.rs`, `src/arakun_teams/mod.rs` (expose the session and `github_id` to the shell; show `/me` notices), `README.md`, `docs/windows.md`.

**Interfaces:** Consumes Tasks 6 and 7. Produces `servers::Way::Account { label: String }` beside `Direct` and `Relay` (a saved `Relay { label, ticket }` entry is shown as "Sign in to reconnect" and becomes `Account` once the label is in `GET /machines`). The client for `Way::Account` dials `/machines/<label>/ws?page=<id>` with the bearer and checks the handshake against the machine's `pub`.

Phone tab, reach Relay:
- Signed out: "Sign in to reach this computer from anywhere" and a "Sign in with GitHub" button that opens the existing sign-in dialog. No code, no QR code.
- Signed in: the account's login, the address `https://app.arakun.dev` to open on the phone (with a QR code of that address), and "Signed-in devices": name, kind, last used, and "Sign out" for each (`cloud::sessions`, `cloud::sign_out_session`).
- `Incoming::NewDevice` shows a notice: "<name> connected for the first time".
- Reach Lan and Tailscale: unchanged.

Servers tab: under the paste box, "Your servers": machines of kind `server` from `cloud::machines`, each with Connect. Signed out, one line: "Sign in to see your servers". The paste box stays for links of servers reached directly; a relay link gets "Relay links are no longer used. Sign in on the server with `arakun-server login`."

`/me` notices show as notices in the app with their link; dismissing one calls `cloud::read_notice`.

- [ ] Tests first: `parse_link` on an old relay link returns the sentence above; a saved `Relay` entry loads and is reported as needing sign-in; `Way::Account` round-trips in `servers.json`; the client's dial URL and header for `Way::Account`; `design_lint` and `words_lint` pass.
- [ ] Fail, implement, pass. `cargo clippy` clean. Check the tab by eye with the `ui-change` skill's headless run if the environment allows; if not, say so in the report.
- [ ] Update the README sections "Phone remote", "Server" and the storage and privacy lists (what `servers.json` and `phone.json` now hold; that the account's email is kept).
- [ ] Commit: `feat(phone)!: sign in to pair through the relay`. Then `git branch -f pr/account-relay-computer HEAD`.

### Task 9: `arakun-server login`

**Files:** `core/src/server.rs`, `core/src/server/store.rs` or a sibling for the stored session, `core/src/server/daemon.rs`, `README.md`.

**Interfaces:** Consumes `cloud::*`. Commands: `login` (prints the code and `https://github.com/login/device`, waits, stores the session; name = the machine's host name, kind `server`), `logout` (calls `cloud::logout`, removes the stored session, tells a running server), `status` shows the login or "not signed in". The session is stored in the server's state folder as `account.json`, mode `0600` on Unix (the Keychain on macOS if the app's helper for it lives in core; else the file). `serve` with reach Relay and no session exits with:

```
Not signed in. Run `arakun-server login`, or serve without an account:
  arakun-server serve --reach lan
  arakun-server serve --reach tailscale
```

`pair` under reach Relay prints: "Open https://app.arakun.dev on the device and sign in as <login>." and no link. Under Lan and Tailscale it works as before. `devices` and `unpair` work as before.

- [ ] Tests first: parsing `login` and `logout`; `serve` without a session under Relay returns the text above and under `--reach lan` starts; `pair` text under Relay; the stored session file has mode `0600` and round-trips; `logout` removes it.
- [ ] Fail, implement, pass: `cargo test --locked --no-default-features server`.
- [ ] README: the "Server" and "The server alone" sections.
- [ ] Commit: `feat(server)!: sign in on the server instead of pairing by link`. Then `git branch -f pr/account-relay-server-login HEAD`.

### Task 10: The phone page

**Files:** `phone/app.js`, `phone/relay.js`, `phone/index.html`, `phone/app.css`, `phone/sw.js` (cache name bump), `phone/test/*`.

**Interfaces:** Consumes the wire contract. `RELAY.detect()` becomes: `GET /auth/config` answers → this host is the account host. Then `GET /me`: `401` → the sign-in screen with one button to `/auth/github/start`; else `GET /machines`: none → "No computers yet. Sign in on a computer running Arakun."; one → connect; several → a list (name, kind, online or last seen) and a way back to it. Connecting opens `/machines/<label>/ws?page=<PAGE>` (the cookie rides along) and checks the handshake against the machine's `pub`. No `{pair}`, no `{auth}`, no stored ticket or token: remove `arakun.relay` from `localStorage` and its cookie on load. `bye` reasons shown as sentences: `signed_out` → the sign-in screen; `blocked`; `not_yours`; `no_machine`; `update` → "Update Arakun on the computer and sign in at <account host>". On a host shaped `<label>.<base>` (`/_relay` answers `410`), show that last sentence with a link. Unread `/me` notices show as a banner with dismiss (`POST /notices/<id>/read`). "Sign out" in the page's settings calls `POST /auth/logout`. Served by the computer itself (network, Tailscale), the page behaves as before.

- [ ] Tests first in `phone/test/` in their existing style: the boot decision (signed out, no machines, one, several) from stubbed fetches; the socket URL; the sentence for each `bye` reason; a page served locally still asks for the pairing code.
- [ ] Fail, implement, pass (the phone tests run in the Worker's CI job: run them the way `ci.yml` does).
- [ ] Commit: `feat(phone)!: sign in on the phone page and pick a computer`. Then `git branch -f pr/account-relay-phone-page HEAD`.

### Task 11: iOS

**Files:** `ios/ArakunKit/Sources/ArakunKit/{Account.swift (new), Pairing.swift, RelayLink.swift, Models.swift}`, their tests, `ios/Arakun/*` pairing and settings views, `ios/README.md`.

**Interfaces:** `Account.swift` mirrors `core::cloud` (device flow with `user:email`, `signIn(name:kind:"ios")`, `me`, `machines`, `sessions`, `logout`). The session lives in the Keychain only. The first screen offers "Sign in with GitHub" (shows the code, opens GitHub) and "Pair on this network" (the existing code, QR and link flow, for direct addresses only; a relay link gets the "no longer used" sentence). Signed in: a machine list, then the inbox of the picked machine; the picked label is remembered. The relay socket is `/machines/<label>/ws?page=<id>` with the bearer header; the handshake is checked against `pub`; no `pair`/`auth` message. Settings: the login, signed-in devices with sign out, notices, Sign out. The `-demo` mode stays behind `#if DEBUG` and gains a signed-in sample.

- [ ] Tests first in `ArakunKit`: decoding `/me`, `/machines`; the socket request's URL and header; an old relay link is refused with the sentence; the Keychain is the only store written.
- [ ] Implement. Run `(cd ios/ArakunKit && swift test)` if Swift is installed; the app build needs macOS and Xcode, so report it as not run on Linux.
- [ ] Commit: `feat(ios)!: sign in to reach computers through the relay`. Then `git branch -f pr/account-relay-ios HEAD`.

### Task 12: Android

**Files:** `android/kit/src/main/kotlin/com/arakun/**` (new `Account.kt`; pairing and relay link code), its tests, `android/app/**` pairing and settings screens, `android/README.md`.

**Interfaces:** The same as Task 11 with `kind: "android"`; the session goes where the pairing credentials already go (the encrypted store the app uses today). `arakun://pair?link=` keeps working for direct links only. `demo` stays debug-only.

- [ ] Tests first in `:kit:test`: the same four as iOS.
- [ ] Implement. Run `(cd android && ./gradlew :kit:test :app:testDebugUnitTest :app:lintDebug :app:assembleDebug)` if the SDK is present; otherwise run what can run and report the rest as not run.
- [ ] Commit: `feat(android)!: sign in to reach computers through the relay`. Then `git branch -f pr/account-relay-android HEAD`.
