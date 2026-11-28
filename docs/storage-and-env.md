# Storage and environment variables

Where Arakun keeps its files, and every environment variable it reads. On Windows, the
data folder is `%LOCALAPPDATA%\arakun`; [docs/windows.md](windows.md) has the same
tables with Windows paths.

## The data folder

Arakun keeps its files in `~/.local/share/arakun` (`$XDG_DATA_HOME/arakun` when
`XDG_DATA_HOME` is an absolute path), on Linux and macOS alike.

The window's state is `state.json` there. Next to it are `state.json.lock` (held while
an Arakun runs on it), `state.json.seen` (the sessions it has seen listed, so it only
deletes the files of sessions it knew) and `state.onboarding.json` (that the first-run
setup was done). `ARAKUN_STATE` names another state file: a second Arakun, such as the
test one `arakun dev` starts, then keeps its own files in that folder.

If Arakun can't read one of its files, it renames it to `<name>.corrupt-<time>` and
starts that part fresh. See [First launch](features.md#first-launch) for how to restore it.

What you pick in Settings › General, Privacy and Branches is kept in one file,
`settings.json` in the same folder, with one section per page. You can back it up, copy
it to another computer or edit it by hand; a setting it leaves out keeps its default.
A running Arakun reads it again when it changes, except for the two Privacy switches,
which a hand edit changes at the next start. Older versions kept each of these
settings in a file of its own (`redacted`, `show-secrets`, `drive-any-page`, `no-auto-update`,
`open-links`, `update-channel`, `settle-after-merge`, `env-cleanup` and
`branch-namer.json`); the first start moves them into `settings.json` and removes them.

A session's transcript (`sessions/`), its linked note (`linked/`) and the images pasted
into it (`pastes/`) stay until you remove the session. If Settings › Branches › Remove
archived sessions after is set (`branches.remove_archived_sessions` in `settings.json`,
in days), an archived session goes with its files once that long has passed since it
was archived or last used. When an environment or project goes, its sessions' files
go with it. The files of a session no state lists any more are deleted a week later.

<!-- paths: generated from core/src/paths.rs -->

### This Arakun's own

With `ARAKUN_STATE` set, these are in the folder of that file instead.

| What | Where |
|---|---|
| Sessions and their transcripts | `~/.local/share/arakun/sessions/` |
| Sessions written out as Markdown for another agent to read | `~/.local/share/arakun/linked/` |
| Images pasted into the composer or sent from the phone | `~/.local/share/arakun/pastes/` |
| Sessions that run without a window | `~/.local/share/arakun/headless/` |
| Memory (or the folder `ARAKUN_MEMORY_DIR` names) | `~/.local/share/arakun/memory/` |
| Your settings from Settings › General, Privacy and Branches | `~/.local/share/arakun/settings.json` |
| The sidebar's layout | `~/.local/share/arakun/sidebar.json` |
| The optional tabs turned on in Settings › Tabs | `~/.local/share/arakun/extensions.json` |
| Prompts sent from the composer, for ↑ and ↓ | `~/.local/share/arakun/history.json` |
| Saved prompts | `~/.local/share/arakun/prompts.json` |
| Notice history | `~/.local/share/arakun/notices.json` |
| Saved palettes | `~/.local/share/arakun/palettes.json` |
| Community widgets: each version's files under `<author>/<name>/<version>`, and `widgets.toml` with where each shows and what you allowed it (its tokens are in the keyring) | `~/.local/share/arakun/widgets/` |
| `Ctrl+K` rules | `~/.local/share/arakun/rules.json` |
| What you pinned from the palette (`Alt+P`) | `~/.local/share/arakun/pins.json` |
| Your to-dos, added from the palette | `~/.local/share/arakun/todos.json` |
| What the palette ranks first | `~/.local/share/arakun/palette-frecency.json` |
| Voice settings | `~/.local/share/arakun/voice.json` |
| The Phone tab's settings and pairings (the private keys are in the keyring) | `~/.local/share/arakun/phone.json` |
| Servers the app is paired with or reaches through your account (Computers tab) | `~/.local/share/arakun/servers.json` |
| What each paired server listed last | `~/.local/share/arakun/servers-inbox.json` |
| GitHub tab settings | `~/.local/share/arakun/github.json` |
| Review drafts (GitHub tab) | `~/.local/share/arakun/github/drafts/` |
| The PR agent's cards you saw and dealt with (arakun-pilot) | `~/.local/share/arakun/pilot.json` |
| The merges archived | `~/.local/share/arakun/settled.json` |
| Your answer about crash reports and usage counts (Settings › Privacy) | `~/.local/share/arakun/telemetry.json` |
| Held while the update channel switches | `~/.local/share/arakun/update-channel.lock` |
| Every panic, kept on this machine | `~/.local/share/arakun/crash.log` |
| That Arakun is running: one left behind means the last run crashed | `~/.local/share/arakun/running` |

### Shared by every Arakun on this computer

These stay in `~/.local/share/arakun/` whatever `ARAKUN_STATE` says.

| What | Where |
|---|---|
| The random install id for crash reports and usage counts | `~/.local/share/arakun/telemetry-id` |
| Surveys from the last fetch | `~/.local/share/arakun/surveys.json` |
| The surveys you saw | `~/.local/share/arakun/surveys-seen.json` |
| Agent adapters (npm), and each agent's log `<agent>.log` with one older copy `<agent>.old.log` | `~/.local/share/arakun/agents/` |
| The usage log and its older copy `usage.old.jsonl` (or the file `ARAKUN_USAGE_LOG` names) | `~/.local/share/arakun/usage.jsonl` |
| Claude seats (`arakun seat add`), one empty file per seat; their tokens are in the keyring | `~/.local/share/arakun/seats/` |
| The Claude seats' last limits | `~/.local/share/arakun/claude-seats.json` |
| The settings that hand Claude Code its API key helper | `~/.local/share/arakun/claude-key-helper.json` |
| OpenCode's plan and limits | `~/.local/share/arakun/opencode-plan.json` |
| Devin's plan and limits | `~/.local/share/arakun/devin-plan.json` |
| The proxy the CLIProxyAPI management key is for (Usage tab); the key is in the keyring | `~/.local/share/arakun/cliproxy-management-key` |
| A typed CLIProxyAPI address (Usage tab) | `~/.local/share/arakun/cliproxy-address` |
| The proxy's accounts' last limits (Usage tab) | `~/.local/share/arakun/cliproxy-plan.json` |
| The skills.sh skills installed | `~/.local/share/arakun/skills-installed.json` |
| Skills and servers installed from repositories | `~/.local/share/arakun/repo-installs.json` |
| On a server: which paired device copied each MCP sign-in there, so it goes when the device does | `~/.local/share/arakun/sign-ins-copied.json` |
| An index of the secrets in the keyring (hashes of their handles only) | `~/.local/share/arakun/secret-refs.json` |
| Secrets handed to agents as files, removed once unused | `~/.local/share/arakun/secret-files/` |
| Arakun's own secrets, when there is no keyring | `~/.local/share/arakun/secrets.json` |
| Each sandboxed agent's folder while it runs | `~/.local/share/arakun/sandboxes/` |
| The socket sandboxed agents reach Arakun through | `~/.local/share/arakun/sandbox/control.sock` |
| Linear sign-ins (who signed in; the API keys are in the keyring) | `~/.local/share/arakun/linear/sessions.json` |
| Linear: the switch for starting sessions, its mappings, how many run at once on each machine and whether pull requests are marked as the agent's (no keys) | `~/.local/share/arakun/linear/delegation.json` |
| Linear: the servers that hold Linear settings | `~/.local/share/arakun/linear/servers.json` |
| Linear: the sessions started from Linear (deliveries handled, runs, reports not yet sent) | `~/.local/share/arakun/linear/runs.json` |
| Doop sign-ins (`sessions.json`; their keys and tokens are in the keyring) and canvases added by link (`pins.json`) | `~/.local/share/arakun/doop/` |
| PostHog sign-ins (`sessions.json`; the keys are in the keyring) and links (`links.json`) | `~/.local/share/arakun/posthog/` |
| Caches: Doop renders, PostHog answers, favicons, marketplaces, Marketplace items | `~/.local/share/arakun/cache/` |
| `arakun server`: its settings, sign-in, Linear settings and runs, sessions and socket (or the folder `ARAKUN_SERVER_DIR` names) | `~/.local/share/arakun/server/` |
| Updates: the version waiting | `~/.local/share/arakun/update.json` |
| Held while an update downloads | `~/.local/share/arakun/update.lock` |
| The update being downloaded | `~/.local/share/arakun/update-download/` |
| The login shell's `PATH`, so a start from the Dock finds tools (macOS only) | `~/.local/share/arakun/login-path.txt` |

<!-- paths: end -->

Outside the data folder:

| What | Where |
|---|---|
| The control socket of the running Arakun | `$XDG_RUNTIME_DIR/arakun/control.sock` |
| The browser's runtime (CEF) | `~/.local/share/arakun/cef`, or a `cef` folder next to the binary |
| arakun-pilot's address | `~/.config/arakun-pilot/client.json` (see [arakun-pilot](github-pilot.md)) |
| The `arakun server` service | `~/.config/systemd/user/arakun-server.service` on Linux (see [Server](server.md#keep-it-running)) |
| Sandboxed Claude Code's settings (Linux) | `~/.claude/.claude.json` |

Keys and tokens go in the system keyring. Without one, Arakun keeps them in
`secrets.json` in the data folder, which only you can read, and tells you so once.

## Logs

What each agent prints to stderr goes to `<agent>.log` in the `agents` folder, and
what each turn used goes to `usage.jsonl` in the data folder (or the file
`ARAKUN_USAGE_LOG` names). Once a log passes its limit (8 MB for an agent log, 16 MB for
the usage log), it moves to `<agent>.old.log` or `usage.old.jsonl` (`<name>.old.<ext>`
next to an `ARAKUN_USAGE_LOG` file) and a new one starts. Arakun keeps only that one
older copy: each move replaces the previous one.
Only your user can read these logs (0600 on Linux and macOS; on Windows only your
account and `SYSTEM` are allowed, and the same goes for `arakun.log`): an agent
prints whatever it reads, and that can include keys and tokens.

`ARAKUN_ACP_TRACE=1` prints every message between Arakun and its agents to stderr (on
Windows, `arakun.log` when Arakun was not started from a terminal): your prompts, the
agent's replies and what its tools read and run. Arakun takes out the keys and tokens it
recognizes, as it does in crash reports, but the trace can still hold sensitive data, so
treat it like the files the agent worked on before you share it.

`crash.log` in the data folder keeps the latest panics. See [Privacy](privacy.md).

## Environment variables

### Settings

| Variable | What it does |
|---|---|
| `ARAKUN_STATE` | Another state file. That Arakun keeps its own files, socket and sign-ins next to it. `arakun dev` sets it. |
| `ARAKUN_MEMORY` | `off` keeps memory out of prompts and stops saving it. |
| `ARAKUN_MEMORY_DIR` | Another folder for memory. Default: `memory` in the data folder. |
| `ARAKUN_USAGE_LOG` | Another file for the usage log. Default: `usage.jsonl` in the data folder. |
| `ARAKUN_SERVER_DIR` | Another folder for `arakun server` (an absolute path), to run a second server. Default: `server` in the data folder. |
| `ARAKUN_TEAMS_URL` | Another Arakun cloud server for the relay and sign-in. Default: `https://app.arakun.dev`. It must be `https://`, except for localhost. |
| `ARAKUN_TELEMETRY` | `0` turns crash reports, usage counts and feedback off and skips the question. `1` turns crash reports and usage counts on. `DO_NOT_TRACK=1` wins over it. |
| `ARAKUN_UPDATES` | `0` turns update checks off for one run. |
| `ARAKUN_REDACTED` | `1` turns redacted mode on for one run. |
| `ARAKUN_NOTIFY` | `off` stops Arakun's desktop notifications. |
| `ARAKUN_NOTICES` | Linux: the apps whose desktop notifications show on the island, comma-separated. Default: `discord`. `0` turns it off. |
| `ARAKUN_ISLAND_EDGE` | The screen's cutout, when the island reads it wrong: `flat`, `notch:<width>x<height>` or `pill:<width>x<height>+<gap to the top>`, in points. |
| `ARAKUN_GITHUB` | `readonly` reads GitHub but sends nothing and marks nothing read. `off` doesn't read GitHub. `arakun dev` sets `readonly`. |
| `ARAKUN_CI` | `off` stops reading GitHub Actions runs. `arakun dev` sets it. |
| `ARAKUN_PILOT_TOKEN` | The arakun-pilot token for `arakun github pilot connect`, so it stays out of the command line. |
| `ARAKUN_PILOT_ADMIN_TOKEN` | The arakun-pilot admin token, the same way. |

### Agents

| Variable | What it does |
|---|---|
| `ARAKUN_AGENT_HINT` | `off` leaves out the note about the `arakun` command in agents' system prompts. `arakun server` sets `off` for itself. |
| `ARAKUN_CLAUDE_ACP` | A command that starts another Claude Code adapter instead of the one installed with npm. |
| `ARAKUN_CODEX_ACP` | A command that starts another Codex adapter instead of the one installed with npm. |
| `ARAKUN_DROID_BIN` | Another `droid` binary. |
| `ARAKUN_DROID_ACP` | Any value but empty or `0` runs Droid over ACP instead of its own protocol. |
| `ARAKUN_DROID` | With `ARAKUN_DROID_ACP`, a command that starts Droid instead. |
| `ARAKUN_OPENCODE_BIN` | Another `opencode` binary. |
| `ARAKUN_OPENCODE_ACP` | A command that starts OpenCode instead. |
| `ARAKUN_OPENCODE_ALL_MODELS` | Any value but empty or `0` lists every OpenCode provider and model, not only the pinned few. |
| `ARAKUN_OPENCODE_AUTH` | OpenCode's credentials file (an absolute path), when it is not in the usual place. |
| `ARAKUN_DEVIN_BIN` | Another `devin` binary. |
| `ARAKUN_DEVIN_ACP` | A command that starts Devin instead of `devin acp`. |
| `ARAKUN_CURSOR_BIN` | Another `cursor-agent` binary. |
| `ARAKUN_CURSOR_ACP` | A command that starts Cursor instead of `cursor-agent acp`. |
| `ARAKUN_AGY_BIN` | Another `agy` binary (Antigravity). |
| `ARAKUN_ANTIGRAVITY_ACP` | A command that starts Antigravity instead of `agy acp`. |
| `ARAKUN_ANTHROPIC_KEY` | An Anthropic API key for Claude models in Arakun's own agent loop. Default: the key in `~/.config/arakun/anthropic-key`. |
| `ARAKUN_ANTHROPIC_URL` | Another Anthropic API address, such as a proxy that holds the key. |
| `ARAKUN_WORKERS_AI_RPM` | Requests per minute the Workers AI agent may make. Default: 20. |
| `ARAKUN_KEEP_WARM` | `0` stops idle Claude sessions in Arakun's own agent loop from keeping the prompt cache warm. |
| `ARAKUN_BWRAP` | The `bwrap` the sandbox uses. Default: `bwrap` on the `PATH`, then `/usr/bin/bwrap`. |

Other variables agents use: `CLAUDE_CODE_EXECUTABLE` picks the Claude Code binary the
adapter runs, and `CLOUDFLARE_API_TOKEN` with `CLOUDFLARE_ACCOUNT_ID` sign in Workers AI.

### Browser tabs

These need the default `browser` cargo feature.

| Variable | What it does |
|---|---|
| `ARAKUN_CDP_PORT` | Opens Chromium's remote debugging port on 127.0.0.1, for tools such as Playwright. Off by default. See [Browser tabs](browser.md) before you use it. |
| `ARAKUN_CEF_DIR` | The folder of the browser's runtime. |
| `ARAKUN_BROWSER_PROFILE` | Another folder for the browser profile. |
| `ARAKUN_BROWSER_FPS` | Frames per second for pages, 1 to 360. Default: the display's refresh rate. |
| `ARAKUN_BROWSER_GPU` | Linux: `1` hands page frames over on the GPU (DMA-BUF). |
| `ARAKUN_BROWSER_IDLE_MS` | How often a visible page is polled, in milliseconds. Default: 33. |
| `ARAKUN_BROWSER_DEBUG` | Logs CEF's message pump to stderr. |

### Install scripts

| Variable | What it does |
|---|---|
| `ARAKUN_VERSION` | The release to install, like `--version v0.2.1`. `nightly` is the same as `--nightly`. |

### For finding problems

| Variable | What it does |
|---|---|
| `ARAKUN_ACP_TRACE` | Prints every message between Arakun and its agents to stderr (see [Logs](#logs)). |
| `ARAKUN_TIMING` | Prints startup and browser timings to stderr. |
| `ARAKUN_CRASH_MARKER` | Lets a debug build leave the crash marker, to try the crash notice. |
| `ARAKUN_SURVEY_NOW` | Debug builds: shows the survey card at once. |
| `ARAKUN_ISLAND_SOCKET` | Another island socket, for a test island. |
| `ARAKUN_ISLAND_PASSTHROUGH` | `1` lets clicks pass through a test island. |
| `ARAKUN_DEMO_EVENTS` | Prints browser events in the `browser_demo` example. |
| `ARAKUN_TERM_CMD` | The command the `terminal_demo` example runs instead of the shell. |
| `ARAKUN_TERM_DOCK` | Shows the `terminal_demo` example as a dock. |

### Set when Arakun is built

| Variable | What it does |
|---|---|
| `ARAKUN_VERSION` | The version Arakun says it is. Default: the crate's version. Nightly builds set it. |
| `ARAKUN_RELEASE_BUILD` | `1` marks a release build, the only kind that updates itself. The release workflow sets it. |
| `ARAKUN_POSTHOG_KEY` | The PostHog project key. Empty sends nothing at all. |
| `ARAKUN_POSTHOG_HOST` | The PostHog host. Default: `https://us.i.posthog.com`. |
| `ARAKUN_SURVEY_USER`, `ARAKUN_SURVEY_ERROR`, `ARAKUN_SURVEY_AGENT` | The PostHog surveys for feedback from a person, after an error, and from an agent. |

### Set by Arakun

Arakun sets these for what it starts. You don't set them yourself.

| Variable | What it is |
|---|---|
| `ARAKUN_SOCKET` | The control socket, for `arakun` in terminals and agents. |
| `ARAKUN_AGENT` | The agent a process belongs to, so `arakun feedback` can name it. |
| `ARAKUN_SANDBOX` | `1` inside an agent's sandbox. |
| `ARAKUN_SECRET_TOKEN` | Tells Arakun's MCP server which session a secret request comes from. |
| `ARAKUN_SERVER_SOCKET` | The socket of the `arakun server` that started Arakun's MCP server. |
| `ARAKUN_SYSTEM_APPEND` | A file whose text goes at the end of a headless agent's system prompt (`--append-system-prompt`). |
| `ARAKUN_DEV` | The window size of the test Arakun `arakun dev` starts. It also turns off notifications and update checks there. |
| `ARAKUN_TOAST_TITLE`, `ARAKUN_TOAST_BODY`, `ARAKUN_SOUND`, `ARAKUN_IMAGE` | Hand a notification, a sound or an image to the system's own tools on Windows and macOS. |
| `ARAKUN_KNOBS`, `ARAKUN_WEB_URL`, `ARAKUN_WEB_BLOCK`, `ARAKUN_WORKERS_AI_URL`, `ARAKUN_WORKERS_AI_KEY`, `ARAKUN_LAB_ALLOW_ALL`, `ARAKUN_CLEF_URL` | Set by the team's agent test bench to tune and watch Arakun's own agent loop. |

### Tests

| Variable | What it does |
|---|---|
| `ARAKUN_BLESS` | Makes tests write the generated tables in the docs (files, shortcuts, commands) and the accessibility baseline instead of failing. |
| `ARAKUN_LINEAR_FIXTURE`, `ARAKUN_POSTHOG_FIXTURE`, `ARAKUN_DOOP_FIXTURE` | A JSON file of made-up answers for that tab, read only by a test Arakun. |
| `ARAKUN_USAGE_FIXTURE` | A JSON file that stands in for Factory's usage API. |
| `ARAKUN_PILOT_BOARD` | A board file to read instead of the arakun-pilot Worker. |
| `ARAKUN_TEAMS_FIXTURE` | A stand-in Teams account, to see the Teams views offline. |
| `ARAKUN_TEAMS_FIXTURE_SIGN_IN` | With the fixture, starts signed out and plays the sign-in to `ok`, `expired` or `denied`. |
| `ARAKUN_TEAMS_E2E` | The account and sessions for the Teams end-to-end test. |
| `ARAKUN_ISLAND_TEST_CHILD` | Marks a test's child process, which gets its own data folder. |
| `ARAKUN_EQ_OUT` | Where an ignored agent-loop test writes its request, for the bench. |
