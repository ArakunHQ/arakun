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
  no tests run on Windows in CI (see [docs/windows.md](docs/windows.md)). Reports of
  what works there are useful too, not only of what breaks.
- **Send a pull request.** For anything bigger than a small fix, open an issue first so
  we can agree on the approach before you spend time on it.

**Security problems:** do not open a public issue. Follow [SECURITY.md](SECURITY.md).

## Contributor License Agreement

First-time contributors are asked to sign our [CLA](CLA.md). A bot comments on
your first pull request, and signing is a single reply:

```
I have read the CLA Document and I hereby sign the CLA
```

In short: **you keep the copyright to your work** and grant the maintainer the
right to distribute it under other terms too (for example a commercial license).
The released code stays AGPL-3.0; the CLA is what keeps dual licensing possible
without chasing every past contributor. Unsigned pull requests can't be merged.

## Setting up

Follow [Build from source](README.md#build-from-source) in the README. For most
work, build **without the browser**. You do not need CEF, and it is what CI does:

```sh
cargo run --no-default-features
```

You only need the `browser` feature (and CEF) if you work on `src/browser` or
`arakun page`. Dependencies are optimized in dev builds as well, so the first build takes
a while and later ones are quick.

The phone relay in [`relay/`](relay) is a separate TypeScript project. It needs Node 24
or newer; see [relay/README.md](relay/README.md).

### Features you can ignore

`team` and `coderlabs` are private parts that are not in the public build. Leave them
off, and do not add code behind them.

## Checks

CI runs these on every pull request. Run the ones that match what you changed before
you push:

```sh
# the app (with CEF set up, you can leave out --no-default-features)
cargo clippy --locked --all-targets --no-default-features -- -D warnings
cargo test --locked --no-default-features

# relay/ or phone/ changed
(cd relay && npm ci && npx tsc --noEmit && npx vitest run)
(cd phone && node --test "test/*.test.mjs")
```

Some notes:

- `--locked` fails if `Cargo.lock` would change. If you add or update a dependency,
  commit the new `Cargo.lock` in the same pull request.
- Warnings fail clippy. Fix what causes them. Use `#[allow(...)]` only with a comment
  that says why the lint is wrong in that place.
- Tests marked `#[ignore]` read this machine's own state (Claude Code, the Cloudflare
  sign-in and so on), so CI skips them. Run one with `cargo test <name> -- --ignored` if
  you work on that part.
- The phone tests check the phone's encryption against vectors the Rust side writes. If
  you change the format in `src/phone`, update both sides.

## Writing code

- **Tests** go in a `#[cfg(test)] mod tests` at the bottom of the file they test. Add a
  test for a bug fix where you can, and for logic that does not need a window (parsing,
  state, git, the control protocol).
- **Style.** There is no `rustfmt.toml`, and CI does not run `cargo fmt`. Do not reformat
  files you are not changing, because that buries the real change in the diff. Match the
  code around you.
- **Comments** say why, not what. A new module starts with a `//!` comment that says what
  it is for. Write plain, short sentences, as in the existing code and docs.
- **New dependencies:** prefer pure Rust crates and crates GPUI already pulls in. Put a
  comment above each new entry in `Cargo.toml` that says what uses it, as the other
  entries have.
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
- **See your change.** On Linux, `arakun dev start` runs a second, fresh Arakun from your
  build on a virtual display (it needs `Xvfb`; `tesseract` for `dev text` and clicking
  by text). It never touches your own screen or Arakun:

  ```sh
  cargo build --no-default-features
  ./target/debug/arakun dev start          # this checkout as its project
  ./target/debug/arakun dev click "Settings"
  ./target/debug/arakun dev shot after.png
  ./target/debug/arakun dev stop
  ```

  `arakun dev` with no arguments lists every subcommand.
- **Add a screenshot** to the pull request, before and after if something changed.

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
    in behavior), `perf`, `test`, `build` (Cargo, dependencies, tools), `ci` or
    `chore`.
  - **scope** (optional): the part of Arakun it touches, usually the module or folder:
    `terminal`, `agent`, `browser`, `phone`, `relay`, `iso`, `windows`...
  - **description:** imperative and lower case, with no period at the end, for example
    `fix(terminal): keep the tab name on resize` or
    `feat(phone): answer permission prompts`.
  - **Breaking changes** (a CLI flag, a setting or a stored file format that no longer
    works as before): add `!` after the type or scope, and a `BREAKING CHANGE:` line in
    the body that says what to do instead.

  Give the pull request title the same format. It becomes the commit message when the
  pull request is squashed.
- **Keep the branch up to date** with `main`, and make sure CI passes before you ask for
  a review. If a check fails and you think it is not your change, say so in the pull
  request.
- **Coding agents are welcome.** Arakun is a tool for working with them, after all. You
  are still responsible for the change: read the diff, run the checks, and make sure you
  can explain it.

## License

By contributing you agree that your contribution is released under the
[GNU Affero General Public License v3.0](LICENSE), in addition to the grants in
the [CLA](CLA.md).
