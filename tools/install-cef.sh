#!/bin/sh
# Prepares the CEF runtime arakun's browser tabs load at run time, plus the tiny "SDK" the
# `cef` crate needs at build time, from an unpacked CEF binary distribution.
#
#   tools/install-cef.sh CEF_DIST [DEST]
#
# CEF_DIST is an unpacked CEF 156.0.15 linux64 distribution (the version cef 156.3 is built
# for; https://cef-builds.spotifycdn.com/index.html). DEST defaults to
# ${XDG_DATA_HOME:-~/.local/share}/arakun/cef. Re-running replaces DEST.
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

[ -f "$src/libcef.so" ] && [ -f "$src/archive.json" ] || {
  echo "install-cef: $src is not an unpacked CEF distribution" >&2
  exit 1
}

tmp="$dest.tmp.$$"
rm -rf "$tmp"
mkdir -p "$tmp/locales" "$tmp/sdk/locales"
trap 'rm -rf "$tmp"' EXIT

echo "install-cef: stripping libcef.so"
strip --strip-unneeded -o "$tmp/libcef.so" "$src/libcef.so"
for f in icudtl.dat v8_context_snapshot.bin resources.pak chrome_100_percent.pak \
  chrome_200_percent.pak libvk_swiftshader.so libvulkan.so.1 vk_swiftshader_icd.json; do
  cp "$src/$f" "$tmp/$f"
done
strip --strip-unneeded "$tmp/libvk_swiftshader.so" "$tmp/libvulkan.so.1"
for l in en-US es es-419; do
  cp "$src/locales/$l.pak" "$tmp/locales/"
done
cp "$src/archive.json" "$tmp/archive.json"

cp "$src/archive.json" "$tmp/sdk/"
cp -r "$src/include" "$tmp/sdk/"
cc -shared -o "$tmp/sdk/libcef.so" -x c /dev/null

rm -rf "$dest"
mv "$tmp" "$dest"
trap - EXIT
# .cargo/config.toml points CEF_PATH at this link, so no machine's path is in the repo.
ln -sfn "$dest/sdk" "$(dirname "$0")/../.cef-sdk"
echo "install-cef: $(du -sh "$dest" | cut -f1) in $dest"
