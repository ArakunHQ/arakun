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
  `browser`, `phone`, `relay`, `teams`, `iso` or `windows`. Leave it out if the change
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
  ready with `gh pr ready`, never before. That script waits for the required checks, and
  for macOS only when the diff touches macOS code; don't wait for the rest. The required
  **Human approval** status is a merge gate, not part of this wait: it only resolves once
  a person approves, and marking ready is what asks for the review. cubic, the review
  bot, reviews drafts too, so its review is often done before the checks. Once the
  pull request is ready and cubic has finished, the reviewers are asked unless cubic
  approved and every review thread is resolved.
- Answer every cubic comment: fix it and push, or reply with the reason you won't.
  Resolve a thread on GitHub once the pushed commit fixes it; leave the ones you
  declined open for the reviewer. Never resolve a thread you didn't fix to get cubic's
  approval. A workflow dismisses cubic's approval when a thread is still open, so
  resolve the fixed ones right after you push. If you change a rule here or in
  CONTRIBUTING.md, check the rules in `cubic.yaml` too.

### Stacked pull requests

A stack is a chain of pull requests where each one targets the branch of the one below
it, and the bottom one targets `main`.

- **Stack** when a change splits into steps that are each reviewable alone, and a later
  step needs an earlier one that is not merged yet (a refactor, then the feature on top).
- **Don't stack** independent changes; branch each from `main`. Don't stack on a pull
  request whose approach is still under discussion, or more than about four deep.
- Name branches `pr/<topic>` and branch each from the one below:
  `git switch -c pr/feature pr/refactor`, then
  `gh pr create --draft --base pr/refactor`. Write `Stacked on #<number>.` at the end of
  Changes.
- To fix a lower pull request, commit on its branch, then rebase the top branch on it:
  `git switch pr/top && git rebase --update-refs pr/refactor`. `--update-refs` moves the
  branches in between, but not one checked out in another worktree; switch that worktree
  away first. Push every branch above the changed one:
  `git push --force-with-lease origin pr/feature pr/top`.
- Merge from the bottom, always into `main`; never merge a pull request into the branch
  below it. Merging is a person's job, not yours.
- After the bottom one merges, GitHub moves the next to `main`, but the branches above
  still hold the squashed commits. Take the merged pull request's last commit from GitHub
  (`gh pr view <number> --json headRefOid --jq .headRefOid`, then
  `git fetch origin main "pull/<number>/head"`), not from your local branch. Run
  `git rebase --update-refs --onto origin/main <that commit>` on the top branch, push
  every remaining branch with `--force-with-lease`, and check the next pull request's
  base is `main` (`gh pr edit <next number> --base main`; without the number it edits the
  top one).

## Before you commit

CI is the gate, so keep local runs short. Run the quick checks for what you changed
and fix every warning:

```sh
# Rust changed
tools/rustfmt.sh --check
cargo clippy --locked --all-targets --no-default-features -- -D warnings
cargo test --locked --no-default-features <module>   # only the modules you touched
# relay/ or phone/ changed
(cd relay && npx tsc --noEmit)
# teams/ changed
(cd teams && npm ci && npm run check)
```

Leave the rest to CI on the draft pull request: the full `cargo test`, the
`arakun-server` clippy, `cargo deny check`, `zizmor .github`, the relay, phone and
teams tests and the other platforms. If CI fails, fix it with the `triage-ci` skill.
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
- Do not add or change code behind the private `team` or `coderlabs` features.
- Comments say why, not what. Docs and comments use plain, short sentences.
- If you change a command, flag, shortcut or storage path, update the README (and
  `docs/windows.md` if it differs on Windows).
