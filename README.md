<div align="center">

<img src="assets/brand/arakun.svg" alt="Arakun logo" width="112" height="112">

# Arakun

**One native window for your coding agents.**

Projects, git worktrees, agent sessions, terminals, a browser and GitHub, side by side.<br>
Written in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

[![CI](https://img.shields.io/github/actions/workflow/status/ArakunHQ/arakun/ci.yml?branch=main&style=flat-square&label=CI&labelColor=151515)](https://github.com/ArakunHQ/arakun/actions/workflows/ci.yml?query=branch%3Amain)
[![Release](https://img.shields.io/github/v/release/ArakunHQ/arakun?style=flat-square&labelColor=151515&color=ffb647)](https://github.com/ArakunHQ/arakun/releases/latest)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-ffb647?style=flat-square&labelColor=151515)](LICENSE)
[![Rust 1.100](https://img.shields.io/badge/rust-1.100-ffb647?style=flat-square&labelColor=151515&logo=rust&logoColor=f1efe8)](rust-toolchain.toml)
<br>
[![Linux](https://img.shields.io/badge/Linux-supported-ffb647?style=flat-square&labelColor=151515&logo=linux&logoColor=f1efe8)](#platform-status)
[![Windows](https://img.shields.io/badge/Windows-beta-ffb647?style=flat-square&labelColor=151515&logo=windows&logoColor=f1efe8)](docs/windows.md)
[![macOS](https://img.shields.io/badge/macOS-experimental-ffb647?style=flat-square&labelColor=151515&logo=apple&logoColor=f1efe8)](#platform-status)
[![Conventional Commits](https://img.shields.io/badge/commits-conventional-ffb647?style=flat-square&labelColor=151515&logo=conventionalcommits&logoColor=f1efe8)](https://www.conventionalcommits.org)

[**Install**](#install) ·
[**Features**](#features) ·
[**Command line**](#command-line) ·
[**Shortcuts**](#keyboard-shortcuts) ·
[**Build from source**](#build-from-source) ·
[**Contributing**](#contributing)

<img src="docs/screenshots/composer.png" alt="Arakun's composer with the model menu open" width="860">

</div>

## Why Arakun

- **Many agents, one place.** Claude Code, Droid, OpenCode and Workers AI sessions
  side by side. Give each task its own git worktree so the agents don't step on each
  other.
- **The agents can use the app.** The `arakun` command line lets an agent open pages,
  read terminals, run the project check and leave you GitHub review drafts.
- **Know what they are doing.** A Dynamic Island, an Inbox and a phone remote tell you
  when a session finishes, fails or needs an answer.
- **Native and fast.** A GPU-rendered Rust app, not a web view, with a keyboard-first
  palette for everything.

```sh
curl -fsSL https://arakun.dev/install | sh      # macOS (Apple silicon) and Linux
```

Windows and other options: see [Install](#install).

## Features

### Workspace

- **Projects and worktrees.** A rail of projects, each with its environments (the main
  checkout plus git worktrees). Every environment has its own tabs, sessions, terminals
  and draft prompt. The Workspace panel shows Sessions, an Explorer and the uncommitted
  Changes; the Branches tab draws every live branch against the default one.
- **Branch names from the task.** A new environment starts on a random branch name
  (`env/brave-otter`); after the first prompt, a small model names it after the task
  (`fix/terminal-tab-name`) and Arakun renames the branch. The folder keeps its name. By
  default the agent the prompt went to names it, when that is Claude Code (with `haiku`)
  or Droid, so the prompt goes nowhere else; other agents leave the name alone.
  Settings › General can pick Claude Code or Droid for every environment, or turn naming
  off; it saves that in `~/.local/share/arakun/branch-namer.json`
  (`%LOCALAPPDATA%\arakun\branch-namer.json` on Windows).
- **Terminals.** A dock of terminals per environment (alacritty_terminal; ConPTY on
  Windows), numbered so agents and scripts can read them with `arakun logs`.
- **Built-in browser.** Browser tabs based on CEF (Chromium Embedded Framework), behind
  the optional `browser` cargo feature. Agents can drive pages on localhost through
  `arakun page` (screenshot, console, click, type, eval).

### Agents

| Agent | How Arakun runs it |
|---|---|
| Claude Code | Through the ACP adapter `@agentclientprotocol/claude-agent-acp` |
| Droid | Factory's `droid` CLI |
| OpenCode | The `opencode` CLI |
| Cloudflare Workers AI | A tool loop Arakun runs itself against the Workers AI API |

- **Agent sessions.** Transcripts render markdown, diffs, tool calls and images;
  sessions can be resumed, and per-turn diffs show what each turn changed, including
  changes made by shell commands.
- **Project check.** A per-project command (`cargo test`, `npm test`...) runs after each
  turn that edits files; on failure the tail of the output goes back to the agent, up to
  three times in a row. Checks run one at a time across all sessions, at low priority and
  with half the cores for builds (`CARGO_BUILD_JOBS`), so several agents finishing
  together don't stall the computer. Arakun also notices when an agent loops (the same
  failing command or the same edit over and over) and offers to stop and rethink.
- **Memory.** Agents save what they learn (`arakun mem`), and every new session starts
  with an index of it in the system prompt. Plain JSON files, one per repository plus a
  global one.
- **Headless agents.** `arakun agent run` starts an agent in its own process that keeps
  going with the app closed. Its tools run inside a bubblewrap sandbox (workspace
  writable, the rest read-only, no credentials, no network unless `--allow net`), and a
  policy (`--policy readonly|workspace|full`) decides which tool calls need your approval
  (`arakun agent approve|deny`). Linux only (needs `bwrap`); headless runs use Workers AI
  models for now.
- **Server.** `arakun server serve` is Arakun without a window, for a machine nobody
  sits at: a build box, the workstation at the office, a VPS. It runs sessions of any
  agent signed in on that machine and keeps them going while nothing is connected.
  Paired devices reach it the way phones reach the app, with the same page, pairing
  and notifications; agents ask for permission as they do in the app, and you answer
  from wherever you are. `arakun-server` is the same server as a program of its own
  that needs no display library. See [Server](#server).

### GitHub

- **CI and checks.** GitHub Actions runs for each project, with jobs, annotations and
  the failing log, read through the `gh` CLI.
- **Review queue.** The GitHub tab is a work queue of what waits on you: pull requests
  whose review is asked of you, mentions and assignments, your own open pull requests,
  what is ready to merge and what was done today. It reads your notifications and runs
  searches through the `gh` CLI and its session, so what you already read on github.com
  still shows. It opens before you add a project too.
- **Pull request view.** Pick an item to see its state, checks, what blocks the merge,
  its timeline, files and commits; comment, approve, request changes or merge from there.
  A comment posts at once; approve, request changes and merge ask a second time, and an
  approval or a merge stops if new commits came after you looked.
- **Agent reviews.** An agent can review a pull request for you and leave a draft
  (`arakun github draft`) that you post, edit or discard. Posting a draft asks a second
  time, and an approval goes out only for the commit the agent read: after new commits
  the draft can only be posted as a comment.
- **Alerts and filters.** Review requests, mentions and assignments raise an alert; so
  does anything new in the organizations and repositories on your watch list. Click the
  line under "Review queue" to show only some organizations or repositories; the rest
  doesn't show, count or alert. "Mark all read" marks only what that choice shows.

<details>
<summary>When the GitHub tab reads GitHub, and what it stores</summary>

Arakun reads GitHub in the background only after you open the tab once, and stops while
`gh` is missing or signed out. It keeps the watch list, the filter choice and whether you
opened the tab in `github.json` next to its state, the days your queue reached zero in
`github-streak.json`, drafts in `github/drafts/` beside them, and the profile pictures
it shows in `github/avatars/` (each downloaded again after a week).

</details>

### Stay in the loop

- **Dynamic Island** (Linux/Wayland only for now). A small overlay at the top of the
  screen that shows what the agents of every project are doing: a dot per busy session, a
  card when one finishes, fails or needs an answer. It runs as its own process
  (`arakun island-host`) and needs a compositor with the wlr-layer-shell protocol.
- **Notification history.** Every notice Arakun shows (on the island or as a desktop
  notification) also lands in the sidebar's Inbox, under the sessions waiting on you,
  grouped by day. Click one to open what it is about. The Inbox's badge counts the
  unread ones; they count as read when you close it. Arakun keeps the last 300 in
  `notices.json` next to its state. `arakun notices [--unseen]` prints them.
- **Phone remote.** A web page served by Arakun lets a paired phone follow sessions, send
  prompts and answer the agent. Phones reach the computer over the local network (the
  default), over Tailscale, or through an end-to-end encrypted relay. The relay defaults
  to the hosted one at `arakun.dev`; you can run your own from [`relay/`](relay)
  (a Cloudflare Worker) and point `relay_base` in `phone.json` at it.

### More tabs

- **Service tabs.** A PostHog tab (personal API keys; matches projects by their `phc_`
  keys) and a Cloudflare tab (uses the `cf` CLI's sign-in).
- **Usage.** Token use and cost from Claude Code's and Droid's own logs.
- **Extensions.** MCP servers, plugins, skills and permission rules of the agents.
- **Automations.** Scheduled shell commands or agent prompts per project.

### Make it yours

- **Color palettes.** Settings › Appearance has built-in palettes (Arakun Night,
  Graphite, Midnight, Forest, Rosé). Customize one to change its tint, background and
  accent; Arakun warns when text or the accent gets hard to read. A palette fits in one
  line (`arakun:palette/v1:…`) that anyone can paste there to get it. The choice and your
  palettes are kept in `palettes.json` next to the state.
- **Corners and spacing.** Settings › Appearance also sets the shape of the whole app,
  live. Corners makes cards, buttons and fields sharper or rounder (0 to 200%; presets
  Sharp, Default, Round). Spacing makes the room inside and between things tighter or
  roomier (60 to 160%; Compact, Default, Roomy). Window gap is the space around and
  between the sidebar, the main area, the panel and the dock (0 to 20 px; None, Default,
  Wide). The defaults (100%, 100%, 8 px) look as before. These are kept in
  `palettes.json` too.

## Platform status

| Platform | Status | Notes |
|---|---|---|
| Linux (x86_64) | Supported | Main platform. Wayland and X11. The Dynamic Island needs Wayland with wlr-layer-shell. |
| Windows 10/11 (x86_64) | Beta | No Dynamic Island yet. Release builds do not include the browser yet; build from source with the `browser` feature to get it. See [docs/windows.md](docs/windows.md). |
| macOS | Experimental | Release builds for Apple silicon, without the browser. Little tested. |

## Install

**macOS (Apple silicon) and Linux (x86_64):**

```sh
curl -fsSL https://arakun.dev/install | sh
```

**Windows (PowerShell):**

```powershell
irm https://arakun.dev/install.ps1 | iex
```

The scripts ([tools/install-release.sh](tools/install-release.sh) and
[tools/install-release.ps1](tools/install-release.ps1)) download the latest release,
check it against `SHA256SUMS`, and install it for your user:

| Platform | Installs to | Uninstall |
|---|---|---|
| macOS | `~/Applications/Arakun.app`, linked as `~/.local/bin/arakun` | `rm -rf ~/Applications/Arakun.app ~/.local/bin/arakun` |
| Linux | `~/.local/bin/arakun` | `rm ~/.local/bin/arakun` |
| Windows | `%LOCALAPPDATA%\Programs\Arakun`, added to your user `PATH`, and a Start menu shortcut | delete that folder and the shortcut, and remove it from `PATH` |

To install a given version, run `curl -fsSL https://arakun.dev/install | sh -s -- --version v0.1.0`,
or set `ARAKUN_VERSION=v0.1.0` (on Windows, `$env:ARAKUN_VERSION = "v0.1.0"`). There is
no build for Intel Macs or Linux on ARM yet; build from source there.

### Manual download

Download the archive for your platform from
[GitHub Releases](https://github.com/arakunhq/arakun/releases) and check it against
`SHA256SUMS`. Release builds are built without the browser feature.

Each archive also has a build provenance attestation. It shows that this repository's
release workflow built the archive, and from which commit. Check it with the
[GitHub CLI](https://cli.github.com):

```sh
gh attestation verify arakun-<version>-linux-x86_64.tar.gz -R arakunhq/arakun
```

**Linux:** extract `arakun-<version>-linux-x86_64.tar.gz` and put `arakun` somewhere on
your `PATH`, for example `~/.local/bin`. It needs glibc 2.35 or newer (Ubuntu 22.04+,
Debian 12+, Fedora 36+); on older systems, build from source. You also need a Vulkan
driver and the usual desktop libraries (libxkbcommon, Wayland or X11, fontconfig,
freetype).

**Windows:** extract `arakun-<version>-windows-x86_64.zip` and run `arakun.exe`.

**macOS:** `arakun-<version>-macos-arm64.tar.gz` holds `Arakun.app`, signed with a
Developer ID and notarized by Apple. Move it to `Applications` and open it.

Arakun does not bundle the agents. Install the ones you want to use:

- Claude Code: Node.js with `npm` (Arakun installs the ACP adapter) and a `claude`
  that is signed in or set up with an API key or another endpoint (`ANTHROPIC_BASE_URL`,
  such as a gateway or CLIProxyAPI) in `~/.claude/settings.json` or the environment.
- Droid: Factory's `droid` CLI.
- OpenCode: the `opencode` CLI, signed in with `opencode auth login`.
- Workers AI: a Cloudflare API token with Workers AI access, either stored through
  `opencode auth login` or set as `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`.

Optional tools: `git` (Changes, worktrees, branches), `gh` (CI, GitHub tab), `bwrap`
(headless agents), `secret-tool` from libsecret (stored logins on Linux), `tailscale`
(phone over Tailscale), `cf` (Cloudflare tab).

## Build from source

You need [rustup](https://rustup.rs). The first `cargo` command in the checkout
installs the Rust version named in `rust-toolchain.toml`, the one CI uses.

`tools/setup-dev.sh` (Linux and macOS) or `tools\setup-dev.ps1` (Windows) installs
everything below for you; see [CONTRIBUTING.md](CONTRIBUTING.md#setting-up). To do it by
hand:

**Ubuntu / Debian:**

```sh
sudo apt install build-essential pkg-config \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb1-dev \
  libfontconfig1-dev libfreetype-dev libvulkan-dev libssl-dev
```

**Arch Linux:**

```sh
sudo pacman -S --needed base-devel pkgconf \
  libxkbcommon libxkbcommon-x11 wayland libxcb fontconfig freetype2 \
  vulkan-icd-loader vulkan-headers openssl
```

**Windows:** MSVC toolchain and Visual Studio Build Tools; see
[docs/windows.md](docs/windows.md).

### Without the browser

The quickest path, and what CI and the release builds use:

```sh
git clone https://github.com/arakunhq/arakun
cd arakun
cargo build --release --no-default-features
./target/release/arakun
```

### With the browser (CEF)

The default `browser` feature needs CEF at build time and at run time.

- **Linux:** download the CEF 156.0.15 linux64 binary distribution (the minimal one is
  enough) from <https://cef-builds.spotifycdn.com/index.html>, unpack it, and run:

  ```sh
  tools/install-cef.sh /path/to/cef_binary_156.0.15_linux64
  cargo build --release
  ```

  This puts the runtime in `~/.local/share/arakun/cef` (loaded on the first browser tab)
  and links the build-time SDK at `.cef-sdk`, which `.cargo/config.toml` points `CEF_PATH`
  at.
- **Windows and macOS:** the first build downloads CEF into `.cef-sdk` by itself. It
  also needs CMake and Ninja (on macOS, `brew install cmake ninja`); for Windows, see
  [docs/windows.md](docs/windows.md).

### The server alone (no display)

`arakun-server` is the [server](#server) built without the window. It needs none of the
system libraries above, only a C compiler, and the binary runs on a machine that has
no desktop installed:

```sh
cargo build --release --locked -p arakun-server
./target/release/arakun-server help
```

Build it on its own, as above. Built in the same command as the app, Cargo gives both
the same GPUI, with the display code in it. Releases do not carry it yet: build it on
the machine, or copy the binary from one with the same glibc.

### Install for your user (Linux)

`tools/install.sh` builds a release binary and installs it to `~/.local/bin`, with a
`.desktop` entry and icons. It prepares CEF with `tools/install-cef.sh` if it is missing
and `CEF_DIST` points at an unpacked distribution.

```sh
tools/install.sh               # --no-browser, --prefix DIR, --uninstall
```

## Command line

While Arakun is running, `arakun <command>` acts on the environment of the current
directory, over a local control socket (`$XDG_RUNTIME_DIR/arakun/control.sock`, or
`%LOCALAPPDATA%\arakun\control.sock` on Windows). Terminals and agents started by Arakun
get `ARAKUN_SOCKET` and `arakun` on their `PATH`, and the agents get a note in their
system prompt about it (`ARAKUN_AGENT_HINT=off` removes it).

```sh
arakun .                          # add this project (or switch to it); starts Arakun if needed
arakun browse -s localhost:3000   # open a page in the side pane
arakun open -s src/main.rs:42     # open a file in the side pane at line 42
arakun run --port 5173 npm run dev   # new terminal; wait until the port answers
arakun logs 2 --until compiled    # wait until terminal 2 prints "compiled"
arakun check                      # run the project check here
arakun rename fix/tab-name         # rename this environment's branch (before it is pushed)
arakun page screenshot            # PNG of the environment's browser tab
arakun notify "done"              # desktop notification
arakun notices --unseen           # notifications you haven't read yet
arakun ls                         # projects, environments and sessions
arakun palette                    # bring the window up with Ctrl+K open
arakun mem search <words>         # search what agents remember about this project
arakun github draft o/r#12 --verdict approve --head <sha> --body-file - --agent Claude < review.md
                                  # save a review draft for the GitHub tab; sends nothing.
                                  # --body-file takes a file or - (stdin); an agent's file
                                  # asks the user first if it is outside its workspace or
                                  # the command shares its line with others. --head is
                                  # the commit the agent read; an approval needs it
arakun agent run <prompt>         # start a headless agent
arakun server serve               # run sessions on this machine without the window (arakun server help)
arakun --version                  # print the version (needs no running Arakun)
```

To open the palette from any app, bind `arakun palette` to a global key in your
desktop. On Hyprland, for example: `bind = SUPER, K, exec, arakun palette`.

`arakun run` and `arakun logs` take `--ready <text>`, `--port <n>` and `--timeout <s>`
before the command, and fail with the tail of the output if the command exits or times
out first.

## Server

To run sessions on another machine, start a server there and pair a device with it.
Where the app is installed, `arakun server` is the command; `arakun-server` is the
same command for a machine with no display (see
[The server alone](#the-server-alone-no-display)).

```sh
arakun-server serve          # runs until Ctrl+C, a signal or `arakun-server quit`
arakun-server pair           # in another terminal: a link and its QR code
```

Open the link on a phone or in any browser within two minutes. It pairs that one
device, which then sees the server's sessions, starts new ones, reads them live,
answers the agents and gets a notification when one finishes or needs you. It is the
page the Phone tab pairs phones with. The app does not connect to a server yet.

```sh
arakun-server add ~/code/app      # a folder sessions can start in; the page lists them
arakun-server new --agent claude "fix the failing test"   # start one here; prints its id
arakun-server ls                  # folders and sessions
arakun-server log <id>            # a session's status and last entries
arakun-server wait <id>           # until it stops working; prints its answer or what it asks
arakun-server send <id> <text>    # a follow-up
arakun-server approve <id>        # or deny: answer the permission it asks for
arakun-server stop <id>           # stop its turn; follow-ups waiting behind it are dropped
arakun-server devices             # paired devices; `unpair <device>` forgets one
arakun-server status
```

`arakun-server help` lists them all.

- **Agents.** Claude Code, Droid, OpenCode and Workers AI, as in the app. Sign in to
  each one on that machine first: the server uses its logins and its permission
  settings. An agent set to allow everything there asks for nothing here either.
- **How devices reach it.** `serve --reach relay` (the default) dials out to the relay,
  so it works behind a firewall, and everything between the device and the server is
  end to end encrypted; `--relay <url>` names a relay of your own. `--reach tailscale`
  serves it at the machine's tailnet name over HTTPS; that takes the machine's HTTPS
  port on the tailnet, as the app's Phone tab on Tailscale does, so only one of the two
  can use it on a machine. `--reach lan` listens on every
  interface over plain HTTP (`--port`, 7474 unless told): use it only on a network you
  trust. What `serve` is started with is kept for the next run.
- **What it keeps.** `~/.local/share/arakun/server/`, which only your user can read:
  `phone.json` (how it is reached, its relay identity, the paired devices),
  `state.json` (folders and sessions), `sessions/` (transcripts, saved as the agents
  work) and `uploads/` (photos sent with prompts). `ARAKUN_SERVER_DIR` names another
  folder, for a second server on the machine. The app's own files are not touched: the
  app and a server can run side by side.
- **The local socket.** The commands above reach the server over `server.sock` in
  that folder, which only your user can open. It is also what keeps a second server
  off the same folder.
- **A turn that is running when the server stops** is cut off. Its session is still
  there after a restart, and the next message picks it up. Follow-ups that were
  waiting behind that turn run first, then the new message.

To keep it running on Linux, put the binary where the unit below looks for it, and a
systemd user unit is enough:

```sh
install -Dm755 target/release/arakun-server ~/.local/bin/arakun-server
```

```ini
# ~/.config/systemd/user/arakun-server.service
[Unit]
Description=Arakun server

[Service]
ExecStart=%h/.local/bin/arakun-server serve
Restart=on-failure

[Install]
WantedBy=default.target
```

```sh
systemctl --user enable --now arakun-server
loginctl enable-linger "$USER"     # keep it running while you are logged out
```

If a session says its agent was not found, give the unit the `PATH` your shell has
(`Environment=PATH=...`).

Tried on Linux and macOS.

## Keyboard shortcuts

- `Ctrl+T` new session, `Ctrl+J` toggle the terminal dock, `Ctrl+B` toggle the sidebar
- `Ctrl+D` / `Ctrl+Shift+D` split right / down, `Ctrl+Alt+arrows` move between panes
- `Ctrl+Tab`, `Ctrl+1...9` switch tabs; `Alt+Up` / `Alt+Down` switch environments
- `Ctrl+F` search the conversation
- `Ctrl+K` (or `Ctrl+P`) the palette: sessions of every project, GitHub, runs, files and
  symbols, views, commands, settings and memory in one search. Start with `>` for
  commands, `/` for files, `:` for settings, `@` for people or `#` for pull requests.
  `#` lists every open pull request of your projects' repositories, not only yours;
  `#86` finds that one even when it is closed or merged, and a first word that names a
  repository or project narrows to it (`#arakun 86`, `#frameshi`).
  In it: `→` opens a row's actions or goes into it (a project, a person, Branches), `←`
  goes back; `Space` after the arrows marks rows for the bar at the bottom; `Ctrl+P` pins
  a row to the top of the palette and the sidebar, and `Ctrl+1...9` opens a pin
  (`Ctrl+Alt+1...9` anywhere, `Alt+Up` / `Alt+Down` reorder them); `Ctrl+Enter` asks an
  agent. A question offers a short answer from Claude Haiku through Claude Code, on
  your Claude account or the key or endpoint Claude Code is set up with: `Enter` on the offer asks, and nothing is sent
  before. A sentence such as "merge #64 when the checks pass and tell me" becomes an
  automation once you confirm it; one Arakun cannot read itself offers to have Claude
  read it, again only on `Enter`.
  "recuérdame en 20 min revisar el PR", "remind me at 5pm to stretch" or
  "timer 10 min" sets a reminder that rings on the island. One that comes due while
  Arakun is closed rings, marked as missed, the next time it opens. "to-do: …" (or a
  reminder with no time) adds a to-do, and typing "to-do" or "pendiente" lists them,
  `Enter` ticking one off. Pins are kept in `pins.json`, rules (reminders too) in
  `rules.json`, to-dos in `todos.json` and what you open in `palette-frecency.json`,
  next to the state file.
- `Ctrl+=` / `Ctrl+-` / `Ctrl+0` zoom the whole interface (in a browser tab, the page)
- In the composer: `Enter` sends, `Shift+Enter` new line, `Up` on an empty composer
  recalls earlier prompts; paste or drop images to attach them
- In the GitHub tab: `J` / `K` (or `↓` / `↑`) next and previous item, `A` approve,
  `C` comment, `R` review with an agent, `U` catch me up, `S` summarize, `O` open on
  GitHub; `Ctrl+Enter` sends a comment, `/` in the comment box lists the actions. In
  a narrow window the keys move through the list and `Enter` opens the item; `Esc`
  goes back to the list

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first: it covers
setup (`tools/setup-dev.sh`), the checks CI runs, and the Conventional Commits titles
pull requests need. Contributors sign the [CLA](CLA.md) on their first pull request.
Please follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report security issues as
[SECURITY.md](SECURITY.md) says, not in public issues.

## License

Arakun is free software under the [GNU Affero General Public License v3.0](LICENSE)
(AGPL-3.0-only). Commercial licenses are available from the copyright holder
(Karlo Aldrete). Contributions require signing the [CLA](CLA.md); see
[CONTRIBUTING.md](CONTRIBUTING.md). Third-party notices are in [NOTICE](NOTICE).

<div align="center">
<br>
<img src="assets/brand/raccoon.svg" alt="" width="48">
<br>
<sub>Made with Rust and a raccoon. <a href="https://arakun.dev">arakun.dev</a></sub>
</div>
