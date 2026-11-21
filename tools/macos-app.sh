#!/bin/sh
# Makes Arakun.app from a built arakun binary. macOS only (sips, iconutil, codesign).
#
#   tools/macos-app.sh [--strip] [--framework DIR] BINARY APP
#
# APP is replaced if it exists. --strip drops the debug info from the copy (symbols stay
# for backtraces). --framework copies "Chromium Embedded Framework.framework" into
# Contents/Frameworks, where core/src/browser/loader.rs looks for CEF first; leave it out for a
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

icons=$(mktemp -d)
# The .icns is for a bundle made without Xcode 26 on macOS 26. Its images are on Apple's icon
# grid (tools/brand-logo.py), so the icon is as big as the others in the Dock. Up to 32 px the
# chunkier small icon reads better.
iconset=$icons/arakun.iconset
mkdir -p "$iconset"
# NAME:PIXELS for each image an iconset holds.
for entry in 16x16:16 16x16@2x:32 32x32:32 32x32@2x:64 128x128:128 128x128@2x:256 \
  256x256:256 256x256@2x:512 512x512:512 512x512@2x:1024; do
  name=${entry%:*} px=${entry#*:}
  src=$here/assets/brand/arakun-macos.png
  if [ "$px" -le 32 ]; then src=$here/assets/brand/arakun-macos-small.png; fi
  sips -z "$px" "$px" "$src" --out "$iconset/icon_$name.png" >/dev/null
done
iconutil -c icns -o "$app/Contents/Resources/arakun.icns" "$iconset"
# macOS 26 draws tools/Arakun.icon in Liquid Glass, tinted or clear as the user picks. Only
# actool from Xcode 26 on macOS 26 compiles it; elsewhere the .icns is all. With
# CFBundleIconName set, older macOS takes the icon from the catalog too: images actool draws
# from the .icon's light look, which is why that look is the brand's. actool ignores an app
# icon set of the same name, so the .icns images cannot replace them.
icon_name=
if xcrun actool "$here/tools/Arakun.icon" --compile "$icons" --app-icon Arakun \
  --output-partial-info-plist "$icons/partial.plist" --platform macosx \
  --target-device mac --minimum-deployment-target 11.0 >"$icons/actool.log" 2>&1 &&
  [ -f "$icons/Assets.car" ]; then
  cp "$icons/Assets.car" "$app/Contents/Resources/"
  icon_name="<key>CFBundleIconName</key><string>Arakun</string>"
else
  cat "$icons/actool.log" >&2
  echo "macos-app: actool did not compile tools/Arakun.icon (it needs Xcode 26 on macOS 26), using the .icns only" >&2
fi
rm -rf "$icons"

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
  $icon_name
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

# The licence and notices travel with the app: the release disk image holds only the app.
cp "$here/LICENSE" "$here/NOTICE" "$app/Contents/Resources/"

if [ -n "$framework" ]; then
  mkdir -p "$app/Contents/Frameworks"
  ditto "$framework" "$app/Contents/Frameworks/Chromium Embedded Framework.framework"
fi
# Ad-hoc: arm64 refuses to run unsigned code. Enough for a local install; the release
# workflow signs it again with the Developer ID certificate and notarizes it.
codesign --force --deep --sign - "$app"
