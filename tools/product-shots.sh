#!/usr/bin/env bash
# Takes product screenshots of Arakun's start screen in dark and light, for the
# brand book, the README and the site.
#
#   tools/product-shots.sh [--out DIR] [--modes "dark light"] [--palette ID]
#                          [--size WxH] [--project DIR] [--wait SECONDS] [--no-build]
#
# Each mode gets a fresh Arakun of its own, built from this checkout, that never
# touches yours: on macOS a separate process with a scratch home, data and
# runtime folder, its window captured alone with `screencapture`; on Linux the
# test Arakun of `arakun dev` on its Xvfb display. The mode and palette are set
# in that Arakun's palettes.json before it starts, so nothing is clicked or
# typed: no keystroke can land in another app.
#
# The shots show a demo project (`/tmp/brave-otter`, made for the run and
# removed after, unless --project names one) and a neutral shell prompt, so no
# real path, account or host name gets in. Look at every file before you
# publish it anyway.
#
# Writes <out>/<mode>.png (default out: ${TMPDIR:-/tmp}/arakun-product-shots).
# Needs: macOS with swiftc (Xcode command line tools), or Linux with what
# `arakun dev` needs (tools/setup-dev.sh --yes --display).
set -euo pipefail

root_dir=$(cd "$(dirname "$0")/.." && pwd)
out="${TMPDIR:-/tmp}/arakun-product-shots"
modes="dark light"
palette=black
size=1440x900
project=""
settle=10
build=1

while [ $# -gt 0 ]; do
  case $1 in
    --out) out=$2; shift 2 ;;
    --modes) modes=$2; shift 2 ;;
    --palette) palette=$2; shift 2 ;;
    --size) size=$2; shift 2 ;;
    --project) project=$2; shift 2 ;;
    --wait) settle=$2; shift 2 ;;
    --no-build) build=0; shift ;;
    -h|--help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "product-shots: unknown argument $1 (see --help)" >&2; exit 2 ;;
  esac
done

for m in $modes; do
  case $m in dark|light) ;; *) echo "product-shots: mode must be dark or light, not $m" >&2; exit 2 ;; esac
done

if [ "$build" = 1 ]; then
  (cd "$root_dir" && cargo build --no-default-features)
fi
bin="$root_dir/target/debug/arakun"
[ -x "$bin" ] || { echo "product-shots: no $bin; build it or drop --no-build" >&2; exit 1; }

scratch=$(mktemp -d "${TMPDIR:-/tmp}/arakun-shots.XXXXXX")
# A test Arakun still running when the script stops, on an error too.
running=""
demo=""
cleanup() {
  [ -n "$running" ] && kill "$running" 2>/dev/null
  [ -n "$demo" ] && rm -rf "$demo"
  rm -rf "$scratch"
}
trap cleanup EXIT

# A demo project with one commit, so the branch and status read like a real one.
# A short fixed path, since the status bar shows it.
if [ -z "$project" ]; then
  project=/tmp/brave-otter
  if [ -e "$project" ]; then
    echo "product-shots: $project exists; remove it or pass --project" >&2
    exit 1
  fi
  demo=$project
  mkdir -p "$project"
  git -C "$project" init -q -b main
  printf '# brave-otter\n\nA demo project for Arakun screenshots.\n' > "$project/README.md"
  git -C "$project" add README.md
  git -C "$project" -c user.name=demo -c user.email=demo@example.com commit -qm "init"
fi
mkdir -p "$out"

# palettes.json for a mode: the palette in use and light or dark.
palettes() {
  local light=false
  [ "$1" = light ] && light=true
  printf '{"current":"%s","light":%s,"custom":[]}\n' "$palette" "$light"
}

# A shell prompt with no user or host in it, for the terminal at the bottom.
neutral_shell() {
  mkdir -p "$1"
  printf "PROMPT='%%1~ %%# '\n" > "$1/.zshrc"
}

shoot_macos() {
  local mode=$1 dir="$scratch/$1"
  mkdir -p "$dir/home" "$dir/data/arakun" "$dir/run"
  palettes "$mode" > "$dir/palettes.json"
  neutral_shell "$dir/zdot"
  # The largest on-screen window of a process, by its CoreGraphics number.
  if [ ! -x "$scratch/winid" ]; then
    cat > "$scratch/winid.swift" <<'SWIFT'
import CoreGraphics
let pid = Int32(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
var best = (0, 0.0)
for w in list where (w[kCGWindowOwnerPID as String] as? Int32) == pid {
  let b = w[kCGWindowBounds as String] as! [String: Double]
  let area = (b["Width"] ?? 0) * (b["Height"] ?? 0)
  if area > best.1 { best = (w[kCGWindowNumber as String] as! Int, area) }
}
print(best.0)
SWIFT
    swiftc -O "$scratch/winid.swift" -o "$scratch/winid"
  fi
  # A clean environment: no API keys, sockets or settings of the person's own
  # Arakun reach the test one. ARAKUN_DEV sets the window size and keeps its
  # notifications off the desktop.
  env -i HOME="$dir/home" PATH=/usr/bin:/bin:/usr/sbin:/sbin TMPDIR="$dir/run" \
    LANG=en_US.UTF-8 USER=demo LOGNAME=demo SHELL=/bin/zsh ZDOTDIR="$dir/zdot" \
    ARAKUN_STATE="$dir/state.json" XDG_DATA_HOME="$dir/data" XDG_RUNTIME_DIR="$dir/run" \
    ARAKUN_CI=off ARAKUN_GITHUB=readonly ARAKUN_DEV="$size" \
    "$bin" "$project" > "$dir/arakun.log" 2>&1 &
  local pid=$! id=0
  running=$pid
  sleep "$settle"
  for _ in $(seq 30); do
    id=$("$scratch/winid" "$pid")
    [ "$id" != 0 ] && break
    sleep 1
  done
  if [ "$id" = 0 ]; then
    echo "product-shots: no window for $mode; see $dir/arakun.log" >&2
    return 1
  fi
  # -o leaves out the window's shadow; -l captures that window alone, even
  # when another one covers it. Right after it shows, the window may not be
  # drawn yet, or a first window may give way to the real one: look again.
  local ok=0
  for _ in 1 2 3 4 5; do
    sleep 2
    id=$("$scratch/winid" "$pid")
    if [ "$id" != 0 ] && screencapture -o -x -l"$id" "$out/$mode.png" 2>/dev/null; then
      ok=1
      break
    fi
  done
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  running=""
  if [ "$ok" = 0 ]; then
    echo "product-shots: could not capture the $mode window; see $dir/arakun.log" >&2
    return 1
  fi
  echo "$out/$mode.png"
}

shoot_linux() {
  local mode=$1
  "$bin" dev stop >/dev/null 2>&1 || true
  # The test Arakun keeps palettes.json next to its state, which
  # `arakun dev start` leaves in place.
  local devroot
  devroot=$(dirname "$("$bin" dev socket)")
  mkdir -p "$devroot"
  palettes "$mode" > "$devroot/palettes.json"
  if command -v zsh >/dev/null; then
    neutral_shell "$scratch/zdot"
    SHELL=$(command -v zsh) ZDOTDIR="$scratch/zdot" "$bin" dev start "$project" --size "$size"
  else
    echo "product-shots: no zsh, so the terminal shows your usual prompt" >&2
    "$bin" dev start "$project" --size "$size"
  fi
  sleep "$settle"
  "$bin" dev shot "$out/$mode.png"
  "$bin" dev stop >/dev/null
  echo "$out/$mode.png"
}

for mode in $modes; do
  case $(uname -s) in
    Darwin) shoot_macos "$mode" ;;
    Linux) shoot_linux "$mode" ;;
    *) echo "product-shots: macOS and Linux only" >&2; exit 1 ;;
  esac
done
