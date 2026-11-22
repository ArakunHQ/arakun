# Release notes

One file per release, `v<version>.md`: the changelog entry for people who use the app.
The version bump pull request adds it, next to the new version in the `Cargo.toml`
files. `.github/workflows/release.yml` puts it at the top of the release's notes on
GitHub, above the generated list of pull requests, and
[arakun.dev/changelog](https://arakun.dev/changelog) shows it from there.

```sh
tools/release-notes.py new     # drafts v<version>.md, with what merged since the last release
tools/release-notes.py check   # what CI and release.yml run
```

CI fails a pull request whose `Cargo.toml` version has no release yet and no file
here, and the release workflow won't tag one either.

## The shape

```md
## Smoother drawing and a session tree

- **Sessions:** Every session is in a tree in the sidebar, with its state.
- **Fixes:** A long subagent list scrolls.
```

A `##` title, then one `- **Area:** change` line per area. Nothing else: arakun.dev
reads this shape and shows the GitHub notes instead when it doesn't match. An HTML
comment, such as the one `new` writes, may stay; nobody sees it.

## Writing it

The reader uses Arakun every day but has never seen its code. They want to know what
is new for them and whether to update.

- Keep only what the reader can see, use or feel: a new feature, a new setting,
  something that works that didn't, something clearly faster. Leave out tests, CI,
  builds, refactors, dependency updates and other plumbing.
- Describe the effect, not the mechanism: "Scrolling long sessions is smoother", not
  "messages are rendered with less repeated work".
- Use the names the reader sees in the app: a tab, button, setting or shortcut. No
  file, function, crate or config key names.
- At most 6 areas, each named with one or two words (Agents, Servers, GitHub, Look,
  App, Usage, Privacy, Windows, macOS, Linux). Each area once. Small fixes go together
  in Fixes, last.
- One or two short, plain sentences per area. Present tense, "you" for the reader.
- No pull request numbers, names, links, emoji or marketing words ("exciting",
  "seamless", "powerful").
- The title names the one to three biggest changes, under 60 characters, in sentence
  case, without the version and without a period.

A release fixed after it is out: edit the notes on GitHub, or add an entry to
`src/release-notes.ts` in ArakunHQ/arakun.dev, which wins over the GitHub notes.
