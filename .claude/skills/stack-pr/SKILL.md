---
name: stack-pr
description: Build and maintain a stack of dependent pull requests in Arakun - decide whether to stack, branch each step from the one below, fix a lower pull request and restack, and rebase the rest onto main after the bottom one is squash-merged. Use when a change splits into steps where a later one needs an earlier unmerged one, or when the user mentions a stack, restacking, or a pull request "on top of" another.
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

## Build it

```sh
git switch -c pr/refactor main
# ...commit, run the checks...
git push -u origin pr/refactor
gh pr create --draft --base main --title "<title>" --body-file - <<'EOF'
...the filled-in template...
EOF

git switch -c pr/feature pr/refactor
# ...commit, run the checks...
git push -u origin pr/feature
gh pr create --draft --base pr/refactor --title "<title>" --body-file - <<'EOF'
...the filled-in template...
EOF
```

End the Changes section of each upper pull request with `Stacked on #<number>.`
Each pull request is still a full Conventional Commits title, because each becomes
its own commit on `main`.

## Fix a lower pull request

Commit on the lower branch, then rebase the top branch on it. `--update-refs`
moves every branch in between.

```sh
git switch pr/refactor && git commit ...
git switch pr/top
git rebase --update-refs pr/refactor
git push --force-with-lease origin pr/feature pr/top   # every branch above the changed one
```

`--update-refs` skips a branch checked out in another worktree. Run
`git worktree list` first and switch that worktree to another branch.

## After the bottom one merges

Pull requests are squashed, so the branches above still hold the old commits.
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
- `gh pr list --author @me --json number,headRefName,baseRefName` shows each base
  is the branch below, and the bottom one is `main`.
- Never merge a pull request into the branch below it; always merge into `main`
  from the bottom. Merging is a person's job.
