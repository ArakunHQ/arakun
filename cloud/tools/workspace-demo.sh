#!/usr/bin/env bash
# One-shot local rehearsal. No registry, cloud CLI or existing rehearsal state.
set -euo pipefail
umask 077
cd "$(dirname "$0")/.."
state=$(mktemp -d /tmp/arakun-workspace-XXXXXXXX)
trap 'rm -rf -- "$state"' EXIT
env -i PATH="$PATH" HOME="$state" TMPDIR="$state" \
  node tools/workspace-demo.mjs "$state" "${1:-swebench-sandbox:latest}"
