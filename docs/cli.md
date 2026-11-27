# Command line

While Arakun is running, `arakun <command>` acts on the environment of the current
directory, over a local control socket (`$XDG_RUNTIME_DIR/arakun/control.sock`, or
`%LOCALAPPDATA%\arakun\control.sock` on Windows). Terminals and agents started by Arakun
get `ARAKUN_SOCKET` and `arakun` on their `PATH`, and the agents get a note in their
system prompt about it (`ARAKUN_AGENT_HINT=off` removes it).

One Arakun runs per state. `arakun` started again brings the running window up and
exits (`arakun <dir>` adds the project there), since two windows on one state would
save over each other and run the same automations twice. The running one holds
`state.json.lock` next to `state.json`; an Arakun with its own `ARAKUN_STATE`, such as
`arakun dev`, has its own lock and runs beside it. It also has its own socket and
`arakun` command next to that state file (`dev.sock` and `dev.runtime/bin/arakun` for a
`dev.json`), so the command line of the daily Arakun keeps reaching the daily Arakun.

Only `arakun` alone and `arakun <dir>` start the app. A word or flag it doesn't know,
such as a typo, prints an error and exits with code 2 (`arakun --help` lists the
commands).

`-h` or `--help` after a command prints its usage and runs nothing. A flag the command
doesn't take prints an error and exits with code 2. Both count only before the
payload, the prompt, message or command line a command passes on, so
`arakun run make --help` runs `make --help`. A payload that starts with a dash needs
`--` before it: `arakun notify -- -1 test failed`.

To open the palette from any app, bind `arakun palette` to a global key in your
desktop. On Hyprland, for example: `bind = SUPER, K, exec, arakun palette`.

`arakun run` and `arakun logs` take `--ready <text>`, `--port <n>` and `--timeout <s>`
before the command, and fail with the tail of the output if the command exits or times
out first.

Agents use it most. These are the commands an agent reaches for in a session:

```sh
arakun browse -s localhost:3000   # open a page in the side pane
arakun run --port 5173 npm run dev   # new terminal; wait until the port answers
arakun logs 2 --until compiled    # wait until terminal 2 prints "compiled"
arakun check                      # run the project check here
arakun page screenshot            # PNG of the environment's browser tab
arakun shrink -- cargo test       # run it and print a summary; the whole output goes to a file
arakun mem search <words>         # search what agents remember about this project
arakun github draft o/r#12 --verdict approve --head <sha> --body-file - < review.md
```

The [server](server.md) has its own commands under `arakun server` (`arakun server help`),
and arakun-pilot's are in [github-pilot.md](github-pilot.md).

## Every command

This is what `arakun help` prints.

<!-- cli: begin; written from src/control.rs -->

```text
arakun — command line for the running arakun window

Usage:
  arakun                       start arakun
  arakun --version             print the version
  arakun <dir> | add [dir]     add a project (or switch to it) and show it
  arakun browse [-s] <url>     open a browser tab (a path opens as file://)
  arakun open [-s] <file[:N]>  open a file tab, at line N
  arakun palette               bring the window up with Ctrl+K open (bind it
                             to a global key, such as Super+K)
  arakun run [--ready <text>] [--port <n>] [--timeout <secs>] <command…>
                             run a command in a new terminal of the dock; with
                             --ready, --port or both, wait until its output
                             shows the text (any case) and the port answers,
                             and fail with its output if it exits first or is
                             not up after --timeout seconds (default 60)
  arakun logs [-n N] [term]    print a dock terminal's last N lines (its number
                             or a word of its title; lists them when unclear)
  arakun logs [term] --until <text> [--timeout <secs>]
                             wait until the terminal prints the text (any
                             case) after this call, and print what it printed;
                             fail if it is not there after --timeout (60 s)
  arakun check                 run the project's check command here, as arakun
                             does after agent turns that edit files
  arakun check --set <command…>
                             make it the project's check command
  arakun rename <branch>       rename this environment's branch (not yet
                             pushed); its folder stays where it is
  arakun notify <message…>     send a desktop notification
  arakun shrink [--] <command…>
                             run a command and print a summary of its output
                             (failures of a test run, a log's repeated lines
                             collapsed, JSON's outline); the whole output goes
                             to a file it names. Needs no running Arakun
  arakun feedback --kind <bug|feature|ux|other> <text…|->
                             tell the Arakun team about a problem or a missing
                             feature in Arakun itself (up to 4000 characters;
                             - reads stdin; needs usage telemetry on)
  arakun notices [--unseen]    the notifications Arakun has shown, newest first
  arakun usage tools [--days N] [--json]
                             where the tokens of Workers AI sessions' tool
                             results go: each tool's calls and tokens, bash by
                             the program it runs (cargo, git…), largest first
  arakun usage where [--days N] [--json]
                             where the tokens of Claude Code sessions go, from
                             their transcripts: unused skills, plugins and MCP
                             servers, long CLAUDE.md files, noisy tools and
                             full sessions, what to fix first (estimates)
  arakun seat add <name>       save a Claude seat: a token from `claude setup-token`,
                             read from stdin and checked with one small call;
                             each new Claude Code session then runs on the seat
                             with the most room in its 5-hour window, for life
  arakun seat ls               the seats and their 5-hour and weekly usage
  arakun seat rm <name>        forget a seat
  arakun ls                    list projects, environments and their sessions
  arakun island [state]        show the Dynamic Island in a state, to try it:
                             sleeping, idle, working, speaking, listening;
                             list pins or unpins the agents' list; auto
                             follows the agents again; fps prints the frames
                             it painted in the last 2 s; with no state, print
                             the one it shows. The island is a program of its
                             own (arakun island-host, which arakun starts when
                             none runs), so these work with arakun closed
  arakun island alert <title> [subtitle]
                             show a card on the island
  arakun island view | later | approve | deny
                             press the card's button (approve and deny
                             answer a headless agent's request)
  arakun agent new [--model <provider/model>] [--effort <level>] [--mode <id>] [--dir <dir>] [--title <text>] [--ask] <prompt…>
                             start a background agent, listed in the sidebar,
                             and print its id: OpenCode, or arakun's own Workers
                             AI agent for a workers-ai/@cf/… model; --effort sets
                             the reasoning (none, low, high, max: per model); it
                             allows its own tool calls unless --ask
  arakun agent ls              list the OpenCode and Workers AI sessions and their status
  arakun agent log [-n N] <id> print a session's status and last N entries (30)
  arakun agent wait [--timeout <secs>] <id>
                             wait until it stops working (at most 3600 s) and
                             print its status and last message; fail on error
  arakun agent send <id> <text…>
                             send it a follow-up
  arakun agent stop <id>       stop its running turn
  arakun agent run [--model <id>] [--dir <dir>] [--title <text>] [--effort <level>]
                   [--policy readonly|workspace|full] <prompt…>
                             start a headless agent (Workers AI) in a process of
                             its own: it goes on with arakun closed; prints its
                             id. ls, log, wait, send and stop work on it too.
                             The policy (default workspace) picks the tool calls
                             it runs alone; the rest wait for approve/deny, and
                             mail, git push, publishing and system changes always do.
                             --wait [--timeout <s>] [--result <file>]: block until
                             the turn ends and print its summary as JSON (what
                             would ask is declined); --no-sandbox --yes confirms
                             no sandbox here; --append-system-prompt <file>
  arakun agent pending [--json]
                             tool calls headless agents wait on you for
  arakun agent approve|deny <id> <req_id>
                             answer one (it times out denied after 24 h)
  arakun agent rm <id>         delete a finished headless session
  arakun server …              arakun without a window, for a machine nobody sits
                             at: it runs agent sessions there, and paired
                             devices follow and answer them (arakun server help)
  arakun fetch [--html | --links] <url>
                             a web page as Markdown (or HTML, or its links),
                             loaded in Cloudflare Browser Run: for sites that
                             turn curl away (arakun fetch --help)
  arakun crawl [--limit N] [--depth N] <url>
                             a site's pages as Markdown, through Browser Run
  arakun show                  bring the window up, also when it was closed
                             with Keep running in the background on
  arakun quit                  quit Arakun
  arakun restart               quit and start again, on the installed build;
                             agent turns running go on by themselves after it
  arakun update [--check]      download the latest release and install it (a
                             running Arakun installs it when it quits);
                             --check only says whether there is one;
                             --channel nightly|stable picks nightly builds
                             of main (they can break) or releases
  arakun mem …                 memory agents keep across sessions: save, search,
                             get, context, list, forget (arakun mem help)
  arakun peer-review <goal>    use one review paired in Teams for this environment
                             and print the team's recommendation
  arakun github draft <owner/repo>#<n> --verdict <v> --body-file <file|-> [--head <sha>] [--agent <name>]
                             save a review as a draft for the GitHub tab (v:
                             approve, request-changes or comment; - reads stdin;
                             --head is the commit read, which an approval needs);
                             nothing is sent to GitHub until you post it there
  arakun github pilot connect <url> --token -
                             connect arakun-pilot, the pull request agent: checks
                             the Worker, then saves its settings (also status,
                             disconnect; arakun github pilot help)
  arakun dev …                 a test arakun on a virtual display, to screenshot
                             and click without touching this screen (arakun dev help)

Browser tabs of the environment (click, type, press and eval only act on
pages served from this machine: localhost, 127.0.0.1, file://):
  arakun page [-t N]                 list the browser tabs (-t N picks one for a command)
  arakun page screenshot [file]      save a PNG of the page and print where
  arakun page console                what the page logged since it loaded
  arakun page text                   the page's title, address and visible text
  arakun page open <url> | reload    load a page in the tab and wait for it
  arakun page click <selector|text>  click an element like a user would
  arakun page type <field> <text>    type into a field (CSS selector, label or placeholder)
  arakun page press <key>            press Enter, Tab, Escape, Backspace, an arrow…
  arakun page eval <js>              run JavaScript in the page and print the result

Commands act on the environment the current directory belongs to.
-s, --side shows the tab in the pane beside the active one.
-h, --help after a command prints its usage. A prompt, message or command line
that starts with a dash needs -- before it (arakun notify -- -1 test failed).
```

<!-- cli: end -->
