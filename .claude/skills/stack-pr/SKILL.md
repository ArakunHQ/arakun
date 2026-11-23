---
name: stack-pr
description: Build and maintain a stack of dependent pull requests in Arakun with `gh stack` (GitHub's stacked pull requests extension), or plain Git when it is missing - decide whether to stack, add each step on top of the one below, open the drafts, fix a lower pull request and restack, and sync after the bottom one is squash-merged. Use when a change splits into steps where a later one needs an earlier unmerged one, or when the user mentions a stack, gh stack, restacking, or a pull request "on top of" another.
---

# Stacked pull requests

The full rules are in `CONTRIBUTING.md` under "Stacked pull requests". Use
`ship-pr` for the checks, title and body of each pull request in the stack.

## Should this be a stack?

- **Yes**, when each step is reviewable alone and a later one needs an earlier one
  that is not merged: a refactor, then the feature; a `ui` component, then the
  views that use it.
- **No**, when the steps don't depend on each other: branch each from `main`.
- **No**, on a pull request whose approach is still being discussed.
- **No** deeper than about four. Get the bottom ones merged first.

## Is `gh stack` there?

```sh
gh stack --version || gh extension install github/gh-stack
```

If it can't be installed, or a branch in the stack comes from a fork (stacks need
every branch in this repository), use [Without gh stack](#without-gh-stack).

Stay off the interactive commands; they need a terminal you don't have: `modify`,
`switch`, `checkout` with no argument, `submit` without `--auto`, and `merge`.

## Build it

```sh
git switch main && git pull --ff-only
gh stack init pr/refactor          # new branch on main; adopts it if it exists
# ...commit with a Conventional Commits message, run the checks...
gh stack add pr/feature            # new branch on top of the current one
# ...commit, run the checks...
gh stack view --short              # bottom to top: pr/refactor, pr/feature
```

Name every branch yourself. `gh stack add -m "..."` without a name makes up a branch
name from the message, which breaks the `pr/<topic>` rule.

## Open the pull requests

Open each one yourself, so it is a draft with a real title and the filled-in
template. Then let `gh stack submit --auto` link them; it only creates pull requests
for branches that have none, so by then it just sets the bases and makes the stack.

```sh
gh stack push
gh pr create --draft --base main --head pr/refactor --title "<title>" --body-file - <<'EOF'
...the filled-in template...
EOF
gh pr create --draft --base pr/refactor --head pr/feature --title "<title>" --body-file - <<'EOF'
...the filled-in template, Changes ending with "Stacked on #<refactor number>."...
EOF
gh stack submit --auto
gh stack view --json               # each branch shows its pull request
```

- Each title is a full Conventional Commits title: each becomes its own commit on
  `main`.
- Never `gh stack submit` before every branch has a pull request: its editor opens
  them ready for review, and `--auto` gives them generated titles that fail the
  title check.
- Never pass `--open`. Wait for each pull request's checks with
  `tools/wait-checks.sh`, then `gh pr ready <number>`, as `ship-pr` says.

## Fix a lower pull request

```sh
git switch pr/refactor             # or gh stack down / gh stack bottom
# ...commit the fix...
gh stack rebase --upstack          # rebases every branch above this one
gh stack push                      # force-with-lease, all branches or none
```

On a conflict, fix the files, `git add` them and `gh stack rebase --continue`, or
`gh stack rebase --abort` to put every branch back. cubic may push its own fixes to
a branch: `git pull --rebase` on each branch it touched before you restack, or the
force push drops its commit.

To add a step later, check out the top branch and `gh stack add pr/<topic>`. To
insert, drop, reorder or fold branches, ask the user to run `gh stack modify`; it is
interactive.

## After the bottom one merges

Merging is a person's job; never run `gh stack merge`. On GitHub, merging a pull
request in a stack merges every one below it too, each squashed onto `main`.

GitHub then moves the next pull request to `main` and rebases the branches above.
Catch up locally and drop the merged branch:

```sh
gh stack sync --prune
gh stack view --short              # the merged branch is gone or marked merged
gh pr view <next number> --json baseRefName --jq .baseRefName   # main
```

If `sync` says GitHub retargeted the stack onto a base that did not merge, stop and
ask the user before passing `--accept-base-change`: it drops that base's commits.

## Without gh stack

Build each branch from the one below and open it against that branch:

```sh
git switch -c pr/refactor main
# ...commit, run the checks...
git push -u origin pr/refactor
gh pr create --draft --base main --title "<title>" --body-file - <<'EOF'
...
EOF
git switch -c pr/feature pr/refactor
# ...commit, run the checks...
git push -u origin pr/feature
gh pr create --draft --base pr/refactor --title "<title>" --body-file - <<'EOF'
...Changes ending with "Stacked on #<refactor number>."...
EOF
```

Fix a lower one, then rebase the top branch; `--update-refs` moves every branch in
between, except one checked out in another worktree (check `git worktree list` and
switch that worktree away first):

```sh
git switch pr/refactor && git commit ...
git switch pr/top
git rebase --update-refs pr/refactor
git push --force-with-lease origin pr/feature pr/top   # every branch above the changed one
```

After the bottom one is squash-merged, the branches above still hold its commits.
Take the merged pull request's last commit from GitHub, not your local branch:

```sh
old=$(gh pr view <merged number> --json headRefOid --jq .headRefOid)
git fetch origin main "pull/<merged number>/head"
git switch pr/top
git rebase --update-refs --onto origin/main "$old"
git push --force-with-lease origin <every remaining branch>
gh pr view <next number> --json baseRefName --jq .baseRefName   # should be main
gh pr edit <next number> --base main                            # if it is not
```

Without a number, `gh pr edit` changes the pull request of the branch you are on,
which is the top one, not the next one.

## Check your work

- `git log --oneline origin/main..pr/top` shows only the stack's own commits, in
  order, and no commits by unknown authors.
- `gh pr list --author @me --json number,headRefName,baseRefName,isDraft` shows each
  base is the branch below, the bottom one is `main`, and none is ready before its
  checks passed.
- Never merge a pull request into the branch below it; always merge into `main`
  from the bottom. Merging is a person's job.
