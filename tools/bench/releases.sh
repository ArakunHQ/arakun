#!/usr/bin/env bash
# Benchmarks two Arakun releases on this machine and compares them.
#
#   tools/bench/releases.sh [--runs N] [--only a,b] [--threshold P] [--out DIR] [BEFORE [AFTER]]
#
# AFTER defaults to the latest release, and BEFORE to the release published
# before AFTER. Both take a release tag (v0.11.0) or a nightly's
# (0.9.1-nightly.202611201542).
#
# Downloads each release's Linux build with `gh`, checks it against the signed
# SHA256SUMS, and runs `bench.py perf` on one after the other. The results go to
# DIR (default target/bench/releases) as <tag>.json, and the comparison as
# comparison.md. Exits 1 when AFTER is slower past the threshold, as
# `bench.py compare` does.
#
# Needs Linux, gh, Xvfb and what a release needs to run (libxkbcommon-x11 and a
# Vulkan driver such as Mesa's). The signature is checked with minisign; without
# it only the checksum is. The Benchmark releases workflow runs this script.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

die() {
  echo "releases.sh: $*" >&2
  exit 2
}

runs=3 only='' threshold=10 out=$root/target/bench/releases
tags=()
while [ $# -gt 0 ]; do
  case $1 in
    --runs | --only | --threshold | --out)
      [ $# -ge 2 ] || die "$1 needs a value"
      case $1 in
        --runs) runs=$2 ;;
        --only) only=$2 ;;
        --threshold) threshold=$2 ;;
        --out) out=$2 ;;
      esac
      shift
      ;;
    -h | --help)
      sed -n '2,18s/^# \{0,1\}//p' "$0"
      exit 0
      ;;
    -*) die "unknown option $1" ;;
    *) tags+=("$1") ;;
  esac
  shift
done
[ ${#tags[@]} -le 2 ] || die "takes two releases at most"

[ "$(uname -s)" = Linux ] || die "the releases are benchmarked on Linux only"
case $(uname -m) in
  x86_64) platform=linux-x86_64 ;;
  aarch64 | arm64) platform=linux-arm64 ;;
  *) die "no release build for $(uname -m)" ;;
esac
for tool in gh Xvfb python3 sha256sum; do
  command -v "$tool" >/dev/null || die "$tool is needed"
done

# gh finds the repository from the checkout, unless GH_REPO names it.
cd "$root"

# A tag as the releases spell it: v0.11.0, or 0.9.1-nightly.202611201542 without the v.
# Also keeps anything else out of the paths below.
tag_of() {
  local tag=$1
  case $tag in
    *-nightly.*) tag=${tag#v} ;;
    v*) ;;
    *) tag=v$tag ;;
  esac
  [[ $tag =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-nightly\.[0-9]+)?$ ]] || die "$1 is not a release tag"
  printf '%s\n' "$tag"
}

if [ -n "${tags[1]:-}" ]; then
  after=$(tag_of "${tags[1]}")
else
  after=$(gh release view --json tagName --jq .tagName) || die "could not find the latest release"
fi
if [ -n "${tags[0]:-}" ]; then
  before=$(tag_of "${tags[0]}")
else
  # The newest release, nightlies aside, from before AFTER was published.
  published=$(gh release view "$after" --json publishedAt --jq .publishedAt) || die "there is no release $after"
  before=$(gh release list --exclude-drafts --exclude-pre-releases --limit 100 --json tagName,publishedAt \
    --jq "map(select(.publishedAt < \"$published\")) | sort_by(.publishedAt) | last | .tagName // empty")
  [ -n "$before" ] || die "found no release from before $after; name one"
fi
[ "$before" != "$after" ] || die "$before twice: name two different releases"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Downloads the release $1, checks it and unpacks it; its binary is then $work/$1/arakun.
fetch() {
  local tag=$1 dir=$work/$1 name=arakun-$1-$platform key ok=''
  local keys=("$root/assets/release-key.pub" "$root/assets/release-key-backup.pub")
  # The nightly key signs nightlies only, as in tools/install-release.sh.
  case $tag in *-nightly.*) keys+=("$root/assets/nightly-key.pub") ;; esac
  mkdir -p "$dir"
  echo "downloading $tag ($platform)" >&2
  gh release download "$tag" --dir "$dir" \
    --pattern "$name.tar.gz" --pattern SHA256SUMS --pattern SHA256SUMS.minisig ||
    die "could not download $name.tar.gz from the release $tag"
  if command -v minisign >/dev/null; then
    for key in "${keys[@]}"; do
      if minisign -Vq -p "$key" -m "$dir/SHA256SUMS" -x "$dir/SHA256SUMS.minisig" 2>/dev/null; then ok=1; fi
    done
    [ -n "$ok" ] || die "the SHA256SUMS of $tag is not signed with one of Arakun's keys"
  else
    echo "warning: the signature of $tag was not checked; install minisign to check it" >&2
  fi
  # Fails too when SHA256SUMS does not list the archive.
  (cd "$dir" && sha256sum --check --ignore-missing --quiet SHA256SUMS) ||
    die "checksum mismatch for $name.tar.gz"
  # The cef folder stays next to the binary, where the app looks for it first.
  tar -C "$dir" -xzf "$dir/$name.tar.gz" --strip-components=1
  [ -x "$dir/arakun" ] || die "the archive of $tag has no arakun binary"
  "$dir/arakun" dev socket >/dev/null 2>&1 ||
    die "$tag has no \`arakun dev\`, which the benchmark drives it with (or it cannot run here)"
}

# Both first, so a release that is missing fails before any time is spent.
fetch "$before"
fetch "$after"

mkdir -p "$out"
rm -f "$out/comparison.md"
for tag in "$before" "$after"; do
  echo "benchmarking $tag" >&2
  "$here/bench.py" perf --bin "$work/$tag/arakun" --runs "$runs" ${only:+--only "$only"} \
    --label "$tag" --out "$out/$tag.json"
done

status=0
{
  echo "## Arakun $before → $after"
  echo
  echo "Median of $runs run(s) of each release's $platform build on Xvfb, on the same machine ($(nproc) CPUs)."
  echo "Flagged: a change past $threshold % and the noise floor."
  echo
  "$here/bench.py" compare --markdown --threshold "$threshold" "$out/$before.json" "$out/$after.json"
} >"$out/comparison.md" || status=$?
cat "$out/comparison.md"
exit "$status"
