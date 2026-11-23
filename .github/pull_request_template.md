<!-- Write this so a reader who opens no files still knows why the pull request is needed and what it does. Aim for 150 words or fewer; screenshots, code blocks and <details> blocks don't count. A section you have nothing for gets one bullet: "- None." -->

## Problem

<!-- First line: who is affected and what is wrong for them today. A fix says what breaks (and on which platform); a feature names the goal and what stands in the way; a chore says who is blocked. It should stay true under a different solution. The code path goes under Changes. -->

<!-- Fixes an issue? Uncomment the line below so merging closes it. Use "Refs #ISSUE_ID" instead when it only does part of the work. -->
<!-- Closes #ISSUE_ID -->

## Changes

<!-- For each change a person can notice, say what they will now see or do differently, not only the code that does it. Mark the rest as mechanical so a reviewer knows nothing user-visible hides in it. -->

<!-- UI changes: add a before and after screenshot. A short GIF or animated WebP can replace the after screenshot for an interaction one picture can't show. `arakun dev start` and `arakun dev shot` take one without touching your own screen. -->

<!-- New CLI commands or flags (`arakun mem`, `arakun page`, `arakun agent`...): show the usage line and one example. -->

<!-- Breaking change (a CLI flag, a setting or a stored file format that no longer works as before): put `!` in the title, a `BREAKING CHANGE:` line in the commit message, and say here what people do instead. -->

<!-- Stacked on another pull request? End this section with "Stacked on #NUMBER." -->

## How did you test this?

<!-- Steps to reproduce and verify, and what you expected to see. Name the automated tests you added or changed and the regression each one catches; if none was needed, say what existing coverage applies or why a test would not help. -->
<!-- Don't paste pass counts for suites CI already runs, and don't repeat the commands ticked under Checks; link the run instead, and say what you did not check. Long output goes in a <details> block. -->
<!-- Agents: do NOT claim manual testing you haven't done. Say what you could not try, and list only the commands you actually ran. -->

**Tested on:** <!-- Linux (Wayland / X11), Windows, macOS; with or without the `browser` feature. For CI-only changes: "GitHub Actions only". -->

## Checks

<!-- Tick only what you ran. Delete an item that does not apply to this diff, so an unticked box always means "not run". A change to docs, `iso/` or `tools/` alone skips the Rust jobs in CI, except `tools/rustfmt.sh`, `tools/install-cef.sh` and the layout audit (`tools/layout_audit.py`, `tools/layout-audit-fixtures/`). The commands are in CONTRIBUTING.md under "Checks". With CEF set up you can leave out `--no-default-features`. -->

- [ ] `tools/rustfmt.sh --check` passes
- [ ] `cargo clippy --locked --all-targets --no-default-features -- -D warnings` passes
- [ ] `cargo test --locked --no-default-features` passes (includes `design_lint`: colors, text sizes and fonts come from `theme` and `ui`)
- [ ] `Cargo.toml`, `Cargo.lock` or `deny.toml` changed: `cargo deny check` passes, `Cargo.lock` is committed, and each new dependency has a comment in `Cargo.toml` saying what uses it
- [ ] `cloud/` or `phone/` changed: the cloud Worker's `tsc` and `vitest`, and the phone tests pass
- [ ] Code for one platform: it is behind `cfg(...)`, and clippy passes for another target too (for example `--target x86_64-pc-windows-gnu`)
- [ ] UI changed: screenshots are under Changes, and `tools/layout-audit.sh` passes (Linux with `Xvfb`)
- [ ] One fix or feature per pull request
- [ ] The title follows [Conventional Commits](https://github.com/arakunhq/arakun/blob/main/CONTRIBUTING.md#pull-requests) (CI checks it; it becomes the commit message on `main`)
- [ ] I have signed the [CLA](https://github.com/arakunhq/arakun/blob/main/CLA.md) (the bot asks on your first pull request; reply with the sentence it gives)

## Docs

<!-- Changed a command, flag, shortcut or storage path? Update the README (and docs/windows.md if it differs on Windows). Also the platform table, a CLI usage string, or a skill in .claude/skills if they changed. Name what you updated, or write "- None." -->

## 🤖 Agent context

<!-- Fill this in if an agent wrote or co-wrote this pull request. Delete it for fully human-written ones. -->

<!-- Keep one: "Human-driven (agent-assisted)" when a person directed the work, or "Fully autonomous" when no one did. -->

**Autonomy:** Human-driven (agent-assisted) - or - Fully autonomous

<!-- The tool and exact model, e.g. "Arakun + Claude Code, Opus 5.5". List each model that wrote code. -->

**Agent:** tool, model

<!-- Every skill that ran (e.g. /arakun-iso, /simplify), or "None". -->

**Skills:** `/skill`

<details>
<summary>More context for AI agents</summary>

<!-- Agents: collapsed by default and not counted toward the 150 words.
     Rewrite it on every push to describe the pull request at that commit; replace stale bullets, never append a log.
     One line per bullet, under 25 words, 8 bullets at most. Delete a field you have nothing for.
     Paraphrase the user's intent, never paste prompts. Leave out tool output and anything private from the session (keys, tokens, paths under your home directory, other people's data). -->

- **Decisions:** one bullet per decision, and why it beat the obvious alternative
- **Follow-ups:** work left for a later pull request
- **Read closely:** files or lines that need a careful review

</details>

<!-- Rules for agents opening a pull request here:
- Title: Conventional Commits, <type>(<scope>): <description>, as AGENTS.md and CONTRIBUTING.md describe. Pull requests are squashed, so the title becomes the commit on main, and CI fails a title that breaks the rules. Check one with `tools/conventional-commits.sh title "<title>"`.
  ✅ fix(terminal): keep the tab name on resize
  ❌ Fixed some terminal bugs.
- Voice: the subject of each sentence is the change, not its author. No "I" in the body; authorship is stated once, under Agent context.
- Body: describe the rationale, not a replay of the session. Pass it straight to `gh pr create --body-file -` on stdin so markdown and newlines survive.
- Draft first: open with `--draft`, get rustfmt, clippy and the tests of the modules you touched green locally, let CI run the rest, and wait for it to pass, then mark it ready with `gh pr ready`.
- Labels: don't set them; a workflow adds area labels from the changed paths.
- Split instead of stuffing: if the diff holds separable steps (a rename then a rewrite, a refactor then a feature), open them as separate pull requests. If a later step needs an earlier one, stack them (`--base` the branch below, "Stacked on #N." under Changes); see "Stacked pull requests" in CONTRIBUTING.md.
- Never write an @mention you have not verified this session (`gh api user`, `gh pr view --json author,assignees`); never guess a handle from a display name.
- This repository is public (AGPL-3.0): no secrets, private logs or other people's data in code, fixtures, commits or this description.
- Agent-written pull requests always need a human review; do not self-merge or approve.
-->
