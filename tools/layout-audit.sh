#!/usr/bin/env bash
# Opens the main screens in a test Arakun with the layout audit on and checks
# what was drawn (see tools/layout_audit.py). Needs a debug build:
#   cargo build --locked && tools/layout-audit.sh
set -euo pipefail
cd "$(dirname "$0")/.."
bin=./target/debug/arakun
out=$(mktemp -d)
frame="$out/frame.json"
snap() { sleep 1.3; cp "$frame" "$out/$1.json"; }

"$bin" dev stop >/dev/null 2>&1 || true
GPUI_LAYOUT_AUDIT="$frame" "$bin" dev start --size 1600x960 >/dev/null
trap '"$bin" dev stop >/dev/null 2>&1 || true' EXIT
sleep 3
snap main
"$bin" dev key ctrl-k;              snap palette
"$bin" dev type is;                 snap palette-search
"$bin" dev key right;               snap palette-actions
"$bin" dev key escape escape;       sleep 0.5
python3 -I tools/layout_audit.py "$out"/*.json
