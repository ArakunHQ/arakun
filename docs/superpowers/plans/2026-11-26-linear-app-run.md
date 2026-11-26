# Linear Delegation, Running Sessions: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the relay hands this computer an issue that was delegated to Arakun in Linear, the app checks it against the switch and the mappings, makes an environment on the issue's branch, runs a session that ends in a draft pull request, takes follow-ups and stops, and tells the issue what happens.

**Architecture:** The relay client turns the `linear` control frame into `Incoming::Linear`. Pure rules and a small store live in `src/linear/` (the guard, the delegated prompt, the record of runs). `src/shell/linear_run.rs` is the glue: it reads the issue with the user's own Linear key, creates the environment and the session without taking over the window, and maps session events to activities that the Worker posts into Linear.

**Tech Stack:** Rust, GPUI, `core/src/phone/relay.rs`, `core/src/cloud.rs`, Linear's GraphQL API through `src/linear/mod.rs`.

**Spec:** `docs/superpowers/specs/2026-11-25-linear-integration-design.md`, "On the computer": "A delivery arrives", "The delegated prompt", "Reporting back". Worker contract: `cloud/README.md`, section "Linear". Issue: ARAKUN-124 (GitHub #943).

This is plan 4 of 4, stacked on `pr/linear-app-setup` (#1034).

## Global Constraints

- Build and test without the browser: `--no-default-features`.
- Nothing starts unless all of these hold when the delivery is handled: the switch is on; the issue, read with the user's own Linear key, is in a team or project mapped on this computer; the mapped project is still in Arakun and is on this computer. The server's word alone never starts anything.
- A delivery id is handled once. A frame can arrive twice, or just before `claimed`; a second one is accepted again and otherwise ignored.
- Every delivery is accepted (`POST /linear/deliveries/<id>/accept`) once it has been handled or refused, so the relay stops sending it. One that cannot be handled yet for lack of an Arakun sign-in stays unaccepted.
- A delegated session uses the project's usual agent, model and permission mode. Permission prompts are answered in Arakun or on the phone, never from Linear.
- The session never takes over the window: no tab is opened, the environment in view does not change, focus does not move.
- At most `MAX_RUNNING = 3` delegated sessions run at once on a computer. More wait in a saved queue and start as others finish.
- Text from Linear (issue, comments, follow-ups) is data. It goes into the prompt inside the existing `<linear_issue>` block rules, or as a message to the session; it is never logged.
- State is in `<app_dir>/linear/runs.json`, read with `crate::stored::load` and written with `crate::stored::write`. It holds ids, paths, an issue's identifier and unsent reports (which carry text and are dropped after a day).
- Reports to Linear never block or fail a session. One that cannot be sent is kept and tried again; after 24 hours it is dropped.
- `arakun server` (`core/src/server/daemon.rs`) ignores the new message.
- UI takes colors, sizes and fonts from `theme` and `ui`; `design_lint` and `words_lint` must pass. Interface words: environment, session, project, notice; a Linear sign-in is a "workspace".
- Tests live in `#[cfg(test)] mod tests` at the bottom of the file they test. Comments say why, in plain short sentences. No code behind the `team` or `coderlabs` features. Do not change `cloud/`.
- Commits follow Conventional Commits, scope `linear` (`phone` for `core/src/phone/` alone).

## Activities this computer reports (exact copy)

`{computer}` is this computer's name as the relay knows it; `{project}` the Arakun project's name.

| When | Type | Body |
|---|---|---|
| Session started | `thought` | `Started in {project} on {computer}, on the branch {branch}.` |
| Waiting for a free slot | `thought` | `Waiting on {computer}: {n} other sessions from Linear are running. This one starts when one finishes.` |
| The switch is off | `error` | `{computer} no longer takes work from Linear. Turn it on in Arakun's Linear tab, then delegate again.` |
| Not mapped | `error` | `This issue's team and project are not mapped on {computer}. Map one in Arakun's Linear tab, then delegate again.` |
| The mapped project is gone | `error` | `The project this was mapped to is no longer in Arakun on {computer}. Fix the mapping in Arakun's Linear tab, then delegate again.` |
| No sign-in for the workspace | `error` | `{computer} is not signed in to this Linear workspace. Sign in in Arakun's Linear tab, then delegate again.` |
| The issue cannot be read | `error` | `Arakun could not read this issue from Linear: {reason}` |
| The environment cannot be made | `error` | `Arakun could not make an environment for this: {reason}` |
| The session asks a question | `elicitation` | the question's text |
| The session waits for a permission | `elicitation` | `Waiting for your permission: {what}. Answer it in Arakun or on your phone.` |
| Finished | `response` | the last reply, cut at 7,500 characters with `…` and a new paragraph `The rest is in Arakun.`; the pull request as a link labelled `Pull request` when one is known |
| Failed | `error` | `The session failed: {reason}` |
| Stopped from Linear | `response` | `Stopped.` |
| A follow-up with no session here | `error` | `{computer} has no session for this any more. Delegate the issue again.` |
| A follow-up whose text cannot be read | `thought` | `Arakun could not read that message, because {computer} was offline when you sent it. Send it again.` |
| The app was closed mid-session | `error` | `Arakun was closed on {computer} while this ran. Reply here to continue.` |
| Still running, nothing reported for 25 minutes | `thought` | `Still working.` |

## Review Focus

1. **The same delivery twice, or a second one before the first finished starting.** One environment, one session, one "Started". Tests in Tasks 3 and 5.
2. **A delivery that must not start** (switch off, unmapped, project removed or on a server, no sign-in for the workspace, the issue unreadable). Nothing is created, the issue gets the matching line, the delivery is accepted, and routes are sent again. Tests in Tasks 3 and 5.
3. **Many deliveries at once** (a computer back after days). Three start, the rest wait in the saved queue and survive a restart; each waiting one is told once. Test in Task 3.
4. **The app quits or crashes mid-session.** On the next start the issue is told, the run is kept, and a reply from Linear resumes the session. Tests in Tasks 3 and 7.
5. **Reports that cannot be sent** (offline, rate limited, the Worker down). They are kept in order, sent later, dropped after a day, and never stop the session; a 4xx other than 429 drops that one report. Test in Task 3.
6. **A branch name from Linear that git will not take, or that is already checked out elsewhere.** The name is cleaned; an empty or unusable one becomes `linear/{identifier}` in lower case; an existing environment on that branch is reused. Test in Task 4.

---

### Task 1: The relay message reaches the app

**Files:**
- Create: `core/src/phone/linear.rs`
- Modify: `core/src/phone/mod.rs` (`pub mod linear;`, `Incoming::Linear`), `core/src/phone/relay.rs` (`heard`), `core/src/server/daemon.rs` (an arm that ignores it), `src/shell/phone.rs` (`phone_incoming`: an arm that calls `self.linear_delivery(d, window, cx)`, a stub until Task 5)

**Interfaces:**
- Produces, in `core/src/phone/linear.rs`:
  - `pub enum DeliveryKind { Created, Prompt, Stop }`
  - `pub struct Delivery { pub id: String, pub kind: DeliveryKind, pub agent_session: String, pub issue: String, pub organization: String, pub activity: Option<String>, pub body: Option<String> }` with a manual `Debug` that prints `body` as `Some(…)`/`None` only.
  - `pub fn parse(msg: &serde_json::Value) -> Option<Delivery>`: `id` must be 32 hex; `agent_session`, `issue` and `organization` non-empty and at most 64 characters; `kind` one of `created`, `prompt`, `stop`; `body` at most 16,000 characters, else `None`.
  - `Incoming::Linear(Delivery)`.

- [ ] **Step 1: Write the failing tests.** In `linear.rs`: a full frame parses; each missing or malformed field gives `None`; an oversized `body` is dropped but the delivery kept; `Debug` shows no body text. In `relay.rs`, after the pattern of `a_device_signed_out_elsewhere_is_forgotten_when_the_relay_says_so`: `tell(ws, json!({"type":"linear", …}))` gives `Incoming::Linear` with the same fields; a malformed one gives nothing and the connection stays up; a `linear` frame that arrives between `challenge` and `claimed` does not break the claim (it is dropped: the relay sends it again after `claimed`).
- [ ] **Step 2: Run and see them fail.** `cargo test --locked --no-default-features -p arakun-core phone::`
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same, then `cargo check --locked --no-default-features` and clippy.
- [ ] **Step 5: Commit** `feat(phone): pass the relay's Linear deliveries to the app`

---

### Task 2: Two more Worker calls

**Files:**
- Modify: `core/src/cloud.rs`

**Interfaces:**
- Produces:
  - `pub fn linear_accept(base: &str, session: &str, delivery_id: &str) -> Result<(), Error>` (`POST /linear/deliveries/<id>/accept`; 204)
  - `pub enum LinearActivity { Thought, Elicitation, Response, Error }` (wire: lower case)
  - `pub struct LinearLinkOut { pub label: String, pub url: String }`
  - `pub fn linear_activity(base: &str, session: &str, agent_session: &str, kind: LinearActivity, body: &str, links: &[LinearLinkOut]) -> Result<(), Error>` (`POST /linear/sessions/<id>/activity`; `links` left out of the JSON when empty). Errors as the other Linear calls: `Status { status, .. }` for 404 `no such session`, 400, 429, 502; `SignedOut` for 401.

- [ ] **Step 1: Write the failing tests** with the module's local test server: method, path, bearer and exact JSON for each; a 404, a 429 and a 502 are told apart; the body text is in the request body only.
- [ ] **Step 2: Run and see them fail.** `cargo test --locked --no-default-features -p arakun-core cloud::`
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests and clippy.
- [ ] **Step 5: Commit** `feat(linear): call the server's accept and activity endpoints`

---

### Task 3: The rules and the record of runs

**Files:**
- Create: `src/linear/runs.rs`
- Modify: `src/linear/delegation.rs` (the guard), `src/linear/prompt.rs` (the delegated prompt), `src/linear/mod.rs` (`pub mod runs;`, `session_for`)

**Interfaces:**
- Produces:
  - `linear::session_for(sessions: &[Session], organization: &str) -> Option<&Session>`: the first sign-in for that organization (the same rule the sync's `keys` uses; make `keys` call it).
  - `delegation::Refusal { Off, Unmapped, ProjectGone }` and `delegation::target_for<'a>(d: &'a Delegation, workspace: &str, team: &str, project: Option<&str>, local: &[PathBuf]) -> Result<&'a Mapping, Refusal>`: `Off` when `!d.enabled`; the project mapping wins over the team's; `ProjectGone` when the winning mapping's project is not in `local`.
  - `prompt::delegated(d: &Detail) -> String`: the same data block and rules as `prompt::work`, with this ending in place of its last two paragraphs:
    ```text
    Find what it asks for in the code and make the change in this checkout. Run the tests that cover it, or say why you could not.

    Nobody may be at this computer, so finish the job: commit on this branch, push it, and open a draft pull request that names {identifier} in its description. Follow this repository's own rules for commits and pull requests. Then say what you changed, what is left, and give the pull request's link.

    Don't change the issue in Linear and don't comment on it: Arakun reports there for you.
    ```
  - `prompt::branch(d: &Detail) -> String`: Linear's `branchName`, cleaned for git (only `A-Z a-z 0-9 . _ - /`; no leading `-` or `/`, no `..`, no trailing `/` or `.lock`, at most 100 characters); if that leaves nothing, `linear/{identifier in lower case}`.
  - In `src/linear/runs.rs`:
    - `pub const MAX_RUNNING: usize = 3;` `pub const HANDLED_KEPT: usize = 500;` `pub const REPORT_KEPT_SECS: u64 = 24 * 3600;` `pub const QUIET_SECS: u64 = 25 * 60;`
    - `pub enum RunState { Waiting, Running, Asked, Done, Interrupted }`
    - `pub struct Run { pub agent_session: String, pub organization: String, pub issue: String, pub identifier: String, pub project: PathBuf, pub environment: Option<PathBuf>, pub branch: String, pub session: Option<String> /* Arakun session id */, pub state: RunState, pub reported_at: u64 }`
    - `pub struct Report { pub agent_session: String, pub kind: Kind /* thought, elicitation, response, error */, pub body: String, pub link: Option<String>, pub made_at: u64 }`
    - `pub struct Runs { handled: Vec<String>, runs: Vec<Run>, outbox: Vec<Report> }` with `load()`, `save()`, `load_from`, `save_to` (`stored`), and:
      - `pub fn seen(&mut self, delivery_id: &str) -> bool`: true if already handled; otherwise records it (keeping the newest `HANDLED_KEPT`).
      - `pub fn add(&mut self, run: Run) -> Added` where `enum Added { Start, Wait { ahead: usize }, Already }`: `Already` when a run for that `agent_session` exists; `Wait` when `MAX_RUNNING` runs are `Running` or `Asked`.
      - `pub fn next_waiting(&mut self) -> Option<&mut Run>`: the oldest `Waiting` run when a slot is free.
      - `pub fn by_agent_session(&mut self, id: &str) -> Option<&mut Run>`, `pub fn by_session(&mut self, arakun_session: &str) -> Option<&mut Run>`
      - `pub fn interrupted(&mut self, live: &dyn Fn(&str) -> bool) -> Vec<String>`: every `Running` or `Asked` run whose Arakun session is not live becomes `Interrupted`; returns their agent sessions.
      - `pub fn quiet(&self, now: u64) -> Vec<String>`: `Running` runs with `now - reported_at >= QUIET_SECS`.
      - `pub fn queue(&mut self, r: Report)`, `pub fn due(&mut self, now: u64) -> Vec<Report>` (drops ones older than `REPORT_KEPT_SECS`, returns the rest oldest first without removing them), `pub fn sent(&mut self, r: &Report)`, `pub fn dropped(&mut self, r: &Report)`
    - `pub fn cut(reply: &str) -> String`: the 7,500-character rule of the table.

- [ ] **Step 1: Write the failing tests.** One test per rule, named after it:
  - `target_for`: off; unmapped; the project mapping wins; a mapping from another workspace never matches; project gone.
  - `delegated`: it has the data block and the three closing paragraphs with the identifier; it does not contain `Don't commit or push`; a hostile title still cannot add a line (reuse the existing helper).
  - `branch`: `rik/eng-412-fix tab` → `rik/eng-412-fix-tab`; `--force`, `../x`, `a..b`, `x.lock`, `` (empty) and a 300-character name each give a name git accepts (`git check-ref-format --branch` semantics, asserted by a small pure validator in the test); empty → `linear/eng-412`.
  - `runs`: `seen` is false then true, and forgets beyond 500; three `add` are `Start`, the fourth is `Wait { ahead: 3 }`, the same agent session again is `Already`; when one becomes `Done`, `next_waiting` gives the oldest waiting; `interrupted` marks only runs whose session is not live; `quiet` at 24 minutes is empty and at 25 has the run; `due` keeps order, drops a report made 24 hours and one second ago, and `sent` removes exactly one; the store survives the disk, and a damaged file reads as empty and is kept aside.
  - `cut`: a short reply is unchanged; 7,501 characters are cut to 7,500 plus `…` and the paragraph; a multi-byte reply is cut on a character.
- [ ] **Step 2: Run and see them fail.** `cargo test --locked --no-default-features linear::`
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests and clippy.
- [ ] **Step 5: Commit** `feat(linear): add the rules and the record for sessions started from Linear`

---

### Task 4: An environment on a given branch, without taking the window

**Files:**
- Modify: `src/shell.rs` (or a new `src/shell/linear_env.rs` if `shell.rs` would grow by more than about 80 lines)

**Interfaces:**
- Produces: `Shell::env_on_branch(&mut self, project: &Path, branch: &str, window: &mut Window, cx: &mut Context<Self>, then: impl FnOnce(&mut Shell, Result<PathBuf, String>, &mut Window, &mut Context<Shell>) + 'static)`:
  - the project must be in `self.projects` and not remote, else `Err("the project is not on this computer")`;
  - if an environment of the project is already on `branch` (by `Environment.branch`, then by `git::worktrees`), `then` gets its path at once;
  - otherwise a worktree is added off the UI thread with `git::add_worktree(repo, path, branch, base, only_new = false)` (base as `quick_create_env_then` picks it; a branch that exists only on the remote uses `git::add_branch_worktree`), the `Environment` is added to the project without `select_env` and without focusing anything, and the project's setup script runs as for any new environment;
  - two calls for the same project and branch while the first is still creating share one creation.
  - A pure helper `fn existing<'a>(envs: &'a [Environment], branch: &str) -> Option<&'a Environment>`.

- [ ] **Step 1: Write the failing tests** for the pure parts: `existing` finds by branch and ignores another branch; the in-flight key `(project, branch)` dedupes. If the repo has a helper to make a throwaway git repository in tests (look at `core/src/git.rs` tests), add a test in `core/src/git.rs` that `add_worktree(.., only_new = false)` checks out an existing local branch and creates a missing one from the base.
- [ ] **Step 2: Run and see them fail.**
- [ ] **Step 3: Implement.** Read `quick_create_env_then`, `environment_created` and `check_out_branch` first; reuse their steps, leaving out selection and focus.
- [ ] **Step 4: Run** the tests, `design_lint`, clippy.
- [ ] **Step 5: Look at it.** In a test Arakun, call it once through a temporary debug command or test hook (not committed) for a branch that does not exist: the environment appears in the sidebar under its project, the view and focus stay where they were. Say what was seen.
- [ ] **Step 6: Commit** `feat(linear): make an environment on a named branch in the background`

---

### Task 5: Taking a delegated issue

**Files:**
- Create: `src/shell/linear_run.rs`
- Modify: `src/shell.rs` (`mod`, a `linear_runs: Runs` field loaded at start), `src/shell/phone.rs` (the stub of Task 1 calls the real thing), `src/shell/linear_sync.rs` (share `may_sync`)

**Interfaces:**
- Consumes: Tasks 1 to 4; `cloud::linear_accept`, `cloud::linear_activity`; `issues::detail(session, issue_id)`; `Shell::start_session_with(env, agent, title, text, show = false, …)`; `may_sync`.
- Produces:
  - `Shell::linear_delivery(&mut self, d: Delivery, window, cx)`: the entry point for all three kinds (Task 6 fills in `Prompt` and `Stop`).
  - `Shell::linear_report(&mut self, agent_session: &str, kind, body: String, link: Option<String>, cx)`: queues a `Report`, saves, and flushes the outbox off the UI thread (in order; stops at the first transport error or 429; a 4xx other than 429 drops that report; a 401 goes to `account_call_failed`). Flushing also runs on every relay `Connected`.
  - A pure `fn refusal_line(r: &Refusal, computer: &str) -> String` and `fn started_line(project: &str, computer: &str, branch: &str) -> String` with the table's copy.

Order for a `Created` delivery:
1. No Arakun account, flag or label (`may_sync` is `None`): do nothing; the relay sends it again later.
2. `runs.seen(id)`: accept again, stop.
3. No sign-in for `d.organization`: report the "not signed in" error, accept, stop.
4. Off the UI thread, `issues::detail`. Unreadable: report it with Linear's message, accept, stop.
5. `target_for(delegation, organization, team, project, local projects)`. A refusal: report its line, accept, call `sync_linear`, stop.
6. `runs.add(run)`: `Already` → accept, stop. `Wait { ahead }` → report the waiting line, accept, stop. `Start` → step 7.
7. `env_on_branch(mapping.project, prompt::branch(&detail), …)`. An error: report it, mark the run `Done`, accept, start the next waiting run.
8. `start_session_with(env, the project's agent, prompt::title(&detail), prompt::delegated(&detail), show = false, …)`; record the session id and environment on the run, state `Running`; report the started line; accept.
When a run reaches `Done`, `next_waiting` is started from step 7 (its issue is read again, and the guard is checked again).

- [ ] **Step 1: Write the failing tests** for the pure pieces: every `Refusal` gives its exact line; the started line; a decision function `fn first_step(seen: bool, gate: bool, signed_in_workspace: bool) -> First { Ignore, AcceptOnly, RefuseNoSignIn, Read }` covering the four combinations in the order above. Against the scripted HTTP server from `linear_sync.rs`'s tests: the outbox flush sends in order, stops at a 429 and keeps the rest, drops a report on a 404, and sends nothing when signed out.
- [ ] **Step 2: Run and see them fail.** `cargo test --locked --no-default-features linear`
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `linear`, `design_lint`, `words_lint` and clippy.
- [ ] **Step 5: Try it.** There is no real Linear delegation to test with, so drive it from the app's side: in a test Arakun with the Linear fixture (which answers `issue(` with a mapped team) and a fixture account, feed one `Delivery` through a debug hook that is compiled only for a test Arakun (`crate::dev::size()`), with the Worker calls pointed at a local scripted server or a local `wrangler dev`. Expected: a new environment on the issue's branch appears under the mapped project, a session starts there with the delegated prompt, the view does not move, and the server sees `accept` and a `thought` with the started line. Feed the same delivery again: nothing new starts. Say exactly what could and could not be driven this way.
- [ ] **Step 6: Commit** `feat(linear): start a session for an issue delegated in Linear`

---

### Task 6: Follow-ups and stops

**Files:**
- Modify: `src/shell/linear_run.rs`, `src/linear/issues.rs` (reading a follow-up's text)

**Interfaces:**
- Produces: `issues::prompt_text(s: &Session, activity_id: &str) -> Result<String, ApiError>` (the body of an agent activity, read with the user's key; check the query against Linear's schema: `agentActivity(id:) { content { ... on AgentActivityPromptContent { body } } }`. If the user's key cannot read it, the function returns the API's error and the caller reports the "could not read that message" line.)

For a `Prompt` delivery, after the same first three steps as `Created`: find the run by `agent_session` (none → the "no session for this any more" error, accept). The text is `d.body`, else `prompt_text`. Unreadable → the "could not read" thought, accept. Otherwise make the session live (`ensure_live`), `send(text)`, set the run `Running`, accept. A message sent while the session is busy waits in the session's queue, as from the composer.
For a `Stop`: find the run; cancel the session's turn; report `Stopped.`; mark the run `Done`; accept; start the next waiting run. A stop for a `Waiting` run removes it from the queue with the same report.

- [ ] **Step 1: Write the failing tests** for what can be pure: `fn follow_up(run: Option<&Run>, text: Option<&str>) -> FollowUp { NoRun, Unreadable, Send }`; a stop of a waiting run leaves the others' order; `parse_prompt_text` over the GraphQL answer, including a content of another type.
- [ ] **Step 2: Run and see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests and clippy.
- [ ] **Step 5: Try it** with the hook of Task 5: a `Prompt` with a body reaches the running session as a message; a `Stop` cancels it and the server sees `Stopped.`
- [ ] **Step 6: Commit** `feat(linear): pass a reply or a stop from Linear to the session`

---

### Task 7: Telling Linear what happens

**Files:**
- Modify: `src/shell/linear_run.rs`, `src/shell/sessions.rs` (call `self.linear_thread_event(env, id, thread, ev, cx)` where `notify_desktop` is called for a thread event)

**Interfaces:**
- Produces:
  - A pure `fn to_report(ev: Seen, run: &Run) -> Option<(Kind, String)>` where `enum Seen { Question(String), Permission(String), Finished { reply: String }, Failed(String), Cancelled }`, using the table's copy; `Cancelled` gives nothing (a stop from Linear already reported; a cancel in Arakun says nothing).
  - `Shell::linear_thread_event(…)`: for a session that belongs to a run, turns the event into a `Seen` the way `notify_desktop` tells them apart (`pending_question` / `pending_permission`, `Finished { stop_reason }`, `ThreadStatus::Error`), reports it, and updates the run (`Asked`, `Done`). A question or permission is reported once per pending ask. On `Finished` it looks up the pull request (`SessionMeta.pr`, else the CI view's `pr_of(branch)` for the run's project) and passes its URL as the link; then it starts the next waiting run.
  - At start-up, after sessions are loaded: `runs.interrupted(…)` and the "closed mid-session" error for each. A later `Prompt` for an `Interrupted` run resumes it through Task 6's path.
  - A timer every 5 minutes while any run is `Running`: `runs.quiet(now)` → `Still working.`; it also flushes the outbox. No timer runs when no run is `Running` and the outbox is empty.

- [ ] **Step 1: Write the failing tests:** `to_report` for each `Seen` with the exact copy; a finished reply over the limit is cut; `Cancelled` is `None`; a `Permission` carries the "answer it in Arakun" line and not the permission's raw arguments.
- [ ] **Step 2: Run and see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `linear`, `design_lint`, `words_lint` and clippy.
- [ ] **Step 5: Try it** with the hook of Task 5 and a scripted or fixture agent if the test Arakun has one (see how `arakun dev` drives a session in the `bench` and `ui-change` skills): a session that finishes makes the server see a `response`; quitting the test Arakun mid-session and starting it again makes it see the "closed" error. Say what could not be driven.
- [ ] **Step 6: Commit** `feat(linear): report a delegated session's progress to its issue`

---

### Task 8: The page tells the truth, and the sidebar shows where a session came from

**Files:**
- Modify: `src/linear_view/delegation.rs`, `src/linear_view.rs`, `src/shell/linear_sync.rs`, `src/model.rs` and `src/state.rs` (a `linear: Option<SharedString>` on a session, saved like `ci_run`), the sidebar's session row

- [ ] **Step 1: Tests first** for each pure change:
  - the "Not active yet" line is gone from the page (the test that pinned it is removed with it);
  - `plan` also emits an empty `Routes` step for a server link whose workspace has no sign-in on this computer (new test), since `PUT /linear/routes` needs no key;
  - the view's `installed` map is replaced from each sync's answer and cleared when the Arakun account changes, through a pure `fn installed_after(old, answer, account_changed) -> HashMap<..>` (new test: a link that went away is forgotten, so the connect card shows again);
  - a session started from Linear carries the issue's identifier, survives `state.json`, and a `TitleChanged` does not remove it.
- [ ] **Step 2: Implement.** The session row shows the identifier as a small muted prefix before the title, with the tokens the row already uses.
- [ ] **Step 3: Run** `linear`, `design_lint`, `words_lint`, the state tests and clippy. Screenshot the page and a sidebar row with the marker in a test Arakun.
- [ ] **Step 4: Commit** `feat(linear): mark sessions started from Linear and tidy the opt-in page`

---

### Task 9: Docs and the pull request

- [ ] **Step 1: README.** Rewrite the Linear entry's delegation paragraph: what happens when an issue is delegated (the checks, the environment on the issue's branch, the session ending in a draft pull request, at most three at once), what the issue is told, how to answer a question or stop from Linear, that permissions are answered in Arakun or on the phone, what happens when the computer is off or the app is closed, what `runs.json` holds and where it is; remove "Nothing starts yet". `docs/windows.md` gets the path. In `cloud/README.md` nothing changes.
- [ ] **Step 2: Deploy order, written down.** In the README and on ARAKUN-126: the Linear secrets go on the production Worker only after an app release with this pull request is out, so no delivery waits for an app that cannot take it.
- [ ] **Step 3: Checks.** `tools/rustfmt.sh --check`, clippy, `cargo test --locked --no-default-features linear`, `-p arakun-core phone::`, `-p arakun-core cloud::`, `design_lint`, `words_lint`.
- [ ] **Step 4: Open the pull request** with the `ship-pr` skill: draft, base `pr/linear-app-setup` (or `main` once #1034 merged), title `feat(linear): start a session when an issue is delegated to Arakun in Linear`, `Closes #943` and `Closes #940`, screenshots from Task 8, what was and was not driven end to end, and "Stacked on #1034." while it is.
