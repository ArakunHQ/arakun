#!/bin/sh
# Sets up a Linux or macOS machine to build Arakun and run the checks CI runs.
# For contributors and for coding agents. Windows: tools/setup-dev.ps1.
#
#   tools/setup-dev.sh [--yes] [--check] [--relay] [--display] [--hooks] [--build]
#
#   --yes      do not ask before installing (implied when stdin is not a terminal)
#   --check    only report what is missing; exit 1 if anything is
#   --relay    also Node 24+ and the npm packages of relay/
#   --display  also Xvfb and tesseract, for `arakun dev` (Linux only)
#   --hooks    check commit messages locally (git config core.hooksPath tools/hooks)
#   --build    build the tests once, so the first clippy or test run is quick
#
# Always: the system libraries GPUI needs (apt or pacman; on macOS the Xcode Command
# Line Tools), and Rust with clippy through rustup. It builds without the browser, as
# CI does; for CEF see "With the browser" in the README. Running it again is safe.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
yes=0
check=0
relay=0
display=0
hooks=0
build=0
while [ $# -gt 0 ]; do
  case $1 in
    -y|--yes) yes=1 ;;
    --check) check=1 ;;
    --relay) relay=1 ;;
    --display) display=1 ;;
    --hooks) hooks=1 ;;
    --build) build=1 ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "setup-dev: unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
# An agent or a CI job cannot answer a prompt.
[ -t 0 ] || yes=1

missing=0
say() { echo "setup-dev: $*"; }
ok() { echo "  ok       $*"; }
lack() { echo "  missing  $*"; missing=1; }

# ask QUESTION: yes with --yes, otherwise the answer (Enter means yes).
ask() {
  [ $yes = 1 ] && return 0
  printf 'setup-dev: %s [Y/n] ' "$1"
  read -r answer || return 1
  case $answer in [nN]*) return 1 ;; *) return 0 ;; esac
}

# as_root CMD...: runs CMD as root. Without a terminal sudo must not wait for a password,
# so then it fails at once and prints the command to run by hand.
as_root() {
  if [ "$(id -u)" = 0 ]; then
    "$@"
  elif ! command -v sudo >/dev/null; then
    say "need root for: $*" >&2
    return 1
  elif [ $yes = 1 ] && ! sudo -n true 2>/dev/null; then
    say "sudo needs a password; run this yourself: sudo $*" >&2
    return 1
  elif [ $yes = 1 ]; then
    sudo -n "$@"
  else
    sudo "$@"
  fi
}

os=$(uname)
pm=
if [ "$os" = Linux ]; then
  if command -v apt-get >/dev/null; then pm=apt
  elif command -v pacman >/dev/null; then pm=pacman
  fi
fi

# The packages from "Build from source" in the README; CI installs the same on Ubuntu.
case $pm in
  apt) pkgs="build-essential pkg-config git curl ca-certificates
    libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb1-dev
    libfontconfig1-dev libfreetype-dev libvulkan-dev libssl-dev"
    display_pkgs="xvfb tesseract-ocr" ;;
  pacman) pkgs="base-devel pkgconf git curl
    libxkbcommon libxkbcommon-x11 wayland libxcb fontconfig freetype2
    vulkan-icd-loader vulkan-headers openssl"
    display_pkgs="xorg-server-xvfb tesseract tesseract-data-eng" ;;
  *) pkgs='' display_pkgs='' ;;
esac
[ $display = 1 ] && pkgs="$pkgs $display_pkgs"

# Prints the packages of $pkgs that are not installed.
not_installed() {
  # shellcheck disable=SC2086 # the package list is split on purpose
  case $pm in
    apt)
      for p in $pkgs; do
        dpkg-query -W -f='${Status}\n' "$p" 2>/dev/null | grep -q '^install ok installed$' || echo "$p"
      done ;;
    # -T prints the ones no installed package satisfies.
    pacman) pacman -T $pkgs || true ;;
  esac
}

# --- system packages ---
say "system packages"
if [ "$os" = Darwin ]; then
  if xcode-select -p >/dev/null 2>&1; then
    ok "Xcode Command Line Tools"
  elif [ $check = 1 ]; then
    lack "Xcode Command Line Tools (xcode-select --install)"
  else
    # It opens a dialog; there is no unattended way to accept Apple's license.
    xcode-select --install || true
    say "finish the Xcode Command Line Tools install in its window, then run this again" >&2
    exit 1
  fi
elif [ -z "$pm" ]; then
  # No list for other distributions yet: they need the same libraries under their names.
  say "no apt-get or pacman here; install the equivalents of the Ubuntu packages in the README" >&2
  [ $check = 1 ] && missing=1
else
  todo=$(not_installed | tr '\n' ' ' | sed 's/ *$//')
  if [ -z "$todo" ]; then
    n=0
    for p in $pkgs; do n=$((n + 1)); done
    ok "$n $pm packages"
  elif [ $check = 1 ]; then
    lack "$todo"
  elif ask "install with $pm: $todo?"; then
    # shellcheck disable=SC2086 # the package list is split on purpose
    case $pm in
      apt)
        # A fresh VM often runs unattended-upgrades at boot: wait for its lock, not fail.
        as_root env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 update -q
        as_root env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 \
          install -y -q --no-install-recommends $todo ;;
      pacman) as_root pacman -S --needed --noconfirm $todo ;;
    esac
  else
    say "skipped; the build needs: $todo" >&2
    exit 1
  fi
fi

# --- Rust ---
say "Rust"
# A shell that started before rustup was installed does not have it on PATH yet.
cargo_bin=${CARGO_HOME:-$HOME/.cargo}/bin
case :$PATH: in *:"$cargo_bin":*) ;; *) [ -d "$cargo_bin" ] && PATH=$cargo_bin:$PATH ;; esac
installed_rustup=0
if command -v rustup >/dev/null; then
  ok "rustup ($(rustc --version 2>/dev/null || echo 'no default toolchain'))"
  if ! rustc --version >/dev/null 2>&1; then
    if [ $check = 1 ]; then lack "a default toolchain (rustup default stable)"
    else rustup default stable
    fi
  fi
elif command -v cargo >/dev/null; then
  # A distribution's Rust works if it is recent enough; rustup is what CI uses.
  ok "cargo without rustup ($(cargo --version)); if the build asks for a newer Rust, use rustup"
elif [ $check = 1 ]; then
  lack "Rust (https://rustup.rs)"
elif ask "install Rust with rustup (https://rustup.rs) into ~/.cargo?"; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
    sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable --component clippy
  PATH=$cargo_bin:$PATH
  installed_rustup=1
else
  say "skipped; the build needs Rust" >&2
  exit 1
fi

if cargo clippy --version >/dev/null 2>&1; then
  ok "$(cargo clippy --version)"
elif [ $check = 1 ]; then
  lack "clippy (rustup component add clippy)"
elif command -v rustup >/dev/null; then
  rustup component add clippy
else
  say "no clippy; install it with your distribution's Rust" >&2
  exit 1
fi

if [ $installed_rustup = 1 ]; then
  # Claude Code's SessionStart hook: later commands in the session get the new PATH.
  if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
    echo "export PATH=\"$cargo_bin:\$PATH\"" >> "$CLAUDE_ENV_FILE"
  else
    say "add $cargo_bin to PATH (or open a new shell after: . \"${CARGO_HOME:-$HOME/.cargo}/env\")"
  fi
fi

# --- relay ---
if [ $relay = 1 ]; then
  say "relay (Node 24+)"
  major=$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo 0)
  if [ "$major" -lt 24 ]; then
    # Node comes in too many ways (nvm, fnm, distribution, nodejs.org) to pick one here.
    lack "Node 24 or newer (have $(node --version 2>/dev/null || echo none)); see https://nodejs.org"
  elif [ $check = 1 ]; then
    ok "node $(node --version)"
    if [ -d "$here/relay/node_modules" ]; then ok "relay/node_modules"; else lack "relay/node_modules (npm ci)"; fi
  else
    ok "node $(node --version)"
    (cd "$here/relay" && npm ci --no-audit --no-fund)
  fi
fi

# --- display tools ---
if [ $display = 1 ]; then
  say "arakun dev (Xvfb, tesseract)"
  if [ "$os" != Linux ]; then
    say "arakun dev only runs on Linux; skipped"
  else
    for t in Xvfb tesseract; do
      if command -v $t >/dev/null; then ok $t; else lack $t; fi
    done
  fi
fi

# --- git hook ---
if [ $hooks = 1 ]; then
  say "commit-msg hook"
  if [ "$(git -C "$here" config core.hooksPath || true)" = tools/hooks ]; then
    ok "core.hooksPath is tools/hooks"
  elif [ $check = 1 ]; then
    lack "core.hooksPath (git config core.hooksPath tools/hooks)"
  else
    git -C "$here" config core.hooksPath tools/hooks
    ok "core.hooksPath set to tools/hooks (undo: git config --unset core.hooksPath)"
  fi
fi

if [ $check = 1 ]; then
  if [ $missing = 1 ]; then say "something is missing; run tools/setup-dev.sh to install it"; exit 1; fi
  say "everything is in place"
  exit 0
fi
# Things this script does not install itself (Node, a distribution without apt or pacman).
[ $missing = 1 ] && { say "done, but see 'missing' above"; exit 1; }

# --- warm build ---
if [ $build = 1 ]; then
  say "building the tests (the first build takes a while)"
  (cd "$here" && cargo test --locked --no-default-features --no-run)
fi

say "ready. The checks CI runs:"
cat <<'EOF'
  cargo clippy --locked --all-targets --no-default-features -- -D warnings
  cargo test --locked --no-default-features
EOF
