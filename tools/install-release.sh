#!/bin/sh
# Installs the latest Arakun release for the current user.
#
#   curl -fsSL https://arakun.dev/install | sh
#   curl -fsSL https://arakun.dev/install | sh -s -- --version v0.1.0
#
# macOS (Apple silicon): ~/Applications/Arakun.app, and ~/.local/bin/arakun linking into it.
# Linux (x86_64):        ~/.local/bin/arakun.
# ARAKUN_VERSION pins a release, like --version. Release builds have no browser tabs; for
# those, build from source (see README.md). Windows: tools/install-release.ps1.
#
# Everything runs from main at the end, so a download cut short runs nothing.
set -eu

repo=arakunhq/arakun

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

main() {
  version=${ARAKUN_VERSION:-}
  while [ $# -gt 0 ]; do
    case $1 in
      --version)
        [ $# -ge 2 ] || die "--version needs a value, such as v0.1.0"
        version=$2
        shift
        ;;
      --version=*) version=${1#--version=} ;;
      -h | --help)
        # Not read from $0: under `curl | sh` there is no file.
        echo "usage: install-release.sh [--version vX.Y.Z]   (or ARAKUN_VERSION=vX.Y.Z)"
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
      [ "$arch" = arm64 ] || die "Intel Macs have no release build; build from source (see README.md)"
      platform=macos-arm64 ext=dmg
      ;;
    Linux)
      case $arch in
        x86_64 | amd64) ;;
        *) die "Linux on $arch has no release build; build from source (see README.md)" ;;
      esac
      check_glibc
      command -v tar >/dev/null || die "tar is needed"
      platform=linux-x86_64 ext=tar.gz
      ;;
    MINGW* | MSYS* | CYGWIN*)
      die "on Windows, run in PowerShell: irm https://arakun.dev/install.ps1 | iex"
      ;;
    *) die "$os has no release build" ;;
  esac

  if [ -z "$version" ]; then
    version=$(latest_tag)
  else
    case $version in v*) ;; *) version=v$version ;; esac
  fi

  name=arakun-$version-$platform
  file=$name.$ext
  base=https://github.com/$repo/releases/download/$version
  tmp=$(mktemp -d)
  mnt=
  # Detaches the disk image too, if the script stops while it is attached.
  trap 'if [ -n "$mnt" ]; then hdiutil detach -quiet "$mnt" || true; fi; rm -rf "$tmp"' EXIT

  say "downloading Arakun $version for $platform"
  curl -fSL --progress-bar -o "$tmp/$file" "$base/$file" ||
    die "could not download $base/$file (does $version have a $platform build?)"
  curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" ||
    die "could not download $base/SHA256SUMS"

  want=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
  [ -n "$want" ] || die "$file is not listed in SHA256SUMS"
  have=$(sha256 "$tmp/$file")
  [ "$have" = "$want" ] || die "checksum mismatch for $file (expected $want, got $have)"

  bindir=$HOME/.local/bin
  mkdir -p "$bindir"

  if [ "$platform" = macos-arm64 ]; then
    mnt=$tmp/mnt
    hdiutil attach -quiet -readonly -nobrowse -noautoopen -mountpoint "$mnt" "$tmp/$file" ||
      die "could not open $file"
    [ -d "$mnt/Arakun.app" ] || die "the disk image has no Arakun.app"
    apps=$HOME/Applications
    app=$apps/Arakun.app
    mkdir -p "$apps"
    # Copied next to the old app first, so the swap is two renames on one disk.
    rm -rf "$apps/.Arakun.app.new"
    ditto "$mnt/Arakun.app" "$apps/.Arakun.app.new"
    hdiutil detach -quiet "$mnt" || true
    mnt=
    rm -rf "$app"
    mv "$apps/.Arakun.app.new" "$app"
    ln -sfn "$app/Contents/MacOS/arakun" "$bindir/arakun"
    say "installed Arakun.app in $apps, and arakun in $bindir"
    uninstall="rm -rf ~/Applications/Arakun.app ~/.local/bin/arakun"
  else
    mkdir "$tmp/x"
    tar -C "$tmp/x" -xzf "$tmp/$file"
    [ -f "$tmp/x/$name/arakun" ] || die "the archive has no arakun binary"
    # A rename, so an Arakun that is running keeps its old binary.
    cp "$tmp/x/$name/arakun" "$bindir/.arakun.new"
    chmod 755 "$bindir/.arakun.new"
    mv -f "$bindir/.arakun.new" "$bindir/arakun"
    say "installed arakun in $bindir"
    uninstall="rm ~/.local/bin/arakun"
  fi

  case :$PATH: in
    *:"$bindir":*)
      found=$(command -v arakun || true)
      if [ -n "$found" ] && [ "$found" != "$bindir/arakun" ]; then
        say "warning: 'arakun' runs $found, which comes before $bindir on your PATH"
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

  say "done. Run 'arakun' to start it."
  say "to uninstall: $uninstall"
}

main "$@"
