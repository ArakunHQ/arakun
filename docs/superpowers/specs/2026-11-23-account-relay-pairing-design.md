# Account sign-in for the relay

## Why

Today a phone or the app reaches a computer through the relay with a pairing link:
`arakun-server pair` or the Phone tab makes a link that works once for two minutes,
and you carry it to the other device. Nothing ties a computer to a person. Its relay
address is a label derived from a key that anyone can make.

We want four things from this change:

- **A better flow.** Sign in on a device and your machines are there. No link to copy.
- **A way to talk to users.** We know who uses the relay and can tell them things.
- **Room for paid plans later.** A plan belongs to an account, not to a label.
- **Limits and abuse protection we can tune.** Limits and blocks apply to an account.
  A blocked person cannot come back by making a new key.

## Goals

1. Through the relay, a device reaches a computer because both are signed in to the
   same Arakun account. Pairing links and codes through the relay are gone.
2. The sign-in is the one the teams Worker already has (GitHub). One session works
   for teams and for the relay.
3. The relay and the teams Worker become one Worker with one database.
4. Plans, limits and blocks are per account, and change without a deploy.
5. The operator can show a notice to one account or to everyone, in the app and on
   the phone.
6. Pairing on the local network and over Tailscale needs no account and works as it
   does today, with a code or QR code.

## Non-goals

- Payments. The plan is a column an operator sets. Nothing charges for it.
- Accounts other than GitHub.
- Sharing a machine with another account.
- A new device needing approval from a trusted device (see "What signing in means").
- Changes to the teams protocol (`PROTOCOL.md`) or to code behind the `team` and
  `coderlabs` features.

## Anti-goals

- A person who never wants an account must still be able to use the phone on their
  own network or tailnet. Do not make sign-in a condition for the Phone tab itself.
- Do not let the relay read traffic. End-to-end encryption stays.
- Do not leave two ways to pair through the hosted relay. If the code path stays
  open there, limits per account mean nothing.

## Approaches

**One Worker or two.**

- *Two Workers.* The relay stays its own Worker and asks the teams Worker about a
  session over a service binding. Self-hosting the relay stays small, and each keeps
  its own deploy rhythm.
- *One combined Worker.* **Picked.** One deploy and one database to operate. The
  cost: someone who hosts their own relay now also needs a D1 database and a GitHub
  OAuth App, and teams changes ship with releases like the relay does.

**What signing in is allowed to do.**

- *Sign-in finds the machine, a trusted device approves the new one once.* Keeps the
  relay unable to impersonate a computer. One more step per device.
- *Sign-in is enough.* **Picked.** The smoothest flow. The cost is stated below so
  nobody is surprised by it.

## What signing in means

Any device with a valid session for an account is let in to every machine of that
account. So:

- Whoever controls the GitHub account, or steals a session token, can run commands on
  those machines. Session tokens are as sensitive as SSH keys.
- The Worker tells a device which key a machine has. A dishonest or broken-into
  Worker could name its own key and sit in the middle. End-to-end encryption still
  stops the relay from reading traffic as it passes, and still proves the far end
  holds the key the Worker named.

What softens it, all in scope:

- Every session has a name and a kind. The account's sessions are listed in the app
  and on the phone, and any one can be signed out from there. Signing one out closes
  its sockets.
- A machine shows a notice the first time a device it has not seen connects
  ("Rik's iPhone connected for the first time").
- A machine only accepts devices of its own account. It learns its account from
  `/me`, and compares it with the account the relay names for each channel.

## Design

### The Worker

One package, `cloud/`, replaces `relay/` and `teams/`. [My call: the name. Moving
both under one neutral name is clearer than folding one into the other.] It keeps
the teams code and its D1 migrations as they are, and takes the relay's Durable
Object (`Relay`), frames, label and limits code. The phone page (`phone/`) is still
served as its assets.

One host serves everything: the page, the API and the sockets. `https://app.arakun.dev`
in production. [My call: the host name.] Labels leave the host name and travel in the
path. `teams.arakun.dev` is not deployed yet (its `wrangler.toml` has no route), so
the app's default teams URL changes to the same host.

Routes added to what teams has:

| Route | What it does |
|---|---|
| `GET /auth/config` | The GitHub client ID, so the app can sign in to a relay someone hosts themselves. |
| `GET /auth/github/start`, `GET /auth/github/callback` | The browser sign-in for the phone page: GitHub's web flow with `state`, ending in a session. |
| `POST /auth/github` | As today, plus `name` and `kind` (`app`, `server`, `phone`, `ios`, `android`) for the session. |
| `GET /sessions`, `DELETE /sessions/<id>` | The account's sessions; sign one out. |
| `GET /machines` | The account's machines: label, public key, name, kind, whether online, last seen. |
| `DELETE /machines/<label>` | Forget a machine. |
| `GET /machines/<label>/pc` | The computer's socket. Needs the session. Registers the machine on first claim. |
| `GET /machines/<label>/ws` | A device's socket. Needs a session of the machine's account. |
| `GET /me` | As today, plus `plan` and `notices`. |

The old hosts `<label>.arakun.dev` keep a wildcard route for one purpose: old apps
and old phone pages get a clear answer. `/pc` and `/ws` close with the reason
`update`, and the page says "Update Arakun and sign in". [My call: a clean break with
a clear message, not a period where both ways work. See "Rollout".]

### Data

New migration in D1:

- `sessions` gains `id` (public, random), `name`, `kind`.
- `users` gains `plan` (default `free`), `blocked_at`, `blocked_reason`, `email`
  (see "Talking to users").
- `machines`: `label` (primary key), `github_id`, `pub`, `name`, `kind` (`app` or
  `server`), `created_at`, `last_seen_at`.
- `notices`: `id`, `github_id` (null for everyone), `text`, `url`, `created_at`,
  `expires_at`. `notice_reads`: `github_id`, `notice_id`.

### The computer's side

A computer (the app or `arakun-server`) with reach set to Relay:

1. Needs a session. The app uses the one from the existing "Sign in with GitHub"
   dialog. `arakun-server login` runs the same device flow in the terminal: it prints
   the code and `https://github.com/login/device`, waits, and stores the session
   where the server keeps its other secrets. `arakun-server logout` ends it.
2. Opens `/machines/<label>/pc` with `Authorization: Bearer <session>`. The Worker
   checks the session, that the account is not blocked, and that the label is either
   new or already this account's. Then the object challenges the key as it does
   today. On the first claim the Worker writes the `machines` row. A label claimed by
   one account can never be claimed by another.
3. Hears `open` for each device with `{channel, device, name, kind, account}`.
   `device` is the session's public `id`. It refuses a channel whose account is not
   its own.

The `tickets`, `pair_open` and `pair_close` control messages are removed. Inside the
encrypted channel the `{pair}` and `{auth}` messages are removed for relay channels:
a channel the relay opened is already known. The computer still keeps a `Device`
record per device id, made on first connect, for its name, last seen, push
subscription and notice choices. `token_hash` and `ticket_hash` stay only for devices
paired on the network.

Without a session, the Relay reach shows "Sign in to reach this computer from
anywhere" with the sign-in button, and `arakun-server serve` (whose default reach is
the relay) stops with: sign in with `arakun-server login`, or use `--reach lan` or
`--reach tailscale`.

### The device's side

- **Phone page.** Opening `https://app.arakun.dev` signed out shows "Sign in with
  GitHub". Signed in, it lists the account's machines with their state; picking one
  connects. With one machine it connects straight away. The session is an
  `HttpOnly; Secure; SameSite=Lax` cookie, and the Worker checks `Origin` on every
  request that changes something and on socket upgrades. [My call: a cookie, so page
  script cannot read the session.]
- **iOS and Android.** The pairing screen becomes "Sign in with GitHub" plus "Pair
  on this network" (the code and QR flow that exists). Sign-in is the device flow,
  as on the desktop: the app shows the code and opens GitHub. [My call: the device
  flow keeps one sign-in path on the server for all native clients.] The session
  goes in the Keychain and in Android's encrypted storage. A machine list replaces
  the single paired computer.
- **The app as a client of servers.** The Servers tab lists the account's machines
  of kind `server` and connects to them with no link. "Paste a link" stays for
  servers reached directly (network or Tailscale).

On connect, the device takes the machine's `pub` from `GET /machines` and checks the
handshake against it, as it checks against the label today.

### Local network and Tailscale

Unchanged. The computer shows a code and QR code, the phone pairs with it, and gets a
token. `arakun-server pair` works for `--reach lan` and `--reach tailscale`; under
the relay it says to sign in on the device.

### Plans, limits and blocks

- The plan is `users.plan`. What a plan allows stays in KV under `plans`, read as
  today, so numbers change without a deploy. Each plan gains `machines` (how many an
  account may register; free: 5).
- The per-machine limits (devices at once, frames a second, frames a day, bytes a
  frame) stay in the machine's object, which is told the account's plan name when a
  socket opens.
- Blocks move from labels to accounts: `PUT|DELETE /admin/users/<github_id>/block`
  sets `blocked_at` and tells each of the account's machine objects to drop
  everyone. `PUT /admin/users/<github_id>/plan` sets the plan. The KV `blocked` list
  of labels and `/_admin/block/<label>` are removed. `disabled` stays.
- `GET /admin/machines` replaces `/_admin/labels` and reads D1, so the KV label
  cards are removed.
- Sign-out and block must end live sockets: the object checks its sockets' sessions
  on its alarm (every few minutes) and when the Worker tells it to.

### Talking to users

- **Notices.** `POST /admin/notices` writes one for an account or for everyone.
  `/me` answers the unread ones. The app shows them as notices, the phone page and
  the native apps as a banner. `POST /notices/<id>/read` dismisses one.
- **Email.** Sign-in asks GitHub for the `user:email` scope and the Worker keeps the
  primary verified address. Nothing sends mail in this change; the address is there
  for when we need to reach someone (a security problem, a plan change). The README's
  privacy section and the teams README say so. [My call, yours to confirm: this is
  the one place we store more about a person than today.]

### Hosting your own

One deploy of `cloud/` on any host, `workers.dev` included: a D1 database, the KV
namespace, and a GitHub OAuth App of your own (its client secret as a Worker secret).
`relay_base` in `phone.json` points the app at it, and the app reads the client ID
from `/auth/config`. The trick of one Worker per computer named after its label goes
away, since labels are no longer in the host name.

## Rollout

- This is a breaking change (`feat!`). A phone paired through the relay must sign in
  after the update. Phones paired on the network are untouched.
- The Worker deploys with the release, as the relay does now. From that moment old
  apps cannot use the relay and see "Update Arakun and sign in".
- Steps for a person, not for the build: create the production D1 database, set
  `GITHUB_CLIENT_SECRET` and `ADMIN_SECRET`, add the callback URL to the GitHub OAuth
  App, point `app.arakun.dev` at the Worker, keep the `*.arakun.dev` route.
- The work splits into pull requests in this order. The first two can be reviewed
  alone; the rest need them.
  1. Combine the Workers into `cloud/` with no change in behavior (CI, deploy
     workflow, `AGENTS.md`, `CONTRIBUTING.md`, `cubic.yaml`).
  2. The Worker: sessions with names, machines, admission by session, plans and
     blocks per account, notices, the browser sign-in.
  3. `core` and the app: the computer's side, the Phone tab, the Servers tab.
  4. `arakun-server login` and `logout`.
  5. The phone page.
  6. iOS. 7. Android.

## Left to the builder

- The wire shapes of the new routes and control messages, kept in step across
  `core/src/phone`, `phone/`, `ios/ArakunKit` and `android/kit`.
- How often the object rechecks sessions, and the exact limits on the new routes.
- Where `arakun-server` stores its session on each platform, following how the app
  stores the teams session.
- The interface text, within the glossary in `CONTRIBUTING.md`.
- Tests: the Worker's in Vitest with real D1 and objects as today; the Rust side in
  each file's `mod tests`.
