# Instructions for coding agents

These rules come from [CONTRIBUTING.md](CONTRIBUTING.md). If the two disagree,
CONTRIBUTING.md is right; update this file to match.

## Setting up

If the build or a check fails because a tool or system library is missing, run the
setup script. It does not ask questions, and you can run it again safely:

```sh
tools/setup-dev.sh --yes --relay       # Linux and macOS
tools/setup-dev.sh --check --relay     # only report what is missing
```

```powershell
powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1 -Yes -Relay   # Windows
```

If it says sudo needs a password, ask the user to run the command it prints. In Claude
Code on the web, `.claude/settings.json` runs it when the session starts, and the log is
in `$TMPDIR/arakun-setup-dev.log` (`/tmp` if `TMPDIR` is not set). Build without the
browser (`--no-default-features`), as CI does.

## Tracking work in issues

Every task you work on has a GitHub issue, so the team sees who is doing what.

- Before you change anything, find the issue (`gh issue list --search "<words>"`) or
  open one with `gh issue create`: what is wrong or missing, and the plan. One issue
  per fix or feature, like the pull requests.
- Assign it to the person running you as soon as you start:
  `gh issue edit <number> --add-assignee "@me"` (`@me` is the account `gh` is
  logged in as).
- Keep it current while you work: edit the body when the plan or scope changes, and
  comment on decisions, blockers and anything you found that a reader would need.
- Link the pull request with `Closes #<number>` (`Refs #<number>` for part of the
  work), as the template asks.
- If you stop before it is done, comment where you stopped and what is left, then
  `gh issue edit <number> --remove-assignee "@me"`.

## Commits and pull requests

Every commit message and pull request title follows
[Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <what the commit does>

<why, if it is not obvious>
```

- **type:** `feat`, `fix`, `docs`, `refactor` (no change in behavior), `perf`, `test`,
  `build` (Cargo, dependencies, tools), `ci`, `chore` or `revert`
  (`revert: <the title of the change you undo>`). Pick the one that matches the
  effect for the user: a bug fix that needs a refactor is still `fix`.
- **scope** (optional): the module or folder it touches, such as `terminal`, `agent`,
  `browser`, `phone`, `cloud`, `server`, `teams`, `ios`, `android`, `iso` or
  `windows`. `cloud` is the Worker in `cloud/` and replaces `relay`. Leave it out if the change
  spans many parts.
- **description:** imperative, lower case, no period at the end, under about 70
  characters. `fix(terminal): keep the tab name on resize`, not `Fixed terminal bug.`
- **Breaking changes** (a CLI flag, a setting or a stored file format that no longer
  works as before): `!` after the type or scope, and a `BREAKING CHANGE:` line in the
  body that says what to do instead.
- Pull requests are squashed, so the title becomes the commit on `main`. CI fails the
  pull request if the title breaks these rules; check one with
  `tools/conventional-commits.sh title "<title>"`.
- One logical change per commit, and one fix or feature per pull request. Put unrelated
  cleanups in their own commit or pull request.
- Fill in `.github/pull_request_template.md`. Under "How did you test this?", list the
  commands you actually ran. Leave a checklist item unchecked if you did not run it.
- Always open a pull request as a draft: `gh pr create --draft --body-file -`. Run the
  checks below, wait for the checks it needs with `tools/wait-checks.sh`, then mark it
  ready with `gh pr ready`, never before. That script waits for the required checks, for
  macOS only when the diff touches macOS code, and for iOS or Android only when the diff
  touches what that job builds; don't wait for the rest. The required
  **Human approval** status is a merge gate, not part of this wait: it only resolves once
  a person approves, and marking ready is what asks for the review. cubic, the review
  bot, reviews drafts too, so its review is often done before the checks. The
  Reviewers workflow asks the reviewers only once the pull request is ready, cubic has
  finished and every cubic comment is resolved or answered, and not at all if cubic
  approved and every review thread is resolved. A maintainer who runs it by hand skips
  the wait for cubic and for its comments. A pull request from outside the team
  always needs a person: the workflow dismisses cubic's approval of it.
- Answer every cubic comment: fix it and push, or reply with the reason you won't.
  Resolve a thread on GitHub once the pushed commit fixes it; leave the ones you
  declined open for the reviewer. Never resolve a thread you didn't fix to get cubic's
  approval. A workflow dismisses cubic's approval when a thread is still open, so
  resolve the fixed ones right after you push. cubic may push its own fixes to your
  branch, so pull before you push. If you change a rule here or in
  CONTRIBUTING.md, check the rules in `cubic.yaml` too.

- A version bump pull request also adds the changelog entry,
  `release-notes/v<version>.md`; CI fails it otherwise. Write it with the
  `release-notes` skill.

### Stacked pull requests

A stack is a chain of pull requests where each one targets the branch of the one below
it, and the bottom one targets `main`.

- **Stack** when a change splits into steps that are each reviewable alone, and a later
  step needs an earlier one that is not merged yet (a refactor, then the feature on top).
- **Don't stack** independent changes; branch each from `main`. Don't stack on a pull
  request whose approach is still under discussion, or more than about four deep.
- Use `gh stack` (`gh extension install github/gh-stack`); the `stack-pr` skill has
  the commands. Name branches `pr/<topic>`: `gh stack init pr/refactor`, commit, then
  `gh stack add pr/feature`, commit. Push with `gh stack push`, open each pull request
  yourself with `gh pr create --draft --base <branch below>` and the filled-in template,
  then link them with `gh stack submit --auto`. Write `Stacked on #<number>.` at the
  end of Changes.
- Never let `gh stack submit` create a pull request: its editor opens them ready for
  review and `--auto` gives them generated titles. Never pass `--open`; mark each pull
  request ready with `gh pr ready` once its checks pass. Skip the interactive commands
  (`modify`, `switch`, `submit` without `--auto`, `checkout` without an argument); they
  need a terminal.
- To fix a lower pull request, commit on its branch, then `gh stack rebase --upstack`
  and `gh stack push`. On a conflict, fix it and `gh stack rebase --continue`.
- Merge from the bottom, always into `main`; never merge a pull request into the branch
  below it. Merging is a person's job, not yours: never run `gh stack merge`.
- After the bottom one merges, GitHub moves the next to `main` and rebases the rest.
  Run `gh stack sync --prune` to update your branches and drop the merged one.
- Without `gh stack` (not installed, or a branch from a fork), use the Git steps under
  "Without `gh stack`" in CONTRIBUTING.md: `git switch -c pr/feature pr/refactor`,
  `git rebase --update-refs`, and after a squash merge
  `git rebase --update-refs --onto origin/main <merged pull request's last commit>`.

## Before you commit

CI is the gate, so keep local runs short. Run the quick checks for what you changed
and fix every warning:

```sh
# Rust changed
tools/rustfmt.sh --check
cargo clippy --locked --all-targets --no-default-features -- -D warnings
cargo test --locked --no-default-features <module>   # only the modules you touched
# cloud/ or phone/ changed
(cd cloud && npm ci && npm run check)
# ios/ changed (macOS with Xcode; brew install xcodegen)
(cd ios/ArakunKit && swift test)
(cd ios && xcodegen generate && xcodebuild build -quiet -project Arakun.xcodeproj \
  -scheme Arakun -destination "generic/platform=iOS Simulator" CODE_SIGNING_ALLOWED=NO)
# android/ changed
(cd android && ./gradlew :kit:test :app:testDebugUnitTest :app:lintDebug :app:assembleDebug)
```

Leave the rest to CI on the draft pull request: the full `cargo test`, the
`arakun-server` clippy, `cargo deny check`, `zizmor .github`, the cloud and phone
tests and the other platforms. If CI fails, fix it with the `triage-ci` skill.
Don't rerun the whole suite locally just to be sure. With the Git hooks installed, push
with `LEFTHOOK_EXCLUDE=cargo-test git push` so the push does not run the full suite
either.
The full list of what CI runs is under "Checks" in CONTRIBUTING.md.

If you add or update a dependency, commit `Cargo.lock` too. Its license must be on the
allow-list in `deny.toml`; do not add a license to that list without asking.
The Rust version is pinned in `rust-toolchain.toml`; bump it only in its own pull
request that also fixes the new lints.
`lefthook install` runs these checks as Git hooks; see "Git hooks" in CONTRIBUTING.md.

## Code

- Format with `tools/rustfmt.sh` (rustfmt defaults). Plain `cargo fmt` stops at the
  private modules this repository does not have.
- The code the app shares with `arakun-server` and the examples is the `arakun-core`
  crate in `core/`; the app reaches it as `crate::agent` and so on. A core item the
  app uses must be `pub`. Where the app and the server differ, core asks through
  `core/src/host.rs`; never include a file by `#[path]` from another package.
- Tests go in a `#[cfg(test)] mod tests` at the bottom of the file they test.
- UI code uses the design system: colors, grays, text sizes and fonts from `theme` and
  `ui`, never hand-written values. `src/design_lint.rs` fails the tests otherwise.
- Interface text uses one word per thing: environment, session, palette, color scheme,
  Workspace panel, notice (the glossary under "Words in the interface" in
  CONTRIBUTING.md). `src/words_lint.rs` fails the tests on a retired word.
- Code for one platform goes behind `cfg(...)`, and the other platforms must still build.
- The iOS app (`ios/`): `ios/ArakunKit` must match `core/src/phone` and `phone/` on the
  wire. Pairing credentials stay in the Keychain, never in `UserDefaults` or logs. Views
  take colors, fonts and corners from `ios/Arakun/Theme.swift`. The `-demo` mode stays
  behind `#if DEBUG`.
- The Android app (`android/`): `android/kit` must match `core/src/phone` and `phone/`
  on the wire. Check a peer's point is on the curve before ECDH and never reuse a
  handshake. Pairing credentials stay sealed by the Android Keystore
  (`CredentialStore.kt`); never log keys, tokens or message contents.
- Do not add or change code behind the private `team` or `coderlabs` features.
- Comments say why, not what. Docs and comments use plain, short sentences.
- If you change a command, flag, shortcut or storage path, update the README (and
  `docs/windows.md` if it differs on Windows).
