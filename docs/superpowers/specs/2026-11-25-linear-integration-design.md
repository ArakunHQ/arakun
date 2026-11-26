# Linear integration

Issues: #939 (the Linear tab), #940 (delegation through the relay).

## Why

People who plan in Linear copy issue text into an Arakun session by hand, and can
only start a session from Arakun or the phone remote. We want Linear and Arakun
connected both ways:

- **Linear from Arakun.** See your Linear issues in Arakun and start a session on one.
- **Arakun from Linear.** Delegate an issue to "Arakun" inside Linear, from any
  device, and have a session start on your own computer. This must work from
  everywhere, so it goes through the relay.

## Goals

1. A Linear tab: sign in, see the open issues assigned to you, open one, and start a
   session on it in the open project.
2. Arakun is an agent in a Linear workspace. Assigning or mentioning it on an issue
   starts a session on the computer of the person who did it.
3. The session runs in a new environment of the Arakun project that the issue's
   Linear team or project is mapped to. The mapping is set in the Linear tab.
4. Linear shows what happens: received, started, needs input, finished or failed.
5. If the computer is off, the issue says so and the session starts when the
   computer is back.

## Non-goals

- Creating or editing Linear issues from the tab (status, assignee, comments). Later.
- Running the agent anywhere but the user's own computer. No cloud sandbox.
- Choosing a computer or project inside Linear at delegation time.
- Guessing the project from a linked GitHub repository.
- Linear's MCP server for agents. Sessions can already add it by hand.
- Code behind the `team` and `coderlabs` features.

## Anti-goals

- The Worker must not store issue text, comments, prompts or progress text. It
  stores ids and tokens only. Linear sends issue text to the Worker in its webhook;
  the Worker reads the ids it needs and drops the rest.
- The relay's phone traffic stays end-to-end encrypted and unreadable. This feature
  uses the relay's own control lane (channel 0), not the phone channels.
- The server must not be able to start a session on a computer that did not opt in,
  or in a project the user did not map.
- A person must not receive another person's delegations by claiming their Linear
  identity.
- The Linear tab must work without an Arakun account. Only delegation needs one.

## Constraints

Facts checked against Linear's developer docs on 2026-11-25
(`linear.app/developers/agents`, `agent-interaction`, `agent-best-practices`,
`agent-signals`, `oauth-2-0-authentication`, `webhooks`, `rate-limiting`):

- An agent app is installed with `GET https://linear.app/oauth/authorize` and
  `actor=app`, scopes `read,write,app:assignable,app:mentionable`. A workspace admin
  must install it. `admin` scope cannot be combined with `actor=app`.
- Access tokens last 24 hours. Refresh tokens are required, last 30 days and rotate
  on every refresh.
- Linear calls one webhook URL per app with `Linear-Event: AgentSessionEvent`,
  action `created` (delegated or mentioned) or `prompted` (a follow-up message; a
  stop arrives as a prompt with `signal: "stop"`). The body carries
  `organizationId`, `appUserId`, `agentSession { id, issueId, issue { identifier,
  teamId }, creatorId }`, and issue text in `promptContext`. It has no project id.
- `Linear-Signature` is the hex HMAC-SHA256 of the raw body with the signing secret.
  `webhookTimestamp` must be within 60 seconds. Answer 200 within 5 seconds.
- After `created` the agent must post an activity within 10 seconds or Linear marks
  it unresponsive. A session with no activity for 30 minutes goes stale; a new
  activity revives it.
- Activities are posted with `agentActivityCreate`, types `thought`, `action`,
  `elicitation` (waits for the user), `response` (ends the turn) and `error`. Only
  the app's own token can post them. Links are set with `agentSessionUpdate`
  (`externalUrls`).
- `creatorId` can be missing, or be the assignee when an automation delegated.
- OAuth with PKCE needs no client secret. Whether a loopback redirect on any port
  is accepted is not documented.

Facts about the code today:

- Service tabs: `View` in `src/sidebar.rs`, `TabKind` in `src/model.rs`, wiring in
  `src/shell.rs` and `src/shell/posthog.rs`. Credentials are 0600 JSON files under
  the app's data folder. HTTP is blocking `ureq` through `src/http.rs`, with
  `Loadable` for background state. Fixtures (`ARAKUN_POSTHOG_FIXTURE`) feed a test
  Arakun.
- "Work on it with agent" for a GitHub issue: `work_on_issue` in
  `src/github_view/mod.rs` emits an event that ends in `Shell::ask_agent`.
- A new environment with a first prompt from code:
  `Shell::quick_create_env_then` (`src/shell.rs`). It picks a random branch name.
- The relay client ignores unknown channel-0 control messages
  (`core/src/phone/relay.rs`, `heard`). The Relay Durable Object closes the socket
  on any channel-0 control from the computer after the claim.
- The Worker has no way yet to make the Durable Object send a control message, no
  webhook verification and no encrypted token storage. Routes are modules chained
  in `cloud/src/teams-router.ts`; the first path segment must be in its `API` set.
- Accounts are GitHub ids. `machines` ties a relay label to an account.

## Approaches

For the path from Linear to the computer, three were considered:

- **A. The Worker is the doorbell and the mouthpiece.** It holds the app token per
  workspace, answers Linear at once, and sends only ids to the computer. The
  computer reads the issue with the user's own Linear sign-in, runs the session and
  sends progress to the Worker, which posts it as Arakun. Nothing with text is
  stored on the server. Most work on the computer side.
- **B. The Worker does everything Linear.** It reads the issue, sends the full
  prompt down and posts all replies. Least code, but the server reads and queues
  every delegated issue.
- **C. A webhook on a label, no agent app.** Cheaper, but there is no Arakun
  teammate in Linear and no live progress, and it still needs an admin.

**Chosen: A** (Rik). It gives real delegation in Linear and keeps content off the
server, and the Linear sign-in it needs on the computer is the one the tab needs.

For where a delegated issue lands, three were considered: a mapping set in Arakun,
asking in Linear each time, and guessing from the linked repository.
**Chosen: the mapping** (Rik). It is predictable and needs no step per issue.

## Design

### Part 1: the Linear tab (#939)

**Sign-in.** One Linear account per workspace, several workspaces allowed, stored in
`linear/sessions.json` (0600) like PostHog's. Two ways in:

- OAuth with PKCE and a loopback redirect, as `src/doop/oauth.rs` does, with the
  public client id of Arakun's Linear app and scope `read`. *(My call.)*
- A personal API key pasted in, as the PostHog tab does. This is the fallback, and
  the only way until the Linear app exists or if Linear refuses the loopback
  redirect. *(My call.)*

The token never leaves the computer, except once for linking (part 2).

**The tab.** A new `View::Linear` and `TabKind::Linear`, off by default like the
other service tabs. It opens on the open issues assigned to you, grouped by state
(started, unstarted, backlog), with identifier, title, team and priority. A search
box filters the list. An issue's sheet shows the description and comments, and has
"Work on it with agent" and "Open in Linear". In the box, `/work` does the same.

```
Linear                                   acme ▾   ⟳
──────────────────────────────────────────────────
In progress
  ENG-412  Tab name is lost on resize        Eng
Todo
  ENG-398  Phone page shows stale sessions   Eng
  WEB-77   Pricing page copy                 Web
```

**Work on it.** Same path as a GitHub issue: a session in the open project through
`Shell::ask_agent`, with a prompt built from the issue's identifier, title,
description, comments and URL. It leaves git and Linear to the user.

**Tests.** `ARAKUN_LINEAR_FIXTURE` for a test Arakun, in the shape of the PostHog
fixture. Unit tests for the GraphQL replies and the prompt.

### Part 2: delegation (#940)

```
Linear ──webhook──▶ Worker ──relay channel 0──▶ computer ──▶ new environment + session
   ▲                  │  ▲                          │
   └──activities──────┘  └────────HTTPS─────────────┘
```

#### On the Worker (`cloud/src/linear.ts`, routes under `/linear`)

**Install.** `GET /linear/install` sends a workspace admin to Linear with
`actor=app`. `GET /linear/callback` exchanges the code with the client secret,
reads `viewer { id organization { id name } }` and stores the workspace: organization
id, app user id, access token, refresh token, expiry. Tokens are encrypted with
AES-GCM under a Worker secret. Refresh is serialized per workspace so a rotated
refresh token is never used twice. *(My calls.)*

**Link.** `POST /linear/link`, signed in to Arakun, with a Linear access token in
the body. The Worker calls Linear's `viewer` with it once, stores
(organization id, Linear user id) → GitHub id, and forgets the token. This is the
proof that the Arakun account owns that Linear user. `DELETE /linear/link` removes
it. *(My call. It is the one time a user's Linear token touches the server.)*

**Routes.** `PUT /linear/routes`, signed in, from a computer: its relay label and
the list of Linear team ids and project ids it takes work for. Ids only. The app
sends it when the mapping or the opt-in changes. An empty list means this computer
takes nothing.

**Webhook.** `POST /linear/webhook`. Verify the signature and the timestamp, answer
200. Repeats are dropped by the agent session's id (a delegation) or the activity's
id (a follow-up), because `webhookId` names the webhook, not the event. Then for
`created`:

1. Find the account by (`organizationId`, `creatorId`). If there is none, post an
   `error` activity: the person has not linked Linear in Arakun, with a line on how.
   If `creatorId` is missing, or the session was made by an automation and not by
   a person, post an `error`: delegate the issue yourself. An automation must not
   start work on someone's computer. *(My call; automations can come later with
   their own opt-in.)* A session with no issue (a mention in a document) gets an
   `error` too: Arakun works on issues.
2. Read the issue's project id from Linear with the app token (the webhook has
   none). Find the computer whose routes name the project, else the team. A project
   wins over a team. If two computers match, the one seen last wins. If there is
   none, post an `error`: no computer is set up for this team.
3. Post a `thought`: sent to the computer by name, or "is offline; the session
   starts when it is back". This is the answer Linear needs within 10 seconds.
4. Store a delivery: agent session id, issue id, organization id, label, kind. Hand
   it to the Relay Durable Object for that label.

For `prompted`, the delivery carries the kind `prompt` or `stop` and the activity's
id, not its text. The computer reads the text from Linear. Only the person who
delegated can steer or stop the session: the Worker compares the activity's author
with the delivery's owner, and answers anyone else with a `thought` naming who can.
*(My call: another member's text must not be typed into your session.)*

A mention on an issue is handled like a delegation, with the comment added to the
prompt. *(My call. A lighter "answer only" mention can come later.)*

**Relay.** A new internal route on the Durable Object, like `/_recheck`, takes a
delivery. The object keeps it in storage and sends
`{ "type": "linear", "id", "kind", "agent_session", "issue", "organization" }` on
channel 0 when the computer is connected, and again after every `claimed` until it
is accepted. Deliveries older than 7 days are dropped, with an `error` activity
saying so. *(My call: 7 days.)* These frames do not count toward the plan's limits.

**From the computer.** Plain HTTPS with the Arakun session, not relay frames, since
the computer has the network when it has the relay:

- `POST /linear/deliveries/<id>/accept` removes the delivery from the queue.
- `POST /linear/sessions/<agent_session>/activity` with a type (`thought`,
  `elicitation`, `response`, `error`), a body and optional links. The Worker checks
  the caller's account owns that delivery, posts it to Linear with the app token and
  stores nothing.

**Rollout.** Everything under `/linear` except the webhook needs the user flag
`linear`, so it can ship before it is open to all. `GET /me` already carries flags.

**Data.** One migration, `0013_linear.sql`: `linear_workspaces`, `linear_links`,
`linear_routes`, `linear_deliveries` (also the record of which account owns an
agent session). Secrets: `LINEAR_CLIENT_ID`, `LINEAR_CLIENT_SECRET`,
`LINEAR_WEBHOOK_SECRET`, `LINEAR_TOKEN_KEY`. Without them `/linear` answers 404.

#### On the computer

**Opt-in.** A switch in the Linear tab, off by default: "Let Linear start sessions
on this computer". It needs an Arakun account sign-in and the relay on. Turning it
on links the Linear account and sends the routes.

**Mapping.** A screen in the Linear tab: rows of Linear team or project → Arakun
project, kept in `linear/routes.json` by the project's root path. Only projects on
this computer can be picked, not projects on a server, and the environment is always
made on this computer. *(My call, from the non-goal.)* If the mapped project has
been removed from Arakun, the delivery gets an `error` and the row shows as broken.

```
Start sessions from Linear                      [ on ]
──────────────────────────────────────────────────
Team  Eng                →  arakun            ✕
Project  Phone remote    →  arakun-phone      ✕
+ Add a mapping
```

**A delivery arrives.** `heard` in `core/src/phone/relay.rs` gets a `linear` arm and
passes it to the shell as a new `Incoming` variant. The shell then:

1. Checks the switch is on, reads the issue with the user's own Linear token, and
   checks its team or project is mapped here. If not, it reports an `error` and
   accepts the delivery, so it is not sent again. This check is what keeps a
   misbehaving server from starting work in an unmapped project.
2. Accepts the delivery.
3. For `created`: makes a new environment in the mapped project on Linear's own
   branch name for the issue (`issue.branchName`), and starts a session there with
   the delegated prompt (below), through `quick_create_env_then` given a branch
   name. The session uses the project's usual agent, model and permissions. If an
   environment for that issue exists, it starts the session there instead.
4. Remembers agent session → Arakun session in `linear/runs.json`.
5. For `prompt`: sends the text to that session as a follow-up message. For `stop`:
   stops the session and reports a `response` saying it stopped.

The session shows in the sidebar and the Inbox like any other, marked with the
issue's identifier.

**The delegated prompt.** The person may be away from the computer, so a delegated
session finishes the job: the "Work on it" prompt, plus an instruction to commit,
push the branch and open a draft pull request that names the issue, following the
project's own rules. The shell adds the pull request's link to the Linear session
(`externalUrls`) when the environment gets one. *(Decided by Rik: a delegated session ends with a draft pull request.)*

**Reporting back.** From `ThreadEvent`, as the phone notices do:

| In Arakun | In Linear |
|---|---|
| Session started | `thought`: started in project, environment, computer |
| Needs a permission or an answer | `elicitation`: the question; answer in Arakun or by reply |
| Finished | `response`: the last reply; the pull request link if one was opened |
| Failed or could not start | `error`: the reason |

A reply to an elicitation in Linear arrives as a `prompt` and goes to the session as
a new message; it does not fill in a question form. While a permission is pending,
the elicitation says where to answer it, and a reply from Linear waits in the
session's queue like any message sent to a busy session. Permission prompts are answered in Arakun or on the phone, not in Linear.
*(My call: a permission is a security decision and stays on the user's devices.)*
While a session runs longer than 25 minutes without one of these, the computer
sends a short `thought` so Linear does not mark it stale.

Reports that fail to send are retried while the app runs, and dropped after a day.

### Order of work

Each step is its own pull request, stacked where it needs the one before:

1. The Linear tab (#939).
2. Worker: install, link, routes, webhook, deliveries, activities, with tests
   against a faked Linear (#940).
3. App: opt-in, mapping, deliveries, the new environment, reporting back (#940).

### What only Rik can do

- Register the "Arakun" app at Linear: name, icon, redirect URLs
  (`https://app.arakun.dev/linear/callback` and the loopback one), the webhook URL
  (`https://app.arakun.dev/linear/webhook`) with agent session events on.
- Put the four secrets on the production Worker and run the migration.

Until then the work is built and tested against a faked Linear, and the tab works
with a personal API key.

## Left to the builder

- GraphQL queries and their page sizes; the exact wording of activities and notices.
- The table columns, and whether refresh is serialized with a Durable Object or a
  D1 compare-and-set.
- Whether the user's own token can read a `prompted` activity's text. If it cannot,
  the Worker passes the text through to a connected computer without storing it,
  and answers "the computer is offline, send it again later" otherwise.
- Whether Linear accepts a loopback redirect on any port; if not, a fixed port or
  the API key only.
- The README section, `cloud/README.md` (routes, secrets, the `linear` control
  message) and `docs/windows.md` paths.
