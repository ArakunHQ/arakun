# Linear Delegation, App Setup: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In the Linear tab, a person turns on "Let Linear start sessions on this computer" and maps Linear teams and projects to Arakun projects; the computer then tells Arakun's server who they are in Linear and which teams and projects it takes.

**Architecture:** Four Worker calls in `core/src/cloud.rs`. A small store and pure rules in `src/linear/delegation.rs`. A third page in the Linear tab that only draws state and emits events. The shell owns everything the view cannot see (the Arakun account, the relay label, the project list) and does the syncing.

**Tech Stack:** Rust, GPUI, blocking `ureq` through `core/src/cloud.rs`, Linear's GraphQL API through `src/linear/mod.rs`.

**Spec:** `docs/superpowers/specs/2026-11-25-linear-integration-design.md`, "On the computer": Opt-in and Mapping. Worker contract: `cloud/README.md`, section "Linear". Issue: ARAKUN-188.

This is plan 3 of 4, stacked on the Worker (`pr/linear-worker`, #1030). Plan 4 (ARAKUN-124) takes deliveries and runs sessions.

## Global Constraints

- Build and test without the browser: `--no-default-features`.
- The switch is off by default. Nothing is sent to Arakun's server about Linear until a person turns it on.
- The user's Linear key leaves the computer only in `POST /linear/link`, only to the Arakun account's own base URL over https, and only as a direct result of turning the switch on. It is never logged.
- The page shows only for an Arakun account with the `linear` flag (`Me::has("linear")`). Without it the Linear tab looks as it does today.
- To turn a computer off, send empty routes (`PUT /linear/routes` with empty lists). Never `DELETE /linear/link` from the switch: that drops every computer's routes in the workspace.
- Only projects on this computer can be mapped (`!Project::is_remote()`), not projects on a server.
- State is in `<app_dir>/linear/delegation.json`, written with `durable::save_json`. It holds ids, names and paths, no tokens.
- UI takes colors, sizes and fonts from `theme` and `ui`; `design_lint` and `words_lint` must pass. Interface words: environment, session (an agent session only), project, notice. A Linear sign-in is a "workspace". A clickable `div` needs a role and a label.
- Tests live in `#[cfg(test)] mod tests` at the bottom of the file they test. Comments say why, in plain short sentences.
- No code behind the `team` or `coderlabs` features. Do not change `cloud/`.
- Commits follow Conventional Commits, scope `linear` (or `cloud` for `core/src/cloud.rs` only if the repo's history uses that for the client; otherwise `linear`).

## Copy (exact)

- Header button: `Start from Linear`
- Page title: `Start sessions from Linear`
- Switch label: `Let Linear start sessions on this computer`
- Under the switch, while off: `When you assign an issue to Arakun in Linear, a session starts here in the project you map. Turning this on sends your Linear key to Arakun once, to prove the Linear user is you. It is not stored there.`
- Blockers, shown in place of the switch's help when they apply, first match wins:
  1. `Sign in to Arakun first.`
  2. `Turn on "Reach this computer from anywhere" in the Phone tab. Linear reaches this computer through it.`
  3. `Arakun is not installed in {workspace} yet. A Linear admin can install it.` with a button `Open the install page`
- Empty mappings: `No mappings yet. Map a Linear team or project to a project on this computer.`
- Add row button: `Add a mapping`
- Broken row note: `This project is no longer in Arakun.`
- Sync line: `Up to date.` / `Sending…` / `Couldn't reach Arakun. It is tried again when this computer reconnects.` / `Too many requests. It is tried again in a moment.`

## Review Focus

1. **A mapped project is removed from Arakun, or moved.** The row shows as broken and its target is left out of what is sent; the others still go. Tests in Tasks 2 and 4.
2. **Signed out of Arakun, or the `linear` flag is lost, while the switch is on.** Nothing is sent, the page says why, and the saved switch and mappings are kept for when it works again. Test in Task 4.
3. **Two Linear workspaces signed in.** Each workspace's routes are sent on their own, and a mapping never leaks into the other workspace's call. Tests in Tasks 2 and 4.
4. **The server refuses or is unreachable** (409 link first, 404 no such computer, 429, offline, 401). One plain line, no retry loop, a new try on the next change or reconnect; a 401 ends the Arakun session as other account calls do. Test in Task 4.
5. **Switching off while offline.** The empty routes are still owed and are sent on the next reconnect, so the server never keeps routes for a computer that said no. Test in Task 4.

---

### Task 1: The Worker calls

**Files:**
- Modify: `core/src/cloud.rs`

**Interfaces:**
- Produces (all blocking, in the style of `join_beta` and `read_notice`; errors are the module's `Error`, with 401 as `Error::SignedOut`):
  - `pub struct LinearLink { pub organization_id: String, pub linear_user_id: String, pub installed: bool }` (serde)
  - `pub fn linear_links(base: &str, session: &str) -> Result<Vec<LinearLink>, Error>` (`GET /linear/link`, reads `links`)
  - `pub fn linear_link(base: &str, session: &str, linear_token: &str) -> Result<LinearLink, Error>` (`POST /linear/link` with `{ "token" }`)
  - `pub fn linear_put_routes(base: &str, session: &str, label: &str, organization_id: &str, teams: &[String], projects: &[String]) -> Result<(), Error>` (`PUT /linear/routes`)
  - A way for callers to tell these refusals apart: 409 (`link linear first`), 404 (`no such computer`), 429. If `Error` has no variant that carries the status, add `Error::Refused { status: u16, message: String }` and use it here only.

- [ ] **Step 1: Write the failing tests** next to the module's existing tests, with the local test server they use: each call sends the right method, path, bearer and JSON body (`linear_link`'s body is exactly `{"token":"lin_api_x"}`; `linear_put_routes`'s has `label`, `organization_id`, `teams`, `projects`); `linear_links` parses two links; a 401 is `SignedOut`; a 409 with `{"error":"link linear first"}` and a 404 are told apart; an `http://` base that is not loopback is refused before anything is sent (the module's existing rule).
- [ ] **Step 2: Run and see them fail.** `cargo test --locked --no-default-features -p arakun-core cloud::`
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same tests, then `cargo clippy --locked --all-targets --no-default-features -- -D warnings`.
- [ ] **Step 5: Commit** `feat(linear): call the server's link and routes endpoints`

---

### Task 2: What is saved, and the rules

**Files:**
- Create: `src/linear/delegation.rs`
- Modify: `src/linear/mod.rs` (`pub mod delegation;`), `src/linear/issues.rs` or a new `src/linear/places.rs` for the Linear read

**Interfaces:**
- Produces:
  - `pub enum Kind { Team, Project }` (serde `snake_case`)
  - `pub struct Mapping { pub workspace: String, pub kind: Kind, pub target: String, pub name: String, pub project: PathBuf }` (`workspace` is the Linear organization id, `target` the team or project id, `name` what Linear calls it, `project` the Arakun project's root)
  - `pub struct Delegation { pub enabled: bool, pub mappings: Vec<Mapping> }` with `Default` (off, empty), `pub fn load() -> Delegation`, `pub fn save(&self) -> Result<(), String>`, `pub fn load_from(file: &Path)`, `pub fn save_to(&self, file: &Path)`
  - `impl Delegation { pub fn add(&mut self, m: Mapping) -> bool` (false when the same workspace, kind and target is already mapped; a target maps to one project) `; pub fn remove(&mut self, workspace: &str, kind: Kind, target: &str); }`
  - `pub struct Routes { pub teams: Vec<String>, pub projects: Vec<String> }`
  - `pub fn routes(d: &Delegation, workspace: &str, local_projects: &[PathBuf]) -> Routes`: empty when `!d.enabled`; otherwise the targets of that workspace whose project is in `local_projects`, sorted, without duplicates.
  - `pub fn broken(m: &Mapping, local_projects: &[PathBuf]) -> bool`
  - `pub fn project_for<'a>(d: &'a Delegation, workspace: &str, team: &str, project: Option<&str>) -> Option<&'a Mapping>`: the project mapping wins over the team mapping (plan 4 uses it).
  - `pub struct Place { pub id: String, pub name: String }`, `pub struct Places { pub teams: Vec<Place>, pub projects: Vec<Place> }`, `pub fn places(s: &Session) -> Result<Places, ApiError>` (`query { teams(first: 250) { nodes { id name } } projects(first: 250) { nodes { id name } } }`, each list sorted by name), with a pure `parse_places(data: &Value) -> Places`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn off_sends_nothing() {
    let d = Delegation { enabled: false, mappings: vec![team("w1", "t1", "/p/a")] };
    assert_eq!(routes(&d, "w1", &[p("/p/a")]), Routes::default());
}
#[test]
fn only_this_workspace_and_only_projects_still_here() {
    let d = on(vec![team("w1", "t1", "/p/a"), project("w1", "x1", "/p/gone"), team("w2", "t9", "/p/a")]);
    let r = routes(&d, "w1", &[p("/p/a")]);
    assert_eq!((r.teams, r.projects), (vec!["t1".to_string()], vec![]));
    assert!(broken(&d.mappings[1], &[p("/p/a")]) && !broken(&d.mappings[0], &[p("/p/a")]));
}
#[test]
fn a_target_maps_to_one_project() {
    let mut d = Delegation::default();
    assert!(d.add(team("w1", "t1", "/p/a")));
    assert!(!d.add(team("w1", "t1", "/p/b")));
    assert!(d.add(project("w1", "t1", "/p/b"))); // another kind is another target
    d.remove("w1", Kind::Team, "t1");
    assert_eq!(d.mappings.len(), 1);
}
#[test]
fn a_project_mapping_wins_over_its_team() {
    let d = on(vec![team("w1", "t1", "/p/a"), project("w1", "x1", "/p/b")]);
    assert_eq!(project_for(&d, "w1", "t1", Some("x1")).unwrap().project, p("/p/b"));
    assert_eq!(project_for(&d, "w1", "t1", None).unwrap().project, p("/p/a"));
    assert!(project_for(&d, "w2", "t1", Some("x1")).is_none());
}
#[test]
fn it_survives_the_disk_and_a_missing_or_broken_file() {
    // save_to then load_from gives the same value; a missing file and a file of garbage give Default
}
#[test]
fn places_are_parsed_and_sorted() { /* two teams out of order, a node without an id is skipped */ }
```

- [ ] **Step 2: Run and see them fail.** `cargo test --locked --no-default-features linear::delegation`
- [ ] **Step 3: Implement.** The file is `<app_dir>/linear/delegation.json`, beside `sessions.json`.
- [ ] **Step 4: Run** the tests and clippy.
- [ ] **Step 5: Commit** `feat(linear): save the Linear opt-in and its mappings`

---

### Task 3: The page

**Files:**
- Create: `src/linear_view/delegation.rs`
- Modify: `src/linear_view.rs` (`Page::Delegation`, the header button, `render_body`, `Render`, `on_key`, new events and setters), `tools/linear-fixture.json` (a `teams(` reply)

**Interfaces:**
- Consumes: Task 2's types.
- Produces:
  - `pub struct Host { pub signed_in: bool, pub flagged: bool, pub relay_on: bool, pub projects: Vec<(String, PathBuf)>, pub install_url: String }` (what the shell tells the view; `projects` are the local ones, name and root)
  - `pub enum Sync { Idle, Sending, Failed, Limited }`
  - `LinearView::set_host(&mut self, host: Host, cx)`, `set_installed(&mut self, workspace: &str, installed: Option<bool>, cx)`, `set_sync(&mut self, sync: Sync, cx)`
  - New `LinearEvent` variants: `DelegationChanged` (the switch or a mapping changed and was saved), `OpenPhoneTab`, `SignInToArakun`; `Open(url)` is reused for the install page.
  - `pub fn blocker(host: &Host, installed: Option<bool>) -> Option<Blocker>` with `enum Blocker { SignIn, Relay, NotInstalled }`, first match wins in the order of "Copy". `installed == None` (not known yet) is not a blocker.
  - `pub fn can_turn_on(host: &Host) -> bool` (signed in and relay on; "not installed" does not block the switch, since linking still works)

Behaviour:
- The header button `Start from Linear` shows only when `host.flagged`; it toggles `Page::Delegation` as the Accounts button does for its page.
- The switch is disabled while `!can_turn_on(host)` and it is off; it can always be turned off.
- Rows show `{kind}  {name} → {project name}` with a remove button; a broken row shows the note and is drawn muted.
- `Add a mapping` opens two menus in turn: the Linear team or project (from `places`, read when the page opens, teams first), then the Arakun project (from `host.projects`). Choosing both adds the mapping, saves, and emits `DelegationChanged`.
- Every change saves `Delegation` before emitting.

- [ ] **Step 1: Write the failing tests** in `src/linear_view/delegation.rs`

```rust
#[test]
fn blockers_come_in_order() {
    let ok = Host { signed_in: true, flagged: true, relay_on: true, projects: vec![], install_url: String::new() };
    assert_eq!(blocker(&Host { signed_in: false, ..ok.clone() }, Some(false)), Some(Blocker::SignIn));
    assert_eq!(blocker(&Host { relay_on: false, ..ok.clone() }, Some(false)), Some(Blocker::Relay));
    assert_eq!(blocker(&ok, Some(false)), Some(Blocker::NotInstalled));
    assert_eq!(blocker(&ok, None), None);
    assert_eq!(blocker(&ok, Some(true)), None);
}
#[test]
fn the_switch_needs_an_account_and_the_relay_but_not_the_install() {
    // can_turn_on: false without sign-in, false without the relay, true otherwise
}
#[test]
fn rows_name_the_project_or_say_it_is_gone() {
    // row_text(&mapping, &host.projects) -> ("Team", "Eng", Some("arakun")) and (.., None) for a broken one
}
```

- [ ] **Step 2: Run and see them fail.** `cargo test --locked --no-default-features linear_view`
- [ ] **Step 3: Implement.** Follow `src/linear_view/accounts.rs` for the page's structure and the header toggle.
- [ ] **Step 4: Run** `linear`, `design_lint`, `words_lint` tests and clippy.
- [ ] **Step 5: Look at it** in a test Arakun with `ARAKUN_LINEAR_FIXTURE` (the `ui-change` skill has the commands). The shell is wired in Task 4, so for this step set a debug `Host` through the fixture: when the fixture is active, the view starts with `signed_in`, `flagged` and `relay_on` true and the open project in `projects`. Screenshot the page with the switch off, with it on and two mappings, and with a broken row.
- [ ] **Step 6: Commit** `feat(linear): add the page for starting sessions from Linear`

---

### Task 4: The shell keeps the server in step

**Files:**
- Create: `src/shell/linear_sync.rs`
- Modify: `src/shell/linear.rs` (events), `src/shell.rs` (`mod`, a field for the sync state), `src/shell/phone.rs` (call the sync when the relay's status becomes `Connected`, and tell the view when the relay goes on or off), where the shell learns the account changed (tell the view)

**Interfaces:**
- Consumes: Task 1's calls, Task 2's `Delegation`, `routes`, Task 3's setters; `self.teams.read(cx).account()` and `me()` (`Me::has("linear")`), `self.signed_in.own_label`, `self.projects`, `linear::sessions()`.
- Produces:
  - `pub(super) struct LinearSync { owed: bool, running: bool }`
  - `Shell::linear_host(&self, cx) -> linear_view::Host`
  - `Shell::sync_linear(&mut self, window, cx)`: the one entry point.
  - A pure planner, testable without GPUI: `pub fn plan(d: &Delegation, workspaces: &[(String /*org id*/, bool /*linked*/)], local: &[PathBuf]) -> Vec<Step>` with `enum Step { Link { workspace: String }, Routes { workspace: String, routes: Routes } }`. Rules: for each signed-in Linear workspace, when `d.enabled` and it has at least one sendable target and is not linked, a `Link` step comes before its `Routes` step; a workspace always gets a `Routes` step (empty when off or nothing sendable); a workspace with no link and nothing to send gets no step at all.
  - `pub fn outcome(e: &cloud::Error) -> Sync` (429 → `Limited`; anything else → `Failed`).

`sync_linear`: does nothing unless signed in to Arakun with the flag, and the relay label is known; then marks `owed`. If a sync is running it stops there. Otherwise, off the UI thread: `linear_links`, then each `Step` in order (`linear_link` with that workspace's key; `linear_put_routes`). On success clears `owed`, sets `Sync::Idle` and tells the view each workspace's `installed`. On a failure it stops, keeps `owed`, sets `outcome(e)`, and for `SignedOut` calls what other account calls use (`account_call_failed`). There is no timer: the next try is the next `DelegationChanged`, the next relay `Connected`, or the next time the account signs in.

- [ ] **Step 1: Write the failing tests** in `src/shell/linear_sync.rs`

```rust
#[test]
fn turning_on_links_then_sends_routes() {
    let d = on(vec![team("w1", "t1", "/p/a")]);
    assert_eq!(plan(&d, &[("w1".into(), false)], &[p("/p/a")]),
        [Step::Link { workspace: "w1".into() }, Step::Routes { workspace: "w1".into(), routes: teams(&["t1"]) }]);
}
#[test]
fn a_linked_workspace_is_not_linked_again() { /* linked = true → only Routes */ }
#[test]
fn off_sends_empty_routes_to_linked_workspaces_and_never_links() {
    let d = off(vec![team("w1", "t1", "/p/a")]);
    assert_eq!(plan(&d, &[("w1".into(), true), ("w2".into(), false)], &[p("/p/a")]),
        [Step::Routes { workspace: "w1".into(), routes: Routes::default() }]);
}
#[test]
fn each_workspace_gets_only_its_own_targets() { /* w1 and w2 mapped: two Routes steps, no mixing */ }
#[test]
fn a_broken_mapping_is_left_out_and_the_rest_still_goes() { /* one project gone: its target absent */ }
#[test]
fn nothing_sendable_and_no_link_is_no_step() { /* enabled, only broken mappings, not linked → [] */ }
#[test]
fn a_limit_reads_differently_from_a_failure() { /* outcome(429) == Limited; outcome(offline, 409, 404) == Failed */ }
```

- [ ] **Step 2: Run and see them fail.** `cargo test --locked --no-default-features linear_sync`
- [ ] **Step 3: Implement** the planner, then `sync_linear` after the pattern of `Shell::join_beta` (`src/shell/phone.rs`), then the wiring: `DelegationChanged` → `sync_linear`; relay `Connected` → `sync_linear`; account or relay state changes → `linear_view.set_host(self.linear_host(cx))`; `OpenPhoneTab` and `SignInToArakun` open what the Phone tab's own buttons open. Remove Task 3's debug `Host` for the fixture if the real host now covers it; keep a fixture path only if the test Arakun cannot be signed in.
- [ ] **Step 4: Run** `linear`, `linear_sync`, `design_lint`, `words_lint` tests and clippy.
- [ ] **Step 5: Try it against a local Worker.** `(cd cloud && npx wrangler dev)` with the four `LINEAR_*` secrets in `.dev.vars` and the migrations applied locally (cloud/README.md, "Development"), `ARAKUN_TEAMS_URL` pointed at it, an account with the `linear` flag. Turn the switch on with one mapping and check the `linear_links` and `linear_routes` rows in the local D1; turn it off and check the routes are gone. If the local Worker cannot be run or signed in to here, say exactly what stopped it and what was not checked.
- [ ] **Step 6: Commit** `feat(linear): tell the server which Linear teams this computer takes`

---

### Task 5: Docs and the pull request

- [ ] **Step 1: README.** In the Linear entry under "Service tabs": the page and its switch, what turning it on sends (the Linear key once, to prove who you are; then the ids of the mapped teams and projects), that nothing starts yet until the next release wires deliveries (say it plainly while that is true), where `delegation.json` is, and that only projects on this computer can be mapped. `docs/windows.md` gets the path.
- [ ] **Step 2: Checks.** `tools/rustfmt.sh --check`, clippy, `cargo test --locked --no-default-features linear`, `linear_sync`, `design_lint`, `words_lint`, and `cargo test --locked --no-default-features -p arakun-core cloud::`.
- [ ] **Step 3: Open the pull request** with the `ship-pr` skill: draft, base `pr/linear-worker` (or `main` once #1030 merged), title `feat(linear): opt a computer in to Linear and map teams to projects`, closing the GitHub issue synced from ARAKUN-188, screenshots from Task 3, and "Stacked on #1030." while it is.
