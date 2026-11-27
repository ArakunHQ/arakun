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
answers `410 { "update": true, "account_host": "<ACCOUNT_HOST>" }` (no `account_host`
while that is unset), and the page itself still loads.
On the account host, a `GET` or `HEAD` outside the API's paths serves the page, so
`https://app.arakun.dev/` opens it; an unknown path under an API route stays a JSON 404.

```
cloud/
  src/index.ts          old label hosts or the account host; exports both Durable Objects
  src/machines.ts       GET /machines, DELETE /machines/<label>, the sockets
                        /machines/<label>/pc and /ws, and the recheck after a sign-out
  src/relay/router.ts   the old label hosts: "update", and the page
  src/relay/relay.ts    the Relay Durable Object: computer claim, registration, devices,
                        channels, limits, the recheck of sessions, the Linear delivery
                        queue, alarm
  src/relay/limits.ts   plans and the kill switch, read from KV
  src/relay/frames.ts   binary frames and control messages
  src/relay/label.ts    label = base32 of the first 80 bits of SHA-256(public key)
  src/teams-router.ts   the teams routes; every error is JSON: { "error": "..." }
  src/auth.ts           POST /auth/github, GET /me, POST /auth/logout, sessions
  src/admin.ts          PUT /admin/github/<github_id>,
                        PUT|DELETE /admin/users/<account_id>/flags/<flag>,
                        DELETE /admin/users/<account_id>/sessions,
                        PUT|DELETE /admin/users/<account_id>/block,
                        PUT /admin/users/<account_id>/plan, GET /admin/machines,
                        POST /admin/notices, DELETE /admin/notices/<id>,
                        GET /admin/cloud/requests,
                        PUT /admin/cloud/requests/<account_id>/grant|decline,
                        GET /admin/cloud/machines,
                        POST /admin/cloud/machines/<label>/sleep,
                        PUT|DELETE /admin/cloud/stop,
                        PUT|DELETE /admin/users/<account_id>/cloud/stop,
                        GET /admin/beta/signups,
                        PUT /admin/beta/signups/<account_id>/<platform>/admit|decline,
                        GET /admin/log
  src/admin_lists.ts    GET /admin/users, GET /admin/users/<account_id>,
                        GET /admin/notices, GET /admin/plans
  src/admin_log.ts      the admin log: record() and GET /admin/log
  src/beta.ts           sign-ups for the phone app betas: GET /beta, POST /beta/<platform>,
                        and the operator's side of them
  src/cloud_access.ts   requests for cloud access: GET|POST /cloud/access, and the
                        operator's side of them
  src/cloud_machine.ts  the CloudMachine Durable Object: one container running
                        arakun-server per cloud machine, its key, start, sleep and wake
  src/cloud_machines.ts the account's cloud machines: /cloud/machines, /cloud/usage,
                        and the operator's side of them
  src/cloud_model.ts    the owner's model credentials for a cloud machine, sealed
                        with AES-GCM under CLOUD_MODEL_KEY
  src/cloud_meter.ts    awake time of cloud machines: the monthly allowance, the
                        task cap and the operator's switches
  machine/              the cloud machine's image: Dockerfile, entrypoint and the
                        wrapper that runs agents as their own user. Noncommercial
                        license (machine/LICENSE), not AGPL-3.0
  src/feedback.ts       feedback from the app as Linear customer requests: POST /feedback
  src/notices.ts        notices: POST /notices/<id>/read, and what /me carries
  src/linear.ts         Linear delegation: the /linear gate (secrets, session and flag), the
                        install, the links, the routes, the delivery accept and the
                        computer's activity
  src/linear_webhook.ts POST /linear/webhook: the signature check, then a delegated issue
                        or a follow-up becomes a delivery for a computer
  src/linear_api.ts     calls to Linear as the app: token exchange and refresh, GraphQL,
                        activities, links and the place of an issue
  src/linear_seal.ts    seals Linear tokens with AES-GCM under LINEAR_TOKEN_KEY
  src/linear_expire.ts  what a session hears when a delivery waited 7 days unanswered
  src/push.ts           the native apps' push service: POST /push/v1/<service>/<token>
  src/teams.ts          devices, teams, invites, envelopes, members, channels
  src/channel.ts        the Channel Durable Object: messages, seq, live sockets
  src/wire.ts           the shapes of PROTOCOL.md and their signature checks
  migrations/           the D1 tables, applied in order by wrangler
  test/                 Vitest inside workerd, with real D1, KV, the real Durable
                        Objects, real P-256 keys and a fake GitHub
  test/relay/           the relay's tests
  scripts/peers.mjs     one fake computer and two fake devices against a running Worker
  admin/                the admin Worker: the operator panel at admin.arakun.dev (see
                        "The admin panel"); its own package, wrangler.toml and tests
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

A session has a kind: `app`, `server`, `phone`, `ios` or `android`, as the sign-in
says, or `cloud`. No sign-in makes a `cloud` session: the Worker makes one for a
machine it runs in a container, bound to that machine's label. An agent in the
container could leak the token, so it is good only at `/machines/<label>/pc` of its
own label and at `GET /sessions`, which answers it with the ids alone (no names or
times): the server asks there which devices are still signed in, and takes a `401` as
the end of its own sign-in. Everywhere else, the other labels and every device door
included, it counts as no session (`401`, or `signed_out` on a socket). A machine has
one cloud session at a time, and a new one ends the one before. Cloud sessions do not count toward the 100,
and a sign-in never ends one to make room. They expire and renew like any session,
`GET /sessions` lists them with kind `cloud`, and `DELETE /sessions/<id>`, a block
and signing out everywhere end them.

An account has an ID of its own, a number. GitHub is one identity linked to it, by the
numeric GitHub ID, which stays when the person renames their GitHub account: a sign-in
finds the account of that GitHub user, or makes one. Accounts made before accounts had
their own IDs kept their GitHub ID as their account ID; a new account gets a random ID
from 2^32 to 2^53, so it is never also a GitHub ID. The app, the phone and the teams
API still call the account ID `github_id`, the name it had before; `/auth/github` and
`/me` answer it as `account_id` too.
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
   `relay-hotfix/*` before, and `main`, for a manual deploy of `main`.
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
   account on it may register (10 when it is missing). The namespace is the same as
   the old relay's, so the other limits carry over:

   ```sh
   npx wrangler kv key put --env production --remote --binding CONFIG plans '{"free":{"machines":10,"phones":5,"burst":100,"daily":100000,"frame":262144}}'
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
the top of that workflow. To ship what is on `main` without a new version, run the
workflow by hand on `main`:

```sh
gh workflow run cloud-deploy.yml --ref main
```

It deploys only if `main` contains the latest release's tag. A later redeploy of that
tag, or a hotfix made from it, goes back to the release without `main`'s changes. The
deploy's message names the release, the hotfix or `main`'s commit. The
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

   Optionally, a Discord webhook URL that hears of each new request for cloud
   access (see Cloud access). Without it, nothing is posted:

   ```sh
   npx wrangler secret put CLOUD_REQUESTS_WEBHOOK --env production
   ```

   Optionally, the keys that let the iOS and Android apps get notices (see Push to
   the apps). Without them, `/push` answers 503 and the apps get none:

   ```sh
   # Apple: the .p8 file's text and its key ID (developer.apple.com → Keys, with
   # Apple Push Notifications service). The team and bundle ID are in wrangler.toml.
   npx wrangler secret put APNS_KEY --env production < AuthKey_XXXXXXXXXX.p8
   npx wrangler secret put APNS_KEY_ID --env production
   # Google: a service account's JSON key from the Firebase project, with the
   # Firebase Cloud Messaging API Admin role.
   npx wrangler secret put FCM_SERVICE_ACCOUNT --env production < service-account.json
   ```

   Optionally, a Linear API key that turns the app's feedback into customer
   requests in the team `LINEAR_TEAM_ID` names (Linear → Settings → Security &
   access → Personal API keys, with access to create issues and customers).
   Without it, `/feedback` answers 503 and feedback stays in PostHog only:

   ```sh
   npx wrangler secret put LINEAR_API_KEY --env production
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

The built-in plans' limits apply until you set `plans` in KV (see below).

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
| `plans` | `{"free":{"machines","phones","burst","daily","frame","cloudMinutes","cloudTaskMinutes"}, "<plan>":{…}}` | Machines an account may register, then per machine: devices at once, frames per second, frames per UTC day, bytes per frame; then the minutes its cloud machines may be awake in a UTC month and in one task (see Cloud machines). Each object rereads it every minute. An account is on `free` until the operator sets another plan. `free` (10 machines, 5 devices, 100 frames per second, 100,000 per day, 256 KiB frames, 20 cloud hours a month and 2 a task) and `internal` (for the team: 50 machines, 10 devices, 500 frames per second, 1,000,000 per day, 1 MiB frames, 200 cloud hours and 8 a task) are built in, and KV overrides their fields; any other name KV does not have is the free plan |
| `disabled` | `true` or a message | Both sockets close with `bye` `disabled` and the message (`message`); a plain request to them answers 503 with it |
| `cloud_stopped` | `true` or a message | Every cloud machine sleeps at its next check and no wake starts one; the message is what owners read. `PUT /admin/cloud/stop` sets it and stops them at once |

```sh
# arakun.dev's own Worker ([env.production])
npx wrangler kv key put --binding CONFIG --env production --remote plans '{"free":{"machines":10,"phones":5,"burst":100,"daily":100000,"frame":262144}}'
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
# every machine: label, owner's account ID, GitHub ID and login, name, kind, created and last seen
auth | curl -fsS -H @- "https://<host>/admin/machines"
# block an account: its computers and devices are closed now, new sockets are refused
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"reason":"abuse report 12"}' \
  "https://<host>/admin/users/<account_id>/block"
auth | curl -fsS -X DELETE -H @- "https://<host>/admin/users/<account_id>/block"
# put an account on a plan from `plans`; open sockets follow at once
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"plan":"pro"}' \
  "https://<host>/admin/users/<account_id>/plan"
# a notice for one account (leave out account_id for everyone). text is 1 to 500 characters,
# url must be https://, expires_at is Unix seconds in the future. Answers {"id": "..."}
auth | curl -fsS -X POST -H @- -H "Content-Type: application/json" \
  -d '{"account_id":1001,"text":"Your plan is now Pro.","url":"https://arakun.dev/pricing","expires_at":1893456000}' \
  "https://<host>/admin/notices"
# take a notice back, with the reads of it
auth | curl -fsS -X DELETE -H @- "https://<host>/admin/notices/<id>"
# accounts, last seen first: {"users": [...], "next": "<cursor>" or null}.
# q is a login prefix, an account ID or a GitHub ID, limit is 1 to 100 (default 50), after is the last next
auth | curl -fsS -H @- "https://<host>/admin/users?q=octo&limit=20"
# one account: user, sessions (no token hashes), machines, cloud_request and notices
auth | curl -fsS -H @- "https://<host>/admin/users/<account_id>"
# every notice, newest first, at most 500, each with how many accounts read it
auth | curl -fsS -H @- "https://<host>/admin/notices"
# the built-in plans and those in the KV `plans`, `free` first: {"plans": ["free", "internal", "pro"]}
auth | curl -fsS -H @- "https://<host>/admin/plans"
# what was changed and by whom, newest first: {"entries": [...], "next": "<cursor>" or null}.
# account_id narrows it to one account, limit is 1 to 100, before is the last next
auth | curl -fsS -H @- "https://<host>/admin/log?account_id=<account_id>&limit=20"
```

Every admin route that changes something adds one row to the `admin_log` table:
when, which operator, what was done, which account and a short detail. The Worker
only appends to it and has no route that edits or deletes a row, and a row stays
after its account is deleted. The reads above change nothing and write no row.

The admin panel names the operator in the record with two optional headers,
`X-Arakun-Operator-Id` (a GitHub ID) and `X-Arakun-Operator-Login`. The Worker reads
them only after the bearer check. A call without them, such as the curl above, is
recorded with no operator. Add them to a call to name yourself:

```sh
printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET" | curl -fsS -X PUT -H @- \
  -H "X-Arakun-Operator-Id: <your GitHub ID>" -H "X-Arakun-Operator-Login: <your login>" \
  "https://<host>/admin/users/<account_id>/flags/teams"
```

`GET /me` lists an account's unread notices, oldest first, at most 5, as
`notices: [{ id, text, url }]`. `POST /notices/<id>/read` with a session dismisses
one: `204`, also on a second read; `404` for one that is not the caller's, has expired
or does not exist.

A blocked account can still sign in and read `/me`; `GET /machines` answers it
`403 { "error": "blocked", "reason": … }` with the reason given. The teams API
answers it `403 { "error": "blocked" }`, and a channel closes its socket at the next
message there. Its teams stay as they are: block a team's only owner and nobody can
change a role, and while no admin is left, nobody can approve a join or make the
next key until the block is lifted.

Each object checks its sockets' sessions and account in D1 every 15 minutes, and at
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

Flags are granted to an account, by its account ID. Keep the list of pilot users out
of this repository: type the IDs on the command line when you grant them. To grant
one to a person by GitHub user, look up their GitHub ID, which is public, and ask for
their account: `PUT /admin/github/<github_id>` answers `{account_id, github_id,
created}`, and makes the account first if they never signed in. Its login is then
empty until they do.

```sh
gh api users/<login> --jq .id
```

With the admin endpoint, in a shell where `ADMIN_SECRET` holds the value you saved
in step 3 (paste it; the variable is not set anywhere else). Run this in Bash or zsh:
`read -s` hides what you paste, and `sh` (dash) has no `-s`. The header goes to curl
on its input (`-H @-`), from the shell's own `printf`, so the secret never shows in
the list of running processes:

```sh
read -rs ADMIN_SECRET
for github_id in <id> <id> <id>; do
  id=$(printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET" |
    curl -fsS -X PUT -H @- "https://<host>/admin/github/$github_id" | jq .account_id)
  printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET" |
    curl -fsS -X PUT -H @- "https://<host>/admin/users/$id/flags/teams"
done
```

Or straight in the database, for an account that exists (the admin route is the way
to make one):

```sh
npx wrangler d1 execute prod-arakun-cloud --env production --remote --command "
  INSERT INTO user_flags (account_id, flag, granted_at)
    SELECT account_id, 'teams', unixepoch() FROM identities
    WHERE provider = 'github' AND subject = '<github_id>' ON CONFLICT DO NOTHING;"
```

To sign a user out everywhere, as when a session token may have leaked (they can
sign in again; take their flags away to keep them out):

```sh
read -rs ADMIN_SECRET
printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET" |
  curl -fsS -X DELETE -H @- "https://<host>/admin/users/<id>/sessions"
```

## The admin panel

`admin/` is a second Worker, `arakun-admin`, with its own package. It serves a React page
for operators (users, cloud access, phone betas, machines, notices, the activity log) and signs them
in with GitHub, using its own OAuth App, not the desktop app's. The browser never holds
`ADMIN_SECRET`: the admin Worker checks the sign-in, then forwards `/api/*` to this
Worker's `/admin/*` routes over the `CLOUD` service binding and adds the secret. A
session is a signed cookie, `__Host-arakun_admin`, that lasts 12 hours.

Sign-in has two steps, so a taken-over GitHub account is not enough to get in:

1. GitHub's web flow says who the operator is. The Worker then sets
   `__Host-arakun_admin_code` (`SameSite=Strict`, ten minutes), which is not a session,
   and sends the browser to `/sign-in?step=code`, or to `/sign-in?step=setup` for an
   operator with no authenticator app yet.
2. The operator types the six-digit code from an authenticator app (TOTP, RFC 6238:
   SHA-1, 30-second steps; the codes of the step before and after count too). The page
   posts it to `POST /auth/code`, which needs the panel's own `Origin`. A right code
   trades the code step's cookie for the session. Each code works once, and none older
   than the last one taken. Each operator gets five codes a minute, wherever they come
   from.

Operators set up their own authenticator app. At the first sign-in, the page shows a new
key as a QR code and as text (`POST /auth/setup/start`), and stores it once a code from
the app is right (`POST /auth/setup`). This step can set up a first key only, never
replace one, so a taken-over GitHub account cannot swap in its own. Until then the
first person through GitHub as that operator gets to set it up, so set it up soon after
adding an operator. Settings replaces the app, for a new phone; that needs a code from
the current app as well as the new one.

The keys are in this Worker's D1, in `operator_totp`, sealed by the admin Worker with
AES-GCM and a key derived from `SESSION_SECRET`: the database alone cannot make a code.
The admin Worker reaches them at `/admin/operators/<github_id>/totp` and never forwards
the page there.

Every change made in the panel is recorded in `admin_log` with the operator's name, and
the panel shows it under Activity.

### Running both Workers locally

The two Workers find each other through wrangler's dev registry, so they must run with
the same wrangler version and the same registry path. `cloud/` and `cloud/admin/` pin
different wrangler versions that use different default paths, so run the cloud Worker
with the admin package's wrangler and set `WRANGLER_REGISTRY_PATH` for both:

```sh
(cd cloud/admin && npm ci)
export WRANGLER_REGISTRY_PATH=/tmp/arakun-dev-registry

# terminal 1, in cloud/: the cloud Worker (see Development above for the migrations)
admin/node_modules/.bin/wrangler dev

# terminal 2, in cloud/admin/: the panel, on http://localhost:5173
npm run dev
```

`cloud/.dev.vars` has `ADMIN_SECRET`. `cloud/admin/.dev.vars` (git-ignored) has the same
`ADMIN_SECRET`, plus:

```sh
ADMIN_SECRET=dev-secret
SESSION_SECRET=<any long random string>
ADMIN_GITHUB_IDS=<github_id>
GITHUB_CLIENT_ID=<your OAuth App's client ID>
GITHUB_CLIENT_SECRET=<your OAuth App's secret>
```

For a real sign-in, create a personal OAuth App on GitHub (Settings, Developer
settings) whose callback is `http://localhost:5173/auth/github/callback`; use the port
vite prints. Without an OAuth App or an authenticator app, print a session cookie for the local
`SESSION_SECRET` and send it from your browser's developer tools or with curl:

```sh
node scripts/dev-cookie.mjs <github_id> <login>   # in cloud/admin
```

The Worker still checks the ID against `ADMIN_GITHUB_IDS`. The dev server logs one
content-security-policy error per page load, from Vite's reload script. It is harmless
and does not happen in a build.

`npm run check`, `npm test` and `npm run build` in `cloud/admin` are the checks CI runs.

### First production deploy of the admin panel

For arakun.dev, the GitHub OAuth App "Arakun Admin" exists and its client ID is already
in `cloud/admin/wrangler.toml`. Do not create another app: its secret would not match
that ID, and sign-in would fail with no hint why. What is left is the four secrets, set
once in `cloud/admin`. `ADMIN_SECRET` is the value the cloud Worker has;
`GITHUB_CLIENT_SECRET` is the secret of the "Arakun Admin" app; `SESSION_SECRET` is 32
random bytes (`openssl rand -base64 32`); `ADMIN_GITHUB_IDS` is the operators' numeric
GitHub IDs, comma-separated (find one at `https://api.github.com/users/<login>`).
Each command asks for the value, so it is never on the command line:

```sh
cd cloud/admin
npx wrangler secret put GITHUB_CLIENT_SECRET --env production
npx wrangler secret put ADMIN_SECRET --env production
npx wrangler secret put ADMIN_GITHUB_IDS --env production
npx wrangler secret put SESSION_SECRET --env production
```

If no deploy came first, the Worker does not exist yet, and wrangler asks whether to
create it with the secrets. Answer yes. The Worker deploys with the next release, or
at once with a manual run of the deploy workflow on `main`:

```sh
gh workflow run cloud-deploy.yml --ref main
```

By hand, `npm run deploy` in `cloud/admin` does the same; it builds with
`CLOUDFLARE_ENV=production`, which a production build needs. Until the secrets are set,
the Worker fails closed: nobody can sign in.

#### A copy of the repository with its own domain

A copy needs an OAuth App of its own, because the committed client ID belongs to
arakun.dev.

1. On GitHub, create an OAuth App named "Arakun Admin" with the callback
   `https://<your admin host>/auth/github/callback`.
2. In `cloud/admin/wrangler.toml`, put the app's client ID under
   `[env.production.vars]`, and set the host under `routes`.
3. Set the four secrets as above, with the new app's secret, then merge the change.

While `GITHUB_CLIENT_ID` is `PLACEHOLDER`, the deploy workflow prints a notice and skips
the admin Worker, so a release still ships the cloud Worker.

### Operators

Who may sign in is the secret `ADMIN_GITHUB_IDS`, so no ID is in the repository. To add
or remove an operator, put the new list and the secret takes effect at once:

```sh
cd cloud/admin
npx wrangler secret put ADMIN_GITHUB_IDS --env production   # e.g. <github_id>,<github_id>
```

The ID is checked on every request, so removing one ends that operator's access at
once, even with a valid cookie. Unset or empty, nobody gets in. Changing
`SESSION_SECRET` signs everyone out.

A new operator sets up an authenticator app at their first sign-in. For one who lost
their phone, forget their key; they set up a new one at their next sign-in, which is
recorded in Activity:

```sh
printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET" | curl -fsS -X DELETE -H @- \
  "https://app.arakun.dev/admin/operators/<github_id>/totp"
```

Changing `SESSION_SECRET` also makes every stored key unreadable, and the page tells
each operator to ask for a reset. Forget them all after such a change:

```sh
npx wrangler d1 execute prod-arakun-cloud --env production --remote --command "DELETE FROM operator_totp"   # in cloud/
```

If `ADMIN_SECRET` on the admin Worker is wrong or missing, operators stay signed in but
the panel's calls fail with "unauthorized" errors (a missing one answers "the admin
secret is not set"). Set it again to the cloud Worker's value.

## Cloud access

Cloud environments run on CPU we pay for, so they need the `cloud` flag, and an
account asks for it rather than getting it at sign-in. The app calls
`POST /cloud/access`; the request keeps what GitHub's public profile said about the
account then (when it was made, public repositories, followers, following) for the
review. One request per GitHub account. A declined account may ask again after
30 days (`DECLINE_COOLDOWN_DAYS` in `src/cloud_access.ts`). Revoking the `cloud`
flag of a granted account lets it ask again; to keep someone out, block the account.

With `CLOUD_REQUESTS_WEBHOOK` set, each new request is posted to that Discord
webhook: the login, the GitHub ID, the account's age, public repositories and
followers. Never the reason the person wrote, an email or a token.

```sh
read -rs ADMIN_SECRET
auth() { printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET"; }
# the requests: pending first, oldest first, then the decided ones. ?status=pending for only those
auth | curl -fsS -H @- "https://<host>/admin/cloud/requests"
# grant one: the request and the `cloud` flag. "by" is optional, at most 100 characters
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"by":"<you>"}' \
  "https://<host>/admin/cloud/requests/<account_id>/grant"
# decline a pending one
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"by":"<you>"}' \
  "https://<host>/admin/cloud/requests/<account_id>/decline"
```

## Cloud machines

A cloud machine is `arakun-server` in a Cloudflare container that we run for an
account with the `cloud` flag (`src/cloud_machine.ts`, the image in `machine/`).
It registers on the relay like any computer of the account, so the app and the
phone reach it at the usual `/machines/<label>` doors. One per account for now.
The phone's Computers screen makes, wakes, stops and removes it, and shows the
hours left this month and why it stopped; an account without the flag asks for
access there.

- **Identity.** The machine's object makes its relay key when the machine is
  made, so the label is known before the container first runs. The key goes in
  only on a start from the image; a snapshot has it on disk.
- **Sign-in.** Each start makes a new `cloud` session for the label, which ends
  the one before and is good only at that label's computer door and for the list of
  the account's sessions the server asks for ("How sign-in works"). It goes in as an
  environment variable. Sleep removes the server's sign-in file and ends the
  session before the snapshot, so no snapshot holds a live token.
- **Model credentials.** The agents run on the owner's own account with the model
  provider, in one of two ways. A plan's sign-in never passes through the Worker,
  and no snapshot holds one that can be read.
  - *A sign-in* to a plan, made inside the machine with the agent's own flow: a
    Claude plan (Pro, Max, Team) for Claude Code, a ChatGPT plan for Codex. Open a
    terminal on the machine and run `claude`, then `/login`, or run
    `codex login` (which asks for a device code here); open the link it prints and enter the code.
    `claude` and `codex` there are `machine/run`, which runs the CLI as `agent`,
    so the sign-in lands in that user's home folder, where the sessions find it.
    The token goes from the provider to the machine's disk: the Worker never sees
    it, and no route takes one. A snapshot holds it only sealed: before each
    one, `model-env forget` seals the two sign-in files under a new key and
    removes them, and the machine's object keeps that key with the snapshot's
    record, never on the disk. The wake from that snapshot hands the key back and
    the files return, so the owner signs in once. A snapshot alone gives no
    token. `/logout` or `codex logout` there, or removing the machine, ends
    it. With both an API key and a sign-in, Claude Code uses the key and Codex
    uses the sign-in.
  - *An API key*, given to `PUT /cloud/machines/<label>/model/<provider>`
    (`src/cloud_model.ts`): an Anthropic API key, Amazon Bedrock or Google Vertex AI
    for Claude Code, one of the three at a time, and an OpenAI API key for Codex.
    The machine's object keeps it sealed with AES-GCM under `CLOUD_MODEL_KEY`, with
    the label and the provider as additional data, and no route gives it back. It
    goes into the container on every start and when it changes:
    `machine/model-env` writes it under `/run/arakun` for root alone, and
    `machine/as-agent` puts it in the environment of each agent it starts, so the
    server never holds it. A session that runs keeps the key it started with.
    Before a snapshot, sleep runs `model-env forget`, which stops the agents,
    removes `/run/arakun` and the copy Codex makes of an API key in its sign-in
    file, and it takes no snapshot when that fails, so no snapshot holds a key. An agent can read its
    own environment, so the key is as safe as what the owner lets the agent do.
  Other agents (Droid, Cursor, Devin, Antigravity, OpenCode) are not in the image:
  the server does not install their CLIs, and each comes from its maker's installer.
- **Lifecycle.** A device at the machine's door wakes it, from its snapshot when
  there is one. Every 5 minutes an alarm asks the server (`arakun-server status`)
  whether an agent turn runs or a device is connected; while one does, it renews
  the container's inactivity timeout, since work inside the container is no
  activity to Cloudflare. After 15 minutes of neither, it snapshots the disk and
  stops the container. Taking the `cloud` flag or blocking the account stops the
  machine at the next check and refuses the next wake.
- **Users.** `arakun` runs the server; `agent` runs the agents, without
  capabilities, through `machine/run`. Tried on a Cloudflare container:
  the server runs without capabilities, and a process started the way
  `machine/as-agent` starts the adapter has none, cannot get them back and cannot
  read the server's sign-in or key. Only what the object starts with `exec` has
  root's capabilities whatever its user, as Cloudflare says of the
  `durable_object` policy; the object runs `arakun-server status`, `rm` and
  `machine/model-env` that way, never an agent. The session's one door still limits a token that leaks.
- **Size.** 1 vCPU, 6 GiB and 20 GB of disk (`INSTANCE`), the most disk an
  account gets and a snapshot holds.
- **Metering** (`src/cloud_meter.ts`). The clock runs from a container's start
  to the stop after its snapshot, and not while it sleeps. Each check adds the
  time to the account's row in `cloud_usage` for the UTC month; a container that
  stopped by itself counts to its last check. The machine sleeps at the first
  of these, and a wake past one answers `403 {error, limit}`:
  - `allowance_used`: the account's machines were awake for the plan's
    `cloudMinutes` this month (20 hours on `free`). It wakes again next month,
    or once the plan has more.
  - `task_cap`: one task, a stretch awake from a wake to the sleep, ran the
    plan's `cloudTaskMinutes` (2 hours on `free`). A device at the door does not
    wake it after that, or a phone left open would start the next task at once;
    the owner's `POST /cloud/machines/<label>/wake` does.
  - `held` and `stopped_all`: the operator's switches, for one account and for
    everyone (below).

  The alarm comes early when a limit is nearer than the next check, so a machine
  stops on time. The owner gets a notice for the first two.
  The allowance and the paid rate past it are not decided yet; the defaults are
  the #561 rehearsal's.

Only the test Worker (the top level of `wrangler.toml`) has the container and the
`CLOUD_MACHINE` binding; production answers 503 on these routes until both are
added under `[env.production]`, with the migration tag `v3`, and the deploy token
may push container images. `wrangler deploy` of the test Worker builds the image
with Docker, from the repository (`/.dockerignore` lets in what it needs); the
tests and `wrangler dev` run without it.

`CLOUD_MODEL_KEY` is 32 random bytes in base64url
(`openssl rand -base64 32 | tr '+/' '-_' | tr -d '='`, then
`npx wrangler secret put CLOUD_MODEL_KEY --env=""`). Without it a machine takes no API
key (`503`) and the owner signs in inside it. Losing or changing it makes every
stored key unreadable: the agents then say they are not signed in, and each owner
sets the key again.

```sh
# the account's machines; make one (it starts at once), wake it, put it to sleep, remove it
curl -fsS -H "Authorization: Bearer <session>" "https://<host>/cloud/machines"
curl -fsS -X POST -H "Authorization: Bearer <session>" -H "Content-Type: application/json" \
  -d '{"name":"Cloud"}' "https://<host>/cloud/machines"
curl -fsS -X POST -H "Authorization: Bearer <session>" "https://<host>/cloud/machines/<label>/wake"
curl -fsS -X POST -H "Authorization: Bearer <session>" "https://<host>/cloud/machines/<label>/sleep"
curl -fsS -X DELETE -H "Authorization: Bearer <session>" "https://<host>/cloud/machines/<label>"
# the model credentials: which providers are set, set one, remove one
curl -fsS -H "Authorization: Bearer <session>" "https://<host>/cloud/machines/<label>/model"
curl -fsS -X PUT -H "Authorization: Bearer <session>" -H "Content-Type: application/json" \
  -d '{"api_key":"<key>"}' "https://<host>/cloud/machines/<label>/model/anthropic"
curl -fsS -X DELETE -H "Authorization: Bearer <session>" "https://<host>/cloud/machines/<label>/model/anthropic"
# this month's awake time and limits
curl -fsS -H "Authorization: Bearer <session>" "https://<host>/cloud/usage"
# the operator: every machine, and stopping one now
auth | curl -fsS -H @- "https://<host>/admin/cloud/machines"
auth | curl -fsS -X POST -H @- "https://<host>/admin/cloud/machines/<label>/sleep"
# stop one account's cloud machines until lifted (its flag stays); answers {"machines": n}
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"reason":"abuse report 12"}' \
  "https://<host>/admin/users/<account_id>/cloud/stop"
auth | curl -fsS -X DELETE -H @- "https://<host>/admin/users/<account_id>/cloud/stop"
# stop every cloud machine at once, until lifted; the reason is what owners read
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"reason":"maintenance until 05:00 UTC"}' \
  "https://<host>/admin/cloud/stop"
auth | curl -fsS -X DELETE -H @- "https://<host>/admin/cloud/stop"
```

The switch for everyone is the KV key `cloud_stopped`, so it also works from
wrangler when the Worker does not answer; a machine that missed the stop sleeps
at its next check, within 5 minutes.

## Phone app betas

The iOS app is tested through TestFlight and the Android app through a Play testing
track. Both invite testers by email and cap how many there are, so testers are
admitted by hand. The desktop app's Phone tab calls `POST /beta/ios` or
`POST /beta/android` with the email to invite: an Apple ID for TestFlight, a Google
account for Play. With no email it takes the account's verified GitHub email. One
sign-up per GitHub account and platform. Sent again while pending, it changes the
email. A declined account may sign up again after 30 days.

Admitting invites no one. To admit someone:

1. List the pending sign-ups and look at the account (age, repositories, followers).
2. Add the email as a tester: in App Store Connect under TestFlight → External
   Testing (or Internal), or in Play Console under Testing → the track's testers list.
3. Mark it admitted, so the app says the invite is on its way.

To remove a tester, remove them in the store, then decline the sign-up.

The admin panel's **Phone betas** screen does this: it lists sign-ups by status and
platform, copies one platform's emails at once to paste into the store, and asks
before it admits or declines. Without the panel, use curl as below.

With `BETA_REQUESTS_WEBHOOK` set, each new sign-up is posted to that Discord webhook,
with the platform and the same public facts as a cloud access request. Never the
email or a token.

```sh
read -rs ADMIN_SECRET
auth() { printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET"; }
# the sign-ups: pending first, oldest first. ?platform=ios|android and ?status=pending|admitted|declined
auth | curl -fsS -H @- "https://<host>/admin/beta/signups?platform=ios&status=pending"
# the pending iOS emails, one a line, to paste into App Store Connect
auth | curl -fsS -H @- "https://<host>/admin/beta/signups?platform=ios&status=pending" | jq -r '.[].email'
# admit one after adding the email in the store. "by" is optional, at most 100 characters
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"by":"<you>"}' \
  "https://<host>/admin/beta/signups/<account_id>/ios/admit"
# decline one, pending or admitted
auth | curl -fsS -X PUT -H @- -H "Content-Type: application/json" -d '{"by":"<you>"}' \
  "https://<host>/admin/beta/signups/<account_id>/android/decline"
```

To see who has which flag:

```sh
npx wrangler d1 execute prod-arakun-cloud --env production --remote --command "
  SELECT u.id, u.login, f.flag FROM user_flags f JOIN accounts u ON u.id = f.account_id"
```

## Push to the apps

The phone page gets its notices from the browser's own push service. The iOS and
Android apps can only be woken through Apple (APNs) and Google (FCM), with Arakun's
keys, so this Worker is their push service. An app makes its own P-256 key and
auth secret, and gives each computer a Web Push subscription whose endpoint is
here: `https://<host>/push/v1/apns/<token>` (`apns-sandbox` for a development
build) or `https://<host>/push/v1/fcm/<token>`. The computer sends what it sends a
browser's service: an RFC 8291 aes128gcm body with a VAPID token. The Worker
checks the token's signature and audience, and passes the ciphertext on as `e`:
in a mutable alert to Apple, which the app's notification service opens, and in
a data message to Google. Neither the Worker, Apple nor Google can read the
notice. Each token gets 30 messages a minute.

## Linear

Delegating an issue to Arakun in Linear starts a session on the delegator's own
computer. The Worker is the doorbell and the mouthpiece: it hears Linear's webhook,
finds the computer, hands it the ids of the issue through the relay, and posts what the
computer reports back to the issue's session as Arakun. The work itself, and the
issue's text, stay between Linear and the computer.

### What is stored

Four D1 tables (`migrations/0013_linear.sql`):

| Table | Holds |
| --- | --- |
| `linear_workspaces` | One row per Linear workspace that installed the app: the workspace id and name, the app's user id, its access and refresh tokens, when the access token ends, who installed it. |
| `linear_links` | Which GitHub account a Linear user is, per workspace. The row goes with its account. |
| `linear_routes` | Which computer takes the issues of a Linear team or project. The row goes with the machine. |
| `linear_deliveries` | One row per webhook event that became work for a computer: ids, its kind (`created`, `prompt` or `stop`), the computer's label, and the times it was announced, accepted and expired. A unique event key makes Linear's retry a no-op (see "A retry" below). |

Both tokens of a workspace are sealed with AES-GCM under `LINEAR_TOKEN_KEY`, with the
workspace id as additional data, so a token copied to another workspace's row does not
open. The Worker stores and logs no issue text, prompt or progress text: only ids,
times and labels. A follow-up's text is passed on only to a connected computer, in one
frame, and is never stored (see "The relay side"). A user's own Linear token, sent to
link, is used for one call and dropped.

### Routes

Every path is under `/linear`. Without all four secrets, or with a `LINEAR_TOKEN_KEY`
that is not 32 bytes of base64url, every `/linear` path answers
`404 { "error": "not found" }`. Except the webhook and the callback, a route needs a
session of an account that is not blocked and has the `linear` flag: `401 { "error":
"not signed in" }` without a session, `403 { "error": "Linear is not available for this
account" }` otherwise. An unknown path under `/linear` is a `404 not found`. Errors are
JSON `{ "error": "..." }`.

| Request | Who | Answer |
| --- | --- | --- |
| `GET /linear/install` | a signed-in account with the flag, in a browser | `302` to Linear's consent page (scopes `read,write,app:assignable,app:mentionable`, `actor=app`). Sets the cookie `__Host-arakun_linear_state` for ten minutes, holding a random state and the account's ID. |
| `GET /linear/callback?code&state` | Linear's redirect of the same browser | Checks the state against the cookie and the account, then trades the code for the app's tokens and saves the workspace (a second install replaces the first): `200` with a plain-text page, "Arakun is installed in <workspace>. You can close this page." `400 bad state` for a missing or wrong state, or one made by another account; `400 linear refused the install` for an `error` parameter, no code or a code over 512 characters; `400 that was not an app install` when the token is not the app's own (a member who authorized without `actor=app`), and nothing is saved; `502 linear refused the install` when Linear refuses the exchange, does not answer, or names the app user or the workspace with an id that is not a UUID. The session and flag are checked here, so `401` or `403` as above. The cookie is cleared on every answer. |
| `GET /linear/link` | the account | `200 { links: [{ organization_id, linear_user_id, installed }] }`, oldest first. `installed` says whether that workspace has installed Arakun. |
| `POST /linear/link` with `{ "token": "<the user's Linear API key or OAuth token>" }` | the account | Asks Linear who the token is, and links that Linear user in that workspace to the account: `200 { organization_id, linear_user_id, installed }`. If another account held the link, it moves here, and that account's routes in the workspace go. `400 bad token` for a token that is missing, empty or over 512 bytes, `400 linear refused that token` when Linear refuses it, when it is an app's token rather than a person's, or when Linear names the user or the workspace with an id that is not a UUID, `429 too many requests` (see Limits), `503` when the limit could not be checked. |
| `DELETE /linear/link/<organization_id>` | the account | `204`, also when there was no link. Removes the account's link and its routes in that workspace. `404` for an id that is not a UUID. |
| `PUT /linear/routes` with `{ "label", "organization_id", "teams": [<uuid>], "projects": [<uuid>] }` | the account | Replaces what that computer takes in that workspace: `204`. At most 200 ids in all, up to 32 KiB. `400 bad routes` for a bad body, `404 no such computer` for a label that is not one of the account's machines, `409 link linear first` when the account has no link in that workspace. Of two computers that take the same issue, the one with the project route wins over the team route, and then the one seen last. |
| `POST /linear/deliveries/<id>/accept` | the account | The computer took the delivery: `204`, also on a second call. The relay forgets it. `404 no such delivery` for an id that is unknown or another account's. |
| `POST /linear/sessions/<agent_session>/activity` with `{ "type", "body", "links"?, "options"? }` | the account | Posts the computer's progress in the session as Arakun: `204`. `type` is `thought`, `elicitation`, `response` or `error`; `body` is 1 to 8000 characters; `options`, for an `elicitation` only, is 2 to 10 choices of 1 to 80 characters, which Linear shows to pick from (the `select` signal); `links` is at most 5 of `{ label, url }`, a label of 1 to 80 characters and an `https://` URL without spaces, at most 2048 characters, which become the session's external URLs. The body is at most 64 KiB. `400 bad activity`, `404 no such session` for a session that is not a `created` delivery of this account (the workspace comes from the delivery, not the caller), `429`, `503`, `502 linear refused` when Linear refuses or does not answer. |
| `POST /linear/sessions/<agent_session>/links` with `{ "links" }` | the account | Adds external URLs to the session without a new activity, for a link that turns up at any time, such as the pull request: `204`. `links` is 1 to 5 of `{ label, url }`, checked as for an activity. `400 bad links`, `404 no such session`, `429`, `503`, `502 linear refused`, as for an activity. |
| `POST /linear/webhook` | Linear | See below. |

### The webhook

Linear signs the body with HMAC-SHA256 under `LINEAR_WEBHOOK_SECRET` and sends it in
`Linear-Signature`. The Worker compares the two as SHA-256 hashes, in constant time.
It answers `413 too large` for a body over 1 MiB, `401 bad signature` for a wrong or
missing signature, and `400 bad webhook` for a body that is not JSON or lacks the ids.
The body's `webhookTimestamp` must be within 60 seconds of the Worker's clock, or the
answer is `401 bad signature`, so a captured webhook cannot be replayed later. Any
event but an `AgentSessionEvent` with the action `created` or `prompted` gets an empty
`200`.

A failure after that is logged as one line, with the route and a status or an error
name only. Linear sends a webhook again after 1 minute, 1 hour and 6 hours when it
fails, and may turn off a webhook that keeps failing, so the answer says whether a retry
can help:

- `500 internal error`, with nothing of Linear's text in it, when it can: a call to
  Linear timed out or Linear answered 5xx or 429, or D1 failed. The retry finishes the
  same delivery.
- An empty `200` when it cannot: the workspace has not installed the app, Linear
  refuses the app's token even after a refresh or refuses the refresh (the app was
  removed), Linear answers 403 or 404, or the stored token does not open under
  `LINEAR_TOKEN_KEY`. These last until someone installs the app again.

**`created`**: an issue was delegated to Arakun, or Arakun was mentioned on one. The
Worker finds the Linear user who created the session, the account that user is linked
to (not blocked, with the `linear` flag), the issue's team and project (asked from
Linear), and the computer whose routes take it. It saves a `created` delivery, hands it
to the computer's relay, and says in the session where it went. Every refusal below is
an activity in the session and an empty `200` to Linear.

**`prompted`**: someone wrote in the session, or stopped it. The Worker finds the
newest `created` delivery of the session that has not expired. The words of the person
who delegated it, and only theirs, go to that computer as a `prompt` delivery, or as a
`stop` if Linear's signal says so. Anyone else's words get the "Only the person" answer
below.

**A retry**: a delivery is keyed by the event: the agent session's id for a
delegation, the activity's id for a follow-up. Both are in the signed body. The body's
`webhookId` is the id of the webhook itself, the same on every event, so it is not
used. A retry finds the row and finishes the same delivery under the same id: it is not
saved twice, and it is not handed to the relay again if the computer took it or it
expired meanwhile. Once a delivery is announced, a retry is an empty `200`. A second
`created` for an agent session that already has a delivery, such as a re-delegation
that reuses the session, is treated as the same event: it finishes the first delivery
or does nothing. A row found under the key that belongs to another session, workspace
or kind is never finished; that is logged, and the answer is an empty `200`.

The Worker's own activities, character for character. `<computer>` is the machine's
name, or `this computer` if its row is gone (`This computer` in the expiry one).

| Type | When | Text |
| --- | --- | --- |
| `error` | the session has no creator | `Arakun starts work only for a person. Delegate this issue yourself.` |
| `error` | no issue in the session | `Arakun works on issues. Mention it on an issue instead.` |
| `error` | the creator is not linked, or the account is blocked or lacks the flag | `The person who delegated this has not connected Linear in Arakun. In Arakun, open the Linear tab and turn on "Let Linear start sessions on this computer".` |
| `error` | no route takes the issue's team or project, or the app cannot see the issue | `No computer is set up for this team. In Arakun's Linear tab, map the team or its project to a project on your computer.` |
| `thought` | delivered and the computer is connected, or it had taken it already | `Sent to <computer>. A session starts there now.` |
| `thought` | delivered and the computer is offline | `<computer> is offline. The session starts when it is back, for up to 7 days.` |
| `error` | the computer already has 50 deliveries waiting (the delivery is not kept) | `Too many requests are waiting for <computer>. Try again when it has caught up.` |
| `error` | a follow-up for a session that has no live delegation | `Arakun has no session for this. Delegate the issue again.` |
| `thought` | a follow-up from someone other than the delegator | `Only the person who delegated this can steer it.` |
| `thought` | a reply from the delegator was kept for a computer that is offline | `<computer> is offline. Your message reaches the session when it is back.` |
| `response` | a stop from the delegator was kept for a computer that is offline | `<computer> is offline. The session there is stopped as soon as it is back.` |
| `error` | a `created` delivery waited 7 days and the computer never took it | `<computer> stayed offline for 7 days, so this was dropped. Delegate it again when the computer is on.` |

A follow-up that is dropped for a full queue is also answered with the full-queue text.
An expired follow-up, or a `created` delivery the computer had taken, says nothing.

### The relay side

A delivery goes to the relay Durable Object of the computer's label, through
`deliver()` in `src/machines.ts` (`POST /_linear` inside the Worker), and is kept in
its storage until the computer accepts it (`POST /linear/deliveries/<id>/accept`, which
calls `/_linear_drop`) or it expires. If the computer is connected, it gets the frame
at once. The frame is a control frame on channel 0 (the computer's control channel,
JSON in a binary frame, like `claimed`):

```json
{ "type": "linear", "id": "<32 hex>", "kind": "created", "agent_session": "<uuid>",
  "issue": "<uuid>", "organization": "<uuid>", "activity": null, "body": null }
```

| Field | Meaning |
| --- | --- |
| `id` | The delivery's id, 32 hex characters. Unique per delivery. |
| `kind` | `created` (start a session for the issue), `prompt` (a follow-up message) or `stop` (the person stopped the session). |
| `agent_session` | The Linear agent session's id. Pass it back to `/linear/sessions/<agent_session>/activity`. |
| `issue` | The issue's id. |
| `organization` | The Linear workspace's id. |
| `activity` | The id of the follow-up's activity in Linear, or `null` for `created`. |
| `body` | The follow-up's text, or `null`. See below. |

`body` is set only on the frame sent the moment a `prompt` arrives while the computer is
connected, and only when the text is 16,000 characters or fewer (a longer one is not
cut; the computer reads it from Linear by `activity`). The relay never stores it, so a
frame sent again has `body: null`, as has one for `created` and `stop`.

A delivery is sent again after every `claimed` until the computer accepts it, oldest
first, so a computer that was offline, or lost the connection before it took the
delivery, gets it back. A computer should ignore a delivery id it has already handled:
in a narrow race a frame can arrive twice, or just before `claimed`.

The queue holds 50 deliveries for one computer. A 51st is refused, and the person
hears so in the session. A delivery lives 7 days from its creation; after that the relay
drops it, marks the row expired and, for a `created` delivery the computer never took,
posts the expiry activity above. These frames do not count toward a plan's frame,
burst or daily limits.

### Limits

`LINEAR_LIMIT` (`wrangler.toml`, `namespace_id` 1017 at the top level and 1018 in
`[env.production]`) counts, per account, the calls
that ask Linear: `POST /linear/link`, `POST /linear/sessions/<id>/activity` and
`POST /linear/sessions/<id>/links`. It is
about 30 a minute, and a call over it answers `429 too many requests`. If the limit
cannot be checked, the answer is `503`. Every call from the Worker to Linear times out
after 3 seconds.

### Known gaps

- Linear's webhook has no typed field that tells a person's delegation from an
  automation's. Linear's documentation says an automation's `creator` is the issue's
  assignee, so an automation that delegates an issue assigned to a linked person starts
  work on that person's computer. The design wanted automations refused.
  `agentSession.sourceMetadata` is an untyped JSON object and is not used.
- A follow-up from an account that is blocked or has lost the `linear` flag is still
  queued for its own computer. The `created` path refuses such an account.
- If Linear's API is slow twice in one webhook, the answer can take longer than Linear's
  5 seconds. Linear then retries, and the retry finishes the same delivery.
- It is not yet known whether Linear's retry carries a fresh `webhookTimestamp`. If it
  does not, the 60 second check refuses the retry (`401`), and a delegation whose first
  try failed for a passing reason is lost: the person delegates again.
- A workspace whose app sees no Linear traffic for 30 days can lose its refresh token.
  Its webhooks then answer `200` with nothing done, and someone must install the app
  again. `installed` in `GET /linear/link` only says that a workspace row exists: it
  does not notice a lost refresh token, nor an app removed in Linear.
- A computer can still report progress for sessions it was given before its account
  unlinked Linear.

### Deploy

1. Register the app in Linear as an OAuth application, with the redirect URL
   `https://app.arakun.dev/linear/callback` and the webhook URL
   `https://app.arakun.dev/linear/webhook`, with agent session events on. The app asks for
   the scopes `read,write,app:assignable,app:mentionable` with `actor=app`; the Worker
   sends them, so they are not set in Linear.
2. Make the key that seals the tokens, and save it in your password manager:

   ```sh
   openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
   ```

   Losing or changing this key makes every stored token unreadable, so every workspace
   must install again.
3. Set the four secrets. Each command asks for the value:

   ```sh
   npx wrangler secret put LINEAR_CLIENT_ID --env production
   npx wrangler secret put LINEAR_CLIENT_SECRET --env production
   npx wrangler secret put LINEAR_WEBHOOK_SECRET --env production
   npx wrangler secret put LINEAR_TOKEN_KEY --env production
   ```

   Without all four, or with a key that is not 32 bytes of base64url, `/linear`
   answers 404.
4. The rate limit `LINEAR_LIMIT` is a binding in `wrangler.toml` (top level and
   `[env.production]`), so there is nothing to set. Apply the migration, then deploy:

   ```sh
   npx wrangler d1 migrations apply prod-arakun-cloud --env production --remote
   npx wrangler deploy --env production
   ```

A workspace admin installs the app by opening `https://app.arakun.dev/linear/install`
in a browser where they are signed in to Arakun with an account that has the `linear`
flag, and approving it in Linear. An operator gives an account that flag with the admin
flag route (see Granting flags):

```sh
printf 'Authorization: Bearer %s\n' "$ADMIN_SECRET" |
  curl -fsS -X PUT -H @- "https://app.arakun.dev/admin/users/<account_id>/flags/linear"
```

## API

Times are Unix seconds. The request and answer bodies of the team endpoints, and
the checks on them, are in [PROTOCOL.md](PROTOCOL.md).

| Request | Answer |
| --- | --- |
| `POST /auth/github` with `{"access_token": "..."}` | `200 {session, expires_at, account_id, github_id, login, flags}`, `github_id` being the account ID under its old name. `400` without a token, `401` if GitHub rejects it, another app issued it or it was already used to sign in, `413` for a body over 4 KiB, `429` after about 10 tries a minute from one IP (see the rate limit above), `502` if GitHub cannot be reached, fails or does not revoke the token, `503` without `GITHUB_CLIENT_SECRET`. |
| `GET /auth/github/start` | The page's sign-in: `302` to GitHub, with the `state` in the cookie `__Host-arakun_oauth`. `302` to `/?sign_in=failed` without `GITHUB_CLIENT_SECRET`. |
| `GET /auth/github/callback?code&state` | `302` to `/` with the session in the cookie `__Host-arakun_session`. `302` to `/?sign_in=failed` for a `state` that is not the cookie's, a refusal by GitHub, the sign-in rate limit or any other failure. Clears the state cookie either way. |
| `GET /me` with the session | `200 {account_id, github_id, login, flags}`, or `401`. |
| `POST /auth/logout` with the session | `204`, and the session is gone. `401` if it was not valid. |
| `PUT /admin/github/<github_id>` with `ADMIN_SECRET` | The account of a GitHub user, made if it has none: `200 {account_id, github_id, created}`. |
| `PUT /admin/users/<account_id>/flags/<flag>` with `ADMIN_SECRET` | Grants the flag: `200 {account_id, flags}`. `404` without such an account, as for block and plan. |
| `DELETE /admin/users/<account_id>/flags/<flag>` with `ADMIN_SECRET` | Takes it away: `200 {account_id, flags}`. |
| `DELETE /admin/users/<account_id>/sessions` with `ADMIN_SECRET` | Ends all the user's sessions: `200 {account_id, closed}`, with how many ended. |
| `GET /cloud/access` with the session | `200 {status, requested_at, decided_at, retry_at}`. `status` is `none`, `pending`, `granted` or `declined`; a time is `null` where it does not apply, and `retry_at` is set only for `declined`: when the account may ask again. `401` without a session. |
| `POST /cloud/access` with the session, `{"reason"?: "..."}` | Asks for cloud access: `201` with the same shape, `pending`. `400` for a reason over 500 characters, `401` without a session, `403 {error: "blocked"}` for a blocked account, `409 {error, status, requested_at, decided_at, retry_at}` when the account has asked already (`already pending`), has access (`already granted`) or was declined less than 30 days ago (`declined recently`), `429` after 5 a minute. |
| `GET /cloud/machines` with a session of an account with the `cloud` flag | `200 [{label, name, running, snapshot, last_active, task_ms, stopped}]`, `stopped` the limit that keeps it asleep or null. `401` without a session, `403` for a blocked account or one without the flag, `503` where cloud machines are not set up. |
| `POST /cloud/machines`, `{"name"?: "..."}` | Makes a machine and starts it: `201` with its state, as in the list. `400` for a name not of 1 to 100 characters, `409` when the account has a cloud machine already or as many machines as its plan allows. |
| `POST /cloud/machines/<label>/wake` | Starts it if it sleeps, as a new task after the task cap: `200` with its state. `403` when the account may not run one now, and `403 {error, limit}` past a limit; `404` for another account's, `503` without containers. |
| `GET /cloud/usage` | `200 {month, used_ms, allowance_ms, left_ms, task_cap_ms, stopped, message}`: this UTC month's awake time, and `stopped` (`allowance_used`, `held` or `stopped_all`) with the sentence to show, or null. |
| `POST /cloud/machines/<label>/sleep` | Snapshots it and stops it: `204`. |
| `DELETE /cloud/machines/<label>` | Stops it and forgets it, with its key, snapshot, registration and session: `204`. |
| `GET /cloud/machines/<label>/model` | The providers the machine has credentials for, never the credentials: `200 [{provider, set_at}]`. |
| `PUT /cloud/machines/<label>/model/<provider>` | Keeps the owner's credentials for `anthropic`, `bedrock`, `vertex` or `openai`: `200 {model: [{provider, set_at}], applied}`. `anthropic` and `openai` take `{"api_key"}`; `bedrock` takes `{"region", "api_key"}` or `{"region", "access_key_id", "secret_access_key", "session_token"?}`; `vertex` takes `{"region", "project_id", "credentials"}`, the JSON of a Google credentials file as an object or a string. One of `anthropic`, `bedrock` and `vertex` replaces the other two. `applied` is `false` when the machine runs and did not take them; its next start has them. `400` for a value that is missing or has spaces or line breaks, `404` for an unknown provider, `503` without `CLOUD_MODEL_KEY`. |
| `DELETE /cloud/machines/<label>/model/<provider>` | Forgets them, on a running machine too: `200 {model, applied}`. |
| `GET /admin/cloud/machines` with `ADMIN_SECRET` | `200 [{label, account_id, login, name, created_at, month_ms, running, snapshot, stopped}]`, newest first; `month_ms` is the owner's awake time this month. |
| `POST /admin/cloud/machines/<label>/sleep` with `ADMIN_SECRET` | Snapshots it and stops it now: `204`, and a `cloud.machine.sleep` entry in the log. `404` for no such machine. |
| `PUT /admin/cloud/stop`, `{"reason"?: "..."}`, with `ADMIN_SECRET` | Stops every cloud machine now and keeps them down: `200 {machines}`, how many stopped at once, and `cloud.stop_all` in the log. `DELETE` lifts it: `204`, `cloud.resume_all`. |
| `PUT /admin/users/<account_id>/cloud/stop`, `{"reason"?: "..."}`, with `ADMIN_SECRET` | The same for one account: `200 {machines}`, `cloud.hold`. `DELETE` lifts it: `204`, `cloud.release`; `404` when it was not stopped. |
| `GET /admin/cloud/requests[?status=]` with `ADMIN_SECRET` | `200 [{account_id, github_id, login, status, reason, requested_at, decided_at, decided_by, retry_at, has_flag, blocked, github: {login, created_at, age_days, public_repos, followers, following}}]`, pending first. `github` is what GitHub said when the account asked; its fields are `null` when GitHub did not answer. |
| `PUT /admin/cloud/requests/<account_id>/grant` with `ADMIN_SECRET`, `{"by"?: "..."}` | Grants the request and the `cloud` flag: `200` with the request. `404` without a request. |
| `PUT /admin/cloud/requests/<account_id>/decline` with `ADMIN_SECRET`, `{"by"?: "..."}` | Declines a pending request: `200` with the request. `404` without a request, `409` if it is not pending. |
| `GET /beta` with the session | `200 {email, ios, android}`: `email` is the account's verified GitHub email or `null`, and each platform is `{status, email, requested_at, decided_at, retry_at}`. `status` is `none`, `pending`, `admitted` or `declined`; `retry_at` is set only for `declined`. `401` without a session. |
| `POST /beta/<ios\|android>` with the session, `{"email"?: "..."}` | Signs up: `201` with the platform's shape, `pending`. While pending, changes the email: `200`. `400` for an address that does not look like one or is over 254 characters, or no email when GitHub gave none. `401`, `403 {error: "blocked"}`, `404` for another platform, `409 {error, ...}` when `already admitted` or `declined recently`, `429` after 5 a minute. |
| `GET /admin/beta/signups[?platform=][&status=]` with `ADMIN_SECRET` | `200 [{account_id, github_id, login, platform, email, status, requested_at, decided_at, decided_by, retry_at, blocked, github: {...}}]`, pending first. |
| `PUT /admin/beta/signups/<account_id>/<platform>/admit\|decline` with `ADMIN_SECRET`, `{"by"?: "..."}` | Marks it admitted or declined, from any state: `200` with the sign-up. The same decision again keeps its first time and name. `404` without a sign-up. |
| `POST /push/v1/{apns,apns-sandbox,fcm}/<token>` with a Web Push request (`Content-Encoding: aes128gcm`, `Authorization: vapid t=..., k=...`, `TTL`, `Urgency`) | `201` once Apple or Google took it. `401` for a missing or bad VAPID token (wrong signature, audience or expiry), `404` for a token that is not one, `410` when Apple or Google say the token is gone, `413` for a body over 2800 bytes, `415` without `aes128gcm`, `429` after 30 a minute for one token or when the service is busy, `502` when it refuses or cannot be reached, `503` without its keys. |
| `GET /healthz` | `200 {"ok": true}` |

The admin endpoints answer `401` without the exact secret and `400` for a bad ID or
flag. A flag name is a lower-case letter, then up to 31 of `a-z`, `0-9` and `_`.
Anything else answers `404`. There is no CORS: only the desktop app calls this.

### Teams

Every request below needs the session and the `teams` flag: `401` without a session,
`403` without the flag. Team requests answer `404` to anyone who is not a member
(pending members included, except on `remind`), and "admin" ones `403` to members who are not an owner or
admin. "Device" means the request carries `X-Arakun-Device` with one of the caller's
devices: `400` without it, `403` if it is someone else's. `github_id`, in paths and
answers, is the account ID (see PROTOCOL.md), except in the answer to an invite.

| Request | Who | Answer |
| --- | --- | --- |
| `POST /devices` `{sig_pub, enc_pub, name, endorsement}` | anyone | `201 {device_id}`, `200` if registered already. `400` for a bad key, name or endorsement; `409` if another account has the key, or past 20 devices. |
| `GET /users/<github_id>/devices` | the user, or someone in a team with them | `200 [{device_id, sig_pub, enc_pub, name, created_at, endorsed_by, endorsement_sig}]` |
| `POST /teams` `{name}` | anyone | `201 {team_id, epoch: 1}`; the caller is the owner. `409` if the caller is in 50 teams already. |
| `GET /teams` | anyone | `200 [{team_id, name, role, epoch, rotation_pending}]` |
| `GET /teams/<team_id>` | member | `200 {team_id, name, epoch, keyed_epoch, rotation_pending, members, channels}` |
| `POST /teams/<team_id>/invites` `{github_login}` | admin | `201 {invite_id, expires_at, github_id, login, invitee_state}`, 7 days, for the GitHub user who has the login now. `404` if none has it, `502` if GitHub could not be asked, `409` if a member or pending already. |
| `GET /teams/<team_id>/invites` | admin | `200 [{invite_id, login, invited_by_login, created_at, expires_at, invitee_state}]`, open ones, newest first. `invitee_state` is `no_account` (never signed in), `not_enabled` (no Teams) or `ready`; only `ready` sees the invite. |
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
| `GET /teams/<team_id>/envelopes/stale` | admin | `200 [{device_id, epoch}]`: members' envelopes whose sender is no longer an owner's or admin's device, newest epoch first, at most 500. |
| `PUT /teams/<team_id>/members/<github_id>` `{role}` | owner | `200 {github_id, role}`; `role` is `owner`, `admin` or `member`, the caller's own included. `404` if not a member, `409` if the team would have no owner. `403` if the caller lost the owner role, the session or the flag meanwhile. |
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
