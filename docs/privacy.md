# Privacy

What Arakun hides on screen, and what it sends.

## Secrets in sessions

Keys, tokens, passwords and private keys an agent prints show as stars in a session: in
replies, your prompts, tool rows and command output, diffs, permission cards and
questions. A key keeps its first four characters (`sk-a****`), which say what it is.
Click a hidden secret to show it in that session's view until the view closes. Copying
text that holds one asks first: the copy button asks "Copy the secret too?" and copies
on a second click, and the selection menu offers "Copy as shown" or "Copy with secret".

Settings › Privacy › Hide secrets in sessions turns this off. The choice is kept in
[`settings.json`](storage-and-env.md#the-data-folder). Redacted mode hides more
(see below), and a click there shows nothing.

## Redacted mode

For streams, demos and screen sharing. `Ctrl+Shift+H`, the palette or Settings › Privacy
turns it on; a "Redacted" pill in the title bar shows it is on, and a click on it turns it
off. While it is on, Arakun hides:

- account emails and names: the Cloudflare, PostHog and Doop accounts, the Linear sign-ins, the claude.ai accounts
  behind CLIProxyAPI, your git name and email in the sidebar, Cloudflare's audit log;
- the phone's pairing QR code and code, its addresses and the names of paired phones
  ("Copy link" still copies the real link);
- the host Claude Code runs on, a server's pairing link, and IP addresses;
- in terminals, agent replies, tool output, tab titles, notifications and the Inbox:
  emails, values shaped like keys or tokens (`sk-…`, `ghp_…` and long random words keep
  their first four characters), the value after a name such as `API_KEY=` or `token:`,
  the word after `Bearer`, IPv4 addresses other than `127.0.0.1`, and your user name
  and the computer's name, as a shell prompt shows them (`you@laptop`).

It works on what it can recognize, so check a view before you share it: a secret that
looks like a plain word still shows. The choice is kept in
[`settings.json`](storage-and-env.md#the-data-folder), so it is still on after a restart.
`ARAKUN_REDACTED=1` turns it on for one run without changing that file.

## Agents in browser tabs

Agents drive pages on localhost and only read the others. Settings › Privacy › Agents
drive any web page lets them click, type and run scripts in any browser tab too, with the
accounts you are signed in to there. It is off by default and kept in
[`settings.json`](storage-and-env.md#the-data-folder).

## Usage and crash reports

Arakun sends nothing until you say so. On first launch it asks whether to send crash
reports and anonymous usage counts to Arakun's PostHog project, so we can see what
breaks and what gets used: as a step of the setup, or in a dialog when you skip the
setup or already have a project. Settings › Privacy has a switch for crash reports and
one for usage counts, and a button for feedback:

- **Crash reports:** a panic's message and stack trace. When an agent stops with an
  error: the agent's name and version number, a fixed category such as "rate
  limited" or "not signed in", the step that failed (such as starting the agent or
  a turn), the agent's error code or exit code, and whether the session's folder
  still exists. Never the error's text.
- **Usage counts:** app launched, app quit (with the minutes it was open), setup
  finished (whether you added a project or skipped, and the step you were on),
  session started (with the agent), agent turn finished (with the agent, how it
  stopped and how many seconds it took), project added, environment created, a
  built-in view opened (with its name) and a browser tab opened.
- **Feedback:** what you write when you press Send on a feedback form or after an
  agent error or a crash. Help in the sidebar opens the form, set to a bug or a
  feature request if you pick one, and links to the [docs](https://arakun.dev/docs). It is sent even with crash reports and usage counts off,
  because pressing Send is your yes. An agent can also send feedback with
  `arakun feedback`, but only with usage counts on, and you get a notice with the
  full text in the Inbox. Surveys made in PostHog show up only with usage counts on,
  at most once per launch. Arakun fetches them from PostHog only with usage counts
  on, in an anonymous request with the project token and no id. With usage counts
  off, the feedback forms use the questions built into Arakun. Feedback other than
  a support question (a bug, feature request, UX note or anything else) also goes
  to the team's Linear, through
  Arakun's cloud, as a customer request. Its issue has the text without paths,
  links, keys or emails, and may become a public GitHub issue. Your text as you
  wrote it and your email stay in Linear. If that fails, the answer still
  arrives in PostHog.

Each event carries Arakun's version, the OS and the CPU architecture. On first launch,
what happens before you answer the question waits in memory: it is sent if you turn
usage counts on, and dropped if you don't. Arakun never
sends prompts, replies or file contents. A panic's message is the one text that is
sent: before it leaves your machine, Arakun replaces your home folder with `~` and
removes paths, links, emails and anything shaped like a key or token. Its other words
are sent as they are, so a crash message can still name something it was working on. Events carry a random id kept in
`telemetry-id` in Arakun's data folder (`~/.local/share/arakun` on Linux and macOS,
`%LOCALAPPDATA%\arakun` on Windows); it is not tied to any account, PostHog makes no
person profile from it and no location from your IP, and deleting the file starts a
new one. Your answer is kept in `telemetry.json` in the same folder.

Whatever you answer, Arakun writes each panic to `crash.log` in the same folder: when it
happened, the version, the thread, the message (cleaned the same way) and the stack
trace. That file stays on your machine; attach it when you report a crash. It keeps the
most recent panics, up to about 256 KB. A panic in background work no longer closes
Arakun: what was running shows an error, and the log says "Arakun carried on".

`DO_NOT_TRACK=1` or `ARAKUN_TELEMETRY=0` turn both off, block all feedback and skip the question;
`ARAKUN_TELEMETRY=1` turns both on. Debug builds send nothing unless
`ARAKUN_TELEMETRY=1` is set, and the test Arakun of `arakun dev` never does. A build
made with `ARAKUN_POSTHOG_KEY=` (empty) sends nothing at all.
