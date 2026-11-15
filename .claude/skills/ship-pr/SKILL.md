---
name: ship-pr
description: Take a finished change in Arakun to a pull request the way CONTRIBUTING.md wants it - run only the CI checks the diff needs, write a Conventional Commits title, fill in the pull request template, open a draft, wait for CI, then mark it ready. Use when the user asks to commit, push, open or ship a pull request, or "get this ready for review".
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

## 2. Run the checks the diff needs

List the changed files (`git diff --name-only main...HEAD`, plus uncommitted ones)
and run what CI would run for them. CI's "What changed" job in
`.github/workflows/ci.yml` decides; in short:

| Changed | Run |
| --- | --- |
| `src/`, `examples/`, `assets/`, `build.rs`, `Cargo.*`, `clippy.toml`, `.cargo/`, `rust-toolchain.toml`, `tools/rustfmt.sh`, any new top-level path | rustfmt, clippy, tests |
| `phone/` | rustfmt, clippy, tests, relay, phone |
| `relay/` | relay, phone |
| `Cargo.toml`, `Cargo.lock`, `deny.toml` | also `cargo deny check` |
| only `docs/`, `iso/`, `tools/`, `.claude/`, `.github/`, `*.md` | nothing to build; proofread |

```sh
tools/rustfmt.sh --check        # fix with tools/rustfmt.sh, never plain cargo fmt
cargo clippy --locked --all-targets --no-default-features -- -D warnings
cargo test --locked --no-default-features
cargo deny check
(cd relay && npm ci && npx tsc --noEmit && npx vitest run)
(cd phone && node --test "test/*.test.mjs")
```

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
clippy on commit and the tests on push. They are safe in a linked worktree. If you
just ran the same checks on this exact tree, `LEFTHOOK=0 git push` skips the
second run.

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
  no API to upload them; leave them for the user to drag in and say so.

## 5. Wait for CI, then mark it ready

```sh
gh pr checks --watch     # or your host's way to wait on a pull request
```

- Red: use the `triage-ci` skill, push the fix, wait again.
- Green: `gh pr ready`. That requests reviewers and starts CodeRabbit, so never
  before CI passes.
- The first pull request of a new contributor needs the CLA signed; only the
  person can do that.
- Never merge, approve or self-review. A person does that.
