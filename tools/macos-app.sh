#!/bin/sh
# Makes Arakun.app from a built arakun binary. macOS only (sips, iconutil, codesign).
#
#   tools/macos-app.sh [--strip] [--framework DIR] BINARY APP
#
# APP is replaced if it exists. --strip drops the debug info from the copy (symbols stay
# for backtraces). --framework copies "Chromium Embedded Framework.framework" into
# Contents/Frameworks, where src/browser/loader.rs looks for CEF first; leave it out for a
# build without the browser. tools/install.sh and the release workflow both use this, so a
# local install and a release download are the same bundle.
set -eu

strip=0 framework=
while [ $# -gt 0 ]; do
  case $1 in
    --strip) strip=1 ;;
    --framework) framework=$2; shift ;;
    *) break ;;
  esac
  shift
done
[ $# = 2 ] || { echo "usage: tools/macos-app.sh [--strip] [--framework DIR] BINARY APP" >&2; exit 2; }
bin=$1 app=$2
here=$(cd "$(dirname "$0")/.." && pwd)

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/arakun"
# Before codesign: changing the binary afterwards breaks the signature.
[ $strip = 0 ] || strip -S "$app/Contents/MacOS/arakun"

iconset=$(mktemp -d)/arakun.iconset
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  sips -z $s $s "$here/assets/brand/arakun.png" --out "$iconset/icon_${s}x$s.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$here/assets/brand/arakun.png" --out "$iconset/icon_${s}x$s@2x.png" >/dev/null
done
iconutil -c icns -o "$app/Contents/Resources/arakun.icns" "$iconset"
rm -rf "$(dirname "$iconset")"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
# LSMinimumSystemVersion matches MACOSX_DEPLOYMENT_TARGET in the release workflow.
cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Arakun</string>
  <key>CFBundleDisplayName</key><string>Arakun</string>
  <key>CFBundleIdentifier</key><string>dev.arakun.Arakun</string>
  <key>CFBundleExecutable</key><string>arakun</string>
  <key>CFBundleIconFile</key><string>arakun</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

if [ -n "$framework" ]; then
  mkdir -p "$app/Contents/Frameworks"
  ditto "$framework" "$app/Contents/Frameworks/Chromium Embedded Framework.framework"
fi
# Ad-hoc: arm64 refuses to run unsigned code. There is no Developer ID, so no
# notarization; a download with curl carries no quarantine flag, so Gatekeeper lets it run.
codesign --force --deep --sign - "$app"
