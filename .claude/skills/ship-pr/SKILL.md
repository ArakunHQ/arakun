---
name: ship-pr
description: Take a finished change in Arakun to a pull request the way CONTRIBUTING.md wants it - run the quick checks the diff needs and leave the rest to CI, write a Conventional Commits title, fill in the pull request template, open a draft, wait for CI, then mark it ready. Use when the user asks to commit, push, open or ship a pull request, or "get this ready for review".
---

# Ship a pull request

The rules live in `CONTRIBUTING.md` ("Checks" and "Pull requests"). This is the
order to apply them in. If the two disagree, CONTRIBUTING.md is right.

## 1. Check the change is one change

- `git status` and `git diff main...HEAD --stat` (plus unstaged work). One fix or
  feature per pull request. If the diff holds an unrelated cleanup, split it out.
  If a later step needs an earlier one, use the `stack-pr` skill instead.
- You are on `main`? Branch first: `git switch -c pr/<topic>`.
- A changed command, flag, shortcut or storage path needs the README updated (and
  `docs/windows.md` if Windows differs). A changed skill workflow needs its
  `.claude/skills/*/SKILL.md` updated.

## 2. Run the quick checks the diff needs

List the changed files with `git diff --name-status main...HEAD` and
`git status --short` (for uncommitted ones). A rename counts at both its old and
new path, as in CI: moving a file out of `src/` still needs the Rust checks. The
table says what CI will run for them; CI's "What changed" job in
`.github/workflows/ci.yml` decides. Find every row that any changed path
matches. A path can match more than
one row (`Cargo.lock` needs the Rust checks and cargo-deny).

| Changed | Run |
| --- | --- |
| any path outside `cloud/` and `ios/` that the last row does not list (`src/`, `examples/`, `assets/`, `Cargo.*`, `tools/rustfmt.sh`, `lefthook.yml`, a `.md` inside a folder such as `examples/`...) | rustfmt, clippy, tests |
| `phone/` | rustfmt, clippy, tests, cloud, phone |
| `cloud/` | cloud, phone |
| `ios/` | on macOS with Xcode: `swift test` in `ios/ArakunKit`, then `xcodegen generate` and the `xcodebuild` simulator build in `ios/` (the commands under "Before you commit" in AGENTS.md). Only `ios/` changed: no Rust checks |
| `.github/workflows/ci.yml`, `.github/actions/` | everything: rustfmt, clippy, tests, cloud, phone |
| `tools/install-cef.sh` | rustfmt, clippy, tests (CI also builds with the browser) |
| `Cargo.toml`, `Cargo.lock`, `deny.toml` | also `cargo deny check` |
| `docs/`, `iso/`, `tools/`, `.claude/`, `.github/`, `LICENSE`, `NOTICE`, `.gitignore`, `deny.toml`, `cubic.yaml`, and `*.md` at the top level, apart from the paths named above | nothing to build; proofread (only when no path matched a row above) |

`phone/`, `assets/brand/mascot.json` and `assets/fonts/` also run CI's iOS job, which
`tools/wait-checks.sh` then waits for.

Locally, run only the quick ones and leave the rest to CI on the draft:

```sh
tools/rustfmt.sh --check        # fix with tools/rustfmt.sh, never plain cargo fmt
cargo clippy --locked --all-targets --no-default-features -- -D warnings
cargo test --locked --no-default-features <module>   # the modules you touched
(cd cloud && npx tsc --noEmit)  # cloud/ changed
```

- Skip the full `cargo test`, `cargo deny check`, `zizmor`, the cloud and phone
  tests and the other platforms. CI runs them; if one goes red, step 5 handles it.
- For a UI change, also run `cargo test --locked --no-default-features design_lint`.
- `cargo: command not found`: run `export PATH="$HOME/.cargo/bin:$PATH"` first.
- Fix every warning. `#[allow(...)]` only with a comment saying why the lint is
  wrong there.
- A test failure you think is not yours: run it on `main` to be sure, and say so
  in the pull request.
- Write down each command you ran and whether it passed. The pull request lists
  exactly these, and nothing else.

## 3. Commit

Title and every commit: `<type>(<scope>): <description>`. Types: `feat`, `fix`,
`docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`. Scope is
the module or folder (`terminal`, `agent`, `phone`, `relay`...), or none if it
spans many. Imperative, lower case, no period, under about 70 characters. Pick
the type by the effect on the user: a fix that needs a refactor is `fix`.
Breaking a flag, setting or stored format: `!` and a `BREAKING CHANGE:` line.

```sh
tools/conventional-commits.sh title "fix(terminal): keep the tab name on resize"
```

Commit `Cargo.lock` with any dependency change. The Git hooks (if installed) run
clippy on commit and the full tests on push. They are safe in a linked worktree.
Push with `LEFTHOOK_EXCLUDE=cargo-test git push` to leave the full suite to CI.

## 4. Open a draft

Fill in `.github/pull_request_template.md` and pass it on stdin so the markdown
survives:

```sh
git push -u origin HEAD
gh pr create --draft --title "<title>" --body-file - <<'EOF'
...the filled-in template...
EOF
```

- Follow the comments in the template, then delete them. Keep it under about
  150 words outside screenshots and `<details>`.
- **Problem** stays true under another solution. **Changes** says what a person
  will see or do differently; mark mechanical parts as mechanical.
- **How did you test this?** Only commands you ran and things you tried. Say
  what you could not try. Tick a box under **Checks** only if you ran it.
- **Agent context**: autonomy, the tool and exact model, every skill that ran
  (this one is `/ship-pr`).
- No "I" in the body. No @mention you have not checked this session. No
  secrets, private logs or home-directory paths: the repository is public.
- UI change: before and after screenshots (see the `ui-change` skill). GitHub has
  no API to upload them: `tools/pr-assets.sh <file>...` pushes them to
  `ArakunHQ/pr-assets` and prints the Markdown, or, without push access there,
  leave them for the user to drag in and say so.

## 5. Wait for the required checks, then mark it ready

```sh
tools/wait-checks.sh     # the pull request of the current branch
```

It waits for the checks `main` requires, and for the macOS job only when the
diff touches macOS code (`cfg(target_os = "macos")`, a file named `*mac*`,
`Cargo.toml`, `Cargo.lock`, `build.rs` or the toolchain). Don't wait for the
other checks, such as the macOS job for other changes or CodeQL: macOS runners
are few, and waiting for them held pull requests up to 100 minutes. A person
still sees them before merging. The required **Human approval** status is not
part of the wait either: it is a merge gate that only resolves after a person
approves, and marking ready is what asks for the review.

- Exit 1 (a check failed): use the `triage-ci` skill, push the fix, wait again.
- Exit 0: `gh pr ready`. cubic, the review bot, reviews drafts too, so its
  review is often done before the checks. Marking ready is what lets the
  reviewers be asked, so never before the required checks pass. They are asked
  only once cubic has finished and every cubic comment is resolved or answered,
  and not at all if cubic approved and every review thread is resolved.
- The first pull request of a new contributor needs the CLA signed; only the
  person can do that.
- cubic comments: fix each one and push, or reply with the reason you
  won't. Once the push is up, resolve the threads it fixed (right away: an
  approval with threads still open is dismissed). Leave the declined ones open
  for the reviewer. Never resolve a thread you didn't fix to get cubic's
  approval. cubic may push its own fixes to the branch: `git pull --rebase`
  before you push.
- Never merge, approve or self-review. A person does that.
