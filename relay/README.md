# Phone relay

The Cloudflare Worker that lets a phone reach a computer running Arakun from outside
the local network. It forwards encrypted frames without being able to read them: each
computer has a label derived from its public key and a Durable Object with that name,
and phones come in through `https://<label>.arakun.dev` with a ticket the computer
issued.

```
relay/
  src/index.ts       routing by Host: label → object; assets; /_relay; /_admin
  src/relay.ts       the Durable Object: computer claim, admission, channels, limits, alarm
  src/limits.ts      plans, kill switch and block list, read from KV
  src/admin.ts       GET /_admin/labels, POST /_admin/block|unblock/<label>
  src/frames.ts      binary frames and control messages
  src/label.ts       label = base32 of the first 80 bits of SHA-256(public key)
  test/              Vitest inside workerd, with real DOs and KV
  scripts/peers.mjs  one fake computer and two fake phones against a deployed relay
```

The phone page (`../phone/`) is served as assets by the same Worker, so the phone
always loads the app from the same commit as the relay.

## Development

Node 24 or newer.

```sh
cd relay
npm install
npm run check     # tsc --noEmit
npm test          # vitest run
```

`wrangler dev` has no subdomains: `LABEL_OVERRIDE` in `.dev.vars` sets the label it
serves. `scripts/peers.mjs` with no arguments creates a key in `.peers-key.json`
(git-ignored) and prints its label and the exact commands:

```sh
node scripts/peers.mjs
echo LABEL_OVERRIDE=<label> > .dev.vars
npx wrangler dev
node scripts/peers.mjs --base http://127.0.0.1:8787
```

The script claims the label, issues a ticket, opens pairing, brings in two phones (one
by code, one by ticket), sends bytes both ways, spends the code, revokes the ticket and
checks the computer hears every close. It exits with 1 at the first failing step.

## Self-hosting on a free account (workers.dev)

Each computer deploys its own Worker named after its label on the account's
`workers.dev` subdomain: `https://<label>.<account>.workers.dev`. It is the same code
as production; the Worker reads the label from the first level of the Host either way.
Then set `relay_base` in Arakun's `phone.json` to your Worker's base URL.

1. A Cloudflare account (the free plan caps spending) and a session:

   ```sh
   npx wrangler login
   ```

   The account needs a registered `workers.dev` subdomain (dashboard: Workers & Pages
   → Overview). A certificate for a freshly registered subdomain takes a minute or two
   to be served; TLS fails until then.

2. The KV namespace, once per account. Put the `id` it returns in `wrangler.toml`
   (`[[kv_namespaces]]`):

   ```sh
   npx wrangler kv namespace create CONFIG
   ```

3. Initial limits and the admin secret (without it, `/_admin` does not exist):

   ```sh
   npx wrangler kv key put --binding CONFIG plans '{"free":{"phones":3,"burst":50,"daily":20000,"frame":262144}}'
   npx wrangler secret put ADMIN_SECRET --name <label> --env=""
   ```

   Any field missing from `plans.free` takes that same default.

4. Deploy under the computer's label (shown in Arakun's Phone tab, or by
   `node scripts/peers.mjs` to test without Arakun):

   ```sh
   npx wrangler deploy --name <label> --env=""
   node scripts/peers.mjs --base https://<label>.<account>.workers.dev
   ```

   `--env=""` picks the top level of `wrangler.toml`. Without it, wrangler warns that
   several environments are defined (`[env.production]` is `arakun.dev`'s own relay)
   and none was picked, then deploys the top level anyway.

## Operating

Everything is tuned in KV without redeploying:

| Key | Value | Effect |
|---|---|---|
| `plans` | `{"free":{"phones","burst","daily","frame"}}` | Phones at once, frames per second, frames per UTC day, bytes per frame. Each object rereads it every minute |
| `disabled` | `true` or a message | 503 on `/pc`, `/ws` and `/_relay` with the message; the page still loads to show it |
| `blocked` | `["<label>", …]` | 403 for those labels |

```sh
npx wrangler kv key put --binding CONFIG disabled "maintenance until 05:00 UTC"
npx wrangler kv key delete --binding CONFIG disabled
```

With the admin secret:

```sh
curl -H "Authorization: Bearer $ADMIN_SECRET" https://<host>/_admin/labels
curl -X POST -H "Authorization: Bearer $ADMIN_SECRET" https://<host>/_admin/block/<label>
curl -X POST -H "Authorization: Bearer $ADMIN_SECRET" https://<host>/_admin/unblock/<label>
```

`labels` lists every computer that claimed (first claim, last seen, phones and frames
of the day). `block` writes to KV and also drops everyone connected under that label.

An object whose computer has not connected in 90 days empties itself (alarm). Nothing
is lost: the computer sends its key and tickets again when it reconnects.

## Hosting one relay for everyone on your own domain

A single Worker serves every computer:

1. DNS: a proxied `*` record (CNAME, or AAAA `100::`).
2. Worker route `*.<domain>/*`, excluding `www.<domain>/*` and `<domain>/*` so the
   landing page stays where it is.
3. `npx wrangler deploy --env=""` under the normal name (`arakun-relay`). For `arakun.dev` itself
   this is `[env.production]` in `wrangler.toml`, which `.github/workflows/relay-deploy.yml`
   deploys when a change to `relay/` or `phone/` lands on `main` (or by hand from the
   Actions tab). The deploy's message names the commit.
4. A zone rate-limiting rule (one rule is free) on `/ws` and `/pc` per IP, so nobody can
   open labels or connections without limit.

Avoid a second-level wildcard such as `*.phone.<domain>`: its certificate is not free.
