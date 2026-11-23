# Cloud

Arakun's Cloudflare Worker, one package for two jobs:

- **The relay** lets a phone reach a computer running Arakun from outside the local
  network. It forwards encrypted frames without being able to read them: each computer
  has a label derived from its public key and a Durable Object with that name. The
  computer comes in with its GitHub account's session and registers the label to the
  account; a device comes in with a session of the same account. The phone page
  (`../phone/`) is served as assets by the same Worker, so the phone always loads the
  app from the same commit as the relay.
- **Teams** has a D1 database and one Durable Object per channel. It signs users in
  with GitHub, tells the app which features each user has, and runs the end-to-end
  encrypted teams of [PROTOCOL.md](PROTOCOL.md): devices, teams, invites, team keys in
  envelopes, and channels with live messages. The server stores and forwards; it never
  holds a key that opens team content.

One host serves both: the account host (`app.arakun.dev` in production). The old
relay hosts, `<label>.<BASE_DOMAIN>` (a label is 16 base32 characters), only tell an
old app or page to update: `/pc` and `/ws` close with the reason `update`, `/_relay`
answers `410 { "update": true }`, and the page itself still loads.
On the account host, a `GET` or `HEAD` outside the API's paths serves the page, so
`https://app.arakun.dev/` opens it; an unknown path under an API route stays a JSON 404.

```
cloud/
  src/index.ts          old label hosts or the account host; exports both Durable Objects
  src/machines.ts       GET /machines, DELETE /machines/<label>, the sockets
                        /machines/<label>/pc and /ws, and the recheck after a sign-out
  src/relay/router.ts   the old label hosts: "update", and the page
  src/relay/relay.ts    the Relay Durable Object: computer claim, registration, devices,
                        channels, limits, the recheck of sessions, alarm
  src/relay/limits.ts   plans and the kill switch, read from KV
  src/relay/frames.ts   binary frames and control messages
  src/relay/label.ts    label = base32 of the first 80 bits of SHA-256(public key)
  src/teams-router.ts   the teams routes; every error is JSON: { "error": "..." }
  src/auth.ts           POST /auth/github, GET /me, POST /auth/logout, sessions
  src/admin.ts          PUT|DELETE /admin/users/<github_id>/flags/<flag>,
                        DELETE /admin/users/<github_id>/sessions,
                        PUT|DELETE /admin/users/<github_id>/block,
                        PUT /admin/users/<github_id>/plan, GET /admin/machines,
                        POST /admin/notices, DELETE /admin/notices/<id>
  src/notices.ts        notices: POST /notices/<id>/read, and what /me carries
  src/teams.ts          devices, teams, invites, envelopes, members, channels
  src/channel.ts        the Channel Durable Object: messages, seq, live sockets
  src/wire.ts           the shapes of PROTOCOL.md and their signature checks
  migrations/           the D1 tables, applied in order by wrangler
  test/                 Vitest inside workerd, with real D1, KV, the real Durable
                        Objects, real P-256 keys and a fake GitHub
  test/relay/           the relay's tests
  scripts/peers.mjs     one fake computer and two fake devices against a running Worker
```

## Teams

D1 holds what is small and relational: users, devices, teams, members, invites,
envelopes and channels. Each channel's messages live in its own SQLite-backed
Durable Object, named `<team_id>/<channel_id>`, which also holds the channel's
sockets (with the hibernation API). The Worker checks the session, the flag, the
membership and the device before it opens a socket on the object, and passes who
is calling in headers it builds itself (the session as its SHA-256, never the
token). On every message the object checks the signature, reads the team's epoch
from D1 again, and checks every socket's session, membership and flag, so a socket
that lost any of them is closed before it gets the message.

## How sign-in works

1. The desktop app runs GitHub's OAuth device flow with the public client ID
   `Ov23lipgmMeATCa5aTr3` and the scope `user:email`. The app holds no client secret.
2. It posts the GitHub token once to `POST /auth/github`.
3. The Worker asks GitHub's Applications API
   (`POST /applications/<client_id>/token`) whether our OAuth App issued the token and
   to whom. That call needs the app's client secret, which only the Worker has. It
   keeps the numeric `id`, the `login` and the primary verified email address
   (`GET /user/emails`, which is what the scope is for), and answers with a session
   token of its own. It never stores the GitHub token, and stores nothing else about
   the user.

   `GET /user` would accept any of the user's GitHub tokens (the `gh` CLI's, a
   personal token, another app's), so whoever held one could sign in as them.
4. Before it answers, the Worker revokes that GitHub token
   (`DELETE /applications/<client_id>/token`), since OAuth App tokens never expire
   and it was only needed to learn who the user is. So one GitHub token gives one
   session, and signing in again means running the device flow again. If GitHub
   does not confirm the revoke, the sign-in fails and gives no session.
5. The app sends `Authorization: Bearer <session>` from then on.

The phone page signs in with GitHub's web flow, since a browser cannot keep a token to
itself. `GET /auth/github/start` sends the browser to GitHub with a random `state`,
which it also keeps in the cookie `__Host-arakun_oauth` for ten minutes.
`GET /auth/github/callback` takes the code only when its `state` is the cookie's,
trades it for a GitHub token with the client secret, and goes on from step 3. The
session goes into the cookie `__Host-arakun_session` (`HttpOnly`, `Secure`,
`SameSite=Lax`), which the page's script cannot read, and the browser lands on `/`.
Every failure lands on `/?sign_in=failed` instead, with no session and nothing of what
GitHub or the request said. A request that comes with the cookie and changes something
(anything but `GET` and `HEAD`), or opens a socket, must carry an `Origin` equal to
the Worker's own, or it answers `403`.

A session lasts 90 days and is extended on use, at most once a day, but never past
180 days after sign-in. Then the app has to sign in again. The cookie lives for those
180 days; the session's row decides when it ends. The database holds only the SHA-256
of the session token. An account keeps at most 100 sessions: a sign-in past that ends
the one used longest ago.

Users are keyed by their numeric GitHub ID, which stays when they rename their account.
Flags are set per user on the server, and every team endpoint checks the `teams`
flag there. Anyone can sign in. If the `flags` that `/me` answers do not include
`teams` (there may be other flags), every team endpoint answers `403`.

## Development

Node 24 or newer.

```sh
cd cloud
npm ci
npm run check     # tsc --noEmit
npm test          # vitest run
```

`npx wrangler deploy --dry-run` (and `--env production --dry-run`) checks the
configuration and the bundle without deploying.

For `wrangler dev`, create the tables in the local database first:

```sh
npx wrangler d1 migrations apply arakun-cloud-dev --local
npx wrangler dev
```

To try the relay, run `scripts/peers.mjs` against `wrangler dev`. It keeps a
computer's key in `.peers-key.json` (git-ignored). With `--local` it writes a test
account and two sessions straight into the local D1, so no GitHub sign-in is needed:

```sh
npx wrangler dev
node scripts/peers.mjs --base http://127.0.0.1:8787 --local
```

Against a deployed Worker, sign in twice with one account and pass both sessions:
`--session-pc <token> --session-phone <token>`. The script claims the label, checks
the account lists the machine online, brings in two devices, sends bytes both ways,
checks a socket without a session is turned away and that the computer hears every
close. It exits with 1 at the first failing step.

`wrangler dev` reads the secrets from `.dev.vars`. If you do not have that file yet,
create it with the two lines below; if you have it, keep your values:

```sh
ADMIN_SECRET=dev-secret
GITHUB_CLIENT_SECRET=<the app secret>
```

## First production deploy

Production moved from the Workers `arakun-relay` and `arakun-teams` to one Worker,
`arakun-cloud`. This is also the first time the teams API is live in production, on
the account host. Do these once, in this order:

1. Before the pull request that adds `cloud/` can merge: in the `main` ruleset, require
   "Cloud (types and tests)" in place of "Relay (types and tests)". That pull request
   has no job with the old name, so the old required check never finishes. Every other
   open pull request still runs the old job: rebase each one on `main` after the merge
   (or merge `main` into it) so its checks run under the new name.
2. Create the database and put the `database_id` it prints in `wrangler.toml` under
   `[[env.production.d1_databases]]`. The deploy workflow fails until you do:

   ```sh
   npx wrangler d1 create prod-arakun-cloud
   ```

3. The `CLOUDFLARE_API_TOKEN` of the `production` environment needs D1 edit permission:
   grant it if it lacks it, since the deploy applies the migrations. The environment's
   deployment rules must let `cloud-hotfix/*` branches through, where they named
   `relay-hotfix/*` before.
4. Set the secrets on the new Worker (see Deploying, step 3):

   ```sh
   npx wrangler secret put ADMIN_SECRET --env production
   npx wrangler secret put GITHUB_CLIENT_SECRET --env production
   ```

5. On Arakun's GitHub OAuth App (the one `GITHUB_CLIENT_ID` names), add the callback
   URL `https://app.arakun.dev/auth/github/callback`. The phone page's sign-in fails
   with GitHub's `redirect_uri` error without it.
6. Add a proxied DNS record for `app.arakun.dev` in the `arakun.dev` zone (an `AAAA`
   to `100::` is enough; the Worker route answers). The `*.arakun.dev/*` route already
   covers the host, and `ACCOUNT_HOST` in `[env.production.vars]` makes it the only
   account host.
7. In the CONFIG KV, give each plan in `plans` its `machines`, how many machines an
   account on it may register (5 when it is missing). The namespace is the same as
   the old relay's, so the other limits carry over:

   ```sh
   npx wrangler kv key put --env production --remote --binding CONFIG plans '{"free":{"machines":5,"phones":3,"burst":50,"daily":20000,"frame":262144}}'
   ```

8. Move the route by hand, from a checkout of the commit you are about to release.
   Remove the `*.arakun.dev/*` route from the old `arakun-relay` Worker (two Workers
   cannot hold the same route, so the deploy fails otherwise), then run these right
   away. No phone or app reaches a computer between the two:

   ```sh
   npx wrangler d1 migrations apply prod-arakun-cloud --env production --remote
   npx wrangler deploy --env production
   ```

   Do not leave this to the release: its Cloud job runs after everything else is built
   and published, and the relay would be down for all that time. When you publish the
   release, that job deploys the same commit again.
9. Delete the old KV keys, which nothing reads once `arakun-cloud` answers: `blocked`,
   and every `labels/*` card:

   ```sh
   npx wrangler kv key delete --env production --remote --binding CONFIG blocked
   npx wrangler kv key list --env production --remote --binding CONFIG --prefix labels/
   # then, for each name it lists:
   npx wrangler kv key delete --env production --remote --binding CONFIG "labels/<label>"
   ```

10. The relay's objects start empty, and computers register again when they claim
    after signing in.
11. Delete the old `arakun-relay` Worker once the relay answers from `arakun-cloud`.

From the merge until the first release that has `cloud/`, CI cannot redeploy or hotfix
the hosted relay: `relay-deploy.yml` is gone from `main`, and the latest release's tag
has no `cloud-deploy.yml`. Until then, deploy by hand with
`npx wrangler deploy --env production`, from `relay/` in a checkout of that tag while
`arakun-relay` still holds the route.

## Deploying

A published release deploys the production Worker by itself:
`.github/workflows/cloud-deploy.yml` applies the D1 migrations and runs
`npx wrangler deploy --env production` from the release's tag, so a change on `main`
waits for the next version. A hotfix without a new version goes on a
`cloud-hotfix/v<version>` branch made from the latest release's tag; the steps are at
the top of that workflow. The deploy's message names the release or the hotfix. The
commands below do the same by hand. They use `--env production`,
which deploys the Worker `arakun-cloud`. Without it, wrangler deploys the top-level
Worker `arakun-cloud-dev`, a separate Worker with its own database, rate-limit
namespace and secrets. A test deploy cannot replace production's bindings, and its
admin secret cannot grant flags or make sessions in production.

1. Create the database once, and put the `database_id` it prints in `wrangler.toml`
   under `[[env.production.d1_databases]]`:

   ```sh
   npx wrangler d1 create prod-arakun-cloud
   ```

2. Apply the migrations, now and after every change in `migrations/`:

   ```sh
   npx wrangler d1 migrations apply prod-arakun-cloud --env production --remote
   ```

3. Set the admin secret. Without it, the `/admin/users/...` endpoints answer 401 to
   everyone. Make one and save it in your password manager first, since Cloudflare
   will not show it again:

   ```sh
   openssl rand -base64 32
   ```

   Then paste it when wrangler asks for it:

   ```sh
   npx wrangler secret put ADMIN_SECRET --env production
   ```

   Then the OAuth App's client secret (GitHub → ArakunHQ → Settings → Developer
   settings → OAuth Apps → Arakun → Generate a new client secret). Without it,
   sign-in answers 503:

   ```sh
   npx wrangler secret put GITHUB_CLIENT_SECRET --env production
   ```

4. Deploy:

   ```sh
   npx wrangler deploy --env production
   ```

   The first deploy creates the `Relay` and `Channel` Durable Object classes from the
   `v1` and `v2` migrations in `wrangler.toml` (`new_sqlite_classes`). Wrangler sends only
   the tags after the one the deployed Worker is at, so never reorder or rename a tag.
   A later change to a class that renames or deletes it needs a new `[[migrations]]`
   tag, in both the top-level and the `[env.production]` lists.

To host the test Worker too, give it its own database and put that `database_id` in
the top-level `[[d1_databases]]`. Never put production's id there. Then follow the
same steps with `arakun-cloud-dev` and without `--env production`, and give it a
different `ADMIN_SECRET`:

```sh
npx wrangler d1 create arakun-cloud-dev
npx wrangler d1 migrations apply arakun-cloud-dev --remote
```

Three rate limits are set in `wrangler.toml` under `[[ratelimits]]`: sign-in, which is
`POST /auth/github` and the page's `GET /auth/github/callback` together
(about 10 a minute per client IP, `namespace_id` 1001 in production, 1003 in the
top-level Worker; `GET /auth/github/start` asks nothing of GitHub and is not counted), the messages each device sends on channel sockets (about 60
every 10 seconds, `namespace_id` 1002 in production, 1004 in the top-level Worker)
and the invites each user makes, since each asks GitHub who has the login (about
30 a minute, `namespace_id` 1006 in production, 1005 in the top-level Worker).
Rate limits with the same `namespace_id` in one account share their counters for
the same key, so a device sending through the test Worker would use up its
production allowance; give a new rate limit an id nothing else in the account uses.
Cloudflare counts each location on its own, and its counters catch up with a
delay, so both are approximate: a client can get a few more.

The compatibility date is after 2026-04-07, so the runtime answers a WebSocket
client's close by itself; `src/channel.ts` relies on that.

## Relay: hosting your own

One deploy of this Worker serves every computer and every device of every account,
on any host, `workers.dev` included. Labels travel in the path, so there is one
Worker, not one per computer.

1. A Cloudflare account (the free plan caps spending) and a session:

   ```sh
   npx wrangler login
   ```

   On `workers.dev`, the account needs a registered subdomain (dashboard: Workers &
   Pages → Overview). A certificate for a freshly registered subdomain takes a minute or
   two to be served; TLS fails until then.

2. The KV namespace and the database, once. Put the `id` and the `database_id` they
   print in the top level of `wrangler.toml` (`[[kv_namespaces]]` and
   `[[d1_databases]]`), then create the tables:

   ```sh
   npx wrangler kv namespace create CONFIG
   npx wrangler d1 create arakun-cloud-dev
   npx wrangler d1 migrations apply arakun-cloud-dev --remote
   ```

3. A GitHub OAuth App of your own (GitHub → Settings → Developer settings → OAuth
   Apps), with device flow enabled and the callback URL
   `https://<your host>/auth/github/callback`. Put its client ID in `GITHUB_CLIENT_ID`
   under `[vars]`, and its client secret and an admin secret in the Worker:

   ```sh
   npx wrangler secret put GITHUB_CLIENT_SECRET --env=""
   npx wrangler secret put ADMIN_SECRET --env=""
   ```

4. Deploy, and check it with a fake computer and device (sign in twice first, see
   Development):

   ```sh
   npx wrangler deploy --env=""
   node scripts/peers.mjs --base https://arakun-cloud-dev.<account>.workers.dev --session-pc <token> --session-phone <token>
   ```

   `--env=""` picks the top level of `wrangler.toml`. Without it, wrangler warns that
   several environments are defined (`[env.production]` is `arakun.dev`'s own Worker)
   and none was picked, then deploys the top level anyway.

5. Point Arakun at the Worker in two places, both with its URL: `relay_base` in
   `phone.json`, where the relay is dialed, and `ARAKUN_TEAMS_URL` in the environment
   of the app or `arakun-server`, where it signs in. A session only goes to the server
   that issued it, so with only `relay_base` set the relay stays signed out. The app
   reads the client ID from `/auth/config` and signs in there.

The free plan's limits apply until you set `plans` in KV (see below).

If you hosted the old relay, you have one Worker per computer, each named after its
label and deployed from the old `relay/` folder. One Worker now serves them all, so
follow the steps above under the new name and delete the old Workers. You can also
deploy over one of them and keep its name, with `npx wrangler deploy --name <label> --env=""`
in step 4: it is at migration `v1` with the class `Relay`, which is `v1` here too, so
wrangler only adds `v2` (`Channel`). A Worker deployed from the old `teams/` folder is
at `v1` with `Channel` and cannot be deployed over: deploy under a new name and delete
the old one.

## Relay: operating

Everything is tuned in KV without redeploying:

| Key | Value | Effect |
|---|---|---|
| `plans` | `{"free":{"machines","phones","burst","daily","frame"}, "<plan>":{…}}` | Machines an account may register (default 5), then per machine: devices at once, frames per second, frames per UTC day, bytes per frame. Each object rereads it every minute. An account is on `free` until the operator sets another plan; a plan KV does not have is the free plan |
| `disabled` | `true` or a message | Both sockets close with `bye` `disabled` and the message (`message`); a plain request to them answers 503 with it |

```sh
# arakun.dev's own Worker ([env.production])
npx wrangler kv key put --binding CONFIG --env production --remote plans '{"free":{"machines":5,"phones":3,"burst":50,"daily":20000,"frame":262144}}'
npx wrangler kv key put --binding CONFIG --env production --remote disabled "maintenance until 05:00 UTC"
npx wrangler kv key delete --binding CONFIG --env production --remote disabled
# a self-hosted Worker (the top level of wrangler.toml)
npx wrangler kv key put --binding CONFIG --env="" --remote disabled "maintenance until 05:00 UTC"
```

Without `--remote`, wrangler v4 writes to local storage and the relay never sees it.

With the admin secret (see Granting flags for how to pass it without it showing in
the list of processes):

```sh
read -rs ADMIN_SECRET
auth() { printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET"; }
# every machine: label, owner's GitHub ID and login, name, kind, created and last seen
auth | curl -fsS -H @- "https://<host>/admin/machines"
# block an account: its computers and devices are closed now, new sockets are refused
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"reason":"abuse report 12"}' \
  "https://<host>/admin/users/<github_id>/block"
auth | curl -fsS -X DELETE -H @- "https://<host>/admin/users/<github_id>/block"
# put an account on a plan from `plans`; open sockets follow at once
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"plan":"pro"}' \
  "https://<host>/admin/users/<github_id>/plan"
# a notice for one account (leave out github_id for everyone). text is 1 to 500 characters,
# url must be https://, expires_at is Unix seconds in the future. Answers {"id": "..."}
auth | curl -fsS -X POST -H @- -H "Content-Type: application/json" \
  -d '{"github_id":1001,"text":"Your plan is now Pro.","url":"https://arakun.dev/pricing","expires_at":1893456000}' \
  "https://<host>/admin/notices"
# take a notice back, with the reads of it
auth | curl -fsS -X DELETE -H @- "https://<host>/admin/notices/<id>"
```

`GET /me` lists an account's unread notices, oldest first, at most 5, as
`notices: [{ id, text, url }]`. `POST /notices/<id>/read` with a session dismisses
one: `204`, also on a second read; `404` for one that is not the caller's, has expired
or does not exist. A `github_id` with no account answers `404`.

A blocked account can still sign in and read `/me`; `GET /machines` answers it
`403 { "error": "blocked", "reason": … }` with the reason given.

Each object checks its sockets' sessions and account in D1 every 5 minutes, and at
once when the Worker asks after a sign-out, a block, a plan change or a forgotten
machine. A socket whose session is gone closes with `signed_out`, a blocked
account's with `blocked`. The object also reads a device's session, its account's
block and the machine's account itself before it gives the device a channel, as it
does for a computer's claim, so a device that got past the Worker just before a
sign-out or a block is refused with the same reasons.

An object whose computer has not connected in 90 days empties itself (alarm). Nothing
is lost: the machine stays in D1 and the computer claims again when it reconnects. A
socket at `/pc` stores nothing until it claims, and is closed if it has not claimed
within 30 seconds.

## Relay: hosting it for everyone on your own domain

One host serves every computer: a proxied DNS record for it (say `app.<domain>`) and a
Worker route `app.<domain>/*`. For `arakun.dev` this is `[env.production]` in
`wrangler.toml`, which the release deploys (see Deploying above); its route
`*.arakun.dev/*` also keeps the old label hosts answering `update`. Add a zone
rate-limiting rule (one rule is free) on `/machines/*` per IP, so nobody can open
sockets without limit. With a wildcard route, set `ACCOUNT_HOST` under `[vars]` to
that one host, so no other name under the domain serves the page or the session
cookie.

## Granting flags

Flags are granted by numeric GitHub ID. Keep the list of pilot users out of this
repository: type the IDs on the command line when you grant them. A user's ID is
public:

```sh
gh api users/<login> --jq .id
```

A flag can be granted before the user signs in for the first time. The user row then
has an empty login until they do.

With the admin endpoint, in a shell where `ADMIN_SECRET` holds the value you saved
in step 3 (paste it; the variable is not set anywhere else). Run this in Bash or zsh:
`read -s` hides what you paste, and `sh` (dash) has no `-s`. The header goes to curl
on its input (`-H @-`), from the shell's own `printf`, so the secret never shows in
the list of running processes:

```sh
read -rs ADMIN_SECRET
for id in <id> <id> <id>; do
  printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET" |
    curl -fsS -X PUT -H @- "https://<host>/admin/users/$id/flags/teams"
done
```

Or straight in the database:

```sh
npx wrangler d1 execute prod-arakun-cloud --env production --remote --command "
  INSERT INTO users (github_id, login, created_at, last_seen_at)
    VALUES (<id>, '', unixepoch(), 0) ON CONFLICT DO NOTHING;
  INSERT INTO user_flags (github_id, flag, granted_at)
    VALUES (<id>, 'teams', unixepoch()) ON CONFLICT DO NOTHING;"
```

To sign a user out everywhere, as when a session token may have leaked (they can
sign in again; take their flags away to keep them out):

```sh
read -rs ADMIN_SECRET
printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET" |
  curl -fsS -X DELETE -H @- "https://<host>/admin/users/<id>/sessions"
```

To see who has which flag:

```sh
npx wrangler d1 execute prod-arakun-cloud --env production --remote --command "
  SELECT u.github_id, u.login, f.flag FROM user_flags f JOIN users u USING (github_id)"
```

## API

Times are Unix seconds. The request and answer bodies of the team endpoints, and
the checks on them, are in [PROTOCOL.md](PROTOCOL.md).

| Request | Answer |
| --- | --- |
| `POST /auth/github` with `{"access_token": "..."}` | `200 {session, expires_at, github_id, login, flags}`. `400` without a token, `401` if GitHub rejects it, another app issued it or it was already used to sign in, `413` for a body over 4 KiB, `429` after about 10 tries a minute from one IP (see the rate limit above), `502` if GitHub cannot be reached, fails or does not revoke the token, `503` without `GITHUB_CLIENT_SECRET`. |
| `GET /auth/github/start` | The page's sign-in: `302` to GitHub, with the `state` in the cookie `__Host-arakun_oauth`. `302` to `/?sign_in=failed` without `GITHUB_CLIENT_SECRET`. |
| `GET /auth/github/callback?code&state` | `302` to `/` with the session in the cookie `__Host-arakun_session`. `302` to `/?sign_in=failed` for a `state` that is not the cookie's, a refusal by GitHub, the sign-in rate limit or any other failure. Clears the state cookie either way. |
| `GET /me` with the session | `200 {github_id, login, flags}`, or `401`. |
| `POST /auth/logout` with the session | `204`, and the session is gone. `401` if it was not valid. |
| `PUT /admin/users/<github_id>/flags/<flag>` with `ADMIN_SECRET` | Grants the flag: `200 {github_id, flags}`. |
| `DELETE /admin/users/<github_id>/flags/<flag>` with `ADMIN_SECRET` | Takes it away: `200 {github_id, flags}`. |
| `DELETE /admin/users/<github_id>/sessions` with `ADMIN_SECRET` | Ends all the user's sessions: `200 {github_id, closed}`, with how many ended. |
| `GET /healthz` | `200 {"ok": true}` |

The admin endpoints answer `401` without the exact secret and `400` for a bad ID or
flag. A flag name is a lower-case letter, then up to 31 of `a-z`, `0-9` and `_`.
Anything else answers `404`. There is no CORS: only the desktop app calls this.

### Teams

Every request below needs the session and the `teams` flag: `401` without a session,
`403` without the flag. Team requests answer `404` to anyone who is not a member
(pending members included, except on `remind`), and "admin" ones `403` to members who are not an owner or
admin. "Device" means the request carries `X-Arakun-Device` with one of the caller's
devices: `400` without it, `403` if it is someone else's.

| Request | Who | Answer |
| --- | --- | --- |
| `POST /devices` `{sig_pub, enc_pub, name, endorsement}` | anyone | `201 {device_id}`, `200` if registered already. `400` for a bad key, name or endorsement; `409` if another account has the key, or past 20 devices. |
| `GET /users/<github_id>/devices` | the user, or someone in a team with them | `200 [{device_id, sig_pub, enc_pub, name, created_at, endorsed_by, endorsement_sig}]` |
| `POST /teams` `{name}` | anyone | `201 {team_id, epoch: 1}`; the caller is the owner. `409` if the caller is in 50 teams already. |
| `GET /teams` | anyone | `200 [{team_id, name, role, epoch, rotation_pending}]` |
| `GET /teams/<team_id>` | member | `200 {team_id, name, epoch, keyed_epoch, rotation_pending, members, channels}` |
| `POST /teams/<team_id>/invites` `{github_login}` | admin | `201 {invite_id, expires_at, github_id, login}`, 7 days, for the GitHub user who has the login now. `404` if none has it, `502` if GitHub could not be asked, `409` if a member or pending already. |
| `GET /teams/<team_id>/invites` | admin | `200 [{invite_id, login, invited_by_login, created_at, expires_at}]`, open ones, newest first. |
| `DELETE /teams/<team_id>/invites/<invite_id>` | admin | `204`. `404` if the team has no such open invite. |
| `GET /invites` | anyone | `200 [{invite_id, team_id, team_name, invited_by_login, created_at, expires_at, member_count}]` |
| `POST /invites/<invite_id>/accept` | the invitee, device | `200 {team_id, status: "pending"}`. `404` if expired or someone else's. |
| `POST /invites/<invite_id>/decline` | the invitee | `204`, and the invite is gone. `404` if expired or someone else's. |
| `GET /pending` | anyone | `200 [{team_id, team_name, requested_at, invited_by_login, invited_by_id, reminded_at}]`, the caller's own requests. |
| `POST /teams/<team_id>/remind` | pending in that team | `204`, sets `reminded_at`. `429 {error, retry_at}` within an hour of the last one; `404` for anyone not pending there, members included. |
| `GET /teams/<team_id>/pending` | admin | `200 [{github_id, login, invited_login, request_id, requested_at, reminded_at, devices}]` |
| `POST /teams/<team_id>/pending/<github_id>/approve` `{request_id, envelopes, devices?, keyed_epoch?}` | admin, device | `200 {github_id, role: "member"}`. `400` unless there is one envelope per device of theirs and epoch with a key; `409` before the team's first key, at 25 members, or if the request, the team's key or their devices changed meanwhile. `403` if the caller lost admin, the session or the flag meanwhile. |
| `POST /teams/<team_id>/pending/<github_id>/reject` `{request_id}` | admin | `204`; `409` if the request changed. |
| `PUT /teams/<team_id>/envelopes` `{envelopes}` | admin, device | `200 {stored, epoch, rotation_pending}`; `stored` counts what was written. At most 500. `409` when a new key lost to a removal, another device's key or a new member or device. |
| `GET /teams/<team_id>/envelopes` | member, device | `200 [Envelope + made_at]`, the device's own; `made_at` is when the epoch's key first reached the team. |
| `DELETE /teams/<team_id>/members/<github_id>` | admin, or the member themselves | `200 {epoch, rotation_pending: true}`. Their sockets close. `409` for the last owner. |
| `POST /teams/<team_id>/channels` `{channel_id, name_ct}` | admin | `201 {channel_id}`. `409` for an ID in use or past 500 channels. |
| `PUT /teams/<team_id>/channels/<channel_id>` `{name_ct}` | admin | `200 {channel_id}`: the name sealed again with the newest key, or a new name. `409` if that key changed meanwhile. |
| `GET /teams/<team_id>/channels/<channel_id>/ws` | member, device | `101`, the channel's socket. `426` without `Upgrade: websocket`. |
| `GET /teams/<team_id>/channels/<channel_id>/messages?after=<seq>&limit=<n>` | member, device | `200 [{seq, at, message}]`, oldest first; `limit` 1 to 200. |

A channel keeps at most 10,000 messages. A send that makes it more than
10,000 drops the oldest, so the newest 9,000 stay. If the first history row has a
`seq` greater than `after + 1`, the missing prefix is no longer available on
the server. Mark the gap, keep any local copies, and resume from the first
returned row instead of requesting the missing prefix again.
