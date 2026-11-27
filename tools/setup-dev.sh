#!/bin/sh
# Sets up a Linux or macOS machine to build Arakun and run the checks CI runs.
# For contributors and for coding agents. Windows: tools/setup-dev.ps1.
#
#   tools/setup-dev.sh [--yes] [--check] [--relay] [--display] [--browser] [--ci-tools]
#                      [--hooks] [--build]
#
#   --yes      do not ask before installing (implied when stdin is not a terminal)
#   --check    only report what is missing; exit 1 if anything is
#   --relay    also Node 24+ and the npm packages of cloud/
#   --display  also what `arakun dev` needs: Xvfb and tesseract (macOS: tesseract,
#              through Homebrew)
#   --browser  also the CEF SDK a build with the browser needs, linked at .cef-sdk (macOS only)
#   --ci-tools also cargo-deny and zizmor, with cargo install (each one compiles for a
#              few minutes the first time)
#   --hooks    turn on the Git hooks in lefthook.yml (lefthook install), and install
#              gitleaks for the secret scan before a commit
#   --build    build the tests once, so the first clippy or test run is quick
#
# Always: the system libraries GPUI needs (apt, dnf or pacman; on macOS the Xcode Command
# Line Tools), and the Rust version in rust-toolchain.toml with clippy and rustfmt, through rustup.
# It builds without the browser, as CI does; for CEF see --browser and docs/browser.md.
# Running it again is safe.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
yes=0
check=0
relay=0
display=0
browser=0
ci_tools=0
hooks=0
build=0
while [ $# -gt 0 ]; do
  case $1 in
    -y|--yes) yes=1 ;;
    --check) check=1 ;;
    --relay) relay=1 ;;
    --display) display=1 ;;
    --browser) browser=1 ;;
    --ci-tools) ci_tools=1 ;;
    --hooks) hooks=1 ;;
    --build) build=1 ;;
    -h|--help) sed -n '2,23p' "$0"; exit 0 ;;
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
  elif command -v dnf >/dev/null; then pm=dnf
  elif command -v pacman >/dev/null; then pm=pacman
  fi
fi

# The packages from docs/building.md; CI installs the same on Ubuntu.
case $pm in
  apt) pkgs="build-essential pkg-config git curl ca-certificates
    libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb1-dev
    libfontconfig1-dev libfreetype-dev libvulkan-dev libssl-dev"
    display_pkgs="xvfb tesseract-ocr" ;;
  # libX11-devel has x11-xcb.pc; vulkan-loader-devel pulls in vulkan-headers.
  dnf) pkgs="gcc gcc-c++ make pkgconf-pkg-config git curl ca-certificates
    libxkbcommon-devel libxkbcommon-x11-devel wayland-devel libX11-devel libxcb-devel
    fontconfig-devel freetype-devel vulkan-loader-devel openssl-devel"
    display_pkgs="xorg-x11-server-Xvfb tesseract tesseract-langpack-eng" ;;
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
    # By what is provided, so the curl-minimal of older Fedora counts as curl.
    dnf)
      for p in $pkgs; do
        rpm -q --whatprovides "$p" >/dev/null 2>&1 || echo "$p"
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
  say "no apt-get, dnf or pacman here; install the equivalents of the Ubuntu packages in docs/building.md" >&2
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
      dnf) as_root dnf install -y -q --setopt=install_weak_deps=False $todo ;;
      pacman) as_root pacman -S --needed --noconfirm $todo ;;
    esac
  else
    say "skipped; the build needs: $todo" >&2
    exit 1
  fi
fi

# --- Rust ---
say "Rust"
# A shell that started before rustup was installed, or one that does not read the
# profile, does not have it on PATH yet.
cargo_bin=${CARGO_HOME:-$HOME/.cargo}/bin
added_cargo_bin=0
case :$PATH: in
  *:"$cargo_bin":*) ;;
  *) [ -d "$cargo_bin" ] && PATH=$cargo_bin:$PATH && added_cargo_bin=1 ;;
esac
if command -v rustup >/dev/null; then
  # From /, so rust-toolchain.toml does not apply: this is the default toolchain.
  ok "rustup ($(cd / && RUSTUP_AUTO_INSTALL=0 rustc --version 2>/dev/null || echo 'no default toolchain'))"
  if ! (cd / && RUSTUP_AUTO_INSTALL=0 rustc --version) >/dev/null 2>&1; then
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
  [ $added_cargo_bin = 1 ] || PATH=$cargo_bin:$PATH
  added_cargo_bin=1
else
  say "skipped; the build needs Rust" >&2
  exit 1
fi

# In the checkout, rustup uses the version and components in rust-toolchain.toml. The
# check must not install it, so rustup's own install on first use is off for it.
rust_missing=0
for component in clippy rustfmt; do
  subcommand=$component
  [ "$component" = rustfmt ] && subcommand=fmt
  if version=$(cd "$here" && RUSTUP_AUTO_INSTALL=0 cargo "$subcommand" --version 2>/dev/null); then
    ok "$version"
  else
    rust_missing=1
    if [ $check = 1 ]; then
      lack "$component of the Rust version in rust-toolchain.toml (rustup toolchain install, in the checkout)"
    fi
  fi
done
if [ $rust_missing = 1 ] && [ $check = 0 ]; then
  if command -v rustup >/dev/null; then
    (cd "$here" && rustup toolchain install --no-self-update)
  else
    say "missing Rust components; install clippy and rustfmt with your distribution's Rust" >&2
    exit 1
  fi
fi

if [ $added_cargo_bin = 1 ]; then
  # Claude Code's SessionStart hook: later commands in the session get the new PATH.
  # --check only reports, so it leaves the file alone.
  if [ $check = 0 ] && [ -n "${CLAUDE_ENV_FILE:-}" ]; then
    echo "export PATH=\"$cargo_bin:\$PATH\"" >> "$CLAUDE_ENV_FILE"
  else
    say "add $cargo_bin to PATH (or open a new shell after: . \"${CARGO_HOME:-$HOME/.cargo}/env\")"
  fi
fi

# --- cargo-deny and zizmor ---
# CI runs them when Cargo.toml, Cargo.lock, deny.toml or .github/ changes. zizmor comes at
# the version the zizmor workflow pins, so it finds what CI finds.
if [ $ci_tools = 1 ]; then
  say "cargo-deny and zizmor"
  zizmor_version=$(sed -n 's/^ *version: *\([0-9][0-9.]*\) *$/\1/p' \
    "$here/.github/workflows/zizmor.yml" | head -n 1)
  for spec in cargo-deny "zizmor${zizmor_version:+@$zizmor_version}"; do
    tool=${spec%@*}
    want=
    case $spec in *@*) want=${spec#*@} ;; esac
    if have=$("$tool" --version 2>/dev/null); then
      # Both print "<name> <version>". Another version stays; the user may want it.
      if [ -n "$want" ] && [ "${have##* }" != "$want" ]; then
        ok "$have; CI runs $want (cargo install --locked $spec)"
      else
        ok "$have"
      fi
    elif [ $check = 1 ]; then
      lack "$tool (cargo install --locked $spec)"
    elif ask "install $spec with cargo install (it compiles for a few minutes)?"; then
      cargo install --locked "$spec"
      ok "$spec"
    else
      lack "$tool (cargo install --locked $spec)"
    fi
  done
fi

# --- relay ---
if [ $relay = 1 ]; then
  say "cloud Worker (Node 24+)"
  major=$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo 0)
  if [ "$major" -lt 24 ]; then
    # Node comes in too many ways (nvm, fnm, distribution, nodejs.org) to pick one here.
    lack "Node 24 or newer (have $(node --version 2>/dev/null || echo none)); see https://nodejs.org"
  elif [ $check = 1 ]; then
    ok "node $(node --version)"
    if [ -d "$here/cloud/node_modules" ]; then ok "cloud/node_modules"; else lack "cloud/node_modules (npm ci)"; fi
  else
    ok "node $(node --version)"
    (cd "$here/cloud" && npm ci --no-audit --no-fund)
  fi
fi

# --- display tools ---
if [ $display = 1 ]; then
  if [ "$os" = Darwin ]; then
    # No Xvfb: the test Arakun runs in a window of its own. Only reading the
    # screen (`arakun dev text`, clicking by text) needs tesseract.
    say "arakun dev (tesseract)"
    if command -v tesseract >/dev/null; then
      ok tesseract
    elif [ $check = 1 ]; then
      lack "tesseract (brew install tesseract)"
    elif ! command -v brew >/dev/null; then
      lack "tesseract: install Homebrew (https://brew.sh), then brew install tesseract"
    elif ask "install tesseract with Homebrew?" && brew install tesseract; then
      ok tesseract
    else
      lack "tesseract (brew install tesseract)"
    fi
  elif [ "$os" != Linux ]; then
    say "arakun dev only runs on Linux and macOS; skipped"
  else
    say "arakun dev (Xvfb, tesseract)"
    for t in Xvfb tesseract; do
      if command -v $t >/dev/null; then ok $t; else lack $t; fi
    done
  fi
fi

# --- browser (CEF) ---
# The `cef` crate's own download reads the CDN's index first, which has grown past what its
# HTTP client accepts. So this fetches the archive the release workflow pins, checks its
# SHA-1, and links it where .cargo/config.toml points CEF_PATH. The SDK lives outside the
# checkout so every worktree shares one copy. Keep the version and checksums in step with
# .github/workflows/release.yml and ci.yml.
if [ $browser = 1 ]; then
  say "browser (CEF 156.0.15)"
  if [ "$os" != Darwin ]; then
    say "on Linux, see docs/browser.md; skipped"
  else
    for t in cmake ninja; do
      if command -v $t >/dev/null; then ok $t; else lack "$t (brew install cmake ninja)"; fi
    done
    # The hardware, not `uname -m`: that says x86_64 in a shell under Rosetta.
    if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then
      plat=macosarm64 sha1=bcdd1877edb300b0fb91d8f71c0b14aae256cd0f
    else
      plat=macosx64 sha1=13756e3029307720fe00898d54aef8a64c9b4043
    fi
    archive=cef_binary_156.0.15+gcf671c0+chromium-156.0.8078.80_${plat}_minimal.tar.bz2
    sdk=${XDG_DATA_HOME:-$HOME/.local/share}/arakun/cef-sdk/156.0.15-$plat
    link=$here/.cef-sdk
    # fetch-cef.py unpacks with tarfile's extraction filters (Python 3.12, or 3.9.17+ and
    # 3.10.12+ and 3.11.4+). The python3 macOS ships is 3.9.6, so look for a newer one.
    py=
    for p in python3 python3.14 python3.13 python3.12 /opt/homebrew/bin/python3 /usr/local/bin/python3; do
      if command -v "$p" >/dev/null &&
        "$p" -I -c 'import sys, tarfile; sys.exit(not hasattr(tarfile, "data_filter"))' 2>/dev/null; then
        py=$p
        break
      fi
    done
    if [ -d "$link" ] && [ ! -L "$link" ]; then
      # A folder the crate's own download left; replacing it is the user's call.
      ok ".cef-sdk is a folder, left as it is (remove it to use the shared SDK)"
    elif [ -f "$sdk/archive.json" ] && [ "$(readlink "$link" || true)" = "$sdk" ]; then
      ok "CEF SDK at .cef-sdk"
    elif [ $check = 1 ]; then
      [ -f "$sdk/archive.json" ] || [ -n "$py" ] ||
        lack "Python 3.12+ to fetch the CEF SDK (brew install python)"
      lack "CEF SDK at .cef-sdk (tools/setup-dev.sh --browser)"
    elif [ ! -f "$sdk/archive.json" ] && [ -z "$py" ]; then
      lack "Python 3.12+ to fetch the CEF SDK (brew install python), then run tools/setup-dev.sh --browser again"
    else
      # Worktrees share the SDK, and fetch-cef.py clears a fixed <sdk>.work folder when it
      # starts, so two setups at once would delete each other's download. mkdir is the
      # lock, as macOS has no flock.
      lock=$sdk.lock
      mkdir -p "$(dirname "$sdk")"
      waited=0
      until mkdir "$lock" 2>/dev/null; do
        [ $waited = 1 ] || say "waiting for another setup-dev to fetch the CEF SDK (if none runs, remove $lock)"
        waited=1
        sleep 2
      done
      trap 'rmdir "$lock" 2>/dev/null || true' EXIT
      trap 'exit 130' INT TERM
      # Another setup may have fetched it while this one waited.
      [ -f "$sdk/archive.json" ] || "$py" -I "$here/tools/fetch-cef.py" "$archive" "$sha1" "$sdk"
      rmdir "$lock"
      trap - EXIT INT TERM
      ln -sfn "$sdk" "$link"
      ok "CEF SDK at .cef-sdk"
    fi
  fi
fi

# --- git hooks ---
if [ $hooks = 1 ]; then
  say "git hooks (lefthook)"
  # An older --hooks pointed git at tools/hooks, which is gone: git then runs no hooks,
  # lefthook's included.
  if [ "$(git -C "$here" config core.hooksPath || true)" = tools/hooks ]; then
    if [ $check = 1 ]; then
      lack "core.hooksPath is tools/hooks, which is gone (git config --unset core.hooksPath)"
    else
      git -C "$here" config --unset core.hooksPath
      ok "core.hooksPath unset (it pointed at tools/hooks, which is gone)"
    fi
  fi
  # --path-format=absolute needs Git 2.31+; with an older Git, --check may report
  # the hooks as off when they are on.
  hookdir=$(cd "$here" && git rev-parse --path-format=absolute --git-path hooks)
  # On only when every hook lefthook.yml sets is lefthook's and lefthook itself
  # is there to run them.
  on=1
  for h in pre-commit pre-push commit-msg; do
    # Git skips a hook that isn't executable.
    { [ -x "$hookdir/$h" ] && grep -qs lefthook "$hookdir/$h"; } || on=0
  done
  command -v lefthook >/dev/null || on=0

  # The pre-commit secret scan needs gitleaks. A release binary, checked against
  # the hash below, into Cargo's bin folder, which is on PATH already. To bump
  # it, take the hashes from the release's gitleaks_<version>_checksums.txt.
  gitleaks_version=8.30.1
  case $os/$(uname -m) in
    Linux/x86_64) gl_arch=linux_x64 gl_sum=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb ;;
    Linux/aarch64|Linux/arm64) gl_arch=linux_arm64 gl_sum=e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080 ;;
    Darwin/arm64) gl_arch=darwin_arm64 gl_sum=b40ab0ae55c505963e365f271a8d3846efbc170aa17f2607f13df610a9aeb6a5 ;;
    Darwin/x86_64) gl_arch=darwin_x64 gl_sum=dfe101a4db2255fc85120ac7f3d25e4342c3c20cf749f2c20a18081af1952709 ;;
    *) gl_arch= gl_sum= ;;
  esac
  gl_url=https://github.com/gitleaks/gitleaks/releases/download/v$gitleaks_version/gitleaks_${gitleaks_version}_$gl_arch.tar.gz
  if have=$(gitleaks version 2>/dev/null); then
    ok "gitleaks $have"
  elif [ $check = 1 ] || [ -z "$gl_arch" ]; then
    lack "gitleaks, for the secret scan before a commit (https://github.com/gitleaks/gitleaks#installing)"
  elif ask "install gitleaks $gitleaks_version into $cargo_bin?"; then
    gl_tmp=$(mktemp -d)
    curl --proto '=https' --tlsv1.2 -sSfL "$gl_url" -o "$gl_tmp/gitleaks.tar.gz"
    if command -v sha256sum >/dev/null; then
      got=$(sha256sum "$gl_tmp/gitleaks.tar.gz")
    else
      got=$(shasum -a 256 "$gl_tmp/gitleaks.tar.gz")
    fi
    if [ "${got%% *}" != "$gl_sum" ]; then
      rm -rf "$gl_tmp"
      say "gitleaks download does not match its hash; not installed" >&2
      exit 1
    fi
    tar -xzf "$gl_tmp/gitleaks.tar.gz" -C "$gl_tmp" gitleaks
    mkdir -p "$cargo_bin"
    mv "$gl_tmp/gitleaks" "$cargo_bin/gitleaks"
    rm -rf "$gl_tmp"
    ok "gitleaks $gitleaks_version"
  else
    lack "gitleaks, for the secret scan before a commit (https://github.com/gitleaks/gitleaks#installing)"
  fi

  if [ $on = 1 ]; then
    ok "lefthook hooks are on"
  elif ! command -v lefthook >/dev/null; then
    lack "lefthook (https://lefthook.dev/installation/, e.g. brew install lefthook), then run tools/setup-dev.sh --hooks again"
  elif [ $check = 1 ]; then
    lack "lefthook hooks (lefthook install)"
  else
    (cd "$here" && lefthook install) || { say "lefthook install failed"; exit 1; }
    ok "lefthook hooks are on (undo: lefthook uninstall)"
  fi
fi

if [ $check = 1 ]; then
  if [ $missing = 1 ]; then say "something is missing; run tools/setup-dev.sh to install it"; exit 1; fi
  say "everything is in place"
  exit 0
fi
# Things this script does not install itself (Node, a distribution without apt, dnf or pacman).
[ $missing = 1 ] && { say "done, but see 'missing' above"; exit 1; }

# --- warm build ---
if [ $build = 1 ]; then
  say "building the tests (the first build takes a while)"
  (cd "$here" && cargo test --locked --no-default-features --no-run)
fi

say "ready. The checks CI runs (the full list is under \"Checks\" in CONTRIBUTING.md):"
cat <<'EOF'
  tools/rustfmt.sh --check
  cargo clippy --locked --all-targets --no-default-features -- -D warnings
  cargo test --locked --no-default-features
  cargo deny check   # Cargo.toml, Cargo.lock or deny.toml changed (--ci-tools)
  zizmor .github     # .github/ changed (--ci-tools)
EOF
