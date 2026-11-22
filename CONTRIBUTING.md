# Contributing to Arakun

Thanks for helping out. Issues, ideas and pull requests are all welcome.

## Ways to help

- **Report a bug** with the [bug report form](https://github.com/arakunhq/arakun/issues/new/choose).
  Steps to reproduce, the version or commit, and the platform help most. Run `arakun`
  from a terminal with `RUST_BACKTRACE=1` for the output, or on Windows read
  `%LOCALAPPDATA%\arakun\arakun.log`. Remove tokens and private paths first.
- **Suggest a feature** with the feature request form. Say what gets in the way today,
  not only the fix you have in mind.
- **Try Arakun on Windows or macOS.** Linux is the only platform used every day, and
  CI only compiles the Windows build, without running its tests (see
  [docs/windows.md](docs/windows.md)). Reports of what works there are useful too, not
  only of what breaks.
- **Send a pull request.** For anything bigger than a small fix, open an issue first so
  we can agree on the approach before you spend time on it.

**Security problems:** do not open a public issue. Follow [SECURITY.md](SECURITY.md).

## Contributor License Agreement

First-time contributors are asked to sign our [CLA](CLA.md). That includes anyone
named in a `Co-authored-by` line, except coding agents such as Claude Code. A bot
comments on your first pull request, and signing is a reply with only this
sentence:

```
I have read the CLA Document and I hereby sign the CLA
```

In short: **you keep the copyright to your work** and grant the maintainer the
right to distribute it under other terms too (for example a commercial license).
The released code stays AGPL-3.0; the CLA is what keeps dual licensing possible
without chasing every past contributor. Unsigned pull requests can't be merged.

## Setting up

A script installs what you need to build Arakun and run the checks CI runs: the
system libraries, Rust with clippy, and with the flags below Node for the relay and the
[Git hooks](#git-hooks). It asks before it installs anything, and you can run it again
safely.

```sh
tools/setup-dev.sh --hooks             # Linux (apt or pacman) and macOS
tools/setup-dev.sh --check             # only report what is missing
```

```powershell
# Windows (winget; Visual Studio Build Tools asks for administrator rights)
powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1 -Hooks
```

`--hooks` (`-Hooks`) runs `lefthook install`; on Windows it installs lefthook with winget
first, and elsewhere it says how to install it if it is missing.
`--relay` (`-Relay`) adds Node 24 and the relay's npm packages, `--display` adds Xvfb and
tesseract for `arakun dev` (on macOS only tesseract, through Homebrew), `--browser`
fetches the CEF SDK a macOS build with the browser needs and links it at `.cef-sdk`,
`--build` (`-Build`) builds the tests once, and `--yes`
(`-Yes`) never asks. `--help` (`-Help`) lists them all. To do it by hand instead,
follow [Build from source](README.md#build-from-source) in the README.

**Coding agents** run `tools/setup-dev.sh --yes --relay` (on Windows,
`tools\setup-dev.ps1 -Yes -Relay`). In Claude Code on the web, `.claude/settings.json`
already runs it when a session starts. For other hosted agents, such as Codex cloud, put it
in the environment's setup script.

For most work, build **without the browser**. You do not need CEF, and it is what CI
does:

```sh
cargo run --no-default-features
```

You only need the `browser` feature (and CEF) if you work on `src/browser` or
`arakun page`. Dependencies are optimized in dev builds as well, so the first build takes
a while and later ones are quick.

The phone relay in [`relay/`](relay) is a separate TypeScript project. It needs Node 24
or newer; see [relay/README.md](relay/README.md). So is the teams Worker in
[`teams/`](teams); see [teams/README.md](teams/README.md).

### Features you can ignore

`team` and `coderlabs` are private parts that are not in the public build. Leave them
off, and do not add code behind them.

## Checks

CI runs the checks below that a pull request can affect. A change to docs, `iso/` or
`tools/` (apart from `tools/rustfmt.sh` and the layout audit) alone skips the Rust
jobs, the relay job runs only when `relay/`, `phone/` or CI itself changes, the teams
job only when `teams/` or CI itself changes, cargo-deny runs only when `Cargo.toml`,
`Cargo.lock` or `deny.toml` changes, and zizmor runs only when `.github/` changes.

CI is the gate, not your machine. Before you push, run the quick checks for what you
changed: `tools/rustfmt.sh --check` and clippy for Rust, and the tests of the modules
you touched (`cargo test --locked --no-default-features <module>`, for example
`terminal` or `design_lint` for a UI change). The full test suite, cargo-deny, zizmor
and the other platforms can be left to CI on your draft pull request; fix what it
finds there. These are the commands CI runs:

```sh
# the app and core/ (with CEF set up, you can leave out --no-default-features)
tools/rustfmt.sh --check
cargo clippy --locked --all-targets --no-default-features -- -D warnings
cargo test --locked --no-default-features

# server/ or core/ changed
cargo clippy --locked -p arakun-server -- -D warnings

# Cargo.toml, Cargo.lock or deny.toml changed (cargo install --locked cargo-deny)
cargo deny check

# relay/ or phone/ changed
(cd relay && npm ci && npx tsc --noEmit && npx vitest run)
(cd phone && node --test "test/*.test.mjs")

# teams/ changed
(cd teams && npm ci && npx tsc --noEmit && npx vitest run)

# .github/ changed (brew install zizmor, or pipx install zizmor)
zizmor .github
```

Some notes:

- The code the app shares with `arakun-server` and the examples is the `arakun-core`
  library in `core/`: agents, the phone, the server, the terminal, the browser, and the
  platform, theme and UI kit they draw with. The app (`src/`), `arakun-server` and the
  examples are thin binaries over it. The app names its modules as its own
  (`use arakun_core::{agent, ...}` in `src/main.rs`), so `crate::agent` works in both.
  What the binaries answer differently (the memory prompt, the control socket), core
  asks through `core/src/host.rs`. A core item the app uses has to be `pub`. A
  dependency more than one package uses goes in `[workspace.dependencies]` in the
  root `Cargo.toml`.
- `arakun-server` (in `server/`) is the app's server built for a machine with no
  display: a package of its own, against GPUI without Wayland and X11. Its code and
  tests are in `core/` (`core/src/server/`), so `cargo test` covers them. CI also
  builds it alone and fails if the binary needs a desktop library
  (`tools/check-server-libs.sh`).

- The code is formatted with rustfmt's default settings. `tools/rustfmt.sh` formats
  it; it runs `cargo fmt`, which on its own stops at the private modules this
  repository does not have. Format-on-save in your editor is fine.
- The Rust version is pinned in `rust-toolchain.toml`, so a new stable release does not
  bring new lints or formatting into your pull request. Bump it in its own pull request
  that also fixes what the new version changes. A weekly workflow opens an issue when
  clippy fails on the latest stable.
- `--locked` fails if `Cargo.lock` would change. If you add or update a dependency,
  commit the new `Cargo.lock` in the same pull request.
- `cargo deny check` reads [`deny.toml`](deny.toml). It fails on a crate with a
  RustSec advisory, a license that is not allowed, or a git source that is not listed.
  It also runs on `main` every day, because new advisories appear without any change.
- `zizmor .github` checks the workflows for security problems. Pin every action to a
  full commit SHA with its version as a comment (`uses: owner/action@<sha> # v1.2.3`);
  Dependabot updates both. Give `actions/checkout` `persist-credentials: false` unless
  the job pushes, and a job only the `permissions:` it uses. If a finding is safe
  where it is, add `# zizmor: ignore[<rule>]` to the line it points at and say why.
  Set `GH_TOKEN` to also run the audits that look up the actions online.
- Warnings fail clippy. Fix what causes them. Use `#[allow(...)]` only with a comment
  that says why the lint is wrong in that place.
- Tests marked `#[ignore]` read this machine's own state (Claude Code, the Cloudflare
  sign-in and so on), so CI skips them. Run one with `cargo test <name> -- --ignored` if
  you work on that part.
- The phone tests check the phone's encryption against vectors the Rust side writes. If
  you change the format in `core/src/phone`, update both sides.

### Git hooks

[`lefthook.yml`](lefthook.yml) runs these checks for you, only for the parts you
changed. Install [lefthook](https://lefthook.dev/installation/) (for example
`brew install lefthook`, `npm install -g lefthook` or `winget install evilmartians.lefthook`),
then turn the hooks on in your checkout:

```sh
lefthook install
```

- **On commit:** clippy if Rust files changed, the relay's types if `relay/` changed,
  the teams Worker's types if `teams/` changed, and the phone tests if `phone/` or
  `core/src/phone/` changed. They check the files on disk, not only what you staged.
- **On push:** `cargo test`, the relay tests and the teams tests, if the commits you
  push change those parts.
- **On the commit message:** the Conventional Commits check (see
  [Pull requests](#pull-requests)).

Skip the hooks for one command with `LEFTHOOK=0 git commit ...`, or one job with
`LEFTHOOK_EXCLUDE=clippy git commit ...`. To leave the full test suite to CI, push with
`LEFTHOOK_EXCLUDE=cargo-test git push`. Put your own changes in `lefthook-local.yml`,
which git ignores. Turn the hooks off with `lefthook uninstall`.

### Benchmarks

[`tools/bench/bench.py`](tools/bench/bench.py) measures Arakun. CI doesn't run it on
pull requests. Run it when a change could make the app slower, and put the `compare`
table in the pull request.

```sh
cargo build --release --locked --no-default-features
tools/bench/bench.py perf --out target/bench/after.json     # frame times, CPU, memory
tools/bench/bench.py compare target/bench/before.json target/bench/after.json --markdown
tools/bench/bench.py agent --tasks fix-bug --model haiku    # Claude Code on a task, in Arakun
```

`perf` plays scenarios (palette, sidebar, zoom, scrolling, a terminal flood, on Linux a session
at work) in a test Arakun (`arakun dev`, Linux and macOS) and reads the time GPUI took
for each frame.
`agent` gives Claude Code the coding tasks in `tools/bench/tasks/`, in Arakun and, with
`--runner arakun,claude`, as bare `claude -p`, and scores each run with tests the
agent never saw. It uses your Claude account or API key, so it costs money. The
[`bench` skill](.claude/skills/bench/SKILL.md) has the details.

To compare two releases, start the **Benchmark releases** workflow from the Actions
tab, or with `gh`. It downloads both Linux builds, runs `perf` on one after the other
on the same runner and puts the `compare` table on the run's summary page. The run
fails when the newer release is slower past the threshold.

```sh
gh workflow run bench-releases.yml -f before=v0.10.0 -f after=v0.11.1
gh workflow run bench-releases.yml            # the latest release against the one before
tools/bench/releases.sh v0.10.0 v0.11.1       # the same on your own Linux machine
```

## Writing code

- **Tests** go in a `#[cfg(test)] mod tests` at the bottom of the file they test. Add a
  test for a bug fix where you can, and for logic that does not need a window (parsing,
  state, git, the control protocol).
- **Style.** rustfmt with its default settings, checked by CI (see [Checks](#checks)).
  Beyond what rustfmt decides, match the code around you.
- **Comments** say why, not what. A new module starts with a `//!` comment that says what
  it is for. Write plain, short sentences, as in the existing code and docs.
- **New dependencies:** prefer pure Rust crates and crates GPUI already pulls in. Put a
  comment above each new entry in `Cargo.toml` that says what uses it, as the other
  entries have. Its license, and those of the crates it pulls in, must be on the
  allow-list in [`deny.toml`](deny.toml): Arakun is also offered under commercial licenses,
  so GPL, LGPL and AGPL crates can't be used, and MPL-2.0 is allowed one crate at a
  time. Ask in the issue before you add a license to the list.
- **Platforms.** Code for one platform goes behind `cfg(...)`, and the others must still
  build. If you can, run clippy for another target too (for example
  `--target x86_64-pc-windows-gnu`). In the pull request, say which platforms you tested
  on.
- **Docs.** If you change a command, a flag, a shortcut or a storage path, update the
  README (and [docs/windows.md](docs/windows.md) if it is different on Windows).

### UI changes

- **Use the design system.** Colors, grays, text sizes and fonts come from `theme` and
  `ui` (`theme::text_muted()`, `theme::border()`, `ui::TEXT_SM`, `theme::FONT_MONO`...).
  A test in `src/design_lint.rs` fails on hand-written values where the system has a
  name for them. The **Design system** tab (Settings → Design system → Open) shows every
  token and component live.
- **See your change.** `arakun dev start` runs a second, fresh Arakun from your build,
  with data of its own. On Linux it runs on a virtual display (it needs `Xvfb`). On
  macOS it opens a window that never takes the focus, and `dev shot` has it render
  that window itself, so it needs no Screen Recording permission. Both need
  `tesseract` for `dev text` and clicking by text. Input goes into the test Arakun
  directly, so it never touches your pointer, your keyboard or your own Arakun:

  ```sh
  cargo build --no-default-features
  ./target/debug/arakun dev start          # this checkout as its project
  ./target/debug/arakun dev click "Settings"
  ./target/debug/arakun dev shot after.png
  ./target/debug/arakun dev stop
  ```

  `arakun dev` with no arguments lists every subcommand.
- **Check the drawn layout.** `tools/layout-audit.sh` opens the main screens in a test
  Arakun (as above, so Linux with `Xvfb`) with `GPUI_LAYOUT_AUDIT` set, and
  `tools/layout_audit.py` checks the boxes GPUI drew: corners that don't nest, uneven
  insets, children cut by a border. It fails on a problem and on a screen it could not
  read, so an empty or stale dump never passes. CI does not run it, since it needs the
  app on a display; a test in `src/design_lint.rs` checks the script itself.

  ```sh
  cargo build --locked --no-default-features
  tools/layout-audit.sh
  ```
- **Add a screenshot** to the pull request, before and after if something changed.
  Drag it into the description. With write access to
  [ArakunHQ/pr-assets](https://github.com/ArakunHQ/pr-assets), `tools/pr-assets.sh
  <file>...` uploads it there instead and prints the Markdown to paste; agents do
  this, since GitHub has no API to upload images. That repository is public: no
  real accounts, tokens, email addresses or home-directory paths in the picture.

### Words in the interface

One thing has one name in everything a person reads: labels, tooltips, messages,
the README. Use these:

| Say | For | Not |
|---|---|---|
| **environment** | a checkout to work in: the main one or a git worktree, on a branch, with its own sessions, terminals and browser tabs | worktree. Say "git worktree" only to explain what an environment is on disk. |
| **session** | one talk with an agent, and what is in it | chat, conversation (GitHub's own Conversation tab keeps its name) |
| **palette** | what `Ctrl+K` opens | Go to, Find anything. The sidebar's Search button opens it. |
| **color scheme** | a set of colors in Customize | palette, which is the above |
| **Workspace panel** | the panel on the right of a session | workspace on its own |
| **notice** | something Arakun tells you, on the island and in the Inbox | notification, alert. A notice can also show as a desktop notification, and GitHub's notifications keep their name. |
| **check** | the project's check command, run after a turn that edits files | |
| **checks** | GitHub's checks on a pull request | CI, when you mean the checks |

Stored names stay as they are (`palettes.json`, `arakun notices`, `~/arakun-workspaces`).
A test in `src/words_lint.rs` fails on a retired word in a string of prose; a phrase
that keeps one on purpose goes on its allow-list.

## Pull requests

- **One fix or feature per pull request.** Split unrelated cleanups into their own.
- **Fill in the template:** what changed and why, the issue it fixes, and how you
  checked it (the commands you ran and what you tried by hand).
- **Commit messages** follow [Conventional Commits](https://www.conventionalcommits.org/):

  ```
  <type>(<scope>): <what the commit does>

  <why, if it is not obvious>
  ```

  - **type:** `feat` (a new feature), `fix` (a bug fix), `docs`, `refactor` (no change
    in behavior), `perf`, `test`, `build` (Cargo, dependencies, tools), `ci`, `chore`
    or `revert` (`revert: <the title of the change you undo>`).
  - **scope** (optional): the part of Arakun it touches, usually the module or folder:
    `terminal`, `agent`, `browser`, `phone`, `relay`, `teams`, `iso`, `windows`...
  - **description:** imperative and lower case, with no period at the end, for example
    `fix(terminal): keep the tab name on resize` or
    `feat(phone): answer permission prompts`.
  - **Breaking changes** (a CLI flag, a setting or a stored file format that no longer
    works as before): add `!` after the type or scope, and a `BREAKING CHANGE:` line in
    the body that says what to do instead.

  Give the pull request title the same format. Pull requests are squashed, so the title
  becomes the commit message on `main`. The **Pull request title** check fails on a title
  that does not follow it; edit the title and the check runs again by itself. GitHub's
  Revert button writes `Revert "…"`, so reword that to `revert: …`.

  Commits inside the pull request are not checked, because they do not reach `main`. To
  check them as you write them, turn on the [Git hooks](#git-hooks).
- **Open it as a draft** (`gh pr create --draft`, or "Create draft pull request" on
  GitHub). Nobody is asked to review a draft, so you can push while CI and cubic (the
  review bot) run: cubic reviews drafts too, so its comments are usually waiting when the
  checks are. Once the required checks pass, mark it ready with `gh pr ready` or the
  button on GitHub. `tools/wait-checks.sh` waits for them, and for the macOS job only
  when the diff touches macOS code. Don't wait for the other checks (macOS for other
  changes, CodeQL), which can queue for a long time. Don't wait for the **Human approval**
  status either: for a pull request from outside the team it is required, but it only
  resolves after a person approves, and marking ready is what asks for the review. It
  stays a merge gate, not a pre-ready one. The Reviewers workflow asks people to review
  only once the pull request is ready, cubic has finished and you have resolved or
  answered every cubic comment, and not at all if cubic approved and every review thread
  is resolved. A maintainer who runs it by hand skips the wait for cubic and for its
  comments. A pull request from outside the team always needs a
  maintainer's approval: a workflow dismisses cubic's, and the **Human approval**
  status waits for a person.
- **Answer every cubic comment.** Fix it and push, or reply with the reason you
  won't. cubic reviews each push and resolves the comments it sees fixed. It labels
  each comment P0 to P3, and approves a change only once all its comments are
  resolved. When asked, cubic can also fix its own comments and push the fix to your
  branch, so pull before you push. When a commit you pushed fixes a comment it left
  open, resolve that thread yourself, right after the push. Only resolve what you fixed:
  leave a comment you declined open for the reviewer. If cubic approves with comments
  still open, a workflow dismisses its approval and asks a person, who resolves them
  and approves. To have cubic review again, comment `@cubic-dev-ai review`. Its review
  rules are in [cubic.yaml](cubic.yaml); cubic reads that file from `main`, so a
  change to it takes effect once merged. If you change a rule in this guide, check the
  rules there too.
- **Keep the branch up to date** with `main`, and make sure the required checks pass before you
  ask for a review. If a check fails and you think it is not your change, say so in the pull
  request.
- **A version bump** also adds the release's changelog entry,
  `release-notes/v<version>.md`, written for people who use the app. The **Changelog
  entry** check fails the pull request without it. See
  [release-notes/README.md](release-notes/README.md).
- **Stack pull requests** when one step needs another that is not merged yet. See
  [Stacked pull requests](#stacked-pull-requests).
- **Coding agents are welcome.** Arakun is a tool for working with them, after all. You
  are still responsible for the change: read the diff, run the checks, and make sure you
  can explain it. [AGENTS.md](AGENTS.md) gives agents the rules from this guide
  (`CLAUDE.md` loads it for Claude Code). If you change a rule here, change it there too.

You don't need to set labels. A workflow gives pull requests area labels
(`area: relay`, `area: ci`, ...) from the paths they change, and issues from
their title prefix (`CI:`, `Relay:`) or the issue form's Area field. The release
notes are grouped by these labels. The labels themselves live in
`.github/labels.yml`.

### Stacked pull requests

A stack is a chain of pull requests where each one builds on the one below it. The
bottom one targets `main`. Each one above targets the branch below it, so its diff
shows only its own step.

**Stack** when a change splits into steps that each make sense on their own, and a later
step needs an earlier one. For example, a refactor and then the feature that uses it, or
a new `ui` component and then the views that use it. Each step is reviewed on its own,
and you can keep working without waiting for the first one to merge.

**Don't stack:**

- Changes that don't depend on each other. Branch each one from `main`, so each can
  merge without the others.
- On a pull request whose approach is still being discussed. If that changes, you must
  rewrite everything above it.
- More than about four deep. Get the bottom ones merged first.

**How:**

1. Name each branch `pr/<topic>` and branch it from the one below:
   `git switch -c pr/feature pr/refactor`.
2. Open it as a draft against the branch below: `gh pr create --draft --base pr/refactor`.
   Write `Stacked on #<number>.` at the end of Changes, so reviewers read the stack in
   order.
3. To change a lower pull request, commit on its branch. Then rebase the top branch on
   the branch you changed. `--update-refs` (git 2.38 or newer) moves every branch in
   between with it, except a branch checked out in another worktree; switch that
   worktree to another branch first. Push every branch above the one you changed:

   ```sh
   git switch pr/top
   git rebase --update-refs pr/refactor
   git push --force-with-lease origin pr/feature pr/top
   ```

4. Merge from the bottom, always into `main`. Don't merge a pull request into the branch
   below it: its title then never becomes a commit on `main`, and the release notes
   lose it.
5. When the bottom one merges, GitHub deletes its branch and moves the next pull request
   to `main`. Pull requests are squashed, so the branches above still hold the merged
   pull request's commits. Drop them by rebasing the top branch onto `main`, from the
   merged pull request's last commit. Take that commit from GitHub, not from your local
   branch, which can be behind it. Then push every remaining branch:

   ```sh
   old=$(gh pr view <number> --json headRefOid --jq .headRefOid)
   git fetch origin main "pull/<number>/head"
   git switch pr/top
   git rebase --update-refs --onto origin/main "$old"
   git push --force-with-lease origin pr/feature pr/top
   ```

   Check that the next pull request now targets `main`. If not, run
   `gh pr edit <next number> --base main`; without the number, `gh` edits the pull
   request of the branch you are on, which is the top one.

## License

By contributing you agree that your contribution is released under the
[GNU Affero General Public License v3.0](LICENSE), in addition to the grants in
the [CLA](CLA.md).
