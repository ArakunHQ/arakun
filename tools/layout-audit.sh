#!/usr/bin/env bash
# Opens the main screens in a test Arakun with the layout audit on and checks
# what was drawn (see tools/layout_audit.py). Needs a debug build:
#   cargo build --locked --no-default-features && tools/layout-audit.sh
set -euo pipefail
cd "$(dirname "$0")/.."
bin=./target/debug/arakun
out=$(mktemp -d)
frame="$out/frame.json"
mark="$out/mark"
prev=()
# Each step touches the mark before it acts, and snap only takes a whole frame
# written after that, so a screen is never checked from the frame before it.
step() { touch "$mark"; "$@"; }
snap() {
  sleep 1.3 # let animations settle
  python3 -I tools/layout_audit.py snap "$frame" "$out/$1.json" \
    --newer-than "$mark" "${prev[@]}"
  prev=(--differs-from "$out/$1.json")
}

"$bin" dev stop >/dev/null 2>&1 || true
touch "$mark"
GPUI_LAYOUT_AUDIT="$frame" "$bin" dev start --size 1600x960 >/dev/null
trap '"$bin" dev stop >/dev/null 2>&1 || true' EXIT
sleep 1.7
snap main
step "$bin" dev key ctrl-k;         snap palette
step "$bin" dev type is;            snap palette-search
step "$bin" dev key right;          snap palette-actions
"$bin" dev key escape escape;       sleep 0.5
python3 -I tools/layout_audit.py "$out"/main.json "$out"/palette.json \
  "$out"/palette-search.json "$out"/palette-actions.json
