#!/usr/bin/env bash
# Runs `cargo fmt` in a checkout without the private modules. rustfmt
# opens the file of every `mod`, even one a cfg feature turns off, and stops at
# the first that is missing. The public repository does not have the `team` and
# `coderlabs` modules, so while rustfmt runs this puts an empty file in place of
# each one, and removes them afterwards. CI runs it with --check.
#
#   tools/rustfmt.sh           # format the code
#   tools/rustfmt.sh --check   # only report what would change
set -euo pipefail
cd "$(dirname "$0")/.."
root=$(pwd -P)

stubs=()
dirs=()
cleanup() {
  local f i
  # ${a[@]+...}: an empty array is "unbound" under set -u in macOS's bash 3.2.
  for f in ${stubs[@]+"${stubs[@]}"}; do rm -f "$f"; done
  # Parents were created first, so remove them last.
  for ((i = ${#dirs[@]} - 1; i >= 0; i--)); do rmdir "${dirs[i]}" 2>/dev/null || true; done
}
trap cleanup EXIT

stub() {
  local file=$1 dir new=()
  dir=$(dirname "$file")
  while [[ ! -d $dir ]]; do new=("$dir" ${new[@]+"${new[@]}"}); dir=$(dirname "$dir"); done
  for dir in ${new[@]+"${new[@]}"}; do mkdir "$dir"; dirs+=("$dir"); done
  # A newline, not an empty file: --check wants a file to end with one.
  echo > "$file"
  stubs+=("$file")
}

# Each rustfmt run finds only one missing module and takes seconds, so first
# guess them from the `mod name;` lines at the top level of each file. The loop
# below still catches any this misses (a #[path], a mod inside a mod block).
while IFS=: read -r file decl; do
  [[ $decl =~ mod\ ([a-z0-9_]+)\; ]] || continue
  name=${BASH_REMATCH[1]}
  case $file in
    */main.rs | */lib.rs | */mod.rs | build.rs | examples/* | src/bin/*) dir=$(dirname "$file") ;;
    *) dir=${file%.rs} ;;
  esac
  [[ -e $dir/$name.rs || -e $dir/$name/mod.rs ]] || stub "$root/$dir/$name.rs"
done < <(git ls-files '*.rs' | xargs grep -H -E '^(pub(\([a-z]+\))? )?mod [a-z0-9_]+;' || true)

while :; do
  status=0
  # Not --all: that also formats path dependencies, such as a local GPUI checkout
  # that [patch] points at. Both packages are named: plain `cargo fmt` leaves
  # out arakun-server (server/), which is no dependency of the app.
  out=$(cargo fmt -p arakun -p arakun-server "$@" 2>&1) || status=$?
  missing=$(printf '%s\n' "$out" | sed -n 's/.*failed to resolve mod `[^`]*`: \(.*\) does not exist.*/\1/p' | head -n 1)
  if [[ -z $missing ]]; then
    [[ -z $out ]] || printf '%s\n' "$out"
    exit "$status"
  fi
  if [[ $missing != "$root"/* || -e $missing ]]; then
    printf '%s\n' "$out" >&2
    echo "rustfmt.sh: cannot stand in for $missing" >&2
    exit 1
  fi
  echo "rustfmt.sh: $missing is missing; retrying with an empty one" >&2
  stub "$missing"
done
