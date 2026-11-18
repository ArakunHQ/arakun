# Teams

The pilot of Arakun teams: a Cloudflare Worker with a D1 database. For now it only
signs users in with GitHub and tells the app which features each user has. Teams,
members and chat come later, on the same database.

```
teams/
  src/index.ts     routes; every error is JSON: { "error": "..." }
  src/auth.ts      POST /auth/github, GET /me, POST /auth/logout, sessions
  src/admin.ts     PUT|DELETE /admin/users/<github_id>/flags/<flag>,
                   DELETE /admin/users/<github_id>/sessions
  migrations/      the D1 tables, applied in order by wrangler
  test/            Vitest inside workerd, with real D1 and a fake GitHub
```

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
Flags are set per user on the server, and every team endpoint must check its flag
there. Anyone can sign in; without a flag, `/me` answers `flags: []`.

## Development

Node 24 or newer.

```sh
cd teams
npm ci
npm run check     # tsc --noEmit
npm test          # vitest run
```

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

To host the test Worker too, give it its own database and put that `database_id` in
the top-level `[[d1_databases]]`. Never put production's id there. Then follow the
same steps with `arakun-teams-dev` and without `--env production`, and give it a
different `ADMIN_SECRET`:

```sh
npx wrangler d1 create arakun-teams-dev
npx wrangler d1 migrations apply arakun-teams-dev --remote
```

The rate limit on `POST /auth/github` (about 10 a minute per client IP) is set in
`wrangler.toml` under `[[ratelimits]]`. Rate limits with the same `namespace_id` in
one account share their counters, so production and the test Worker have different
ids, and each id must be unique among the account's rate limits. Cloudflare counts it in each location on its own, and its
counters catch up with a delay, so it is approximate: a client can get a few more.

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
in step 3 (paste it; the variable is not set anywhere else):

```sh
read -rs ADMIN_SECRET
for id in <id> <id> <id>; do
  curl -fsS -X PUT -H "Authorization: Bearer $ADMIN_SECRET" \
    "https://<teams host>/admin/users/$id/flags/teams"
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
curl -fsS -X DELETE -H "Authorization: Bearer $ADMIN_SECRET" \
  "https://<teams host>/admin/users/<id>/sessions"
```

To see who has which flag:

```sh
npx wrangler d1 execute arakun-teams --env production --remote --command "
  SELECT u.github_id, u.login, f.flag FROM user_flags f JOIN users u USING (github_id)"
```

## API

Times are Unix seconds.

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
