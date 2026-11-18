# Arakun teams: protocol

The contract between the `teams/` Worker and the clients (the desktop app now, the
phone later). The server stores and forwards; it never holds a key that opens team
content. Everything below "the server sees" is metadata we accept showing.

Version: `v1`. Every encrypted object carries `"v": 1`.

## Words

- **User**: a GitHub account, keyed by its numeric ID (`github_id`). Signs in with
  `POST /auth/github` (see README.md) and holds a session.
- **Device**: one install of the app for one user. Has two P-256 key pairs made on
  the device and never sent anywhere:
  - `sig`: ECDSA P-256 with SHA-256, signs what the device publishes;
  - `enc`: ECDH P-256, receives team keys.
  `device_id` = base64url (no padding) of the first 16 bytes of
  SHA-256(`sig` public key, uncompressed SEC1, 65 bytes).
- **Team**: has members with a role (`owner`, `admin`, `member`), channels, and a
  current **epoch**. Each epoch has a random 32-byte **team key**.
- **Channel**: a stream of messages inside a team.

All binary values in JSON are base64url without padding. Public keys are
uncompressed SEC1 (65 bytes). Times are Unix seconds. IDs the client makes are
ULIDs (26 characters, Crockford base32), so they sort by time.

## Server-side checks

Every endpoint below needs a valid session and the user's `teams` flag; otherwise
`401` (no session) or `403 {"error":"teams is not enabled for this account"}`.
Team endpoints also need membership (`404` if not a member, so a team's existence
does not leak), and admin endpoints need `owner` or `admin` (`403`).

## Devices

`POST /devices` registers the calling device for the signed-in user:

```json
{ "sig_pub": "...", "enc_pub": "...", "name": "karlo-laptop",
  "endorsement": null | { "by": "<device_id>", "sig": "..." } }
```

- The server computes `device_id` from `sig_pub` and stores the row
  (`github_id`, `device_id`, `sig_pub`, `enc_pub`, `name`, `created_at`, `endorsed_by`).
- A user's first device has `endorsement: null`. Any later device must be endorsed
  by one of the same user's existing devices: `sig` = ECDSA by that device over the
  UTF-8 bytes of `arakun-teams/v1/device|<github_id>|<sig_pub>|<enc_pub>`. The
  server checks it (`400` if wrong), and so does every client before trusting the
  device. This is what stops the server from adding a device of its own.
- Answer: `201 { "device_id": "..." }`. Registering the same `sig_pub` again
  answers `200` with the same ID.

`GET /users/<github_id>/devices` lists a user's devices (the user must share a team
with the caller, or be the caller): `[{ device_id, sig_pub, enc_pub, name,
created_at, endorsed_by }]`. Clients verify each endorsement chain back to the
first device and show "new device" notices for devices they have not seen.

Every request a device makes about its own keys carries `X-Arakun-Device: <device_id>`.

## Teams

- `POST /teams` `{ "name": "Arakun" }` → `201 { team_id, epoch: 1 }`. The caller
  becomes `owner`. The name is plaintext (it shows in invites before anyone holds
  a key). The creating device then uploads the epoch-1 envelope for itself (below).
- `GET /teams` → the caller's teams: `[{ team_id, name, role, epoch }]`.
- `GET /teams/<team_id>` → `{ team_id, name, epoch, members: [{ github_id, login,
  role, joined_at }], channels: [{ channel_id, name_ct, created_at }] }`.

## Invites and approval

1. An admin invites by GitHub login: `POST /teams/<team_id>/invites
   { "github_login": "rikvisser-dev" }` → `201 { invite_id }`. The server resolves
   nothing with GitHub; the login is matched on the invitee's next sign-in or
   `/me`. Invites expire after 7 days.
2. The invitee sees it in `GET /invites` → `[{ invite_id, team_id, team_name,
   invited_by_login, created_at }]` and accepts with
   `POST /invites/<invite_id>/accept` (from a registered device) → the user becomes
   a **pending** member.
3. Admins see pending members in `GET /teams/<team_id>/pending` → `[{ github_id,
   login, devices: [...] }]`.
4. An admin's device approves: `POST /teams/<team_id>/pending/<github_id>/approve`
   with the envelopes for every device of the new member and every epoch they may
   read (all of them, by default):
   `{ "envelopes": [Envelope, ...] }`. The server checks there is one envelope per
   (device, epoch) it expects, stores them, and makes the user a `member`.
   `.../reject` drops the request.

The approval happens on the admin's device because only it holds the team key;
the server cannot approve anyone.

## Envelopes (team keys, per device)

An envelope carries one epoch's team key to one device:

```json
{ "v": 1, "team_id": "...", "epoch": 3, "device_id": "<recipient>",
  "from": "<sender device_id>", "eph_pub": "...", "nonce": "...", "ct": "...",
  "sig": "..." }
```

- The sender makes a fresh ECDH P-256 key pair, agrees with the recipient's
  `enc_pub`, and derives a 32-byte key with HKDF-SHA256: salt = `eph_pub` bytes,
  info = UTF-8 `arakun-teams/v1/envelope|<team_id>|<epoch>|<device_id>`.
- `ct` = AES-256-GCM(key, `nonce` (12 random bytes), the 32-byte team key), with
  the same info string as associated data.
- `sig` = ECDSA by the sender's `sig` key over the UTF-8 bytes of
  `arakun-teams/v1/envelope|<team_id>|<epoch>|<device_id>|<eph_pub>|<nonce>|<ct>`.
  The recipient checks that the sender is a device of an admin of the team.
- `PUT /teams/<team_id>/envelopes` `{ "envelopes": [...] }` stores them;
  `GET /teams/<team_id>/envelopes` (with `X-Arakun-Device`) returns the caller
  device's envelopes for every epoch.

## Rotation

When a member leaves or is removed (`DELETE /teams/<team_id>/members/<github_id>`,
admin, or the member themselves), the server bumps the team's `epoch` and marks it
**rotation pending**. The next time an admin device connects it makes a new team
key and uploads envelopes for every remaining device
(`PUT /teams/<team_id>/envelopes`). Until then, clients keep sending with the old
epoch; the server refuses messages with an epoch older than the current one only
after the new epoch's envelopes exist. A removed member keeps what they already
read, never what comes after.

## Channels and messages

- `POST /teams/<team_id>/channels` `{ "channel_id": "<ulid>", "name_ct": Sealed }`
  (admin) → `201`. Names are encrypted with the team key (`Sealed`, below).
- Messages live in one Durable Object per channel.

`Sealed` is anything encrypted with a team key:

```json
{ "v": 1, "epoch": 3, "nonce": "...", "ct": "..." }
```

`ct` = AES-256-GCM(team key of `epoch`, `nonce`, plaintext), associated data =
UTF-8 `arakun-teams/v1/<what>|<team_id>|<channel_id>|<id>` where `<what>` is
`channel` or `message`.

A message on the wire:

```json
{ "id": "<ulid>", "device_id": "...", "epoch": 3, "nonce": "...", "ct": "...",
  "sig": "..." }
```

- `sig` = ECDSA by the sender device over UTF-8
  `arakun-teams/v1/message|<team_id>|<channel_id>|<id>|<epoch>|<nonce>|<ct>`, so
  the server cannot forge or alter messages. Clients drop messages whose signature
  or sender device does not check out.
- The plaintext inside `ct` is JSON:

```json
{ "kind": "text" | "bot" | "decision" | "decision_accept",
  "text": "...", "reply_to": "<message id>" | null,
  "mentions": ["bot"],
  "bot": { "range": ["<first id>", "<last id>"], "via": <github_id>,
           "cites": { "1": "<message id>", "2": "..." } } | null,
  "decision": { "of": "<bot message id>" } | null }
```

  - `text`: the user's message, or the bot's answer.
  - `reply_to`: the message this one answers, shown like a Discord reply.
  - `bot`: present when `kind` is `bot`. `range` is every message from `reply_to`
    of the asking message through the asking message itself; `via` is who ran it;
    `cites` maps the `[n]` in `text` to message IDs.
  - `decision_accept` with `decision.of`: a member accepted the decision a bot
    message proposed. The decision log is these, read back from the channels.

### Realtime

`GET /teams/<team_id>/channels/<channel_id>/ws` upgrades to a WebSocket (session
in `Authorization`, device in `X-Arakun-Device`). Frames are JSON text:

- client → server: `{ "type": "send", "message": <wire message> }`.
- server → client:
  - `{ "type": "message", "seq": 42, "at": 1795000000, "message": <wire message> }`
    for every stored message, the sender's own included (that is its ack);
  - `{ "type": "error", "id": "<message id>", "error": "..." }` when one is refused.

`seq` is the channel's order, set by the server. History, for a client that was
away: `GET /teams/<team_id>/channels/<channel_id>/messages?after=<seq>&limit=200`
→ `[{ seq, at, message }]`, oldest first.

The server checks on `send`: the sender device belongs to the session's user, the
user is a member, `epoch` is current (see Rotation), sizes (`ct` at most 64 KiB),
and that `id` is new. It cannot check `sig` against content it cannot read, but it
does check `sig` is a valid signature by that device, so a client cannot post as
another device.

## What the server sees

GitHub IDs and logins of users; team names; who is in which team and with which
role; device public keys and names; when each message was sent, its size, sender
device and channel; invites. It never sees team keys, channel names, message text,
who replied to what, bot answers or decisions.

## Not in v1

Attachments, editing and deleting messages, direct messages, recovery keys, and
the agent bus. They fit in the same shapes: new `kind`s and sealed blobs in R2.
