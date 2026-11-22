---
name: release-notes
description: Write the changelog entry for an Arakun release, release-notes/v<version>.md, that the version bump pull request must add - draft it with tools/release-notes.py, find out what each merged pull request changes for someone using the app, write it for that reader, and pass the check. Use when the user bumps the version, prepares a release, or asks for release notes, a changelog entry or "what's new".
---

# Release notes

The rules for the entry (its shape, what to keep, the wording) are in
`release-notes/README.md`. Read it first; it is the source of truth, and this skill
only adds how to get there. `release.yml` publishes the file at the top of the GitHub
release, and arakun.dev/changelog shows it.

## Steps

1. **Bump.** The entry belongs in the version bump pull request: `version` in
   `Cargo.toml`, `core/Cargo.toml` and `server/Cargo.toml`, and `Cargo.lock`
   (`cargo update --workspace --offline`). Branch from an up-to-date `main`.

2. **Draft.** Fetch the tags first: without them the draft lists nothing.

   ```sh
   git fetch origin main --tags
   tools/release-notes.py new
   ```

   It writes `release-notes/v<version>.md` with a hidden comment listing the pull
   request titles merged since the last `v*` tag, minus `build`, `ci`, `test`,
   `docs`, `chore` and `refactor`. Done when the file exists and the list is not
   empty (an empty list means no tags, or truly nothing user-facing: ask the user).

3. **Find the effect.** A title says what the code does, not what the reader sees.
   For every listed pull request, read its body until you can say in one plain
   sentence what someone using the app notices, or decide they notice nothing:

   ```sh
   gh pr view <number> --json title,body --jq '.title, .body'
   ```

   The number is the `(#123)` at the end of the title. Done when every listed pull
   request is either a sentence or dropped as unseen.

4. **Write.** Group the sentences into areas, follow every rule in the README, and
   replace both `TODO` lines. The reader is the leading word: someone who uses Arakun
   daily and has never read its code. Keep the hidden comment or delete it; nobody
   sees it.

5. **Check.**

   ```sh
   tools/release-notes.py check
   ```

   Done when it prints `release-notes/v<version>.md is fine`. The check only covers
   the shape; reread the entry against the README's "Writing it" list for names of
   files, functions or settings keys, pull request numbers and marketing words.

6. **Ship.** Commit the bump and the entry together
   (`chore(release): bump to <version>`) and open the pull request with
   `ship-pr`. The **Changelog entry** check runs on it. Releasing after the merge is a
   person's job: `gh workflow run release.yml --ref main -f version=<version>` waits
   for approval.

## After a release is out

To fix the wording of a published entry, edit the release notes on GitHub, or add an
entry to `src/release-notes.ts` in ArakunHQ/arakun.dev, which wins over GitHub. The
file here is only read when the release is made.
