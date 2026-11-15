# Windows

Arakun builds for Windows 10/11 (x86_64, MSVC toolchain). Linux is still the main
platform and the only one used daily; this guide says what has been verified and what has
not.

**What was verified (from Linux):** `cargo check` and `cargo clippy --all-targets` for
`x86_64-pc-windows-gnu` with no errors, and the unit tests running as Windows `.exe` files
under Wine: 170 of 176 pass, two are ignored just as on Linux, and the other four need
`git`, which the Wine prefix did not have. The window could not be opened under Wine (gpui
uses DirectManipulation, which Wine does not implement), so **the interface has not been
tested on real Windows**. If something fails, the sections below help diagnose it.

Release builds for Windows (from GitHub Releases) contain only `arakun.exe`, built
without the `browser` feature. For browser tabs, build from source as described here.
To install the latest release, run this in PowerShell; it puts `arakun.exe` in
`%LOCALAPPDATA%\Programs\Arakun` and adds that folder to your user `PATH`:

```powershell
irm https://arakun.dev/install.ps1 | iex
```

## Requirements

- **Rust via rustup**, with the `x86_64-pc-windows-msvc` host. rustup installs the version
  in `rust-toolchain.toml` on the first `cargo` command in the checkout.
- **Visual Studio Build Tools 2022** with the *Desktop development with C++* workload
  (the MSVC compiler and a Windows SDK). `--release` needs `fxc.exe` from the Windows
  SDK: gpui compiles its HLSL shaders with it. If it is not found automatically, point
  `GPUI_FXC_PATH` at `C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64\fxc.exe`.
- **Git for Windows**: the Changes panel, worktrees and the CI graph use `git`.
- For the agents: **Node.js** (with `npm`) for the Claude Code adapter, and Factory's
  `droid` CLI for Droid. `gh` if you want the CI panel.
- For browser tabs: **CMake** and **Ninja**. They come with the *C++ CMake tools for
  Windows* component of Visual Studio Build Tools (selected by default in *Desktop
  development with C++*) and are on the `PATH` of the Developer PowerShell.

`tools\setup-dev.ps1` installs Git, the Build Tools and Rust with winget and rustup. Add
`-Relay` for Node, and `-Browser` to check for CMake and Ninja. It cannot add a workload
to a Visual Studio you already have; it tells you what to add in the Visual Studio
Installer instead. It has not been run on real Windows yet; please report what fails.

```powershell
powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1          # -Check only reports
```

There is no need to install CEF by hand: the first build downloads the CEF 156.0.15
distribution for Windows (a few hundred MB) into `.cef-sdk\` and copies its runtime
(`libcef.dll`, the `.pak` files, `locales\`...) next to `arakun.exe` in
`target\<profile>\`. gpui renders with Direct3D 11.

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

## Where things are stored

| What | Where |
|---|---|
| State, sessions, transcripts | `%LOCALAPPDATA%\arakun\` |
| Agent adapters (npm) | `%LOCALAPPDATA%\arakun\agents\` |
| Log (when started without a terminal) | `%LOCALAPPDATA%\arakun\arakun.log` |
| Branch naming agent and model (Settings › General) | `%LOCALAPPDATA%\arakun\branch-namer.json` |
| Control socket | `%LOCALAPPDATA%\arakun\control.sock` |
| `arakun.exe` for the command line | `%LOCALAPPDATA%\arakun\bin\arakun.exe` |
| Factory sessions | Credential Manager, as `arakun:<account>` |

On Linux those sessions live in the keyring (`secret-tool`); on Windows, in the Credential
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
- **Title bar:** Windows handles dragging and the minimize, maximize and close buttons as
  native system areas, not through Arakun's mouse events.
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

## Not available on Windows

- **Dynamic Island:** it needs a Wayland layer shell, so it only runs on Linux.
- **Headless agents (`arakun agent run`):** their sandbox is bubblewrap, which is
  Linux-only.
- **`arakun dev`** (the test instance in Xvfb) and the **Claude SEO** installer in
  Extensions are Linux-only; on Windows they print a notice instead of running.

## Known issues

- **Browser tabs are untested on real Windows.** They use the same off-screen CEF as on
  Linux. `libcef.dll` is delay-loaded (`/DELAYLOAD`, see `build.rs`) and only loaded when
  the first tab opens, so Arakun starts without it and the copy of `arakun.exe` in
  `%LOCALAPPDATA%\arakun\bin` does not need it. If the tab says it cannot find the
  runtime, `libcef.dll` is missing next to `arakun.exe` (build again) or `ARAKUN_CEF_DIR`
  points at another folder. The browser profile lives in `%LOCALAPPDATA%\arakun\browser\`
  and the CEF log in `cef.log` inside it.
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
- **The build fails on a C dependency:** the MSVC environment is missing (see above).
- **`Failed to find fxc.exe`:** the Windows SDK is missing, or set `GPUI_FXC_PATH`.
- **The build fails in `cef-dll-sys`:** CMake or Ninja is missing (see above), or CEF
  could not be downloaded. Without the browser, build with
  `cargo build --release --no-default-features`.
- **`LNK` errors about missing libraries:** the *Desktop development with C++* workload or
  the Windows SDK is not installed.
- **"Access denied (os error 5)" when opening a file:** Windows does not allow opening a
  file that another process has locked. Arakun does not lock its own files; if it
  happens, note which file it was and open an issue, since this is exactly the kind of
  failure that only shows up on Windows.
