# Better phone notifications

## Why

Phone notifications today say too little and come too often. The person
wants a notification to tell them what happened, to arrive only when they
are away from the computer and the news is worth it, and to be under their
control.

Three cases of an unneeded notification, in their words:

- (a) They were at the computer, just in another window.
- (b) A short turn ended that they would have looked at anyway.
- (c) A session they don't care about.

Out of scope for now: too many notices in a row (bundling, throttling).

## How it works today

- The desktop app (`src/shell/sessions.rs:2119`, `notify_phones` in
  `src/shell/phone.rs:2005`) and the headless server
  (`core/src/server/daemon.rs`, around line 3800) each build and send notices.
  The logic is duplicated.
- There are three kinds: `done`, `input` and `error`. Each phone (`phone::Device`
  in `core/src/phone/mod.rs`) keeps `notices: Notices { done, input, error }` on
  the computer. The phone page (`phone/src/routes/settings.tsx`), iOS
  (`ios/Arakun/Views/AccountView.swift`, `ArakunKit/WebPush.swift` `Wanted`) and
  Android (`ui/AccountSheet.kt`, `kit/WebPush.kt` `Wanted`) set it through
  `POST /push`.
- A notice is `{kind, session, title, body}` JSON, sealed for the phone
  (`core/src/phone/push.rs`, `notice()`). The title can have up to 120
  characters and the body up to 600. The whole notice may take at most
  `MAX_NOTICE` (2600) bytes.
- Bodies are fixed text: "Finished its turn.", "Needs permission: <tool
  title>", "Has a question for you.", "Has a plan ready for your approval."
  Only errors carry real text.
- Phones are notified whenever the Arakun window is not focused. Phones that
  have that session on screen are skipped.

## Goals

1. **More info.** Each notice says what happened:
   - `done`: the agent's last message as plain text (no Markdown), and how
     long the turn took.
   - Permission: the tool and what it acts on (the command, the file path or
     the URL).
   - Question: the question's text. If there are several, the first one and
     "+N more".
   - Plan: the first lines of the plan.
   - `error`: as today.
   - Every notice also names its environment.
2. **Only when away (a).** The computer sends a notice only when the person
   is away from it. See "Away rule".
3. **Only long turns (b).** A `done` notice is sent only when the turn took
   at least the minimum the person set. `input` and `error` notices are
   always sent, as long as the away rule passes.
4. **Mute a session (c).** From the session screen on the phone page, iOS
   and Android, the person can mute a session. A muted session sends that
   phone no notices of any kind.
5. **Configurable.** Each phone sets the away time, the shortest turn and its
   muted sessions, next to the three kind switches it has today.

## Non-goals

- Answering or approving from the notification (action buttons). It is a
  later step.
- Bundling or throttling bursts (d).
- Quiet hours and do-not-disturb schedules. The phone's own Focus or Do Not
  Disturb covers this.
- Changing what the desktop's own notifications do.

## Anti-goals (failures even if it works)

- A permission request or a question that never reaches the person while
  they are away. Waiting is fine; losing it is not.
- A notice that arrives after the person already handled the thing on the
  computer.
- A notice that shows content on the lock screen that the push services or
  the Worker can read. Content stays sealed end to end as today.
- An older app version that breaks on the new notice or settings fields.

## Approaches considered for (a)

1. **Wait, then check again:** hold the notice for a set delay, and drop it
   if the person looked at the session on the computer or the session moved
   on. Works everywhere, but someone busy in another app for 10 minutes still
   gets the notice.
2. **System idle time:** send only after no keyboard or mouse input for N
   minutes. It's the best match for "at my desk", but needs code for each
   platform, Wayland support is uneven, and the headless server has no input.
3. **Option 2, with option 1 as the fallback (chosen):** use idle time where
   the system gives it, and wait-then-check where it doesn't. The person
   picked this because only option 2 really solves (a), and the fallback means
   no platform gets worse.

## Design

### One notice pipeline in core

Move the "who gets which notice, and when" logic into core, for example
`core/src/phone/notify.rs`, so the app and `arakun-server` share it. The two
callers keep turning thread events into a notice candidate. Core decides the
rest: kind switches, mute, shortest turn, the away rule, skipping phones that
have the session on screen, and the pending queue.

### Away rule

- Core asks the host how long the person has been idle, through
  `core/src/host.rs`: `fn idle_for(&self) -> Option<Duration>`. `None` means
  the host doesn't know.
- The app implements it for each platform:
  - macOS: `CGEventSourceSecondsSinceLastEventType`.
  - Windows: `GetLastInputInfo`.
  - Linux X11: the MIT-SCREEN-SAVER extension through `x11rb` (already a
    dependency).
  - Linux Wayland: the `org.freedesktop.ScreenSaver` or Mutter IdleMonitor
    D-Bus call where it is present, otherwise `None`.
  - `arakun-server` returns `None`.
- **Away**, when idle time is known: idle time ≥ the phone's away time.
- **Away**, when it is not known (the fallback): the notice has waited the
  away time, the Arakun window is not focused, and nobody opened that session
  on the computer since the notice was made. On `arakun-server`, which has no
  window, the wait alone counts.
- A candidate that is not away yet goes into a **pending queue**, which holds
  at most one notice per session and phone (a newer one replaces it). Core
  checks the queue on a timer, about every 15 seconds. A pending notice is
  dropped when:
  - the session's state changed: the agent is working again, the permission
    or question was answered, or the error was cleared;
  - the person opened that session on the computer;
  - a phone with that session on screen saw it.
- **While the person is at the computer** (idle time known and below the
  away time when the notice fires):
  - `done` and `error` notices are dropped. The person was there, and the
    desktop's own notification covered it.
  - `input` notices (permission, question, plan) wait in the queue and go out
    once the person is away, but only if the agent is still waiting.
- **Followed servers:** the desktop app tells each `arakun-server` it follows
  how long the person has been idle, sent on change and at least every 30
  seconds. The server uses the freshest idle time it heard from any app as
  its `idle_for`, and falls back to the wait when it heard none in the last
  minute.
- **Default away time:** 2 minutes. 0 means always send, which is today's
  behavior apart from the window-focus check.

### Shortest turn

- The candidate carries the turn's duration: wall-clock time from the prompt
  to the turn's end, including time spent waiting for the person (my call: a
  turn that waited on you is one you weren't watching). `turn_started` is
  already in the shared `core/src/agent/thread.rs`.
- A `done` notice is dropped when the duration is below the phone's
  `min_turn`.
- **Default:** 30 seconds. 0 means every turn.

### Mute

- `Device` gets `muted: Vec<String>`: the session ids this phone does not
  hear from. When a session is deleted, it is removed from the list.
- The phone page, iOS and Android each add a mute switch on the session
  screen. It sends one session at a time, `POST /mute {"session": id,
  "muted": bool}`, so two clients changing different sessions can't
  overwrite each other (my call). `GET /me` returns the list.
- The session list shows a muted session with a muted bell.

### Settings

- `Notices` gains `away_after_secs: u32` (default 120) and `min_turn_secs:
  u32` (default 30). `muted` lives on `Device`, not in `Notices`. All new
  fields use `#[serde(default)]`, so stored files and older phones keep
  working. `POST /push` takes each one as an optional field, as it does the
  kind switches.
- Phones already paired get the new defaults too (my call: less noise is the
  point, and the settings ship to all three clients in the same release).
- The phone page, iOS and Android show, under the kind switches:
  - "Only when I'm away for": Always, 1 min, 2 min, 5 min, 15 min.
  - "Turns shorter than this stay quiet": Off, 15 s, 30 s, 1 min, 5 min.
- The words follow CONTRIBUTING's glossary, and `words_lint` must pass.

### Richer content

- The notice JSON gains optional `env` (the environment name) and `secs`
  (the turn length), and keeps `kind`, `session`, `title` and `body`. Older
  apps ignore the new fields.
- iOS (`NotificationService.swift`) shows `env` as the subtitle. Android shows
  it as the sub text. The phone page's service worker puts it before the body.
- The body for each kind follows Goal 1, cut to fit `MAX_NOTICE` as
  `notice()` already does.
- `ArakunKit` and `android/kit` decode the new fields and stay compatible on
  the wire with `core/src/phone` and `phone/`.

## Constraints

- AGENTS.md rules: open an issue first, use Conventional Commits, open draft
  pull requests, follow the design system, the word list and per-platform
  `cfg`, and make no changes behind `team`/`coderlabs`.
- Notices stay end-to-end sealed, and the Worker (`cloud/src/push.ts`) needs no
  change apart from size limits, which stay as they are.
- `ios/ArakunKit` and `android/kit` must match core on the wire.

## Left to the builder

- The exact module layout in core, and how the pending queue's timer runs in
  the app (GPUI) and in the daemon.
- The plain-text conversion of the last agent message.
- Which Wayland D-Bus interfaces to try, and in what order.
- How to split the work into pull requests. A suggested order: (1) richer
  content, (2) the core pipeline with the away rule and idle detection,
  (3) the desktop app sending idle time to followed servers, (4) shortest
  turn, mute and the settings screens on all three clients.
