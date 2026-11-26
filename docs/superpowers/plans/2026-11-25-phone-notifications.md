# Better Phone Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phone notices say what happened, go out only when the person is away from the computer and the turn was long enough, and can be set and muted from every phone client.

**Architecture:** A new `core/src/phone/notify.rs` holds the notice text and a `Notifier`: the pending queue plus the away, shortest-turn and mute rules. The desktop app (`src/shell`) and `arakun-server` (`core/src/server/daemon.rs`) both feed it. Idle time comes from the host (`core/src/host.rs`). The app measures it per platform and sends it to the servers it follows.

**Tech Stack:** Rust (GPUI, serde, x11rb, zbus), TypeScript (phone page), Swift (ArakunKit, iOS), Kotlin (android/kit, Compose).

**Spec:** `docs/superpowers/specs/2026-11-25-phone-notifications-design.md` · Linear ARAKUN-119

## Global Constraints

- Defaults: away time `away_after_secs = 120`; shortest turn `min_turn_secs = 30`. `0` turns each rule off.
- Away-time choices: Always (0), 1 min, 2 min, 5 min, 15 min. Shortest-turn choices: Off (0), 15 s, 30 s, 1 min, 5 min.
- Labels: "Only when I'm away for" and "Turns shorter than this stay quiet". `src/words_lint.rs` must pass.
- New fields are `#[serde(default)]` in Rust and optional in every client decoder. Older apps and stored files keep working.
- A notice stays `≤ MAX_NOTICE` (2600 bytes) and end-to-end sealed. `cloud/` does not change.
- The pending queue is checked about every 15 s. A server trusts idle time it heard from an app for 60 s. The app sends idle time on change and at least every 30 s.
- At the computer: `done` and `error` are dropped, and `input` waits until the person is away, as long as the agent is still waiting.
- `ios/ArakunKit` and `android/kit` match `core/src/phone` and `phone/` on the wire.
- Per AGENTS.md: Conventional Commits; one draft pull request per part below, each with `Refs ARAKUN-119` (the last one `Closes`); run only the quick checks for what changed; rebuild and commit `phone/dist` when `phone/` changes. Code for one platform goes behind `cfg`. No changes behind `team` or `coderlabs`.
- Rust check per task: `tools/rustfmt.sh --check && cargo clippy --locked --all-targets --no-default-features -- -D warnings`.

## Review Focus

1. **A permission request answered on the computer while its notice waits.** It must never reach the phone. Test: `tick_drops_input_no_longer_waiting` (Task 4).
2. **A stored device file from before this change** (no `away_after_secs`, `min_turn_secs` or `muted`). It loads with 120, 30 and an empty list. Tests: `notices_default_when_missing` (Task 4) and `device_without_muted_loads` (Task 8).
3. **A last agent message that is only Markdown, a code block, or very long multibyte text.** The body falls back to "Finished its turn." or is cut to fit `MAX_NOTICE`. Test: `finished_body_falls_back_and_fits` (Task 1).
4. **Away time 0 while the person is active.** The notice goes out at once, as it does today. Test: `away_zero_sends_now` (Task 4).
5. **An older client gets a notice with `env` and `secs`, or a new client gets one without them.** Both decode; there's no subtitle when `env` is missing. Tests in Task 3 on each client.

---

## Part 1 — Richer content (pull request `feat(phone): say what happened in each notice`)

### Task 1: Notice text in core

**Files:**
- Create: `core/src/phone/notify.rs` (and `pub mod notify;` in `core/src/phone/mod.rs`)
- Modify: `core/src/phone/push.rs` (`notice()`)

**Interfaces:**
- Produces:
  - `pub enum Kind { Done, Input, Error }` with `fn as_str(self) -> &'static str` returning `"done"`, `"input"` or `"error"`.
  - `pub fn finished_body(entries: &[Entry]) -> String`
  - `pub fn permission_body(p: &PermissionRequest, tool: Option<&ToolCall>) -> String`
  - `pub fn question_body(q: &AskUserRequest) -> String`
  - `pub fn turn_secs(entries: &[Entry]) -> Option<u64>`
  - `push::notice(kind: &str, session: &str, title: &str, text: &str, env: &str, secs: Option<u64>) -> String`. It adds `"env"` (left out when empty) and `"secs"` (left out when `None`).

- [ ] **Step 1: Write failing tests** in `notify.rs` `mod tests`:
  - `finished_body_takes_last_agent_text`: entries `[User, Agent{"**Fixed** the `login` bug.\n\nSee [PR](http://x)."}]` gives `"Fixed the login bug. See PR."`.
  - `finished_body_falls_back_and_fits`: no agent entry, or one that is only a code block, gives `"Finished its turn."`. 5000 `"é"` gives a notice with `push::notice(...).len() <= MAX_NOTICE`.
  - `permission_body_shows_subject`:
    - kind `execute` with `raw_input {"command":"cargo test"}` gives `"Run: cargo test"`, where the title is "Run";
    - kind `edit` with `locations ["/a/b.rs"]` gives `"<title>: /a/b.rs"`;
    - kind `fetch` with `raw_input {"url":"https://x"}` gives `"<title>: https://x"`;
    - with a plan, gives `"Plan ready: "` plus the plan's first 3 non-empty lines joined by `" / "`.
  - `question_body_first_and_more`: two questions give `"<first question> (+1 more)"`.
  - `turn_secs_from_last_user`: the last `User{elapsed_ms: Some(65_400)}` gives `Some(65)`.
  - In `push.rs`, extend the existing notice test: `"env":"app"` and `"secs":65` are present; both are absent when empty or `None`.
- [ ] **Step 2: Run** `cargo test --locked --no-default-features -p arakun-core phone::` and expect FAIL (the items aren't defined).
- [ ] **Step 3: Implement.** Plain text: drop fenced code blocks, turn `[text](url)` into `text`, strip `#`, `*`, `_` and backticks, and collapse whitespace. Fall back when the result is empty. Fit the size with the existing `shorten` loop.
- [ ] **Step 4: Run** the same command and expect PASS.
- [ ] **Step 5: Commit** `feat(phone): build notice text from the session`.

### Task 2: Both senders use it

**Files:**
- Modify: `src/shell/sessions.rs:2119-2180` (the fixed strings), `src/shell/phone.rs` (`notify_phones` takes `env: &str` and `secs: Option<u64>`), `core/src/server/daemon.rs` around line 3800 (same)

**Interfaces:**
- Consumes: Task 1.
- `src/shell/sessions.rs` drops its own `Notice` enum in favor of `notify::Kind`.
- `env` is the environment's display name. In the daemon it's the place name from `self.store`.

- [ ] **Step 1: Write a failing test** in `daemon.rs` tests: a finished session whose last agent text is "Done: 3 files" gives a payload whose `body` is "Done: 3 files" and whose `env` is the place name. Base it on the existing notice tests in that file.
- [ ] **Step 2: Run** `cargo test --locked --no-default-features -p arakun-core server::daemon` and expect FAIL.
- [ ] **Step 3: Implement** in both callers. Find the permission's tool by `tool_call_id` among `Entry::Tool`.
- [ ] **Step 4: Run** the daemon tests and `cargo test --locked --no-default-features shell::` and expect PASS.
- [ ] **Step 5: Commit** `feat(phone): send the agent's words, the command and the question`.

### Task 3: Clients show the environment and turn length

**Files:**
- Modify: `phone/public/sw.js:24`
- Modify: `ios/ArakunKit/Sources/ArakunKit/WebPush.swift` (the notice type gains `env: String`, `secs: Int?`)
- Modify: `ios/Notifications/NotificationService.swift`
- Modify: `android/kit/src/main/kotlin/com/arakun/kit/WebPush.kt:180` (`Notice` gains `env: String = ""`, `secs: Long? = null`)
- Modify: `android/app/src/main/java/com/arakun/android/Push.kt` (`show`)
- Tests: `phone/src/lib/push.test.ts` (or a new sw test), `ios/ArakunKit/Tests`, `android/kit/src/test`

**Interfaces:**
- One subtitle on every client: `env`, plus `" · 4m 12s"` when `secs ≥ 60`, or `" · 42s"` when it's shorter. When `env` is empty, there's no subtitle.
  - iOS: `content.subtitle`
  - Android: `setSubText`
  - web: put it first in the body, on its own line

- [ ] **Step 1: Write failing tests**, one per client:
  - A notice with `env:"app", secs:252` gives the subtitle `"app · 4m 12s"`.
  - A notice without either decodes with an empty subtitle.
  - The old `{"kind","session","title","body"}` notice still decodes.
- [ ] **Step 2: Run** `(cd phone && npm test)`, `(cd ios/ArakunKit && swift test)` (macOS) and `(cd android && ./gradlew :kit:test)`, and expect FAIL.
- [ ] **Step 3: Implement** a shared `subtitle(env:secs:)` helper per client.
- [ ] **Step 4: Run** the same commands and expect PASS. Run `(cd phone && npm run build)` and commit `phone/dist`.
- [ ] **Step 5: Commit** `feat(phone): show the environment and turn length on notices`. Open Part 1's draft pull request.

## Part 2 — Away rule (pull request `feat(phone): notify only when away from the computer`)

### Task 4: The Notifier

**Files:**
- Modify: `core/src/phone/notify.rs`
- Modify: `core/src/phone/mod.rs` (`Notices` gains `away_after_secs: u32` and `min_turn_secs: u32`, with defaults 120 and 30, and `POST /push` reads both as optional `u32`s)

**Interfaces:**
- Produces:
```rust
pub struct Candidate { pub session: String, pub kind: Kind, pub turn_secs: Option<u64>, pub payload: String }
pub struct Presence { pub idle: Option<Duration>, pub focused: bool }
pub struct Send { pub device: String, pub sub: push::Subscription, pub payload: String }
#[derive(Default)] pub struct Notifier { /* pending: one per (device, session) */ }
impl Notifier {
    pub fn offer(&mut self, c: Candidate, devices: &[Device], looking: &HashSet<String>, p: Presence, now: Instant) -> Vec<Send>;
    pub fn tick(&mut self, devices: &[Device], looking: &HashSet<String>, p: Presence, still: impl Fn(&str, Kind) -> bool, now: Instant) -> Vec<Send>;
    /// The person opened the session on the computer.
    pub fn seen(&mut self, session: &str);
    pub fn has_pending(&self) -> bool;
}
```
- Rules, per device:
  - Skip when the kind's switch is off, the device has no subscription, or it is in `looking`.
  - Skip a `done` with `turn_secs < min_turn_secs`. `None` counts as long.
  - When `away_after_secs == 0`, send now.
  - When `idle` is known: if `idle ≥ away`, send now. Otherwise drop `done` and `error`, and queue `input`.
  - When `idle` is unknown: queue everything. In `tick`, send once `now - made ≥ away` and `!focused`.
  - In `tick`: when `idle` is known, send queued `input` once `idle ≥ away`. Drop any pending notice whose `still(session, kind)` is false. `seen` drops the session's pending notices.
  - A newer candidate for the same session and device replaces the older one.

- [ ] **Step 1: Write failing tests**:
  - `away_zero_sends_now`
  - `idle_known_away_sends`
  - `idle_known_present_drops_done_and_error`
  - `idle_known_present_queues_input_until_away`
  - `tick_drops_input_no_longer_waiting`
  - `idle_unknown_waits_then_sends_if_unfocused`
  - `idle_unknown_focused_keeps_waiting`
  - `seen_drops_pending`
  - `short_turn_done_dropped_input_kept`
  - `newer_replaces_pending`
  - `looking_device_skipped`
  - `notices_default_when_missing`: old JSON `{"done":true,"input":true,"error":true}` gives 120 and 30.
  - `POST /push` with `{"notices":{"away_after_secs":0}}` stores 0 and keeps the others. Extend the tests near `core/src/phone/mod.rs:2756`.
- [ ] **Step 2: Run** `cargo test --locked --no-default-features -p arakun-core phone::` and expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same command and expect PASS.
- [ ] **Step 5: Commit** `feat(phone): hold notices until the person is away`.

### Task 5: Idle time from the host

**Files:**
- Modify: `core/src/host.rs`: `pub idle_for: fn() -> Option<Duration>`, default `|| None`
- Create: `src/idle.rs`: `pub fn idle_for() -> Option<Duration>`, set in `main` where `host::set` is called
- Modify: `Cargo.toml`:
  - `windows-sys` features `Win32_UI_Input_KeyboardAndMouse` and `Win32_System_SystemInformation`;
  - `x11rb` feature `screensaver`;
  - `zbus` (Linux only, at the version already in `Cargo.lock`). Commit `Cargo.lock`.

**Implementation per platform:**
- macOS: `CGEventSourceSecondsSinceLastEventType(kCGEventSourceStateCombinedSessionState = 0, kCGAnyInputEventType = !0)`, through `#[link(name = "CoreGraphics", kind = "framework")]`.
- Windows: `GetLastInputInfo` against `GetTickCount64`.
- Linux:
  - On X11 (`DISPLAY` set, no `WAYLAND_DISPLAY`): `screensaver::query_info(root).ms_since_user_input`.
  - On Wayland: `org.gnome.Mutter.IdleMonitor` `/org/gnome/Mutter/IdleMonitor/Core` `GetIdletime` (ms) on the session bus. Then try `org.freedesktop.ScreenSaver` `GetSessionIdleTime` (s), and return `None` when neither answers.
  - Cache the D-Bus connection, and cache a "no answer" result for the life of the process.

- [ ] **Step 1: Write a failing test** in `src/idle.rs`: `ms_to_duration_saturates` for the Windows tick math (input later than now gives 0). That's a pure helper, `fn since(now_ms: u64, last_ms: u64) -> Duration`.
- [ ] **Step 2: Run** `cargo test --locked --no-default-features idle::` and expect FAIL.
- [ ] **Step 3: Implement** per platform behind `cfg`.
- [ ] **Step 4: Run** the test and expect PASS. Run the Rust check. Check on Linux by hand: `cargo run --no-default-features -- dev` prints nothing new; add a temporary `eprintln!` of `idle_for()`, see it grow while you don't touch the machine, then remove the print.
- [ ] **Step 5: Commit** `feat(phone): read how long the computer has been idle`.

### Task 6: Wire the Notifier into both senders

**Files:**
- Modify: `src/shell/phone.rs` (`notify_phones` hands a `Candidate` to a `Notifier` kept on the phone link; a 15 s timer runs while `has_pending()`)
- Modify: `src/shell/sessions.rs:2174` (drop the `window.is_window_active()` gate; `Presence { idle: (host::get().idle_for)(), focused: window.is_window_active() }`)
- Modify: `core/src/server/daemon.rs` (same, with `focused: false`)

**Interfaces:**
- Consumes: Tasks 4 and 5.
- `still(session, Kind::Input)` is true while the thread has `pending_permission()` or `pending_question()` or `needs_install()`. For `Done` and `Error` it is true while the status is not `Working`.
- Call `seen(session)` where the app makes a session the active one on screen.

- [ ] **Step 1: Write a failing test** in `daemon.rs`: when the server can't tell idle time, a finished turn gives no immediate push, and gives one after `tick` at `made + 120 s`. Use the injected `now` and capture sends through the existing test hook for pushes. If there's no such hook, add `fn take_sends` behind `cfg(test)`.
- [ ] **Step 2: Run** `cargo test --locked --no-default-features -p arakun-core server::daemon` and expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the daemon tests, `cargo test --locked --no-default-features shell::` and the Rust check, and expect PASS.
- [ ] **Step 5: Commit** `feat(phone): send notices through the away rule`. Open Part 2's draft pull request.

## Part 3 — Followed servers (pull request `feat(server): use the app's idle time for notices`)

### Task 7: The app tells servers its idle time

**Files:**
- Modify: `core/src/phone/mod.rs:90` (`ClientMsg::Idle { secs: u64 }`, `"type": "idle"`)
- Modify: `core/src/server/daemon.rs` (store `heard_idle: HashMap<ClientId, (u64, Instant)>`, and drop the entry when the client disconnects)
- Modify: `src/servers/client.rs` and `src/shell/servers.rs` (send `{"type":"idle","secs":n}` every 30 s, and when the value crosses a whole minute)

**Interfaces:**
- The daemon's presence is `idle = min(secs)` over entries heard less than 60 s ago, or `None` when there are none. Phones never send `idle`, and the daemon ignores it from phone devices.

- [ ] **Step 1: Write failing tests** in `daemon.rs`:
  - `idle_from_app_used`: a client that sent `secs:10` makes a finished turn get dropped, because the person is at the desk.
  - `stale_idle_falls_back`: an `idle` heard 61 s ago gives `None`.
  - `lowest_idle_wins`: two clients sent 10 and 500, so idle is 10.
- [ ] **Step 2: Run** `cargo test --locked --no-default-features -p arakun-core server::daemon` and expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests and the Rust check, and expect PASS.
- [ ] **Step 5: Commit** `feat(server): use the app's idle time for notices`. Open Part 3's draft pull request.

## Part 4 — Mute and settings (pull request `feat(phone): set away time, shortest turn and mute sessions`)

### Task 8: Mute in core

**Files:**
- Modify: `core/src/phone/mod.rs`:
  - `Device.muted: Vec<String>`, `#[serde(default)]`;
  - `POST /mute {"session","muted"}` replies `{"muted":[...]}`;
  - `GET /me` adds `"muted"`;
  - `Shared::forget_session(id)` removes the session from every device.
- Modify: `core/src/phone/notify.rs` (skip muted sessions in `offer`)
- Modify: the places in the daemon and the app that delete a session, so they call `forget_session`

- [ ] **Step 1: Write failing tests**:
  - `device_without_muted_loads`
  - `post_mute_toggles_one_session`: two calls for different sessions keep both.
  - `muted_session_not_offered`
  - `forget_session_unmutes`
- [ ] **Step 2: Run** `cargo test --locked --no-default-features -p arakun-core phone::` and expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same command and expect PASS.
- [ ] **Step 5: Commit** `feat(phone): mute a session's notices`.

### Task 9: Phone page settings and mute

**Files:**
- Modify: `phone/src/routes/settings.tsx:20,158-219`. `Notices` gains `away_after_secs` and `min_turn_secs`. Under the kind switches, add two choice rows with the Global Constraints labels and choices.
- Modify: `phone/src/routes/session.tsx`: a bell button in the header, plus a "Mute" or "Unmute" item that calls `POST /mute`.
- Modify: `phone/src/routes/sessions.tsx`: a muted session shows a muted bell icon.
- Tests: `settings.test.tsx`, `session.test.tsx`, `sessions.test.tsx`

- [ ] **Step 1: Write failing tests**:
  - Picking "5 min" posts `{"notices":{"away_after_secs":300}}`.
  - Missing fields show "2 min" and "30 s".
  - The mute toggle posts `{session, muted:true}`, and the list shows the muted icon.
- [ ] **Step 2: Run** `(cd phone && npm test)` and expect FAIL.
- [ ] **Step 3: Implement**, using the page's existing `Toggle` and choice components.
- [ ] **Step 4: Run** `(cd phone && npm test && npm run build)` and expect PASS. Commit `phone/dist`.
- [ ] **Step 5: Commit** `feat(phone): away time, shortest turn and mute on the phone page`.

### Task 10: iOS

**Files:**
- Modify: `ios/ArakunKit/Sources/ArakunKit/WebPush.swift` (`NoticeChoice` gains `awayAfterSecs: Int = 120` and `minTurnSecs: Int = 30`, read from and written to the snake_case keys)
- Modify: `ios/ArakunKit/Sources/ArakunKit/Account.swift` or wherever the machine client calls `POST /push` (add `mute(session:muted:)`)
- Modify: `ios/Arakun/Views/AccountView.swift:120-160` (two pickers)
- Modify: the session view and session list in `ios/Arakun/Views` (mute button, muted bell). Colors and fonts come from `Theme.swift`.

- [ ] **Step 1: Write failing tests** in `ios/ArakunKit/Tests`:
  - `NoticeChoice(JSON)` without the new keys gives 120 and 30.
  - Encoding round-trips them.
  - `mute` builds `{"session":"s","muted":true}`.
- [ ] **Step 2: Run** `(cd ios/ArakunKit && swift test)` and expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `swift test` and the `xcodebuild` command from AGENTS.md, and expect PASS.
- [ ] **Step 5: Commit** `feat(ios): away time, shortest turn and mute`.

### Task 11: Android

**Files:**
- Modify: `android/kit/src/main/kotlin/com/arakun/kit/WebPush.kt:153` (`Wanted` gains `awayAfterSecs: Int = 120` and `minTurnSecs: Int = 30` in `json()` and the parser; add a mute request builder)
- Modify: `android/app/src/main/java/com/arakun/android/Push.kt` (store the two values in prefs)
- Modify: `android/app/src/main/java/com/arakun/android/ui/AccountSheet.kt:157+` (two choice rows), plus the session screen and list (mute)

- [ ] **Step 1: Write failing tests** in `android/kit/src/test`, the same three as Task 10.
- [ ] **Step 2: Run** `(cd android && ./gradlew :kit:test)` and expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `(cd android && ./gradlew :kit:test :app:testDebugUnitTest :app:lintDebug :app:assembleDebug)` and expect PASS.
- [ ] **Step 5: Commit** `feat(android): away time, shortest turn and mute`. If the README describes phone notification settings, update it. Open Part 4's draft pull request with `Closes` for ARAKUN-119's GitHub issue.
