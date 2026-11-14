# Instructions for coding agents

These rules come from [CONTRIBUTING.md](CONTRIBUTING.md). If the two disagree,
CONTRIBUTING.md is right; update this file to match.

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
  `browser`, `phone`, `relay`, `iso` or `windows`. Leave it out if the change spans
  many parts.
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
- Fill in `.github/pull_request_template.md`. Under "How I checked it", list the
  commands you actually ran. Leave a checklist item unchecked if you did not run it.

## Before you commit

Run what CI runs for the parts you changed, and fix every warning:

```sh
cargo clippy --locked --all-targets --no-default-features -- -D warnings
cargo test --locked --no-default-features
# relay/ or phone/ changed
(cd relay && npm ci && npx tsc --noEmit && npx vitest run)
(cd phone && node --test "test/*.test.mjs")
```

If you add or update a dependency, commit `Cargo.lock` too.

## Code

- Do not run `cargo fmt` on whole files or the whole crate. There is no rustfmt config,
  and reformatting buries the real change. Match the code around you.
- Tests go in a `#[cfg(test)] mod tests` at the bottom of the file they test.
- UI code uses the design system: colors, grays, text sizes and fonts from `theme` and
  `ui`, never hand-written values. `src/design_lint.rs` fails the tests otherwise.
- Code for one platform goes behind `cfg(...)`, and the other platforms must still build.
- Do not add or change code behind the private `team` or `coderlabs` features.
- Comments say why, not what. Docs and comments use plain, short sentences.
- If you change a command, flag, shortcut or storage path, update the README (and
  `docs/windows.md` if it differs on Windows).
