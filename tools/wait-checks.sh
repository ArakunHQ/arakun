#!/usr/bin/env bash
# Waits for the checks a pull request needs before `gh pr ready`, and exits 0
# when they passed, 1 when one failed or was cancelled.
#
#   tools/wait-checks.sh          # the pull request of the current branch
#   tools/wait-checks.sh 393
#
# It waits for the checks main's ruleset requires, apart from "Human approval",
# which waits for a review that only comes after `gh pr ready`. It also waits
# for the macOS job when the diff touches macOS code. Other checks, such as the
# macOS job for other changes and CodeQL, can still be running: they take a
# shared runner pool, and waiting for them cost up to 100 minutes a pull request.
#
# The required checks come from the ruleset, not from the checks already on
# the pull request, so it does not stop early before they have started.
# WAIT_CHECKS_TIMEOUT sets how many minutes it waits (60 by default).
set -euo pipefail
export LC_ALL=C

MACOS_CHECK="Clippy and tests (macOS, no browser)"
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

required=$(gh api "repos/{owner}/{repo}/rules/branches/main" \
  --jq '.[] | select(.type == "required_status_checks")
        | .parameters.required_status_checks[].context') ||
  die "could not read the required checks of main"
wanted=$(grep -vxF "$SKIP_CHECK" <<< "$required" || true)

# macOS builds code behind cfg(target_os = "macos") and macOS-only files, and
# a new dependency or toolchain can break only there. Only changed lines in .rs
# files count, so docs that quote the cfg don't. A changed line inside a macOS
# function but far from its cfg is not seen; such files are named *mac*.
macos_reason=""
if ! files=$(gh pr diff ${pr:+"$pr"} --name-only); then
  macos_reason="could not list the changed files"
elif hit=$(grep -iE 'macos|(^|/)mac(\.rs$|/)|^Cargo\.(toml|lock)$|(^|/)build\.rs$|^rust-toolchain\.toml$' <<< "$files" | head -n 1) && [ -n "$hit" ]; then
  macos_reason="$hit changed"
elif ! diff=$(gh pr diff ${pr:+"$pr"}); then
  macos_reason="could not read the diff"
elif awk '/^diff --git / { rs = ($NF ~ /\.rs$/) } rs && /^[-+]/ && !/^(\+\+\+|---) /' <<< "$diff" |
  grep -qE 'target_os *= *"macos"|target_vendor *= *"apple"'; then
  macos_reason="the diff changes code behind target_os = \"macos\""
fi
if [ -n "$macos_reason" ]; then
  echo "Waiting for the macOS job too: $macos_reason."
  wanted=$(printf '%s\n%s\n' "$wanted" "$MACOS_CHECK")
else
  echo "The diff touches no macOS code; not waiting for the macOS job."
fi
wanted=$(grep -v '^$' <<< "$wanted")

deadline=$(($(date +%s) + TIMEOUT_MIN * 60))
last=""
while :; do
  # gh exits 8 while checks are pending, so read the JSON, not the exit code.
  checks=$(gh pr checks ${pr:+"$pr"} --json name,bucket 2> /dev/null || true)
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
    echo "The checks this pull request needs passed."
    exit 0
  fi
  now=$(printf '%s, ' "${pending[@]}")
  [ "$now" = "$last" ] || echo "Waiting for: ${now%, }"
  last=$now
  [ "$(date +%s)" -lt "$deadline" ] || die "still waiting after $TIMEOUT_MIN minutes"
  sleep "$INTERVAL"
done
