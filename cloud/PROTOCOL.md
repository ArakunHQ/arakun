# Arakun teams: protocol

The contract between the Worker in `cloud/` and the clients (the desktop app now, the
phone later). The server stores and forwards; it never holds a key that opens team
content. Everything below "the server sees" is metadata we accept showing. v1 keeps
team content from a server that follows this protocol and reads everything it
stores, not from one that acts against its users: see "What a hostile server can
do".

Version: `v1`. Envelopes and `Sealed` objects carry `"v": 1`. A message on the wire
has no `v`, and the server drops fields it does not know, so a client must not expect
one on the messages it receives.

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
uncompressed SEC1 (65 bytes). Signatures are raw r||s, 64 bytes, as WebCrypto
makes them. Times are Unix seconds. IDs are ULIDs (26 characters, upper-case
Crockford base32), so they sort by time; the server makes team and invite IDs.
Names (devices, teams) are 1 to 100 characters.

## Server-side checks

Every endpoint below needs a valid session and the user's `teams` flag; otherwise
`401` (no session) or `403 {"error":"teams is not enabled for this account"}`.
Team endpoints also need membership (`404` if not a member, so a team's existence
does not leak), and admin endpoints need `owner` or `admin` (`403`). The one
exception is `POST /teams/<team_id>/remind`, which only a pending user may call.

## Devices

`POST /devices` registers the calling device for the signed-in user:

```json
{ "sig_pub": "...", "enc_pub": "...", "name": "karlo-laptop",
  "endorsement": null | { "by": "<device_id>", "sig": "..." } }
```

- The server computes `device_id` from `sig_pub` and stores the row
  (`github_id`, `device_id`, `sig_pub`, `enc_pub`, `name`, `created_at`, `endorsed_by`,
  `endorsement_sig`): the endorsement's `by` and `sig`, both null for a first device.
- A user's first device has `endorsement: null`. Any later device must be endorsed
  by one of the same user's existing devices: `sig` = ECDSA by that device over the
  UTF-8 bytes of `arakun-teams/v1/device|<github_id>|<sig_pub>|<enc_pub>`. The
  server checks it (`400` if wrong), and so does every client before trusting the
  device. This stops the server from adding a later device of its own to a user
  whose first device a client has pinned.
- It does not make the first device trustworthy: the server could register one in
  the user's name. Clients trust a user's first device on first use: they pin it
  the first time they see that user, and show its fingerprint for an optional
  check with the person through another channel. If a user's first device later
  differs from the pinned one, or a fingerprint check fails, the client does not
  seal a team key to any of that user's devices and warns the user instead.
- Answer: `201 { "device_id": "..." }`. Registering the same `sig_pub` again
  answers `200` with the same ID; `409` if another account has it.
- At most 20 devices per user (`409`). v1 cannot remove a device, so every
  device ever registered counts (see "What a hostile server can do").

`GET /users/<github_id>/devices` lists a user's devices (the user must share a team
with the caller, or be the caller): `[{ device_id, sig_pub, enc_pub, name,
created_at, endorsed_by, endorsement_sig }]`. Clients verify each endorsement chain
back to the first device and show "new device" notices for devices they have not
seen. A device registered before the server kept `endorsement_sig` has it null
while `endorsed_by` is set; its chain cannot be checked.

`GET /devices` lists the same rows for many users in one answer: the caller and
everyone who shares a team with them, or with `?users=<github_id>,...` (at most
1,250) only those of them. A user out of reach is left out, without an error; a
user with no devices is listed with none. The answer is
`[{ github_id, devices: [<as above>] }]`, by `github_id`. A client refreshing its
teams reads this once instead of each member's list once per team. A server from
before this call answers `404`; the client then asks for each user.

`GET /teams/<team_id>/channels/<channel_id>/senders/<device_id>` (member, with
`X-Arakun-Device`) finds who wrote a message the client cannot place: someone who
has left the team, whose devices neither `GET /users/<github_id>/devices` nor
`GET /devices` lists any more.
It answers `{ github_id, login, devices: [...] }`, `devices` being that user's
devices in the same shape as above, so the client checks the endorsement chain
and the pinned first device the same way. `404` unless the channel still keeps a
message from that device, so a member cannot list the devices of someone who
never wrote there. A client shows such a message as from a former member when
its sender is not a member now. An older server answers `404` for the path too;
the client then hides those messages as ones it could not check.

Every request a device makes about its own keys carries `X-Arakun-Device: <device_id>`:
accept, approve, both envelope requests, the channel socket and history. `400`
without it, `403` if it is not one of the caller's devices.

## Teams

- `POST /teams` `{ "name": "Arakun" }` → `201 { team_id, epoch: 1 }`. The caller
  becomes `owner`. The name is plaintext (it shows in invites before anyone holds
  a key). The creating device then makes the epoch-1 key and uploads an envelope
  for each of its user's devices (below). `409` if the caller is in 50 teams
  already.
- `GET /teams` → the caller's teams: `[{ team_id, name, role, epoch,
  rotation_pending }]`.
- `GET /teams/<team_id>` → `{ team_id, name, epoch, keyed_epoch, roster_epoch,
  rotation_pending, members: [{ github_id, login, role, joined_at, first_key_epoch }], channels:
  [{ channel_id, name_ct, created_at }] }`.
- `roster_epoch` equals `epoch` on Workers that enforce it at write time.
  `first_key_epoch` is the oldest envelope held by any device of this member,
  or `null` when none exists. Removal deletes those envelopes before a rejoin.
- `rotation_pending` is true while the current epoch has no envelopes yet: from
  creation until the first key, and after every removal (see Rotation).
- `keyed_epoch` is the newest epoch with envelopes (0 before the first key). A
  client sends the one it read here as `keyed_epoch` with an envelope upload, so a
  rotation that lost is refused (see below).

## Invites and approval

1. An admin invites by GitHub login: `POST /teams/<team_id>/invites
   { "github_login": "rikvisser-dev" }` → `201 { invite_id, expires_at,
   github_id, login }`. The server asks GitHub which user has the login now and
   keeps that user's numeric ID; the invite is theirs, whatever login they have
   when they sign in. `login` is that user's login as GitHub spells it. `404` if
   no GitHub user has the login (an organization's login included), `502` if
   GitHub could not be asked, `429` after 30 invites in a minute by one admin.
   Invites expire after 7 days. Inviting the same person again replaces their
   invite; `409` if they are a member or pending already.
   Admins see the team's open invites in `GET /teams/<team_id>/invites` →
   `[{ invite_id, login, invited_by_login, created_at, expires_at }]`, newest
   first, without expired ones (`login` is lower case). An admin withdraws one
   with `DELETE /teams/<team_id>/invites/<invite_id>` → `204`; `404` if the team
   has no such open invite.
2. The invitee sees it in `GET /invites` → `[{ invite_id, team_id, team_name,
   invited_by_login, created_at, expires_at, member_count }]` (`member_count`
   leaves out pending members) and accepts with
   `POST /invites/<invite_id>/accept` (from a registered device) → `200 { team_id,
   status: "pending" }`: the user becomes a **pending** member. A pending member
   is not a member: every team endpoint but `POST /teams/<team_id>/remind` (below)
   answers them `404`. Or they decline it:
   `POST /invites/<invite_id>/decline` → `204`, and the invite is gone; `404` if
   it is expired or someone else's.
   A pending user sees their own requests in `GET /pending` → `[{ team_id,
   team_name, requested_at, invited_by_login, invited_by_id, reminded_at }]`,
   oldest first, so a client finds them again after a restart. They may remind
   the team's admins: `POST /teams/<team_id>/remind` → `204`, which sets
   `reminded_at`. At most once an hour per request: sooner answers
   `429 { "error": "...", "retry_at": <unix seconds> }` with a `Retry-After`
   header. Anyone who is not pending in that team, members included, gets `404`.
   The server sends nothing anywhere; admins see the time in the pending list.
3. Admins see pending members in `GET /teams/<team_id>/pending` → `[{ github_id,
   login, invited_login, request_id, requested_at, reminded_at, devices: [...] }]`
   (`reminded_at` is null until the first reminder). `login` is the user's login
   now, `invited_login` the one the invite named, and `request_id` the accepted
   invite's ID (an opaque string for requests made before it was kept). The
   invite went to whoever had the login when it was made. GitHub can give a
   login that was renamed or deleted to someone else, so that may be another
   person than the admin meant. The admin's client shows the
   `github_id` and the login next to each other when it asks to approve, and
   warns when `login` and `invited_login` differ (ignoring case). The `github_id`
   is who asked, and the one the client pins.
4. An admin's device approves: `POST /teams/<team_id>/pending/<github_id>/approve`
   with the request it showed and the envelopes for every device of the new member
   and every epoch they may read (all of them, by default):
   `{ "request_id": "...", "envelopes": [Envelope, ...] }`. `409` if the
   request is no longer that one: it was rejected and the user accepted a new
   invite, so the admin looks again. Clients also send what the admin reviewed,
   `"devices": [device_id, ...]` (the new member's) and `"keyed_epoch"` (from
   `GET /teams/<team_id>`), so a device added or a rotation ended since then
   answers `409` rather than `400`. The server checks there is one envelope per
   (device, epoch) it expects, stores them, and makes the user a `member`:
   `200 { github_id, role: "member" }`. It expects every device of the new member
   and every epoch with a key (one that has envelopes) from the lowest one given
   through the newest, so an admin may leave out old epochs but never a device. An
   epoch that two removals skipped has no key and is left out. A request holds at
   most 500 envelopes; a grant of more (20 devices and over 25 epochs) goes in
   parts: approve with the newest epochs, then upload the older ones for the new
   member's devices with `PUT /teams/<team_id>/envelopes`, 500 at a time. While a rotation is
   pending, the rotation then covers the new member too. `409` if the team has no
   key yet, has 25 members already, or its newest key or the new member's devices
   changed since the admin read them (fetch them again and retry). `403` if the
   caller stopped being an owner or admin, or lost the session or the flag, while
   the approval ran; nobody is added then. `.../reject` `{ "request_id": "..." }`
   drops that request: `204`, or `409` if the request is no longer that one.

The approval happens on the admin's device because only it holds the team key.
A server that follows the protocol cannot let anyone in by itself; one that acts
against its users can, by adding a member row (see below).

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
  When it opens an envelope, the recipient checks that the sender is a device of a
  current owner or admin of the team, and refuses it otherwise. The server keeps an
  envelope after its sender leaves, so a device can find one it refuses; it then
  asks an admin, whose device seals that epoch again for it (an upload replaces an
  envelope whose sender is no longer an admin's device).
- `PUT /teams/<team_id>/envelopes` `{ "envelopes": [...] }` (admin) stores them →
  `200 { stored, epoch, rotation_pending }`. The optional `keyed_epoch` is the
  one the client saw when it made the envelopes; an upload carrying the current
  epoch whose `keyed_epoch` is behind is a rotation that lost, answered `409`,
  not a rewrap that may replace the envelopes in use. Clients granting historical
  keys send `roster_epoch` from the membership they reviewed. A removal changes
  that epoch: a stale grant returns `409` without writing any envelopes. The
  write also requires a live admin session. At most 500 per request. Each
  envelope's `from` is the calling device and its `sig` must check out; each
  recipient is a device of a member; its epoch already has envelopes or is the
  current one. The first upload for the current epoch must cover every device of
  every member at once, and it ends the rotation. The server checks that coverage
  again as it stores them, and answers `409` if a removal started a newer epoch,
  another upload made this epoch's key first, even one from the same device (fetch
  it), or a member or device was added in between (fetch the devices and retry).
  An upload replaces an envelope for the same (device, epoch) only if the same
  device sent the old one, or its sender is no longer an admin's device. `stored`
  counts what was written: a device that made a new key and sees fewer than it sent
  must fetch the key in use. At most 25 members per team, so a rotation fits in one
  request.
- `GET /teams/<team_id>/envelopes` (with `X-Arakun-Device`) returns the caller
  device's envelopes for every epoch, oldest first. Each is the envelope as it was
  sent, plus `made_at`: when that epoch's key first reached the team (the oldest
  stored envelope of that epoch), so a device added later still sees how old a
  key is. The server sets `made_at` and nothing signs it: a client must not trust
  it for anything but display, and must check `sig` without it.

## Rotation

When a member leaves or is removed (`DELETE /teams/<team_id>/members/<github_id>`,
admin, or the member themselves), the server bumps the team's `epoch` and marks it
**rotation pending** → `200 { epoch, rotation_pending: true }`. Only an owner
removes an owner, and the last owner cannot leave (`409`). The server deletes the
removed member's envelopes and closes their channel sockets (code `4403`).

The next time an admin device connects it makes a new team key and uploads
envelopes for every remaining device (`PUT /teams/<team_id>/envelopes`). The
server accepts a message, or a channel name, only when its epoch is the current
one and that epoch has envelopes. So while a rotation is pending it accepts none:
the newest key with envelopes is one the removed member holds, and the current
epoch's key may never reach the members (two removals before a rotation skip an
epoch, and nothing is ever sealed with it). This holds from the moment the removal
commits, also for a device that has not read the team since: its next send is
refused with the code `rotation_pending` (see Realtime). An Arakun client that
knows of the rotation (`rotation_pending`) does not try: it shows that the
rotation is pending. A message refused this way was not stored; the client reads
the team again and, once the new key has reached it, seals the message again with
that key and sends it. A removed member keeps what they already read, and never
what comes after the removal.

Servers from before this rule accepted the newest epoch with envelopes during a
rotation, so a device that had not read the team yet could still send with the
old key. Arakun clients read the team at least every 20 seconds and stop sending
when they see `rotation_pending`, so against such a server that window remains.

## Channels and messages

- `POST /teams/<team_id>/channels` `{ "channel_id": "<ulid>", "name_ct": Sealed }`
  (admin) → `201 { channel_id }`. Names are encrypted with the team key
  (`Sealed`, below), of the epoch a message would use now (`400` otherwise). At
  most 500 channels per team; `409` for that or an ID in use.
- `PUT /teams/<team_id>/channels/<channel_id>` `{ "name_ct": Sealed }` (admin) →
  `200 { channel_id }` seals a channel's name again, with the same rule on its
  epoch, or renames it. `404` if the team has no such channel, `409` if the
  team's newest key changed meanwhile. After it makes a new key, the admin's
  device seals every channel's name again with it, so a member approved without
  the older epochs can read every name.
- Messages live in one Durable Object per channel.

`Sealed` is anything encrypted with a team key:

```json
{ "v": 1, "epoch": 3, "nonce": "...", "ct": "..." }
```

`nonce` is 12 random bytes, new for each encryption: the server refuses any
other length, and a nonce used twice with one key breaks AES-GCM.
`ct` = AES-256-GCM(team key of `epoch`, `nonce`, plaintext), associated data =
UTF-8 `arakun-teams/v1/<what>|<team_id>|<channel_id>|<id>` where `<what>` is
`channel` or `message`, and `<id>` is the message's `id` for a message and the
channel's `channel_id` again for a channel name (so it appears twice).

A message on the wire:

```json
{ "id": "<ulid>", "device_id": "...", "epoch": 3, "nonce": "...", "ct": "...",
  "sig": "..." }
```

- `sig` = ECDSA by the sender device over UTF-8
  `arakun-teams/v1/message|<team_id>|<channel_id>|<id>|<epoch>|<nonce>|<ct>`, so
  the server cannot forge or alter messages. Clients drop messages whose signature
  or sender device does not check out. A sender who has left the team is looked
  up with `.../senders/<device_id>` (see Devices).
- The plaintext inside `ct` is JSON:

```json
{ "kind": "text" | "bot" | "decision" | "decision_accept" | "decision_reverse"
          | "edit" | "agent_review",
  "text": "...", "reply_to": "<message id>" | null,
  "mentions": ["bot"],
  "bot": { "range": ["<first id>", "<last id>"], "via": <github_id>,
           "cites": { "1": "<message id>", "2": "..." } } | null,
  "decision": { "of": "<bot message id>" } | null,
  "coordination": { "step": "ready" | "request" | "notes" | "done" | "blocked", ... } | null }
```

  - `text`: the user's message, or the bot's answer.
  - `reply_to`: the message this one answers, shown like a Discord reply.
  - `bot`: present when `kind` is `bot`. `range` is every message from `reply_to`
    of the asking message through the asking message itself; `via` is who ran it;
    `cites` maps the `[n]` in `text` to message IDs.
  - `decision_accept` with `decision.of`: a member accepted the decision a bot
    message proposed. `reply_to` is that bot message. The decision log is these,
    read back from the channels: for each decision (`decision.of`), one
    acceptance per member, the oldest. A member may accept many decisions.
  - `decision_reverse` with `decision.of`: a member reversed the decision a bot
    message proposed. `reply_to` is that bot message and `text` the reason, which
    may be empty: `{ "kind": "decision_reverse", "text": "...", "reply_to":
    "<bot message id>", "decision": { "of": "<bot message id>" } }`. Any member
    may send one. A decision with a reversal stays reversed for good, and clients
    show the oldest reversal. To decide again, someone runs `@bot` again, which
    makes a new decision.
  - `edit`: a new text for an earlier message: `{ "kind": "edit", "text": "<new
    text>", "reply_to": "<id of the edited message>", "mentions": [...] }`.
    Clients apply it only when its sender's `github_id` is the edited message's
    sender's `github_id`, and the edited message is a `text` (not a `bot`, a
    `decision*` or another `edit`). They show the newest edit by `seq`, with an
    "edited" mark. Mentions in an edit never start `@bot`. The old text stays in
    the channel's history: every member already has it.
  - A plain `@bot` without `reply_to` reads only the asking message.
  - `bot` may also carry, for clients to render:
    - `read`: how many messages the range held;
    - `parts`: present when a long range was summed up in parts first;
    - `answer`: `{ ideas: [{ label, title, verdict: "works" |
      "works_with_conditions" | "wont_work", reason, cites: [n] }], verified:
      [{ text, source }], assumed: [text], unchecked: [text], decision: text }`.
    `cites` maps only the numbers the answer cites. Clients ignore a `bot`
    message whose `via` is not its sender's `github_id`. All of this is inside
    the encrypted body; the server never sees it.

### Realtime

`GET /teams/<team_id>/channels/<channel_id>/ws` upgrades to a WebSocket (session
in `Authorization`, device in `X-Arakun-Device`). With `?presence=0` the socket
opens **quiet**: it does not count toward presence (below). `?presence=1`, the
default, counts; any other value answers `400`. A client opens a channel it reads
but does not show quiet (for the decision log, or kept open after the user moved
on), so other members do not see it there. A server older than this ignores the
parameter, and every socket counts. Frames are JSON text:

- client → server:
  - `{ "type": "send", "message": <wire message> }`;
  - `{ "type": "presence", "on": true | false }` makes the socket count toward
    presence (`true`, when the user opens the channel) or go quiet (`false`, when
    they leave it), without a new socket. Nothing answers it; when the list
    changes, the presence frame that follows reaches every counted socket, this
    one included when it counts now. Asking for what the socket has already does
    nothing. An `on` that is not a boolean is refused with an `error` frame whose
    `id` is null, and so is the whole frame by a server older than this.
- server → client:
  - `{ "type": "message", "seq": 42, "at": 1795000000, "message": <wire message> }`
    for every stored message, the sender's own included (that is its ack);
  - `{ "type": "error", "id": "<message id>", "error": "...", "code": "..." }`
    when one is refused (`id` is null when the frame had none). A refused message
    was not stored. `error` is for people; clients match `code`, which only some
    refusals carry, and ignore a code they don't know:
    - `exists`: the channel already holds a message with this `id` (a resend of
      one that was stored), so nothing is left to send;
    - `rotation_pending`: a rotation is pending, so no epoch is accepted. Read
      the team again and send once the new key has reached the device;
    - `stale_epoch`: a newer epoch has envelopes. Read the team again, fetch the
      new key, seal the message again with it and send it;
    - `unknown_epoch`: the epoch is newer than the team's. Read the team again;
    - `retry`: the server could not run its checks (D1 or the rate limiter
      failed). The client may send the same message again, with the same `id`.
    A refusal without a code (a bad signature, too many messages) asks nothing
    of the client but to show it. Every epoch refusal says "epoch" in
    `error`, none says "exists", and a `retry` one ends in "send it again", for
    clients from before `code`;
  - `{ "type": "presence", "here": [<github-id>, ...] }` to every counted socket
    on the channel when a counted one opens, closes, or starts or stops counting:
    the GitHub IDs with a counted socket open on this channel, sorted, each once.
    A new counted socket gets one right away. A quiet socket is not listed, gets
    no presence frames, and its opening and closing send none. A socket whose
    session ended, or whose user is no longer a member or lost the `teams` flag,
    gets none and is not listed. Clients ignore a `type` they don't know.
- A frame is at most 96 KiB. Each device may send about 60 messages every 10
  seconds; more are refused. The count is kept in each Cloudflare location on its
  own and catches up with a delay, so a few more can get through.
- The server closes a socket with `4403` when its user is removed. On every
  message it also checks each socket on the channel, sender and recipients: one
  whose session ended (logout or expiry), whose user is no longer a member, or who
  lost the `teams` flag is closed with `4403` and gets nothing more. A socket that
  opened while a message was being checked, too late for that check, gets `4503`
  instead of the message: the client opens a new socket and reads the history.

`seq` is the channel's order, set by the server. History, for a client that was
away: `GET /teams/<team_id>/channels/<channel_id>/messages?after=<seq>&limit=200`
→ `[{ seq, at, message }]`, oldest first. `after` defaults to 0, `limit` to 200,
its maximum.

A channel keeps at most 10,000 messages. A send that makes it more than
10,000 drops the oldest, so the newest 9,000 stay. If the first history row has a
`seq` greater than `after + 1`, the missing prefix is no longer available on
the server. Mark the gap, keep any local copies, and resume from the first
returned row instead of requesting the missing prefix again.

A client keeps the highest `seq` it holds with no gap before it. Once its socket
is open, it reads the history after that `seq`. When a live frame's `seq` is more
than one past it, it reads the history in between. A message stored while a socket
was opening may not reach that socket live, and no later frame may show the gap,
so the server closes such a socket with `4503`; the client then opens a new one and
reads the history again in the same way.

The server checks on `send`: the sender device belongs to the session's user, the
user is a member, `epoch` is the current one and has envelopes (see Rotation) and
is not older than the channel's last stored message, sizes (`ct` at most 64 KiB), and that
`id` is new in the channel. So a channel's epochs never go back as `seq` grows. It cannot check `sig` against content it
cannot read, but it does check `sig` is a valid signature by that device, so a
client cannot post as another device. Membership and epoch are read again on
every `send`, so a removal or rotation applies to the next message.

## What the server sees

GitHub IDs and logins of users; team names; who is in which team and with which
role; device public keys and names; when each message was sent, its size, sender
device and channel; invites, who withdrew or declined them, requests to join and
when each was last reminded; when each envelope was first stored. It never sees team
keys, channel names, message text, who replied to what, edits, bot answers,
decisions or their reversals: these are messages like any other to it.

## What a hostile server can do

v1 does not protect a team against an actively malicious server: the operator of
the Worker or of its D1 database, or anyone who takes control of them. Members,
roles and the list of a user's devices are plain rows that clients take as the
server reports them.

Such a server can:

- Add any GitHub ID to a team as a `member`, `admin` or `owner`, without an
  invite or an approval. For a user no client has seen yet, it can also register
  a first device it made in that user's name, which clients trust on first use.
- Read everything the team sends from then on. Rotation must cover every device
  of every member the server lists, so honest admins seal the next key to its
  device, and that device, as an "admin", may make the key of a new epoch itself
  (the server can bump the epoch at any time). Recipients accept envelopes from
  any device of an owner or admin the server lists.
- Drop, delay or hold back messages, envelopes, invites and members, and show
  different clients different lists.
- Stall a rotation: a client does not seal a key to a user whose pinned first
  device changed, and the server refuses a rotation that leaves out a device. The
  team then can't send: nothing is accepted while a rotation is pending.
- Read the metadata above, as any server here can.

It cannot:

- Read messages or channel names sealed with an epoch whose key its device never
  got: the epochs before it joined, unless an honest admin's device seals an older
  epoch to it.
- Forge or change a message or an envelope of a real device: each is signed by
  that device over where it belongs (team, epoch, and channel and `id` or the
  recipient) and its ciphertext.
- Add a later device to a user whose first device a client has pinned, or swap
  that first device, without the client warning (the endorsement chain).
- Make a removed member's device read what is sealed after a rotation that
  honest admins made without it.

A client shows what it can check so people may notice: a new member or role, a
user's new device, a first device that differs from the pinned one, and whether a
rotation is pending (`rotation_pending`).

Not in v1 either, and tracked as follow-ups:

- a membership log signed by admin devices, which clients check before they
  trust a member, a role or an envelope's sender
  ([#373](https://github.com/ArakunHQ/arakun/issues/373));
- removing a device: a revocation signed by another of the user's devices, which
  bumps the epoch in each of their teams; a cap that counts only active devices;
  and a way to leave a device or user out of a rotation on purpose
  ([#374](https://github.com/ArakunHQ/arakun/issues/374)). Until then a lost or
  stolen device gets every new key, and a user who loses every device cannot
  register a new one.

## Paired agent reviews

`agent_review` is an encrypted body kind. Its `coordination.step` is `ready`,
`request`, `notes`, `done`, `blocked` or `withdrawn`; `text` is readable by
older clients.
An enrollment binds a fresh random ticket to one local device, channel, key
epoch and peer GitHub ID for 15 minutes. The `ready` event advertises the peer
and ticket. A `request` names both tickets (`from`, `to`) and a fresh `job`;
`notes` returns the same job with the tickets reversed. Check the sender from
the verified wire signature, not from text or metadata. Only a matching live
enrollment can run a turn, once. If both devices send a `request` before
either sees the other's, only the one with the smaller `job` runs; the other
device answers it and drops its own. `done`, `blocked` and `withdrawn` never
trigger work. Cancel on disconnect, account or project change, or key rotation.
A device that cancels an unused ticket while the channel can still carry a
message posts `withdrawn` with that ticket; do not send a `request` to a
withdrawn ticket, and stop waiting for `notes` from one. A device whose
enrollment ends during its review turn answers the `request` with `blocked`.
The 15 minutes bound how long a ticket waits to be used; an exchange that
started in them may end after them. Each turn runs for at most 4 minutes, and a
requester stops waiting for `notes` after 5 minutes, since a peer that lost its
socket cannot say so. No grant survives a restart. The server does not schedule or see these steps.

Members explicitly choose to send the selected text to their own Claude Code.
The peer runner uses no tools, no project context and no MCP servers. Results
are recommendations, not authority to run tools or accept decisions.

## Not in v1

Attachments, deleting messages, direct messages, recovery keys, and general
agent task execution. They fit in the same shapes: new `kind`s and sealed blobs
in R2. The signed membership log and device revocation are above.
