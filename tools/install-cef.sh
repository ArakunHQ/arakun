#!/bin/sh
# Prepares the CEF runtime arakun's browser tabs load at run time, plus the tiny "SDK" the
# `cef` crate needs at build time, from an unpacked CEF binary distribution.
#
#   tools/install-cef.sh CEF_DIST [DEST]
#
# CEF_DIST is an unpacked CEF 156.0.15 linux64 or linuxarm64 distribution (the version cef 156.3 is built
# for; https://cef-builds.spotifycdn.com/index.html). DEST defaults to
# ${XDG_DATA_HOME:-~/.local/share}/arakun/cef. Re-running replaces DEST. Either layout works:
# the archive as unpacked (Release/, Resources/, include/), or the flat directory with an
# archive.json that the `cef` crate's own download leaves.
#
# DEST/            what the app dlopens on the first browser tab (~300 MB)
#   libcef.so      stripped
#   *.pak, icudtl.dat, v8_context_snapshot.bin, swiftshader
#   locales/       en-US and es only
#
# macOS: the build downloads CEF into .cef-sdk by itself, so with no CEF_DIST this copies
# the framework from there; DEST then only holds Chromium Embedded Framework.framework.
#
# DEST/sdk/        what CEF_PATH points at while building (through the .cef-sdk link that
#                  .cargo/config.toml names)
#   archive.json, include/, libcef.so (an empty stub: the app never links the real one,
#   so a cef symbol missing from src/browser/loader.rs fails the link instead of
#   silently adding libcef.so to the binary's startup dependencies)
set -eu

if [ "$(uname)" = Darwin ]; then
  framework="Chromium Embedded Framework.framework"
  # Whatever the build downloaded: `uname -m` says x86_64 in a shell under Rosetta even
  # when cargo builds for arm64.
  src=${1:-}
  if [ -z "$src" ]; then
    for d in "$(dirname "$0")"/../.cef-sdk/*/cef_macos_*; do src=$d; done
  fi
  dest=${2:-${XDG_DATA_HOME:-$HOME/.local/share}/arakun/cef}
  [ -d "$src/$framework" ] || {
    echo "install-cef: no $framework in $src (run cargo build first, or pass CEF_DIST)" >&2
    exit 1
  }
  tmp="$dest.tmp.$$"
  rm -rf "$tmp"
  mkdir -p "$tmp"
  trap 'rm -rf "$tmp"' EXIT
  ditto "$src/$framework" "$tmp/$framework"
  cp "$src/archive.json" "$tmp/archive.json"
  rm -rf "$dest"
  mv "$tmp" "$dest"
  trap - EXIT
  echo "install-cef: $(du -sh "$dest" | cut -f1) in $dest"
  exit 0
fi

src=${1:?usage: tools/install-cef.sh CEF_DIST [DEST]}
dest=${2:-${XDG_DATA_HOME:-$HOME/.local/share}/arakun/cef}

if [ -f "$src/Release/libcef.so" ] && [ -d "$src/Resources" ]; then
  bin=$src/Release res=$src/Resources
  # The `cef` crate takes the version from the name in archive.json, and the folder may
  # have been renamed, so the name comes from the headers.
  version=$(sed -n 's/^#define CEF_VERSION "\(.*\)"$/\1/p' "$src/include/cef_version.h")
  [ -n "$version" ] || {
    echo "install-cef: no CEF_VERSION in $src/include/cef_version.h" >&2
    exit 1
  }
elif [ -f "$src/libcef.so" ] && [ -f "$src/archive.json" ]; then
  bin=$src res=$src version=
else
  echo "install-cef: $src is not an unpacked CEF distribution" >&2
  exit 1
fi

tmp="$dest.tmp.$$"
rm -rf "$tmp"
mkdir -p "$tmp/locales" "$tmp/sdk/locales"
trap 'rm -rf "$tmp"' EXIT

echo "install-cef: stripping libcef.so"
strip --strip-unneeded -o "$tmp/libcef.so" "$bin/libcef.so"
for f in v8_context_snapshot.bin libvk_swiftshader.so libvulkan.so.1 vk_swiftshader_icd.json; do
  cp "$bin/$f" "$tmp/$f"
done
for f in icudtl.dat resources.pak chrome_100_percent.pak chrome_200_percent.pak; do
  cp "$res/$f" "$tmp/$f"
done
strip --strip-unneeded "$tmp/libvk_swiftshader.so" "$tmp/libvulkan.so.1"
for l in en-US es es-419; do
  cp "$res/locales/$l.pak" "$tmp/locales/"
done
if [ -n "$version" ]; then
  # The fields the `cef` crate reads. The archive's checksum is not known here.
  case $(uname -m) in
    aarch64 | arm64) platform=linuxarm64 ;;
    *) platform=linux64 ;;
  esac
  printf '{\n  "type": "minimal",\n  "name": "cef_binary_%s_%s_minimal",\n  "sha1": ""\n}\n' \
    "$version" "$platform" >"$tmp/archive.json"
else
  cp "$src/archive.json" "$tmp/archive.json"
fi

cp "$tmp/archive.json" "$tmp/sdk/"
cp -r "$src/include" "$tmp/sdk/"
cc -shared -o "$tmp/sdk/libcef.so" -x c /dev/null

rm -rf "$dest"
mv "$tmp" "$dest"
trap - EXIT
# .cargo/config.toml points CEF_PATH at this link, so no machine's path is in the repo.
ln -sfn "$dest/sdk" "$(dirname "$0")/../.cef-sdk"
echo "install-cef: $(du -sh "$dest" | cut -f1) in $dest"
