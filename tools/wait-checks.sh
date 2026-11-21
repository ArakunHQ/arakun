#!/usr/bin/env bash
# Waits for the checks a pull request needs before `gh pr ready`, and exits 0
# when they passed, 1 when one failed or was cancelled.
#
#   tools/wait-checks.sh          # the pull request of the current branch
#   tools/wait-checks.sh 393
#
# It waits for the checks main's ruleset requires, apart from "Human approval",
# which waits for a review that only comes after `gh pr ready`. It also waits
# for the macOS job when the diff touches macOS code, and for the iOS job when
# it touches the iOS app or what it reads. Other checks, such as the
# macOS job for other changes and CodeQL, can still be running: they take a
# shared runner pool, and waiting for them cost up to 100 minutes a pull request.
#
# The required checks come from the ruleset, not from the checks already on
# the pull request, so it does not stop early before they have started.
# WAIT_CHECKS_TIMEOUT sets how many minutes it waits (60 by default).
set -euo pipefail
export LC_ALL=C

MACOS_CHECK="Clippy and tests (macOS, no browser)"
IOS_CHECK="iOS (Swift tests and simulator build)"
SKIP_CHECK="Human approval"
INTERVAL=20
TIMEOUT_MIN=${WAIT_CHECKS_TIMEOUT:-60}

die() {
  echo "wait-checks: $*" >&2
  exit 2
}

pr=${1:-}
command -v gh > /dev/null || die "needs the GitHub CLI (gh)"
command -v jq > /dev/null || die "needs jq"
# Pin the pull request now: without a number, each gh call would pick the
# pull request of whatever branch is checked out at that moment.
if [ -z "$pr" ]; then
  pr=$(gh pr view --json number --jq .number) || die "no pull request for the current branch"
fi

required=$(gh api "repos/{owner}/{repo}/rules/branches/main" \
  --jq '.[] | select(.type == "required_status_checks")
        | .parameters.required_status_checks[].context') ||
  die "could not read the required checks of main"
required=$(grep -vxF "$SKIP_CHECK" <<< "$required" || true)

head_sha() {
  gh pr view "$pr" --json headRefOid --jq .headRefOid 2> /dev/null || true
}

# macOS builds code behind cfg(target_os = "macos") and macOS-only files, and
# a new dependency or toolchain can break only there. Only .rs files count, so
# docs that quote the cfg don't. Context lines count too, so a change next to
# its cfg is seen. A change inside a macOS function but far from its cfg is
# not; such files are named *mac*. The greps read all their input: an early
# exit could kill the writer with SIGPIPE and, under pipefail, hide a match.
classify() {
  local files diff hit reason=""
  if ! files=$(gh pr diff "$pr" --name-only); then
    reason="could not list the changed files"
  elif hit=$(grep -iE 'macos|(^|/)mac(\.rs$|/)|^Cargo\.(toml|lock)$|(^|/)build\.rs$|^rust-toolchain\.toml$' <<< "$files" | sed -n 1p) && [ -n "$hit" ]; then
    reason="$hit changed"
  elif ! diff=$(gh pr diff "$pr"); then
    reason="could not read the diff"
  elif awk '/^diff --git / { rs = ($NF ~ /\.rs$/); next } rs && /^[-+ ]/ && !/^(\+\+\+|---) /' <<< "$diff" |
    grep -E 'target_os *= *"macos"|target_vendor *= *"apple"' > /dev/null; then
    reason="the diff changes code near target_os = \"macos\""
  fi
  if [ -n "$reason" ]; then
    echo "Waiting for the macOS job too: $reason."
    wanted=$(printf '%s\n%s\n' "$required" "$MACOS_CHECK")
  else
    echo "The diff touches no macOS code; not waiting for the macOS job."
    wanted=$required
  fi
  # The paths ci.yml's `changes` job runs the iOS job for.
  if [ -n "$files" ] && grep -qE '^(ios|phone)/|^assets/brand/mascot\.json$|^assets/fonts/|^\.github/workflows/ci\.yml$|^\.github/actions/' <<< "$files"; then
    echo "Waiting for the iOS job too: the diff touches the iOS app or what it reads."
    wanted=$(printf '%s\n%s\n' "$wanted" "$IOS_CHECK")
  fi
  wanted=$(grep -v '^$' <<< "$wanted" || true)
}

deadline=$(($(date +%s) + TIMEOUT_MIN * 60))
last=""
classified=""
while :; do
  # A push during the wait can bring macOS changes, so classify each new head.
  sha=$(head_sha)
  if [ -z "$classified" ] || { [ -n "$sha" ] && [ "$sha" != "$classified" ]; }; then
    [ -z "$classified" ] || echo "The pull request has a new head; checking its diff again."
    classify
    classified=${sha:-unknown}
    last=""
  fi
  # gh exits 8 while checks are pending, so read the JSON, not the exit code.
  checks=$(gh pr checks "$pr" --json name,bucket 2> /dev/null || true)
  jq -e 'type == "array"' > /dev/null 2>&1 <<< "$checks" || checks='[]'

  pending=() failed=()
  while IFS= read -r name; do
    # A check can report more than once; any failure fails it.
    buckets=$(jq -r --arg n "$name" '.[] | select(.name == $n) | .bucket' <<< "$checks")
    if grep -qxE 'fail|cancel' <<< "$buckets"; then
      failed+=("$name")
    elif [ -z "$buckets" ] || grep -qx pending <<< "$buckets"; then
      pending+=("$name")
    fi
  done <<< "$wanted"

  if [ ${#failed[@]} -gt 0 ]; then
    printf 'Failed: %s\n' "${failed[@]}"
    exit 1
  fi
  if [ ${#pending[@]} -eq 0 ]; then
    # The checks may belong to a head pushed after the one classified.
    [ "$(head_sha)" = "$classified" ] || continue
    echo "The checks this pull request needs passed."
    exit 0
  fi
  now=$(printf '%s, ' "${pending[@]}")
  [ "$now" = "$last" ] || echo "Waiting for: ${now%, }"
  last=$now
  [ "$(date +%s)" -lt "$deadline" ] || die "still waiting after $TIMEOUT_MIN minutes"
  sleep "$INTERVAL"
done
