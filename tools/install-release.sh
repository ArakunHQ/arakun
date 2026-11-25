#!/bin/sh
# Installs the latest Arakun release for the current user.
#
#   curl -fsSL https://arakun.dev/install | sh
#   curl -fsSL https://arakun.dev/install | sh -s -- --version v0.1.0
#   curl -fsSL https://arakun.dev/install-server | sh
#   curl -fsSL https://arakun.dev/install-server | sh -s -- --nightly
#
# macOS (Apple silicon): ~/Applications/Arakun.app, and ~/.local/bin/arakun linking into it.
# Linux (x86_64, arm64): ~/.local/bin/arakun, and the browser's CEF runtime in
#                        ${XDG_DATA_HOME:-~/.local/share}/arakun/cef, and the app
#                        launcher's entry and icons in applications/arakun.desktop
#                        and icons/hicolor/*/apps/arakun.* there.
# --server (Linux):      only ~/.local/bin/arakun-server, the server without
#                        the window. arakun.dev/install-server serves this script
#                        with `main "$@"` at the end changed to `main --server "$@"`,
#                        so keep that line as it is.
#                        It also installs what the server runs agents with, unless
#                        --no-tools: git and gh with the package manager (as root or
#                        with sudo that needs no password), Node.js from nodejs.org
#                        in ${XDG_DATA_HOME:-~/.local/share}/arakun/node, and Claude
#                        Code with its own installer in ~/.local/bin/claude.
# --nightly:             the newest nightly build of main instead of the latest release.
# ARAKUN_VERSION pins a release, like --version (`nightly` is --nightly).
# Windows: tools/install-release.ps1.
#
# Everything runs from main at the end, so a download cut short runs nothing.
set -eu

repo=arakunhq/arakun
# The minisign keys SHA256SUMS may be signed with: the one release.yml uses
# (assets/release-key.pub), and an offline backup (assets/release-key-backup.pub).
release_keys="RWRHh4aSkL+9oL41eNxaL791lQYzu/un+QvVqfckIj7839aB8lQBoxyn RWSxlRjKJX7FQmC0+on+hpDPmZ/ShKCW4KzyJhbnm0bAc6JGTI7KV+xg"
# The key of the nightly builds (assets/nightly-key.pub). Accepted for a nightly
# version only, as the updater does, so it can never sign a release.
nightly_key="RWTZ1q5XZwa1CsB/oR68Esdai4A24ex+Ez5D/iWLMVCVJGTduA5jPa+y"

say() { printf 'arakun: %s\n' "$*"; }
die() {
  printf 'arakun: %s\n' "$*" >&2
  exit 1
}

# The release tag that /releases/latest redirects to. No API call, so no rate limit.
latest_tag() {
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") ||
    die "could not reach GitHub to find the latest release"
  tag=${url##*/tag/}
  if [ "$tag" = "$url" ] || [ -z "$tag" ]; then
    die "no release found at https://github.com/$repo/releases"
  fi
  printf '%s\n' "$tag"
}

# The tag of the newest nightly build, such as 0.9.1-nightly.202611201136. The
# nightlies are pre-releases, which /releases/latest skips, so this asks the
# API (60 calls an hour from one address). The number after `nightly.` is the
# time it was built.
latest_nightly() {
  json=$(curl -fsSL -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/$repo/releases?per_page=30") ||
    die "could not list the releases on GitHub"
  # One field per line, whether or not the JSON is pretty-printed.
  tag=$(printf '%s\n' "$json" | tr ',' '\n' |
    sed -n 's/^ *"tag_name": *"\([0-9]*\.[0-9]*\.[0-9]*-nightly\.[0-9]*\)" *$/\1/p' |
    sort -t. -k4,4n | tail -n 1)
  [ -n "$tag" ] || die "no nightly build found at https://github.com/$repo/releases"
  printf '%s\n' "$tag"
}

# The checksum SHA256SUMS ($2) lists for the file $1, or nothing.
listed() {
  awk -v f="$1" '$2 == f || $2 == "*" f { print $1 }' "$2"
}

# Whether release $1 has a signed SHA256SUMS: every release after v0.3.0 does.
signed_release() {
  printf '%s\n' "$1" | awk -F. '{
    sub(/^v/, ""); sub(/-.*/, "")
    exit !($1 > 0 || $2 > 3 || ($2 == 3 && $3 > 0))
  }'
}

# Checks SHA256SUMS ($1) against its signature ($2) by one of $keys, with minisign, or with OpenSSL 3 where minisign is missing, using the
# directory $3 for its files. Returns 2 when there is neither, and 1 for any
# other failure. Never pass a tool's own status through: minisign exits 2 on an
# empty or broken signature, which would read as "no verifier" and install.
verify_sums() {
  if command -v minisign >/dev/null; then
    tool=minisign
  else
    case $(openssl version 2>/dev/null) in
      "OpenSSL "[3-9]*) tool=openssl ;;
      *) return 2 ;;
    esac
  fi
  for key in $keys; do
    "verify_with_$tool" "$key" "$@" && return 0
  done
  return 1
}

# verify_with_minisign KEY SUMS SIG
verify_with_minisign() {
  minisign -V -q -P "$1" -m "$2" -x "$3" >/dev/null 2>&1
}

# verify_with_openssl KEY SUMS SIG DIR. It checks the signature of the file,
# not the one of the trusted comment, which nothing here reads.
verify_with_openssl() {
  # The key is "Ed", an 8-byte key id and the 32-byte Ed25519 key; the
  # signature is "ED", the key id and a 64-byte signature of the file's
  # BLAKE2b-512. OpenSSL reads the key as DER: a fixed 12-byte prefix, then it.
  printf '%s\n' "$1" | openssl base64 -d -A >"$4/key.bin" &&
    sed -n 2p "$3" | openssl base64 -d -A >"$4/sig.bin" &&
    { printf '\060\052\060\005\006\003\053\145\160\003\041\000' && tail -c 32 "$4/key.bin"; } >"$4/key.der" &&
    tail -c 64 "$4/sig.bin" >"$4/ed25519.sig" &&
    openssl dgst -blake2b512 -binary "$2" >"$4/blake2b" &&
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$4/key.der" -rawin \
      -in "$4/blake2b" -sigfile "$4/ed25519.sig" >/dev/null 2>&1
}

sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# The release binary is linked against glibc 2.35 (see .github/workflows/release.yml).
check_glibc() {
  v=$(getconf GNU_LIBC_VERSION 2>/dev/null | cut -d' ' -f2) || v=
  if [ -z "$v" ]; then
    say "warning: could not find glibc; the release build needs glibc 2.35 or newer"
    return
  fi
  major=${v%%.*} minor=${v#*.}
  minor=${minor%%.*}
  if [ "$major" -lt 2 ] || { [ "$major" = 2 ] && [ "$minor" -lt 35 ]; }; then
    die "this system has glibc $v; the release build needs 2.35 or newer (build from source instead)"
  fi
}

# The Node.js major the Claude Code adapter needs at least (the "engines" of
# @agentclientprotocol/claude-agent-acp, CLAUDE_ACP_PACKAGE in core/src/agent/launch.rs).
node_major=22

# The major version of the node on PATH, or nothing.
node_version() {
  v=$(node --version 2>/dev/null) || return 0
  v=${v#v}
  printf '%s\n' "${v%%.*}"
}

# Runs a command as root: directly when this is root, else with sudo when it
# needs no password. Returns 1 without running it otherwise.
as_root() {
  if [ "$(id -u)" = 0 ]; then
    "$@"
  elif command -v sudo >/dev/null && sudo -n true 2>/dev/null; then
    sudo "$@"
  else
    return 1
  fi
}

# The command that installs the system packages $@, or nothing on a system
# this script does not know.
pkg_install_cmd() {
  if command -v apt-get >/dev/null; then
    echo "apt-get install -y $*"
  elif command -v dnf >/dev/null; then
    echo "dnf install -y $*"
  elif command -v zypper >/dev/null; then
    echo "zypper --non-interactive install $*"
  elif command -v pacman >/dev/null; then
    echo "pacman -S --noconfirm --needed $*"
  elif command -v apk >/dev/null; then
    echo "apk add $*"
  fi
}

# Installs the system packages $@. Says what to run instead when it cannot.
pkg_install() {
  cmd=$(pkg_install_cmd "$@")
  if [ -z "$cmd" ]; then
    say "warning: install $* with your package manager; arakun-server needs it"
    return 0
  fi
  say "installing $*"
  if command -v apt-get >/dev/null; then
    # A fresh VPS often has no package lists yet.
    as_root apt-get update -qq >/dev/null 2>&1 || true
  fi
  # shellcheck disable=SC2086 # $cmd is a command line on purpose
  if ! as_root env DEBIAN_FRONTEND=noninteractive $cmd >/dev/null 2>&1; then
    say "warning: could not install $*; run this as root, then start the server:"
    printf '\n  %s\n\n' "$cmd"
  fi
}

# Installs Node.js $node_major from nodejs.org into the data folder and links
# node, npm and npx into $bindir. For the user only, so it needs no root, and
# distributions often ship an older Node than the adapter needs.
install_node() {
  case $platform in
    linux-x86_64) node_arch=linux-x64 ;;
    linux-arm64) node_arch=linux-arm64 ;;
  esac
  dist=https://nodejs.org/dist/latest-v$node_major.x
  curl -fsSL -o "$tmp/node-sums" "$dist/SHASUMS256.txt" ||
    die "could not download $dist/SHASUMS256.txt (use --no-tools to skip Node.js)"
  node_file=$(awk -v a="$node_arch" '$2 ~ "^node-v[0-9.]+-" a "\\.tar\\.gz$" { print $2; exit }' "$tmp/node-sums")
  [ -n "$node_file" ] || die "nodejs.org lists no Node.js $node_major for $node_arch"
  say "downloading ${node_file%.tar.gz}"
  curl -fSL --progress-bar -o "$tmp/$node_file" "$dist/$node_file" ||
    die "could not download $dist/$node_file"
  want=$(listed "$node_file" "$tmp/node-sums")
  have=$(sha256 "$tmp/$node_file")
  [ "$have" = "$want" ] || die "checksum mismatch for $node_file (expected $want, got $have)"

  mkdir -p "${node_dir%/*}" "$tmp/node"
  tar -C "$tmp/node" -xzf "$tmp/$node_file"
  rm -rf "$node_dir.new" "$node_dir.old"
  mv "$tmp/node/${node_file%.tar.gz}" "$node_dir.new"
  if [ -e "$node_dir" ]; then mv "$node_dir" "$node_dir.old"; fi
  mv "$node_dir.new" "$node_dir"
  rm -rf "$node_dir.old"
  for b in node npm npx; do
    # A node of the user's own there stays; it would come first on PATH.
    if [ -e "$bindir/$b" ] && [ ! -L "$bindir/$b" ]; then
      say "warning: $bindir/$b is not Arakun's; left as it is"
      continue
    fi
    ln -sfn "$node_dir/bin/$b" "$bindir/$b"
  done
  say "installed Node.js $(PATH="$bindir:$PATH" node_version) in $node_dir, linked in $bindir"
}

# What the server runs agents with: git for their worktrees, Node.js and npm
# to install the Claude Code and Codex adapters, and gh for pull requests.
# Without Node, a new session fails as it starts.
server_tools() {
  node_dir=${XDG_DATA_HOME:-$HOME/.local/share}/arakun/node
  command -v git >/dev/null || pkg_install git
  if ! command -v gh >/dev/null; then
    # Not every distribution packages gh; the server works without it.
    if [ -n "$(pkg_install_cmd gh)" ] && as_root true 2>/dev/null; then
      pkg_install gh
    else
      say "gh is not installed; the server shows no pull requests without it (https://cli.github.com)"
    fi
  fi
  # The one $bindir gives, else the one PATH gives: a new one goes in $bindir.
  have_node=$(PATH="$bindir:$PATH" node_version)
  if [ -z "$have_node" ] || [ "$have_node" -lt "$node_major" ] ||
    ! PATH="$bindir:$PATH" command -v npm >/dev/null; then
    if [ -n "$have_node" ]; then
      say "Node.js $have_node is too old; the Claude Code adapter needs $node_major or newer"
    fi
    install_node
  fi
  # The service keeps the PATH it is installed with, and the server looks in
  # $bindir only after it: an older node ahead of $bindir is the one it runs.
  path_node=$(node_version)
  if [ -n "$path_node" ] && [ "$path_node" -lt "$node_major" ]; then
    say "warning: 'node' on your PATH is $(command -v node), Node.js $path_node; the server needs $node_major or newer."
    say "put $bindir first on your PATH, then run 'arakun-server service install' again if it runs as a service:"
    # shellcheck disable=SC2016 # printed for the user to paste, not expanded here
    printf '\n  export PATH="$HOME/.local/bin:$PATH"\n\n'
  fi
  # Only the links that are Arakun's, from this run or an earlier one: a
  # node of the user's own was left in place and must stay.
  for b in node npm npx; do
    if [ -L "$bindir/$b" ] && [ "$(readlink "$bindir/$b")" = "$node_dir/bin/$b" ]; then
      uninstall="$uninstall ~/.local/bin/$b"
    fi
  done
  if [ -d "$node_dir" ]; then uninstall="$uninstall \"$node_dir\""; fi
  install_claude
}

# Installs Claude Code with Anthropic's native installer, which puts `claude` in
# $bindir. The server needs it signed in before a session can start, and the
# copy the adapter bundles is not on PATH. `npm install -g` would put it in
# $node_dir/bin, which is not on PATH either.
install_claude() {
  if PATH="$bindir:$PATH" command -v claude >/dev/null; then return 0; fi
  claude_cmd='curl -fsSL https://claude.ai/install.sh | bash'
  if ! command -v bash >/dev/null; then
    say "warning: Claude Code's installer needs bash; install bash, then run: $claude_cmd"
    return 0
  fi
  # A data folder left by an earlier Claude install is the user's; the
  # uninstall line must not delete it.
  claude_data="${XDG_DATA_HOME:-$HOME/.local/share}/claude"
  claude_data_new=1
  if [ -e "$claude_data" ]; then claude_data_new=; fi
  say "installing Claude Code"
  curl -fsSL -o "$tmp/claude-install.sh" https://claude.ai/install.sh &&
    bash "$tmp/claude-install.sh" >/dev/null || true
  if [ -x "$bindir/claude" ]; then
    say "installed Claude Code in $bindir; sign in with 'claude' before the first session"
    uninstall="$uninstall ~/.local/bin/claude"
    if [ -n "$claude_data_new" ]; then uninstall="$uninstall \"$claude_data\""; fi
  else
    say "warning: could not install Claude Code; run this, then sign in with 'claude':"
    printf '\n  %s\n\n' "$claude_cmd"
  fi
}

# Linux: the app launcher's entry for $bindir/arakun in $data/applications, and the
# icons of the unpacked release $1 in $data/icons/hicolor, as tools/install.sh puts
# them. Releases from before the icons were packed ship none; their entry gets the
# desktop's default one. Sets $launcher to the files it wrote, quoted for the
# uninstall line. The caller runs this with errexit off, so every step returns
# on failure.
launcher_entry() {
  launcher="\"$data/applications/arakun.desktop\""
  for icon in scalable/apps/arakun.svg 256x256/apps/arakun.png; do
    if [ -f "$1/icons/hicolor/$icon" ]; then
      mkdir -p "$data/icons/hicolor/${icon%/*}" &&
        cp "$1/icons/hicolor/$icon" "$data/icons/hicolor/$icon" || return 1
      launcher="$launcher \"$data/icons/hicolor/$icon\""
    fi
  done
  mkdir -p "$data/applications" || return 1
  # Exec is quoted, so a home folder with a space in it still starts.
  cat >"$data/applications/.arakun.desktop.new" <<EOF || return 1
[Desktop Entry]
Type=Application
Name=Arakun
Comment=Coding agents, terminals and a browser in one window
Exec="$bindir/arakun"
Icon=arakun
Terminal=false
Categories=Development;IDE;
StartupWMClass=arakun
EOF
  mv -f "$data/applications/.arakun.desktop.new" "$data/applications/arakun.desktop" || return 1
  # Best effort: a desktop finds them without these caches too. Each is refreshed
  # only where one exists already, since a new one would go stale and hide what
  # other apps add later without refreshing it.
  if command -v gtk-update-icon-cache >/dev/null && [ -f "$data/icons/hicolor/icon-theme.cache" ]; then
    gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
  fi
  if command -v update-desktop-database >/dev/null && [ -f "$data/applications/mimeinfo.cache" ]; then
    update-desktop-database -q "$data/applications" 2>/dev/null || true
  fi
  return 0
}

main() {
  version=${ARAKUN_VERSION:-}
  server=
  tools=1
  while [ $# -gt 0 ]; do
    case $1 in
      --version)
        [ $# -ge 2 ] || die "--version needs a value, such as v0.1.0"
        version=$2
        shift
        ;;
      --version=*) version=${1#--version=} ;;
      --server) server=1 ;;
      --nightly) version=nightly ;;
      --no-tools) tools= ;;
      -h | --help)
        # Not read from $0: under `curl | sh` there is no file.
        echo "usage: install-release.sh [--server [--no-tools]] [--nightly | --version vX.Y.Z]   (or ARAKUN_VERSION=vX.Y.Z)"
        echo "  --server    install arakun-server (Linux), the server without the window,"
        echo "              and what it runs agents with: git, gh, Node.js $node_major and Claude Code"
        echo "  --no-tools  with --server: leave git, gh, Node.js and Claude Code to you"
        echo "  --nightly   install the newest nightly build of main (it can break)"
        return 0
        ;;
      *) die "unknown option $1" ;;
    esac
    shift
  done

  command -v curl >/dev/null || die "curl is needed"

  os=$(uname -s) arch=$(uname -m)
  case $os in
    Darwin)
      # A shell under Rosetta says x86_64 on Apple silicon.
      if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then
        arch=arm64
      fi
      # The app carries the server: `arakun server`.
      [ -z "$server" ] || die "arakun-server is built for Linux only; on a Mac, install the app and run 'arakun server'"
      [ "$arch" = arm64 ] || die "Intel Macs have no release build; build from source (see README.md)"
      # The disk image, or the archive for a release from before there was one.
      command -v tar >/dev/null || die "tar is needed"
      platform=macos-arm64 ext=dmg
      ;;
    Linux)
      case $arch in
        x86_64 | amd64) platform=linux-x86_64 ;;
        aarch64 | arm64) platform=linux-arm64 ;;
        *) die "Linux on $arch has no release build; build from source (see README.md)" ;;
      esac
      check_glibc
      command -v tar >/dev/null || die "tar is needed"
      ext=tar.gz
      ;;
    MINGW* | MSYS* | CYGWIN*)
      die "on Windows, run in PowerShell: irm https://arakun.dev/install.ps1 | iex"
      ;;
    *) die "$os has no release build" ;;
  esac

  # A version the user asked for may be one from before signing; the latest
  # must be signed, or whoever can edit releases could mark an unsigned one
  # as latest.
  pinned=$version
  case $version in
    "") version=$(latest_tag) ;;
    nightly) version=$(latest_nightly) ;;
    # Nightly tags have no v: 0.9.1-nightly.202611201136.
    *-nightly.*) version=${version#v} ;;
    v*) ;;
    *) version=v$version ;;
  esac
  keys=$release_keys key_name="release key"
  case $version in
    *-nightly.*)
      keys="$keys $nightly_key" key_name="release or nightly key"
      say "$version is a nightly build: it can break, crash or lose a session; see https://github.com/$repo#nightly-builds"
      ;;
  esac

  name=arakun-$version-$platform
  if [ -n "$server" ]; then name=arakun-server-$version-$platform; fi
  file=$name.$ext
  base=https://github.com/$repo/releases/download/$version
  tmp=$(mktemp -d)
  mnt=
  # Detaches the disk image too, if the script stops while it is attached.
  trap 'if [ -n "$mnt" ]; then hdiutil detach -quiet -force "$mnt" || true; fi; rm -rf "$tmp"' EXIT

  # First, since it says which files the release has.
  curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" ||
    die "could not download $base/SHA256SUMS (is there a release $version?)"
  # Before any checksum in it is trusted.
  if [ -z "$pinned" ] && ! signed_release "$version"; then
    die "the latest release, $version, is not signed; to install it anyway, run this again with --version $version"
  fi
  if signed_release "$version"; then
    curl -fsSL -o "$tmp/SHA256SUMS.minisig" "$base/SHA256SUMS.minisig" ||
      die "could not download $base/SHA256SUMS.minisig, which every release after v0.3.0 has"
    mkdir "$tmp/sig"
    verify_sums "$tmp/SHA256SUMS" "$tmp/SHA256SUMS.minisig" "$tmp/sig" && signature=0 || signature=$?
    case $signature in
      0) say "SHA256SUMS is signed with Arakun's $key_name" ;;
      2) say "warning: the signature of SHA256SUMS was not checked; install minisign or OpenSSL 3 to check it" ;;
      *) die "SHA256SUMS is not signed with Arakun's $key_name; nothing was installed" ;;
    esac
  else
    say "$version is from before releases were signed; only its checksums are checked"
  fi
  want=$(listed "$file" "$tmp/SHA256SUMS")
  # v0.2.1 has the macOS app in a tar.gz and no disk image.
  if [ -z "$want" ] && [ "$ext" = dmg ]; then
    ext=tar.gz file=$name.tar.gz
    want=$(listed "$file" "$tmp/SHA256SUMS")
  fi
  if [ -z "$want" ] && [ "$platform" = linux-arm64 ]; then
    die "$version has no Linux arm64 build; the first release with one is the one after v0.4.1"
  fi
  if [ -z "$want" ] && [ -n "$server" ]; then
    die "$version has no arakun-server build; the first release with one is the one after v0.4.0"
  fi
  [ -n "$want" ] || die "$version has no $platform build (it is not listed in SHA256SUMS)"

  if [ -n "$server" ]; then what=arakun-server; else what=Arakun; fi
  say "downloading $what $version for $platform"
  curl -fSL --progress-bar -o "$tmp/$file" "$base/$file" ||
    die "could not download $base/$file"
  have=$(sha256 "$tmp/$file")
  [ "$have" = "$want" ] || die "checksum mismatch for $file (expected $want, got $have)"

  bindir=$HOME/.local/bin
  mkdir -p "$bindir"

  if [ -n "$server" ]; then
    mkdir "$tmp/x"
    tar -C "$tmp/x" -xzf "$tmp/$file"
    [ -f "$tmp/x/$name/arakun-server" ] || die "the archive has no arakun-server binary"
    # A rename, so a server that is running keeps its old binary until it restarts.
    cp "$tmp/x/$name/arakun-server" "$bindir/.arakun-server.new"
    chmod 755 "$bindir/.arakun-server.new"
    mv -f "$bindir/.arakun-server.new" "$bindir/arakun-server"
    say "installed arakun-server in $bindir"
    uninstall="rm -rf ~/.local/bin/arakun-server"
    if [ -n "$tools" ]; then server_tools; fi
  elif [ "$platform" = macos-arm64 ]; then
    apps=$HOME/Applications
    app=$apps/Arakun.app
    mkdir -p "$apps"
    # Put next to the old app first, so the swap is two renames on one disk.
    rm -rf "$apps/.Arakun.app.new"
    if [ "$ext" = dmg ]; then
      hdiutil attach -quiet -readonly -nobrowse -noautoopen -mountpoint "$tmp/mnt" "$tmp/$file" ||
        die "could not open $file"
      mnt=$tmp/mnt
      [ -d "$mnt/Arakun.app" ] || die "the disk image has no Arakun.app"
      ditto "$mnt/Arakun.app" "$apps/.Arakun.app.new"
      # Before the old app goes: a failure here leaves it in place. The image is
      # read-only, so forcing it loses nothing. $mnt stays set on failure, and the
      # EXIT trap tries once more.
      hdiutil detach -quiet "$mnt" || hdiutil detach -quiet -force "$mnt" || {
        rm -rf "$apps/.Arakun.app.new"
        die "could not detach the disk image at $mnt; nothing was installed"
      }
      mnt=
    else
      mkdir "$tmp/x"
      tar -C "$tmp/x" -xzf "$tmp/$file"
      [ -d "$tmp/x/$name/Arakun.app" ] || die "the archive has no Arakun.app"
      mv "$tmp/x/$name/Arakun.app" "$apps/.Arakun.app.new"
    fi
    rm -rf "$app"
    mv "$apps/.Arakun.app.new" "$app"
    ln -sfn "$app/Contents/MacOS/arakun" "$bindir/arakun"
    say "installed Arakun.app in $apps, and arakun in $bindir"
    uninstall="rm -rf ~/Applications/Arakun.app ~/.local/bin/arakun"
  else
    mkdir "$tmp/x"
    tar -C "$tmp/x" -xzf "$tmp/$file"
    [ -f "$tmp/x/$name/arakun" ] || die "the archive has no arakun binary"
    # The browser's runtime goes where core/src/browser/loader.rs looks first: a cef folder
    # next to the binary, from a manual install, else the data folder (where
    # tools/install.sh keeps it too). Releases from before the browser have none.
    cef=${XDG_DATA_HOME:-$HOME/.local/share}/arakun/cef
    if [ -f "$bindir/cef/libcef.so" ]; then cef=$bindir/cef; fi
    swap_cef=
    if [ -d "$tmp/x/$name/cef" ] && ! cmp -s "$tmp/x/$name/cef/archive.json" "$cef/archive.json"; then
      swap_cef=1
      # Chromium's child processes load the runtime again, so under a running Arakun
      # they would get another CEF build than the window. The same build is left as
      # is, and with none installed yet a running Arakun has none loaded.
      if [ -f "$cef/libcef.so" ]; then
        # pgrep exits 1 when nothing matches; anything else means it could not tell.
        command -v pgrep >/dev/null ||
          die "pgrep is needed to check that Arakun is not running (it comes with procps)"
        running=0
        pgrep -x -u "$(id -u)" arakun >/dev/null 2>&1 || running=$?
        case $running in
          0) die "this release has a new browser runtime; quit Arakun (close this terminal too if it is Arakun's) and run the installer again" ;;
          1) ;;
          *) die "could not check whether Arakun is running (pgrep exited $running)" ;;
        esac
      fi
    fi
    # A rename, so an Arakun that is running keeps its old binary.
    cp "$tmp/x/$name/arakun" "$bindir/.arakun.new"
    chmod 755 "$bindir/.arakun.new"
    if [ -n "$swap_cef" ]; then
      mkdir -p "${cef%/*}"
      rm -rf "$cef.new" "$cef.old"
      mv "$tmp/x/$name/cef" "$cef.new"
      if [ -e "$cef" ]; then mv "$cef" "$cef.old"; fi
      mv "$cef.new" "$cef"
      rm -rf "$cef.old"
    fi
    mv -f "$bindir/.arakun.new" "$bindir/arakun"
    if [ -d "$cef" ]; then
      say "installed arakun in $bindir, and its browser in $cef"
      uninstall="rm -rf ~/.local/bin/arakun \"$cef\""
    else
      say "installed arakun in $bindir"
      uninstall="rm -f ~/.local/bin/arakun"
    fi
    data=${XDG_DATA_HOME:-$HOME/.local/share}
    if launcher_entry "$tmp/x/$name"; then
      say "added Arakun to the app launcher"
      uninstall="$uninstall $launcher"
    else
      say "warning: could not add Arakun to the app launcher in $data"
    fi
  fi

  if [ -n "$server" ]; then bin=arakun-server; else bin=arakun; fi
  case :$PATH: in
    *:"$bindir":*)
      found=$(command -v "$bin" || true)
      if [ -n "$found" ] && [ "$found" != "$bindir/$bin" ]; then
        say "warning: '$bin' runs $found, which comes before $bindir on your PATH"
      fi
      ;;
    *)
      case ${SHELL:-} in
        */zsh) rc=.zshrc ;;
        */bash) if [ "$os" = Darwin ]; then rc=.bash_profile; else rc=.bashrc; fi ;;
        */fish) rc= ;;
        *) rc=.profile ;;
      esac
      say "$bindir is not on your PATH."
      if [ -n "$rc" ]; then
        say "add this line to ~/$rc, then open a new terminal:"
        # shellcheck disable=SC2016 # printed for the user to paste, not expanded here
        printf '\n  export PATH="$HOME/.local/bin:$PATH"\n\n'
      else
        say "run this once, then open a new terminal:"
        printf '\n  fish_add_path ~/.local/bin\n\n'
      fi
      ;;
  esac

  if [ -n "$server" ]; then
    say "done. Run 'arakun-server serve' to start it, or 'arakun-server service install' to keep it running."
    # Root's `service install` writes a system unit; anyone else's, a user unit.
    if [ "$(id -u)" = 0 ]; then systemctl=systemctl; else systemctl="systemctl --user"; fi
    say "if it already runs as a service, restart it on the new binary: $systemctl restart arakun-server"
    say "(a server in another folder, ARAKUN_SERVER_DIR, is the unit arakun-server-<folder>)"
    say "to uninstall: $uninstall"
    return 0
  fi
  # Ubuntu 23.10 and later: Chromium's sandbox needs an AppArmor profile (README.md).
  if [ "$os" = Linux ] && [ "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1 ]; then
    say "browser tabs need an AppArmor profile on this system; see"
    say "https://github.com/$repo#browser-tabs-on-ubuntu"
  fi
  say "done. Run 'arakun' to start it."
  say "to uninstall: $uninstall"
}

main "$@"
