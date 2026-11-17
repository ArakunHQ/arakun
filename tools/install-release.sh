#!/bin/sh
# Installs the latest Arakun release for the current user.
#
#   curl -fsSL https://arakun.dev/install | sh
#   curl -fsSL https://arakun.dev/install | sh -s -- --version v0.1.0
#
# macOS (Apple silicon): ~/Applications/Arakun.app, and ~/.local/bin/arakun linking into it.
# Linux (x86_64):        ~/.local/bin/arakun, and the browser's CEF runtime in
#                        ${XDG_DATA_HOME:-~/.local/share}/arakun/cef.
# ARAKUN_VERSION pins a release, like --version. Windows: tools/install-release.ps1.
#
# Everything runs from main at the end, so a download cut short runs nothing.
set -eu

repo=arakunhq/arakun
# The minisign keys SHA256SUMS may be signed with: the one release.yml uses
# (assets/release-key.pub), and an offline backup (assets/release-key-backup.pub).
release_keys="RWRHh4aSkL+9oL41eNxaL791lQYzu/un+QvVqfckIj7839aB8lQBoxyn RWSxlRjKJX7FQmC0+on+hpDPmZ/ShKCW4KzyJhbnm0bAc6JGTI7KV+xg"

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

# Checks SHA256SUMS ($1) against its signature ($2) by one of the release
# keys, with minisign, or with OpenSSL 3 where minisign is missing, using the
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
  for key in $release_keys; do
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
      # The disk image, or the archive for a release from before there was one.
      command -v tar >/dev/null || die "tar is needed"
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

  # A version the user asked for may be one from before signing; the latest
  # must be signed, or whoever can edit releases could mark an unsigned one
  # as latest.
  pinned=$version
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
      0) say "SHA256SUMS is signed with Arakun's release key" ;;
      2) say "warning: the signature of SHA256SUMS was not checked; install minisign or OpenSSL 3 to check it" ;;
      *) die "SHA256SUMS is not signed with Arakun's release key; nothing was installed" ;;
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
  [ -n "$want" ] || die "$version has no $platform build (it is not listed in SHA256SUMS)"

  say "downloading Arakun $version for $platform"
  curl -fSL --progress-bar -o "$tmp/$file" "$base/$file" ||
    die "could not download $base/$file"
  have=$(sha256 "$tmp/$file")
  [ "$have" = "$want" ] || die "checksum mismatch for $file (expected $want, got $have)"

  bindir=$HOME/.local/bin
  mkdir -p "$bindir"

  if [ "$platform" = macos-arm64 ]; then
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
    # A rename, so an Arakun that is running keeps its old binary.
    cp "$tmp/x/$name/arakun" "$bindir/.arakun.new"
    chmod 755 "$bindir/.arakun.new"
    # Where src/browser/loader.rs and tools/install.sh keep it. Releases from before
    # the browser have none. Swapped with renames, so a running Arakun keeps the
    # libcef.so it loaded.
    cef=${XDG_DATA_HOME:-$HOME/.local/share}/arakun/cef
    if [ -d "$tmp/x/$name/cef" ]; then
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
      uninstall="rm -rf ~/.local/bin/arakun $cef"
    else
      say "installed arakun in $bindir"
      uninstall="rm ~/.local/bin/arakun"
    fi
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

  # Ubuntu 23.10 and later: Chromium's sandbox needs an AppArmor profile (README.md).
  if [ "$os" = Linux ] && [ "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1 ]; then
    say "browser tabs need an AppArmor profile on this system; see"
    say "https://github.com/$repo#browser-tabs-on-ubuntu"
  fi
  say "done. Run 'arakun' to start it."
  say "to uninstall: $uninstall"
}

main "$@"
