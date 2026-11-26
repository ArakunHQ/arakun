# Windows

Arakun builds for Windows 10/11 (x86_64, MSVC toolchain). Linux is still the main
platform and the only one used daily; this guide says what has been verified and what has
not.

The **Connect SSH** panel for local agent sessions is available on Linux and
macOS only. The Computers tab for paired Arakun servers is separate.

**What was verified (from Linux):** `cargo check` and `cargo clippy --all-targets` for
`x86_64-pc-windows-gnu` with no errors, and the unit tests running as Windows `.exe` files
under Wine: 170 of 176 pass, two are ignored just as on Linux, and the other four need
`git`, which the Wine prefix did not have. The window could not be opened under Wine (gpui
uses DirectManipulation, which Wine does not implement), so **the interface has not been
tested on real Windows** beyond the check below. If something fails, the sections below
help diagnose it.

**What was verified on real Windows:** the GitHub tab, once, on Windows 11 with a release
build without the `browser` feature and `ARAKUN_GITHUB=readonly`. The queue, a pull
request's timeline, files, checks and commits, moving with J/K, and an approve refused as
read-only all worked, with no console windows. Real writes (comment, approve, merge,
close) and error states (no `gh`, signed out) were not tested, and neither were the rest
of `Ctrl+K`, the palettes and the notices.

Browser tabs were verified once on Windows 11 with a release build with the `browser`
feature: `arakun browse` opened a page, and `arakun page text` and `arakun page
screenshot` read and showed it. Typing, clicking, downloads and DevTools were not tested.

Release builds for Windows (from GitHub Releases) include the browser: `arakun.exe`,
`arakun-helper.exe`, `arakun.pdb` (function names for crash reports) and the CEF
runtime next to them. Links you click open in a browser
tab; Settings › General › Open links in Arakun sends them to your own browser instead.
To install the latest release, run this in PowerShell; it puts them in
`%LOCALAPPDATA%\Programs\Arakun` and adds that folder to your user `PATH`:

```powershell
irm https://arakun.dev/install.ps1 | iex
```

Windows has no built-in way to check the signature of the release's `SHA256SUMS`, so
the script checks it only when [minisign](https://jedisct1.github.io/minisign/) is on
your `PATH`. Without it, it checks only the checksum and says so. Arakun's own updater
always checks the signature.

## Requirements

- **Rust via rustup**, with the `x86_64-pc-windows-msvc` host. rustup installs the version
  in `rust-toolchain.toml` on the first `cargo` command in the checkout.
- **Visual Studio Build Tools 2022** with the *Desktop development with C++* workload
  (the MSVC compiler and a Windows SDK). `--release` needs `fxc.exe` from the Windows
  SDK: gpui compiles its HLSL shaders with it. If it is not found automatically, point
  `GPUI_FXC_PATH` at `C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64\fxc.exe`.
- **Git for Windows**: the Changes panel, worktrees and the CI graph use `git`.
- For the agents: **Node.js** (with `npm`) for the Claude Code and Codex adapters, Factory's
  `droid` CLI for Droid, the Devin CLI for Devin, the Cursor CLI (`cursor-agent`) for
  Cursor, and the Antigravity CLI (`agy`, found in `%LOCALAPPDATA%\agy\bin` too) for
  Antigravity. `gh` if you want the CI panel.
- For browser tabs: **CMake** and **Ninja**. They come with the *C++ CMake tools for
  Windows* component of Visual Studio Build Tools (selected by default in *Desktop
  development with C++*) and are on the `PATH` of the Developer PowerShell.

`tools\setup-dev.ps1` installs Git, the Build Tools and Rust with winget and rustup. Add
`-Relay` for Node, `-Browser` to check for CMake and Ninja, and `-CiTools` for
cargo-deny and zizmor. It cannot add a workload
to a Visual Studio you already have; it tells you what to add in the Visual Studio
Installer instead. It has not been run on real Windows yet; please report what fails.

```powershell
powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1          # -Check only reports
```

There is no need to install CEF by hand: the first build downloads the CEF 156.0.15
distribution for Windows (a few hundred MB) into `.cef-sdk\` and copies its runtime
(`libcef.dll`, the `.pak` files, `locales\`...) next to `arakun.exe` in
`target\<profile>\`. gpui renders with Direct3D 11. If that download fails with "the
response body is larger than request limit", fetch the archive with
`python tools\fetch-cef.py <archive> <sha1> <folder>` (the names and checksums are in
`.github\workflows\release.yml`) and set `CEF_PATH` to that folder.

## MSVC environment

Build from a terminal that has the MSVC environment loaded: **Developer PowerShell for
VS 2022**, or any terminal after running `vcvars64.bat`. In a terminal without it, `cc`
cannot find `cl`/`link` and the build fails on the first C dependency.

## Build and run

```powershell
cargo build --release        # the first build takes several minutes
target\release\arakun.exe
```

Without the browser (no CMake, Ninja or CEF download needed):

```powershell
cargo build --release --no-default-features
```

A debug `cargo build` does not need `fxc.exe` (gpui compiles the shaders at run time).

The linker may print messages in the system language (for example `Creating library ...`
in a localized form); Rust shows them as a `linker_messages` warning and they are harmless.

### The console

`arakun.exe` is a console application. If you open it with a double click (or from a
shortcut), Windows creates a console for it; Arakun detaches from it on startup and sends
its output to `%LOCALAPPDATA%\arakun\arakun.log`, so you see the console for an instant
and then only the window. If you start it from a terminal, the output stays in that
terminal.

The browser's child processes (GPU, renderers, utilities) are started through
`arakun-helper.exe`, which is built next to `arakun.exe` when the `browser` feature is on
and is a windowed application, so they open no consoles. If it is missing, CEF starts
`arakun.exe` again and every process flashes a console. Ship both when distributing a
build with the browser.

## The server

`arakun server` (and `arakun-server`) run on Windows, but `service install`, `status`,
`logs` and `uninstall` do not: they print an error and exit. To keep the server running,
start `arakun-server serve --service` from Task Scheduler, with the trigger "At log on".
With `--service` a server that is not signed in yet stays up and waits for
`arakun-server login`; without it, started in a console, it says so and exits.

## Where things are stored

| What | Where |
|---|---|
| State, sessions, transcripts | `%LOCALAPPDATA%\arakun\` |
| That the first-launch setup was done | `%LOCALAPPDATA%\arakun\state.onboarding.json` |
| Agent adapters (npm) | `%LOCALAPPDATA%\arakun\agents\` |
| Log (when started without a terminal) | `%LOCALAPPDATA%\arakun\arakun.log` |
| Agent logs and the usage log, each with one older copy once full | `%LOCALAPPDATA%\arakun\agents\<agent>.log` and `<agent>.old.log`; `%LOCALAPPDATA%\arakun\usage.jsonl` and `usage.old.jsonl` (or the file `ARAKUN_USAGE_LOG` names and its `.old` copy) |
| Sessions the startup clean-up has seen listed | `%LOCALAPPDATA%\arakun\state.json.seen`; with `ARAKUN_STATE` set, `<state file>.seen` next to it |
| Branch naming agent and model (Settings › Branches) | `%LOCALAPPDATA%\arakun\branch-namer.json` |
| Where links open (Settings › General) | `%LOCALAPPDATA%\arakun\open-links` |
| Archive after merge (Settings › Branches), and the merges archived | `%LOCALAPPDATA%\arakun\settle-after-merge`; by default `%LOCALAPPDATA%\arakun\settled.json`, with `ARAKUN_STATE` set `settled.json` next to that state file |
| Remove old environments (Settings › Branches) | `%LOCALAPPDATA%\arakun\env-cleanup` |
| Linear sign-ins (personal API keys) | `%LOCALAPPDATA%\arakun\linear\sessions.json` |
| Doop sign-ins, canvases added by link, and cached renders | `%LOCALAPPDATA%\arakun\doop\sessions.json`, `%LOCALAPPDATA%\arakun\doop\pins.json` and `%LOCALAPPDATA%\arakun\cache\doop\` |
| Control socket | `%LOCALAPPDATA%\arakun\control.sock` |
| Lock held by the running Arakun (a second `arakun` hands off to it and exits) | `%LOCALAPPDATA%\arakun\state.json.lock`; with `ARAKUN_STATE` set, `<that file>.lock` next to it |
| `arakun server`: its settings, sign-in (`account.json`), sessions and socket | `%LOCALAPPDATA%\arakun\server\` |
| Servers the app is paired with or reaches through your account (Computers tab) | `%LOCALAPPDATA%\arakun\servers.json` |
| What each paired server listed last | `%LOCALAPPDATA%\arakun\servers-inbox.json` |
| `arakun.exe` for the command line | `%LOCALAPPDATA%\arakun\bin\arakun.exe`; with `ARAKUN_STATE` set, `bin\arakun.exe` in a folder next to that state file (`dev.runtime` for `dev.json`) |
| Factory sessions | Credential Manager, as `arakun:<account>` |
| Account session (the Profile dialog, the Phone tab's relay, the Computers tab) | Credential Manager, as `arakun:teams-session` |
| Teams device keys (Teams tab) | Credential Manager, as `arakun:teams-device-<account>-<scope>`; scope identifies the server and profile |
| Teams keys and trusted devices, sealed | `%LOCALAPPDATA%\arakun\teams-<account>-<scope>.keys`, or next to the `ARAKUN_STATE` file. The old `teams.keys` and credential are preserved when a verified device migrates. |
| GitHub tab settings, review drafts | `%LOCALAPPDATA%\arakun\github.json`, `%LOCALAPPDATA%\arakun\github\drafts\` |
| The PR agent's cards you saw and dealt with (arakun-pilot) | By default `%LOCALAPPDATA%\arakun\pilot.json`; with `ARAKUN_STATE` set, `pilot.json` next to that state file |
| arakun-pilot's Worker address and token | By default `%USERPROFILE%\.config\arakun-pilot\client.json` (`%XDG_CONFIG_HOME%\arakun-pilot\client.json` when set to an absolute path); with `ARAKUN_STATE` set, `arakun-pilot\client.json` in the folder of that state file |
| Notice history, saved palettes, `Ctrl+K` rules | `notices.json`, `palettes.json`, `rules.json` in `%LOCALAPPDATA%\arakun\` |
| CLIProxyAPI management key (Usage tab) | Credential Manager, as `arakun:cliproxy-management-key` |
| The proxy that key is for, a typed address and its accounts' last limits (Usage tab) | `cliproxy-management-key`, `cliproxy-address`, `cliproxy-plan.json` in `%LOCALAPPDATA%\arakun\` |
| Claude seats' tokens (`arakun seat add`), one file per seat | `%LOCALAPPDATA%\arakun\seats\<name>` |
| Crash reports and usage counts: the random install id, and your answer (Settings › Privacy) | `telemetry-id`, `telemetry.json` in `%LOCALAPPDATA%\arakun\` |
| Redacted mode, while it is on (Settings › Privacy) | `redacted` in `%LOCALAPPDATA%\arakun\` |
| Updates: the version waiting, the lock, the switch while it is off, the download (Settings › General) | `update.json`, `update.lock`, `no-auto-update`, `update-channel`, `update-channel.lock`, `update-download\` in `%LOCALAPPDATA%\arakun\` |
| A downloaded update, until Arakun quits | `.arakun.exe.update` and `.arakun.exe.update.pdb` next to `arakun.exe` |

On Linux those sessions live in the keyring (the Secret Service); on Windows, in the Credential
Manager (Windows Credentials, Generic Credentials).

## Differences from Linux

- **Terminals:** ConPTY. The shell is `pwsh` (PowerShell 7) if it is on the `PATH`,
  otherwise Windows PowerShell.
- **Environment scripts and shell Automations** run in PowerShell (`-Command`), not in
  `sh`. A setup script written for `sh` has to be rewritten, or called with
  `bash -lc '...'` if you have Git Bash.
- **Project checks** run with `cmd.exe /C`, so `cargo test` or
  `npm run typecheck && npm test` work the same; anything that is `sh` syntax does not.
- **`arakun run` and project scripts** open their terminal in PowerShell, at
  *BelowNormal* priority, and print `[exit N]` when done, as on Linux.
- **Time limits on `git` and `gh`:** the same as on Linux. Calls that only read the
  repository may run for 60 s, calls to a remote for 2 min, and work you start that runs
  hooks (commit, push, pull, merge, clone, adding a worktree) for 30 min. On Linux and
  macOS a call past its limit gets 2 s to clean up before it is killed. Windows has no
  such request, so there it is killed at once, and git may leave a lock file behind.
- **Title bar:** Windows handles dragging and the minimize, maximize and close buttons as
  native system areas, not through Arakun's mouse events. The buttons look different too:
  on Windows each is 40 px wide, as tall as the title bar and square, after a thin line
  that separates them from the bar; on Linux and macOS they are 28 px squares with
  rounded corners (8 px at the default roundness). This comes from the code
  (`src/shell/titlebar.rs`) and has not been checked on real Windows.
- **Private files:** on Linux and macOS, `notices.json` (it can hold agent replies),
  `cliproxy-plan.json` and the Claude seats' tokens in
  `seats\` are written owner-only (mode 0600, and 0700 for a folder it creates).
  Windows has no such modes, so there they get the default access rules of
  `%LOCALAPPDATA%\arakun\`. A seat's token signs in to that Claude subscription, so
  check those rules before you add one.
  `github.json`, `github\drafts\` and `palettes.json` are not written owner-only on any
  platform. By default `%LOCALAPPDATA%` is in your profile, and only your account,
  `SYSTEM` and the Administrators can read it. To check who can read the folder and the
  files in it, or to take away the access it inherits and give full access to your
  account and `SYSTEM`, run in PowerShell:

  ```powershell
  icacls "$env:LOCALAPPDATA\arakun" /T
  icacls "$env:LOCALAPPDATA\arakun" /inheritance:r /grant:r "${env:USERNAME}:(OI)(CI)F" "SYSTEM:(OI)(CI)F"
  ```

  The second command does not make the folder private by itself. `/inheritance:r`
  removes only inherited entries, and `/grant:r` changes only the accounts it names, so
  an entry set directly on the folder or a file for any other account (Administrators
  included) stays. Run the first command again: if it lists an account you don't want,
  remove what it grants with `icacls "<path>" /remove:g "<account>"`, on the folder and
  on each subfolder (such as `github\drafts`) and file that lists it. `/remove:g` keeps
  a `(DENY)` entry; a plain `/remove` would drop it too, and the account could then read
  through a group it is in. A subfolder or file that shows no `(I)` entry for your
  account does not inherit from the folder, so the second command gave you nothing
  there: before you take a group such as `Users` off it, give your account access, or
  you lose it too. On a subfolder use
  `icacls "<path>" /grant "${env:USERNAME}:(OI)(CI)F"`, so the files in it inherit it;
  on a file, `icacls "<path>" /grant "${env:USERNAME}:F"`. When only inherited
  entries gave it, the second command also drops the Administrators' access, which some
  backup tools need.
- **Command line (`arakun browse`, `arakun run`, ...):** the same as on Linux. Arakun's
  terminals and agents have `%LOCALAPPDATA%\arakun\bin` on their `PATH`, with a hard link
  to `arakun.exe` (or a copy if the disk does not support hard links). The socket is
  AF_UNIX, which Windows 10 1803+ supports.
- **Agents:** they run inside a Job Object, so closing a session (or Arakun) kills the
  agent's whole process tree, like the process group on Linux. Arakun looks for `droid`,
  `claude` and `npm` on the `PATH`, in `~\.local\bin`, `~\bin` and `%APPDATA%\npm`, with
  the extensions in `%PATHEXT%`.
- **Branch naming:** each naming run works in a new folder in the temporary folder
  (`%TMP%`, else `%TEMP%`). On Linux and macOS Arakun makes that folder owner-only (mode
  0700). On Windows it only keeps the access rules of the temporary folder: by default
  that is in your profile and private to you, but a `TMP` or `TEMP` set to a shared
  folder may let other users open it.
- **Notifications:** Windows toasts through PowerShell; they appear under the name
  "Windows PowerShell".
- **Opening external files and links:** with the Windows default program.
- **Updates:** a release build downloads a new release as `.arakun.exe.update` next to
  `arakun.exe`. Windows does not let a running `.exe` be replaced, so when Arakun quits
  it renames `arakun.exe` to `.arakun.exe.old` and puts the new one in its place. Then
  it restarts the Dynamic Island with the new version. A later start deletes the old
  file once nothing runs it; an old file still in use is moved aside, with a number
  after its name, and deleted the same way. The release's `arakun.pdb` waits as
  `.arakun.exe.update.pdb` and replaces `arakun.pdb` right after the new `.exe` is in
  place. If that move fails, the update still goes through, and crash reports lack
  function names until the next one. A release without a `.pdb` updates the same way.
  Settings has no **Restart now** button on Windows: quit Arakun and open it again. The
  dialog the `update-policy` feature flag opens (see "Updates" in the README) says
  **Quit and update** instead of **Restart now**: it quits Arakun, which installs the
  release, and you open it again. A forced one can't be closed: it offers that, **Quit
  Arakun**, and **Try again** after a failed download. Not tested on real Windows yet.
- **Dynamic Island:** an always-on-top window at the top center of the main display's
  work area, where Linux uses a layer-shell surface. Clicks around its shape reach the
  windows below, and clicking it never takes the focus from the window you type in.
  Arakun starts `arakun island-host` when it opens, and the island stays up after
  Arakun closes. Its socket is `island.sock` in `%LOCALAPPDATA%\arakun`, next to
  `control.sock`, and `arakun island …` works as on Linux. It shows no headless agents,
  since they don't run on Windows, and no notifications from other apps. After a
  change of the display's scale in Settings, the island keeps its old size until it
  restarts: Windows doesn't tell it about the change.
- **Agents outside Arakun on the island (Windows only for now):** Claude Code running
  in another terminal or an editor can show on the island through its hooks.
  `arakun island hook` reads the JSON a hook gets on stdin. It lists the session while
  it works, and shows a card when it waits for you or finishes. It never fails the
  hook, even when the island is not running. Add this to `~\.claude\settings.json`,
  with your own path to `arakun.exe`:

  ```json
  {
    "hooks": {
      "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "\"C:/Users/<you>/AppData/Local/arakun/bin/arakun.exe\" island hook" }] }],
      "PostToolUse": [{ "hooks": [{ "type": "command", "command": "\"C:/Users/<you>/AppData/Local/arakun/bin/arakun.exe\" island hook" }] }],
      "Notification": [{ "hooks": [{ "type": "command", "command": "\"C:/Users/<you>/AppData/Local/arakun/bin/arakun.exe\" island hook" }] }],
      "Stop": [{ "hooks": [{ "type": "command", "command": "\"C:/Users/<you>/AppData/Local/arakun/bin/arakun.exe\" island hook" }] }],
      "SessionEnd": [{ "hooks": [{ "type": "command", "command": "\"C:/Users/<you>/AppData/Local/arakun/bin/arakun.exe\" island hook" }] }]
    }
  }
  ```

  `arakun island hook <name>` shows another name than "Claude Code". Any other agent
  can tell the island by hand:
  `arakun island session <id> <working|waiting|done|error|ended> [project] [doing]`.
  "Ver" on such a card only puts it away, since Arakun has nothing to open for it.

## Not available on Windows

- **Headless agents (`arakun agent run`):** their sandbox is bubblewrap on Linux
  and Seatbelt on macOS; Windows has neither.
- **`arakun dev`** (the test instance in Xvfb) and the **Claude SEO** installer in the
  Marketplace tab are Linux-only; on Windows they print a notice instead of running.

## Known issues

- **Browser tabs are barely tested on real Windows** (see the top of this page). They use
  the same off-screen CEF as on Linux. `libcef.dll` is delay-loaded (`/DELAYLOAD`, see `build.rs`) and only loaded when
  the first tab opens, so Arakun starts without it and the copy of `arakun.exe` in
  `%LOCALAPPDATA%\arakun\bin` does not need it. If the tab says it cannot find the
  runtime, `libcef.dll` is missing next to `arakun.exe` (build again) or `ARAKUN_CEF_DIR`
  points at another folder. The browser profile lives in `%LOCALAPPDATA%\arakun\browser\`
  and the CEF log in `cef.log` inside it (`cef.previous.log` is the run before).
- **The terminal tab name does not follow the foreground program.** On Linux Arakun reads
  it from the PTY (`tcgetpgrp`); ConPTY does not expose it, so the tab keeps the shell's
  name.
- **Primary selection** (select to copy, middle click to paste) does not exist on Windows;
  in the terminal use `Ctrl+Shift+C`/`Ctrl+Shift+V`.
- **No tests run on Windows in CI.** Every pull request that changes Rust code runs
  Clippy on Windows, so it must compile there, but the tests run only on Linux and macOS.
  Only the release workflow makes a release build, which also compiles the shaders.

## If something fails

- **The window does not open or closes by itself:** start it from a terminal with
  `$env:RUST_BACKTRACE = 1; target\release\arakun.exe` and read the output, or check
  `%LOCALAPPDATA%\arakun\arakun.log`.
- **`could not open its window` with Direct3D in the error:** no graphics device could
  be used. Started from the Start menu or Explorer, this shows as a notification too, and
  the whole error is in `arakun.log`. Update the graphics driver, or turn on 3D acceleration in a VM. If the error
  names DirectComposition, set `GPUI_DISABLE_DIRECT_COMPOSITION=1`.
- **The build fails on a C dependency:** the MSVC environment is missing (see above).
- **`Failed to find fxc.exe`:** the Windows SDK is missing, or set `GPUI_FXC_PATH`.
- **The build fails in `cef-dll-sys`:** CMake or Ninja is missing (see above), or CEF
  could not be downloaded. Without the browser, build with
  `cargo build --release --no-default-features`.
- **`LNK` errors about missing libraries:** the *Desktop development with C++* workload or
  the Windows SDK is not installed.
- **A git command fails with `Unable to create '...\.git\index.lock': File exists`:**
  a git call that ran past its time limit was killed before it could remove its lock
  file. If no other git is running in that repository, delete the file the error names
  (usually `.git\index.lock`, or a `.lock` file under `.git\refs\`) and try again.
- **"Access denied (os error 5)" when opening a file:** Windows does not allow opening a
  file that another process has locked. The running Arakun locks only
  `state.json.lock`, which nothing needs to read. If it happens with another file,
  note which file it was and open an issue, since this is exactly the kind of failure
  that only shows up on Windows.
