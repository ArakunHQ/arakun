# Surveys and feedback

## Why

We want to hear from people using Arakun, and from the agents running in it, so we can:

- triage bugs and requests into GitHub issues,
- watch trends in PostHog,
- track UX problems and the features people ask for.

The questions must be written and changed in PostHog's survey editor, with no
release needed. Arakun draws the forms itself, with its own design system. It does
not embed PostHog's web widget.

## Goals

1. **Surveys from PostHog.** The app fetches the surveys in Arakun's PostHog project
   and draws them natively. Their answers go back as PostHog survey responses.
2. **User feedback.** A "Send Feedback…" form the user can open at any time.
3. **Agent feedback.** An agent running in Arakun can file feedback about Arakun with
   `arakun feedback`.
4. **Error feedback.** An optional "what happened?" form:
   - after an agent error,
   - on the launch after a crash.
5. **One set of categories.** Every response carries a kind: `bug`, `feature`, `ux`
   or `other`. GitHub labels and PostHog breakdowns use the same values.

## Non-goals

- Every PostHog question type or targeting rule. See "What the renderer supports".
- Filing GitHub issues from the app. PostHog does that (see "GitHub triage").
- Building a dashboard in Arakun to read feedback. That lives in PostHog.
- Surveys about the user's own PostHog project in the PostHog panel (`posthog_view`).
  This feature is only about Arakun's own project.

## Anti-goals (failure even if it works)

- **Silent sends.** Feedback leaves the machine without the person knowing. Agent
  feedback always shows a notice.
- **Leaking content.** A prompt, a transcript, file contents, paths or secrets are sent.
- **Nagging.** A survey interrupts someone typing, or shows up again after it was
  dismissed.
- **Out of place.** A form looks foreign: hand-written colors or sizes. `design_lint`
  must pass.

## Approaches

| Option | What it is | Result |
|---|---|---|
| **A. Everything is a PostHog survey** | All three forms, plus any future ones, are surveys in PostHog. The app fetches the definitions and draws them. Answers go out as `survey sent` events. GitHub issues come from a PostHog destination. | **Picked.** It is the only option that lets new surveys be created in PostHog. Trends, UX and feature requests all sit where the team already looks. |
| B. Hard-coded forms | The three forms are fixed in the app. PostHog only stores the events. | Rejected: every change to a question needs a release. |
| C. Like A, but the relay opens the issues | The Cloudflare relay in `relay/` files the GitHub issues. | Rejected: another service to deploy and secure, for what PostHog's destination already does. |

## Consent (decided by the user)

| What | Sent when |
|---|---|
| User feedback and error feedback | The person presses Send, even if usage telemetry is off. Pressing Send is the consent. |
| Agent feedback | Only when usage telemetry is on. |
| Surveys PostHog shows on its own (popover type) | Shown only when usage telemetry is on. |
| Anything | Never under `DO_NOT_TRACK` or `ARAKUN_TELEMETRY=0`. In that case, the forms and the command say why nothing can be sent. |

More rules:

- Debug builds, tests and `arakun dev` send nothing. This is the same as today.
- Agent feedback text goes through `telemetry::scrub` before it leaves the machine.
  Typed user text is sent as written: the person sees what they wrote. The form says
  what is attached.
- Every response carries the existing random `telemetry-id` and creates no person
  profile. This is the same as today.
- The README section "Usage and crash reports" and the first-launch points
  (`telemetry_dialog::POINTS`) get one line about feedback.

## Design

### 1. Fetching surveys (`src/surveys.rs`, new)

- **Source.** `GET {HOST}/api/surveys/?token={KEY}`, using the same constants as
  `telemetry.rs`. It runs on a background thread with the existing `ureq` client.
  - It runs at launch, then at most every 6 hours.
  - The last good copy is cached in `app_dir/surveys.json`, so the app works offline.
- **What is kept.** Only active surveys: started, not ended, not archived.
- **The three built-in surveys.** They are identified by survey id, kept as constants.
  A build can override each id with an env var, the way `ARAKUN_POSTHOG_KEY` works.
  If a built-in survey is missing from the fetch and the cache, the app uses a
  fallback with the same questions compiled in. Feedback must always work.
- **What has been seen.** `app_dir/surveys-seen.json` records each survey as shown,
  dismissed or answered. A popover survey never comes back after any of these. The
  built-in surveys can be opened by hand any number of times.

### 2. What the renderer supports

| Question type | How it is drawn |
|---|---|
| `open` | A multi-line text field. |
| `rating` | A number scale (`scale` of 5 or 10) or an emoji scale (`display: emoji`, 3 or 5), with the lower and upper labels. |
| `single_choice` | Radio-style list. "Other" with a text field when `hasOpenChoice` is set. |
| `multiple_choice` | Checkbox-style list. "Other" works the same way. |
| `link` | A button that opens the URL in Arakun's browser tab. |

- **Other properties.** `optional`, `buttonText` and `description` are honored. So is
  next-question branching (`branching`: `next_question`, `end`,
  `response_based`, `specific_question`).
- **Unsupported question types** are skipped. A survey whose required question cannot
  be drawn is not shown at all.
- **Appearance.** PostHog's `appearance` colors and fonts are ignored. Everything comes
  from `theme` and `ui`.
- **Targeting.** The only targeting supported is:
  - a schedule (start and end date),
  - `conditions.events` (show after an event Arakun captures, such as
    `session_started`),
  - a minimum app age (`wait_period`-style: not in the first launch day).

  URL, selector, device and feature flag conditions make a popover survey ineligible.
  The `linked_flag_key` condition does too. Adding flags is a later step.

### 3. How forms appear

- **Opened forms** (user feedback, error feedback): a dialog in the style of
  `telemetry_dialog`, built from `ui` components. Each question shows one at a time,
  with Back and Next buttons.
  - The kind (`bug`, `feature`, `ux`, `other`) is the first question of the user
    feedback survey. In the error survey it is preset to `bug`.
  - The dialog ends with "Thanks. Sent to the Arakun team."
- **Popover surveys from PostHog:** a small card in the bottom-right corner of the
  window, with a close button. Rules:
  - at most one per launch;
  - never in the first 2 minutes;
  - never while a text input has focus and the user typed in the last 10 seconds.

  Closing the card counts as dismissed.
- **Entry points for user feedback:**
  - a palette action, "Send Feedback…";
  - a menu item: on macOS a new Help menu, on other platforms the app menu;
  - a row in Settings › Privacy, next to the telemetry choices.

### 4. Agent feedback: `arakun feedback`

The command:

```
arakun feedback --kind bug|feature|ux|other "<text>"
arakun feedback --kind bug - < note.md     # text from stdin
```

- **`--kind` is required.** A missing kind is a usage error that lists the four kinds.
  A default of `other` would keep real bugs out of GitHub. *(Builder's call.)*
- **The agent's name.** The app sets `ARAKUN_AGENT` when it launches an agent, and the
  command passes it in the request. Without it, the name is `agent`. It is only a
  label, so a process that spoofs it does no harm. *(Builder's call.)*
- **How it travels.** It goes over the control socket (`src/control.rs`) like the other
  commands. The app answers `ok`, or why nothing was sent: consent off,
  `DO_NOT_TRACK`, too long, or the rate limit.
- **Limits.** Text is at most 4000 characters. Each environment can send at most 5
  reports per hour.
- **What is sent** to the agent feedback survey:
  - the kind;
  - the scrubbed text;
  - the agent's name (`claude`, `codex`…);
  - the Arakun version and the OS;
  - a `source: agent` property.

  Never the prompt, the transcript, or the project's name or path.
- **What the user sees.** A notice (`AlertKind` info) titled "<Agent> sent feedback",
  whose subtitle is the first line of the text. The full text stays in the Inbox, so
  the user can always see what was sent.
- **How agents learn about it.** One line is added to the agent's hint text in
  `src/agent/launch.rs`, next to `arakun open`:
  "`arakun feedback --kind <bug|feature|ux> "…"` tells the Arakun team about a
  problem or a missing feature in Arakun itself (not in the user's project)."
- **Docs.** The README lists the command. `docs/windows.md` too, if usage differs.

### 5. Error feedback

- **Agent error.** When `shell/sessions.rs` reports an `AgentError`, its notice gets a
  "Tell us what happened" action. That action opens the error survey, with these
  attached as hidden properties:
  - the error category (which is already a fixed label);
  - the agent's name;
  - the uuid of the `$exception` event that `telemetry::error` sent, so the response
    links to the error in PostHog.
- **Crash.** This uses a clean-exit marker. Arakun writes `app_dir/running` at launch
  and deletes it on a normal quit. If the file is there at the next launch, the last
  run ended badly, whatever the cause (panic, abort, OOM, forced kill). *(Builder's
  call.)* On the launch after such an exit, a notice "Arakun quit unexpectedly.
  Tell us what happened?" opens the same survey, with the crash's event uuid attached.
  To support this, `crash.rs` writes that uuid into the marker when a panic report was
  sent. Without a uuid, the form shows the text in the next bullet.
- **When the user declined crash reports.** If crash reports are off, the error event
  was never sent. The form still works and says "No crash report was sent, so describe
  what you were doing."

### 6. What gets sent (the event)

**How the kind is found.** The app finds the kind question by its fixed question id.
It maps the chosen option to `bug`, `feature`, `ux` or `other` by its position. Editors
can reword the labels but must keep the order. The survey's description in PostHog
says so. The app sends `kind` as a separate property, so the GitHub labels and the
breakdowns never depend on label text. *(Builder's call.)*

The `survey sent` event, through the existing `telemetry` queue, carries:

- `$survey_id`;
- `$survey_name`;
- `$survey_questions` (an array of `{id, question}`);
- `$survey_response_<questionId>` for each answer;
- `$survey_completed: true`;
- the shared properties: `kind`, `source` (`user`, `agent` or `error`),
  `app_version`, `os`, and the error fields above when present.

Also:

- `survey shown` and `survey dismissed` are sent for popover surveys, so PostHog's
  response rates are right.
- Responses that the consent table allows without usage consent need their own path
  past the `USAGE` flag. They go through `telemetry` as a new `send_feedback`. This
  keeps a single sender, retry and queue.

### 7. PostHog setup (done with the PostHog tools, not in code)

This is done first, in Arakun's live PostHog project (the user approved this). The
surveys stay drafts until the app that draws them ships. The app needs their survey
ids and question ids compiled in. The GitHub token for the destination is added by the
user.

**The three surveys.** Each is created as an API-type survey, so PostHog's own SDKs
never show it:

| Survey | Questions |
|---|---|
| **User feedback** | kind (single choice); "What's on your mind?" (open); "Can we follow up? Leave an email" (open, optional). |
| **Agent feedback** | kind (single choice); text (open). |
| **Error feedback** | "What were you doing when it happened?" (open); "How bad was it?" (rating, 1–5, optional). |

**GitHub triage.** A PostHog destination on `survey sent`, filtered to `kind` in
`bug`, `feature` or `ux`, opens an issue in `ArakunHQ/arakun` with:

- labels `feedback` plus `feedback:<kind>`, and `from-agent` for agent reports;
- the response text, scrubbed the way `telemetry::scrub` does: no paths, links,
  keys or emails;
- the source, version and OS;
- a link to the full response in PostHog, and to the linked error if there is one.

The repo is public, so the follow-up email answer never goes into the issue, and no
other answer does unscrubbed. Before Send, the user and error forms say
"Bug reports may become a public GitHub issue." (Decided by the user.)

If PostHog has no GitHub template, a webhook destination calls GitHub's REST API with
a fine-grained token that can only write issues on that repo. `other` responses stay
in PostHog only.

**Insights.** One PostHog dashboard, "Feedback":

- responses per week, by kind and by source;
- the top agent-reported problems;
- error-feedback responses linked to their error.

## Constraints

- Rust and GPUI. The UI uses `theme` and `ui` only, and must pass `design_lint` and
  `words_lint`. "Notice", "palette" and "session" follow the glossary.
- Must build on Linux, macOS and Windows. The Help menu is macOS-only behind `cfg`.
- No new crates are expected: `ureq`, `serde` and `serde_json` are already in.
- Do not touch the `team` or `coderlabs` features.

## Left to the builder

- Module layout inside `src/surveys.rs` and the dialog file, and the names of types.
- Exact wording of buttons and the thank-you text, within the brand voice.
- How a popover card animates in, if it does.
- Fetch backoff when PostHog is unreachable.
- Test cases. At least:
  - parsing real survey JSON, including unknown types;
  - branching;
  - the eligibility rules;
  - the consent table;
  - rate limiting;
  - `arakun feedback` argument parsing.
