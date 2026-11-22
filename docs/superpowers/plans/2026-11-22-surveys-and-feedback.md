# Surveys and Feedback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Arakun draws PostHog surveys natively. It adds user, agent (`arakun feedback`) and error feedback, and sends every answer to Arakun's PostHog project as a survey response.

**Architecture:** A new `src/surveys/` module does five things:

- parses PostHog's public survey JSON;
- caches it in `app_dir/surveys.json`;
- decides which survey may show;
- walks a survey's questions;
- builds the `survey sent` event.

`telemetry.rs` gets a separate send path for feedback, so the consent table in the spec applies. A native `SurveyDialog` (a modal) and a popover card draw the surveys. Agents reach the app over the existing control socket with `arakun feedback`.

**Tech Stack:** Rust, GPUI, `ureq`, `serde`/`serde_json` and `getrandom`, all already dependencies. No new crates.

**Spec:** `docs/superpowers/specs/2026-11-22-surveys-and-feedback-design.md`

## Global Constraints

- The UI uses only `theme` and `ui` / `ui::kit` values. `src/design_lint.rs` and `src/words_lint.rs` must pass.
- Interface words follow the glossary: notice, palette, session, environment.
- No new crates. Builds on Linux, macOS and Windows. Platform code goes behind `cfg`.
- Do not touch the `team` or `coderlabs` features.
- Tests go in `#[cfg(test)] mod tests` at the bottom of each file.
- Before each commit, run `tools/rustfmt.sh --check` and `cargo clippy --locked --all-targets --no-default-features -- -D warnings`.
- Run only the touched modules' tests: `cargo test --locked --no-default-features <module>`.
- Commit messages follow Conventional Commits, with no Claude attribution lines.

**Consent table** (copied verbatim from the spec):

| What | Sent when |
|---|---|
| User and error feedback | On Send, even if usage telemetry is off. |
| Agent feedback | Only when usage telemetry is on. |
| Popover surveys | Shown only when usage telemetry is on. |
| Everything | Blocked under `DO_NOT_TRACK` / `ARAKUN_TELEMETRY=0`. |

Debug builds, tests and `arakun dev` send nothing.

**Fixed values:**

| Value | Setting |
|---|---|
| Survey refetch | At most every 6 h. |
| Popover survey | At most 1 per launch, never in the first 2 min, never within 10 s of typing in a focused input. |
| Agent feedback text | At most 4000 chars. |
| Agent feedback rate | 5 reports per environment per hour. |
| Kinds | `bug`, `feature`, `ux`, `other`, in that order. |
| Kind for an error survey | Preset to `bug`. |

**Copy:**

- `"Bug reports may become a public GitHub issue."`, shown before Send on user and error forms.
- `"Thanks. Sent to the Arakun team."`
- `"<Agent> sent feedback"`, a notice title.
- `"Arakun quit unexpectedly. Tell us what happened?"`
- `"No crash report was sent, so describe what you were doing."`

## Review Focus

1. **PostHog unreachable at the first launch** (no cache): "Send Feedback…" still opens and sends, using the compiled-in fallback with the same question ids. *Test: Task 2, `builtin_falls_back_without_cache`.*
2. **A PostHog editor adds an unsupported question** (matrix, NPS with an unknown display) to the user feedback survey. The form still opens: an optional unsupported question is skipped. A required one makes a popover survey ineligible, but never blocks the built-in forms, which then use the fallback. *Test: Task 1, `unsupported_optional_is_skipped`, and Task 2, `builtin_with_required_unsupported_uses_fallback`.*
3. **`arakun feedback` with consent off or `DO_NOT_TRACK`**: the agent gets a clear non-zero reply saying why, and no notice is shown. *Test: Task 6, `feedback_refused_without_usage`.*
4. **Typing when a popover is due**: it waits rather than stealing focus. *Test: Task 2, `eligible_waits_while_typing`.*
5. **A user pastes a token or path into user feedback**: it is sent as typed, but the GitHub issue text is scrubbed. That is the PostHog destination's job (Task 0). The app also sends `$survey_response_<id>_scrubbed`, so the destination never needs its own scrubber. *Test: Task 3, `sent_event_carries_scrubbed_copy`.*

---

### Task 0: Create the surveys, destination and dashboard in PostHog

The user approved writing to Arakun's live PostHog project.

**Files:**
- Create: `src/surveys/ids.rs`

**Interfaces:**
- Produces:
  - `pub const USER: Builtin`, `pub const AGENT: Builtin` and `pub const ERROR: Builtin`.
  - `Builtin { survey_id: &'static str, env_var: &'static str, kind_question: Option<&'static str>, questions: &'static [(&'static str /*id*/, &'static str /*text*/)] }`.

**Steps:**

- [ ] **Step 1:** Read the schema of the `survey` command with the PostHog `exec` tool. Then create three surveys with `type: "api"`, left as drafts (no `start_date`), with these questions:
  - **"Arakun · User feedback"**
    - kind (single_choice, choices `Bug`, `Feature request`, `UX`, `Other`, in that order);
    - "What's on your mind?" (open);
    - "Can we follow up? Leave an email" (open, optional).
  - **"Arakun · Agent feedback"**
    - kind (the same choices);
    - "Feedback" (open).
  - **"Arakun · Error feedback"**
    - "What were you doing when it happened?" (open);
    - "How bad was it?" (rating, scale 5, number display, optional).

  Every survey description says: "Shown natively by Arakun. Reword choices freely, but keep their order and don't delete questions."
- [ ] **Step 2:** Read each survey back and write its id and question ids into `src/surveys/ids.rs`. The env var overrides are `ARAKUN_SURVEY_USER`, `ARAKUN_SURVEY_AGENT` and `ARAKUN_SURVEY_ERROR`, read with `option_env!` the way `telemetry::KEY` is.
- [ ] **Step 3:** Create the GitHub destination on `survey sent`. It is filtered to `kind in [bug, feature, ux]` and opens an issue in `ArakunHQ/arakun` with:
  - labels `feedback` and `feedback:{kind}`, plus `from-agent` when `source = agent`;
  - a body built only from the `*_scrubbed` properties (Task 3), with `source`, `$app_version`, `$os`, and links to the response and to `error_event` when set;
  - never the email question.

  Use a GitHub template if PostHog has one, else a webhook to `POST /repos/ArakunHQ/arakun/issues`. Leave it **disabled**, with an empty token input. Report back that the user must add a fine-grained token (issues: write, on that repo only) and enable it.
- [ ] **Step 4:** Create the dashboard "Feedback" with three insights:
  - `survey sent` per week, broken down by `kind`;
  - the same, broken down by `source`;
  - a table of the latest agent responses.
- [ ] **Step 5:** Commit: `chore(telemetry): add the posthog survey ids`.

### Task 1: The survey model and the question walk

**Files:**
- Create: `src/surveys/mod.rs`, `src/surveys/model.rs`
- Modify: `src/main.rs`, which adds `mod surveys;`

**Interfaces:**
- Produces:
  - `Survey { id, name, kind: SurveyType /*Popover|Api|Other*/, questions: Vec<Question>, start_date: Option<String>, end_date: Option<String>, archived: bool, conditions: Conditions }`.
  - `Question { id: String, text: String, description: String, optional: bool, button_text: Option<String>, body: Body, branching: Branching }`.
  - `Body::{Open, Rating{scale: u8, emoji: bool, lower: String, upper: String}, Single{choices: Vec<String>, open_choice: bool}, Multi{same}, Link{url: String}, Unsupported}`.
  - `Branching::{Next, End, SpecificQuestion(usize), ResponseBased(Vec<(String /*answer key*/, Target)>)}`.
  - `Answer::{Text(String), Rating(u8), Choice(String), Choices(Vec<String>), Skipped}`.
  - `pub fn parse(json: &str) -> Vec<Survey>`: every survey; malformed entries are dropped.
  - `pub fn next(s: &Survey, at: usize, answer: &Answer) -> Option<usize>`: `None` means the end. It skips `Unsupported` optional questions.
  - `pub enum Kind { Bug, Feature, Ux, Other }`, with `as_str`.
  - `pub fn kind_of(q: &Question, answer: &Answer) -> Option<Kind>`: by the chosen choice's index, 0 to 3.
  - `pub fn drawable(s: &Survey) -> bool`: false if a required question is `Unsupported`.

**Steps:**

- [ ] **Step 1: Write failing tests** in `model.rs`, using a fixture copied from a real `/api/surveys/?token=` reply:
  - `parses_real_reply`: 3 surveys, with the right question types.
  - `unknown_type_is_unsupported`.
  - `unsupported_optional_is_skipped`: `next` jumps over it.
  - `branching_response_based`: a rating of 1 jumps to question 2, a rating of 5 ends.
  - `kind_by_position`: a reworded label `"Broken thing"` at index 0 gives `Kind::Bug`.
  - `required_unsupported_not_drawable`.
- [ ] **Step 2:** Run `cargo test --locked --no-default-features surveys::model`. Expect FAIL.
- [ ] **Step 3:** Implement with serde. For the PostHog JSON, unknown fields are ignored and enums use `#[serde(other)]`. For rating branching keys, use PostHog's buckets: `negative`, `neutral`, `positive`, `detractors`, `passives` and `promoters` map from the scale the way posthog-js `getRatingBucketForResponseValue` does.
- [ ] **Step 4:** Run the tests again. Expect PASS.
- [ ] **Step 5:** Commit: `feat(telemetry): parse posthog surveys`.

### Task 2: Fetching, cache, seen state and eligibility

**Files:**
- Create: `src/surveys/store.rs`

**Interfaces:**
- Consumes:
  - `model::{Survey, parse, drawable}`;
  - `ids::{USER, AGENT, ERROR, Builtin}`;
  - `telemetry::{KEY, HOST}`, made `pub(crate)`;
  - `telemetry::effective()`.
- Produces:
  - `pub fn refresh()`: a background fetch, no more than every 6 h, that writes `app_dir/surveys.json`.
  - `pub fn builtin(b: &Builtin) -> Survey`: from the cache when it is there and `drawable`, else the fallback compiled from `b.questions`.
  - `pub enum Seen { Shown, Dismissed, Answered }`.
  - `pub fn mark(id: &str, seen: Seen)`, which writes `app_dir/surveys-seen.json`.
  - `pub struct Moment { pub usage: bool, pub since_launch: Duration, pub since_typing: Duration, pub shown_this_launch: bool, pub now_unix: u64, pub events_seen: &HashSet<String> }`.
  - `pub fn eligible<'a>(surveys: &'a [Survey], seen: &SeenMap, m: &Moment) -> Option<&'a Survey>`.

**Steps:**

- [ ] **Step 1: Write failing tests:**
  - `builtin_falls_back_without_cache`: same ids as `USER`.
  - `builtin_with_required_unsupported_uses_fallback`.
  - `eligible_needs_usage`.
  - `eligible_not_in_first_two_minutes`.
  - `eligible_waits_while_typing`: `since_typing` is 3 s, so the result is `None`.
  - `eligible_once_per_launch`.
  - `dismissed_never_returns`.
  - `api_type_never_pops`: built-ins are type `Api`.
  - `outside_schedule_is_skipped`.
  - `flag_or_url_condition_is_skipped`.
  - `event_condition_waits_for_event`.
- [ ] **Step 2:** Run `cargo test --locked --no-default-features surveys::store`. Expect FAIL.
- [ ] **Step 3:** Implement `eligible` as a pure function, plus the IO around it. The fetch uses `ureq` with a 10 s timeout. On failure it keeps the cache, and the next try waits twice as long, up to 6 h. Nothing is fetched under `cfg(test)`, in debug builds without `ARAKUN_TELEMETRY=1`, or when `telemetry` is blocked.
- [ ] **Step 4:** Run the tests again. Expect PASS.
- [ ] **Step 5:** Commit: `feat(telemetry): fetch posthog surveys and pick one to show`.

### Task 3: Sending responses past the usage flag

**Files:**
- Modify: `src/telemetry.rs`
- Create: `src/surveys/send.rs`

**Interfaces:**
- Produces, in `telemetry`:
  - `pub enum Source { User, Error, Agent, Popover }`.
  - `pub fn feedback_allowed(src: Source) -> Result<(), Refused>`.
  - `pub enum Refused { Blocked /*DNT or env*/, NoUsage, DevBuild }`, which implements `Display` with one sentence each.
  - `pub fn send_feedback(event: &str, props: Map<String, Value>) -> Result<(), Refused>`. It checks `feedback_allowed` and calls `enqueue(event_json(..))` directly, never `hold`.
  - `event_json` now adds `"uuid"` (a v4 from `getrandom`).
  - `pub fn error(...) -> Option<String>` now returns that uuid.
- Produces, in `surveys::send`:
  - `pub fn response_props(s: &Survey, answers: &[(usize, Answer)], src: Source, extra: &[(&str, Value)]) -> Map<String, Value>`.
  - `pub fn submit(s: &Survey, answers, src, extra) -> Result<(), Refused>`.
  - `pub fn shown(s: &Survey)` and `pub fn dismissed(s: &Survey)`, which use the normal `capture` and so need usage consent.

**Steps:**

- [ ] **Step 1: Write failing tests:**
  - In `telemetry`, `feedback_allowed` covers every row of the consent table, using the existing test helpers that set `CRASHES` and `USAGE`, plus `env_choice`:
    - `user_sends_without_usage`;
    - `agent_needs_usage`;
    - `blocked_refuses_all`.
  - Also `error_returns_event_uuid`.
  - In `send`:
    - `props_have_posthog_keys`: `$survey_id`, `$survey_name`, `$survey_questions` (`[{id, question}]`), `$survey_response_<qid>`, `$survey_completed: true`, `source`, `kind`.
    - `sent_event_carries_scrubbed_copy`: `$survey_response_<qid>_scrubbed` equals `telemetry::scrub(text, home)` for every open answer.
    - `email_question_has_no_scrubbed_copy`: the third question of `USER`.
    - `error_kind_is_bug`.
- [ ] **Step 2:** Run `cargo test --locked --no-default-features telemetry surveys::send`. Expect FAIL.
- [ ] **Step 3:** Implement as specified above.
- [ ] **Step 4:** Run the same command again. Expect PASS.
- [ ] **Step 5:** Commit: `feat(telemetry): send survey responses under the feedback consent rules`.

### Task 4: The native survey dialog and the "Send Feedback…" entry points

**Files:**
- Create: `src/survey_dialog.rs`
- Modify:
  - `src/shell.rs`: a `Modal::Survey` variant, a `SendFeedback` action in `gpui::actions!`, and `open_survey(&mut self, survey: Survey, src: Source, extra: Vec<(String, Value)>, window, cx)` modelled on `ask_telemetry` (`src/shell.rs:2394`).
  - `src/app_menu.rs`: "Send Feedback…". On macOS it goes in a new "Help" menu behind `cfg(target_os = "macos")`; elsewhere in the "Arakun" menu under Settings.
  - The palette's action list, where `QuickSwitch` actions are registered.
  - `src/settings_view.rs`: a "Send Feedback…" row in `Section::Privacy`.
  - `src/main.rs`: `mod survey_dialog;`.

**Interfaces:**
- Consumes: `model::{Survey, Question, Body, Answer, next, kind_of}` and `send::submit`.
- Produces:
  - `SurveyDialog::new(survey, src, extra, window, cx)`.
  - `pub enum SurveyEvent { Sent, Dismissed, Failed(String) }`.

**Steps:**

- [ ] **Step 1: Write failing tests** for the dialog's state machine, without rendering:
  - `walks_with_branching`;
  - `back_restores_previous_answer`;
  - `required_open_blocks_next_when_empty`;
  - `last_step_shows_public_notice_for_user_and_error`, which checks the exact copy and that it is absent for `Popover`;
  - `refused_shows_reason`.
- [ ] **Step 2:** Run `cargo test --locked --no-default-features survey_dialog`. Expect FAIL.
- [ ] **Step 3:** Implement the dialog:
  - Build it from `ui::modal_panel`, `ui::modal_header` and `ui::button`.
  - Open questions use `input::TextInput`. Rating uses `kit::toggle_group`. Single choice uses `kit::choice_row` + `kit::radio`. Multiple choice uses `kit::switch_row`. Link uses `ui::button`, opening the URL with `push_browser_tab`.
  - It shows one question at a time with Back and Next, and ends with the thank-you copy.
  - Wire it to `SendFeedback`, which calls `open_survey(store::builtin(&ids::USER), Source::User, vec![])`.
- [ ] **Step 4:** Run the tests again. Expect PASS. Then run the design and words lints: `cargo test --locked --no-default-features design_lint words_lint`.
- [ ] **Step 5:** Check it by eye with the `ui-change` skill: `arakun dev`, open the palette, run "Send Feedback…", and take a screenshot of each question type, in light and dark.
- [ ] **Step 6:** Commit: `feat(telemetry): add a native feedback form`.

### Task 5: The popover survey card

**Files:**
- Create: `src/shell/survey_card.rs`
- Modify:
  - `src/shell.rs`: track `last_typed: Instant`, updated in the existing key-down path, and `survey_shown_this_launch: bool`. A 30 s timer calls `store::eligible`. Call `store::refresh()` at startup.

**Interfaces:**
- Consumes: `store::{eligible, mark, Moment}`, `send::{shown, dismissed}` and `open_survey` (Task 4).
- Produces: `fn survey_card(survey: &Survey, cx) -> AnyElement`, a corner card made from `kit::surface_card`, with the first question's text, "Answer" and a close button.

**Steps:**

- [ ] **Step 1: Write failing tests:**
  - `moment_from_shell_state`, a pure helper `fn moment(&self, now) -> Moment`;
  - `close_marks_dismissed`.
- [ ] **Step 2:** Run `cargo test --locked --no-default-features survey_card`. Expect FAIL.
- [ ] **Step 3:** Implement the card. "Answer" opens `open_survey(s, Source::Popover, vec![])`. Showing the card calls `mark(Shown)` and `send::shown`. Close calls `mark(Dismissed)` and `send::dismissed`.
- [ ] **Step 4:** Run the tests again. Expect PASS. Then check the card by eye in `arakun dev`, with a local `surveys.json` fixture holding one popover survey.
- [ ] **Step 5:** Commit: `feat(telemetry): show posthog popover surveys in a corner card`.

### Task 6: `arakun feedback` for agents

**Files:**
- Modify:
  - `src/control.rs`: the `"feedback"` parse, its usage line, and help.
  - `src/shell/commands.rs`: handling the request.
  - `src/agent/launch.rs`: `.env("ARAKUN_AGENT", kind.id())` at the spawn (around line 1038), and one hint line after the `arakun open` line.
  - `src/shell/island.rs`: the notice.
  - `README.md` and `docs/windows.md` if usage differs.

**Interfaces:**
- Consumes: `send::submit` with `Source::Agent`, and `ids::AGENT`.
- Produces:
  - CLI: `arakun feedback --kind <bug|feature|ux|other> <text…|->`, where `-` reads stdin.
  - The request: `Request { cmd: "feedback", args: [kind, text, agent] }`, where `agent` comes from `ARAKUN_AGENT` or defaults to `"agent"`.
  - `fn feedback_args(rest: &[String], stdin: impl Read, agent: Option<String>) -> Result<Vec<String>, Stop>`.
  - `struct RateLimit` in `commands.rs`, keyed by environment path, allowing 5 per rolling hour.

**Hint line, verbatim:**

```
- `arakun feedback --kind <bug|feature|ux|other> "<text>"` tells the Arakun team about a problem or a missing feature in Arakun itself (not in the user's project). Never include code, paths or secrets.
```

**Steps:**

- [ ] **Step 1: Write failing tests:**
  - In `control`:
    - `feedback_needs_kind`: the usage error lists the 4 kinds;
    - `feedback_rejects_unknown_kind`;
    - `feedback_reads_stdin_dash`;
    - `feedback_over_4000_chars_refused`;
    - `feedback_passes_agent_env`.
  - In `commands`:
    - `rate_limit_sixth_in_hour_refused`;
    - `rate_limit_per_environment`;
    - `feedback_refused_without_usage`: the reply is not ok, carries `Refused::NoUsage`'s text, and no notice is shown.
- [ ] **Step 2:** Run `cargo test --locked --no-default-features control commands`. Expect FAIL.
- [ ] **Step 3:** Implement. On success:
  - the text is scrubbed before `submit`;
  - `notify(AlertKind::Info, "<Agent display name> sent feedback", first_line, None, cx)` is called, so the full text is kept in notices for the Inbox;
  - the reply is `Reply::ok("sent to the Arakun team")`.
- [ ] **Step 4:** Run the tests again. Expect PASS. Update the README's command list, and `docs/windows.md` if it lists commands.
- [ ] **Step 5:** Commit: `feat(agent): let agents send feedback with arakun feedback`.

### Task 7: Error feedback after agent errors and crashes

**Files:**
- Modify:
  - `src/shell/sessions.rs`: `report_agent_error` keeps the uuid that `telemetry::error` now returns.
  - The agent error notice: add the action "Tell us what happened".
  - `src/shell/island.rs`: `Open::Feedback { error_event: Option<String>, agent: Option<String>, category: Option<&'static str> }`, with a `key()` of `open:feedback`.
  - `src/crash.rs`: `pub fn mark_running()` and `pub fn mark_clean_exit()`, and the panic hook writes `{"event": uuid}` into `app_dir/running`.
  - `src/main.rs` / `src/shell.rs`: the startup check.
  - `telemetry::quit` calls `crash::mark_clean_exit()`.

**Interfaces:**
- Consumes: `open_survey(store::builtin(&ids::ERROR), Source::Error, extra)`.
- Produces: `pub fn last_run_ended_badly() -> Option<Option<String>>`, which is outer `Some` if the marker existed, and inner the crash event uuid. It is read once, before `mark_running`.

**Steps:**

- [ ] **Step 1: Write failing tests:**
  - In `crash`:
    - `marker_absent_after_clean_exit`;
    - `marker_present_after_kill`: write the marker, skip the clean exit, and check;
    - `marker_carries_panic_uuid`.
  - In `send`, `error_extra_props`: `error_event`, `agent`, `category`, and `kind = bug`.
  - In `survey_dialog`, `no_report_note_when_no_uuid`: the note shows the exact copy.
- [ ] **Step 2:** Run `cargo test --locked --no-default-features crash survey_dialog surveys::send`. Expect FAIL.
- [ ] **Step 3:** Implement.
  - At startup, after the window opens, when `last_run_ended_badly()` is `Some` and it is not the first launch, show `notify(AlertKind::Warning, "Arakun quit unexpectedly. Tell us what happened?", "", Some(Open::Feedback{..}), cx)`.
  - The agent error notice uses the same `Open::Feedback`, carrying the agent and the category.
  - Under `arakun dev` and in tests, the marker is never written.
- [ ] **Step 4:** Run the tests again. Expect PASS. Then check by eye in `arakun dev`: kill it with `kill -9`, relaunch, and check the notice opens the error form.
- [ ] **Step 5:** Commit: `feat(telemetry): ask what happened after an agent error or a crash`.

### Task 8: Privacy text and docs

**Files:**
- Modify:
  - `src/telemetry_dialog.rs`: a fourth entry in `POINTS`, with icon `message01` or the closest existing icon. Text: `"Feedback you choose to send, even with these off. Bug reports may become a public GitHub issue, without paths, links or keys."`
  - `src/telemetry.rs`: the module doc gains a paragraph on feedback consent.
  - `README.md`, section "Usage and crash reports": a "Feedback" bullet that covers the consent table, `arakun feedback`, and the GitHub issue note.

**Steps:**

- [ ] **Step 1:** Update the `POINTS` array length wherever it is typed (`[(&str, &str); 4]`) and in onboarding (`src/shell/onboarding.rs:822`).
- [ ] **Step 2:** Run `cargo test --locked --no-default-features telemetry_dialog onboarding words_lint design_lint`. Expect PASS.
- [ ] **Step 3:** Commit: `docs(telemetry): explain what feedback sends`.
