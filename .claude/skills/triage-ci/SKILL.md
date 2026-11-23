---
name: triage-ci
description: Find out why an Arakun pull request's GitHub checks failed and fix it - read the failing job's log with gh, map it to the local command, reproduce, fix, and tell real failures apart from ones the change did not cause. Use when CI, a check, clippy, the tests, cargo-deny, the cloud job or the pull request title check is red, or the user asks why a pull request is failing.
---

# Fix a failing check

## Find the failure

```sh
gh pr checks                          # which checks failed, with links
gh run list --branch "$(git branch --show-current)" --limit 5
gh run view <run id> --log-failed     # only the failing steps
```

Read the first error, not the last line. Cargo prints the real error above the
summary.

## Map it to a local command

| Check | Run locally |
| --- | --- |
| Clippy (no warnings), "cargo fmt" step | `tools/rustfmt.sh --check`, then `cargo clippy --locked --all-targets --no-default-features -- -D warnings` |
| Tests (Linux, no browser) | `cargo test --locked --no-default-features` (one test: add its name) |
| Clippy (Linux, browser) | needs CEF; read the log, the error is usually in `core/src/browser/` or `arakun-helper` |
| Clippy and tests (macOS, no browser) | not reproducible on Linux; see below |
| Cloud (types and tests) | `(cd cloud && npm ci && npx tsc --noEmit && npx vitest run)` and `(cd phone && node --test "test/*.test.mjs")` |
| Dependencies (cargo-deny) | `cargo deny check` |
| Conventional Commits | `tools/conventional-commits.sh title "<title>"` |
| CLA | only the person can sign; tell the user |

If `cargo` is not found, run `export PATH="$HOME/.cargo/bin:$PATH"` first.

Reproduce before you fix. A fix you could not reproduce is a guess; say so.

## Common causes

- **Format:** run `tools/rustfmt.sh` (not plain `cargo fmt`) and commit.
- **Clippy warning:** fix the code. `#[allow(...)]` only with a comment that says
  why the lint is wrong there.
- **`--locked` fails:** `Cargo.lock` is out of date. Build once and commit it.
- **design_lint:** a hand-written color, size or font; use the token it names (see
  the `ui-change` skill).
- **macOS only:** usually code for one platform without `cfg(...)`, or an unused
  import on that platform. Read the log, fix by reasoning, and check the other
  platforms still build. Say in the pull request that you could not run it.
- **Phone tests:** the encryption format in `src/phone` and `phone/` must change
  together.
- **cargo-deny:** read which rule failed (advisory, license, source) and the
  crate it names, then check `gh run list --branch main --workflow Dependencies
  --limit 3`. A license or source error after a change to `deny.toml` or
  `Cargo.*` is yours to fix. An advisory that fails on `main` too is not: say so
  in the pull request, and the fix (`fix(deps): ...`) goes in its own pull
  request. Never add a license to `deny.toml` without asking.
- **Title check:** `gh pr edit --title "<fixed title>"`. The check reruns by itself.
- **A test that fails on CI only:** look for time, ordering, environment
  variables (`GIT_DIR`, `HOME`) or this machine's state. Tests marked `#[ignore]`
  read local state and CI skips them.

## Is it yours?

Check `main`'s latest run: `gh run list --branch main --workflow CI --limit 3`.
If `main` fails the same way, the pull request did not cause it. Say that in the
pull request rather than fixing it there.

A failure that looks flaky (a network step, a runner that died): rerun once with
`gh run rerun <run id> --failed`, and mention it. Don't rerun again and again.

## After the fix

Commit with a Conventional Commits message, push, and wait for the checks again.
Don't mark the pull request ready until every check passes.
