# Teams

The pilot of Arakun teams: a Cloudflare Worker with a D1 database and one Durable
Object per channel. It signs users in with GitHub, tells the app which features
each user has, and runs the end-to-end encrypted teams of [PROTOCOL.md](PROTOCOL.md):
devices, teams, invites, team keys in envelopes, and channels with live messages.
The server stores and forwards; it never holds a key that opens team content.

```
teams/
  src/index.ts     routes; every error is JSON: { "error": "..." }
  src/auth.ts      POST /auth/github, GET /me, POST /auth/logout, sessions
  src/admin.ts     PUT|DELETE /admin/users/<github_id>/flags/<flag>,
                   DELETE /admin/users/<github_id>/sessions
  src/teams.ts     devices, teams, invites, envelopes, members, channels
  src/channel.ts   the Channel Durable Object: messages, seq, live sockets
  src/wire.ts      the shapes of PROTOCOL.md and their signature checks
  migrations/      the D1 tables, applied in order by wrangler
  test/            Vitest inside workerd, with real D1, the real Durable
                   Object, real P-256 keys and a fake GitHub
```

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
   `Ov23lipgmMeATCa5aTr3` and no scopes. The app holds no client secret.
2. It posts the GitHub token once to `POST /auth/github`.
3. The Worker asks GitHub's Applications API
   (`POST /applications/<client_id>/token`) whether our OAuth App issued the token and
   to whom. That call needs the app's client secret, which only the Worker has. It
   keeps the numeric `id` and the `login`, and answers with a session token of its
   own. It never stores the GitHub token, and stores nothing else about the user.

   `GET /user` would accept any of the user's GitHub tokens (the `gh` CLI's, a
   personal token, another app's), so whoever held one could sign in as them.
4. Before it answers, the Worker revokes that GitHub token
   (`DELETE /applications/<client_id>/token`), since OAuth App tokens never expire
   and it was only needed to learn who the user is. So one GitHub token gives one
   session, and signing in again means running the device flow again. If GitHub
   does not confirm the revoke, the sign-in fails and gives no session.
5. The app sends `Authorization: Bearer <session>` from then on.

A session lasts 90 days and is extended on use, at most once a day, but never past
180 days after sign-in. Then the app has to sign in again. The database holds only
the SHA-256 of the session token.

Users are keyed by their numeric GitHub ID, which stays when they rename their account.
Flags are set per user on the server, and every team endpoint checks the `teams`
flag there. Anyone can sign in. If the `flags` that `/me` answers do not include
`teams` (there may be other flags), every team endpoint answers `403`.

## Development

Node 24 or newer.

```sh
cd teams
npm ci
npm run check     # tsc --noEmit
npm test          # vitest run
```

`npx wrangler deploy --dry-run` (and `--env production --dry-run`) checks the
configuration and the bundle without deploying.

For `wrangler dev`, create the tables in the local database first:

```sh
npx wrangler d1 migrations apply arakun-teams-dev --local
npx wrangler dev
```

`wrangler dev` reads the secrets from `.dev.vars`. If you do not have that file yet,
create it with the two lines below; if you have it, keep your values:

```sh
ADMIN_SECRET=dev-secret
GITHUB_CLIENT_SECRET=<the app secret>
```

## Deploying

Nothing deploys this automatically yet. The commands below use `--env production`,
which deploys the Worker `arakun-teams`. Without it, wrangler deploys the top-level
Worker `arakun-teams-dev`, a separate Worker with its own database, rate-limit
namespace and secrets. A test deploy cannot replace production's bindings, and its
admin secret cannot grant flags or make sessions in production.

1. Create the database once, and put the `database_id` it prints in `wrangler.toml`
   under `[[env.production.d1_databases]]`:

   ```sh
   npx wrangler d1 create arakun-teams
   ```

2. Apply the migrations, now and after every change in `migrations/`:

   ```sh
   npx wrangler d1 migrations apply arakun-teams --env production --remote
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

   The first deploy creates the `Channel` Durable Object class from the `v1`
   migration in `wrangler.toml` (`new_sqlite_classes`). A later change to the class
   that renames or deletes it needs a new `[[migrations]]` tag, in both the
   top-level and the `[env.production]` lists.

To host the test Worker too, give it its own database and put that `database_id` in
the top-level `[[d1_databases]]`. Never put production's id there. Then follow the
same steps with `arakun-teams-dev` and without `--env production`, and give it a
different `ADMIN_SECRET`:

```sh
npx wrangler d1 create arakun-teams-dev
npx wrangler d1 migrations apply arakun-teams-dev --remote
```

Two rate limits are set in `wrangler.toml` under `[[ratelimits]]`: `POST /auth/github`
(about 10 a minute per client IP, `namespace_id` 1001 in production, 1003 in the
top-level Worker) and the messages each device sends on channel sockets (about 60
every 10 seconds, `namespace_id` 1002 in production, 1004 in the top-level Worker).
Rate limits with the same `namespace_id` in one account share their counters for
the same key, so a device sending through the test Worker would use up its
production allowance; give a new rate limit an id nothing else in the account uses.
Cloudflare counts each location on its own, and its counters catch up with a
delay, so both are approximate: a client can get a few more.

The compatibility date is after 2026-04-07, so the runtime answers a WebSocket
client's close by itself; `src/channel.ts` relies on that. `relay/` has its own date.

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
    curl -fsS -X PUT -H @- "https://<teams host>/admin/users/$id/flags/teams"
done
```

Or straight in the database:

```sh
npx wrangler d1 execute arakun-teams --env production --remote --command "
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
  curl -fsS -X DELETE -H @- "https://<teams host>/admin/users/<id>/sessions"
```

To see who has which flag:

```sh
npx wrangler d1 execute arakun-teams --env production --remote --command "
  SELECT u.github_id, u.login, f.flag FROM user_flags f JOIN users u USING (github_id)"
```

## API

Times are Unix seconds. The request and answer bodies of the team endpoints, and
the checks on them, are in [PROTOCOL.md](PROTOCOL.md).

| Request | Answer |
| --- | --- |
| `POST /auth/github` with `{"access_token": "..."}` | `200 {session, expires_at, github_id, login, flags}`. `400` without a token, `401` if GitHub rejects it, another app issued it or it was already used to sign in, `413` for a body over 4 KiB, `429` after about 10 tries a minute from one IP (see the rate limit above), `502` if GitHub cannot be reached, fails or does not revoke the token, `503` without `GITHUB_CLIENT_SECRET`. |
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
| `POST /teams` `{name}` | anyone | `201 {team_id, epoch: 1}`; the caller is the owner. |
| `GET /teams` | anyone | `200 [{team_id, name, role, epoch, rotation_pending}]` |
| `GET /teams/<team_id>` | member | `200 {team_id, name, epoch, keyed_epoch, rotation_pending, members, channels}` |
| `POST /teams/<team_id>/invites` `{github_login}` | admin | `201 {invite_id, expires_at}`, 7 days. `409` if a member or pending already. |
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

A channel keeps at most 10,000 messages. Once its sequence exceeds 10,000,
each successful send keeps the newest 9,000. If the first history row has a
`seq` greater than `after + 1`, the missing prefix is no longer available on
the server. Mark the gap, keep any local copies, and resume from the first
returned row instead of requesting the missing prefix again.
