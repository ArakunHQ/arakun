#!/bin/sh
# Builds arakun in release mode and installs it for the current user.
#
#   tools/install.sh [--debug] [--no-browser] [--prefix DIR]
#   tools/install.sh --uninstall [--prefix DIR]
#
# PREFIX defaults to ~/.local:
#   PREFIX/bin/arakun
#   PREFIX/share/applications/arakun.desktop
#   PREFIX/share/icons/hicolor/<size>/apps/arakun.png (and scalable/apps/arakun.svg)
#
# macOS: ~/Applications/Arakun.app (CEF inside it) and PREFIX/bin/arakun linking into it.
#
# --debug installs the dev-profile build instead: ready in seconds, for trying
# changes; it runs slower and without the release polish.
#
# Browser tabs also need the CEF runtime in ${XDG_DATA_HOME:-~/.local/share}/arakun/cef.
# When it is missing and an unpacked CEF distribution is around, tools/install-cef.sh
# runs first (it also provides the build-time SDK). --no-browser builds without CEF.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
# The owner's build: --features team,coderlabs (the team hub and the CoderLabs parts).
# A tree without their sources (the public one) builds without them.
features=
if [ -e "$here/src/team.rs" ] && [ -e "$here/src/lab.rs" ]; then
  features="--features team,coderlabs"
fi
prefix=$HOME/.local
browser=1
uninstall=0
debug=0
while [ $# -gt 0 ]; do
  case $1 in
    --no-browser) browser=0 ;;
    --debug) debug=1 ;;
    --uninstall) uninstall=1 ;;
    --prefix) prefix=$2; shift ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "install: unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

# macOS: an Arakun.app in ~/Applications, with CEF inside it (Contents/Frameworks, which
# src/browser/loader.rs looks in first) and PREFIX/bin/arakun pointing into it.
if [ "$(uname)" = Darwin ]; then
  app=$HOME/Applications/Arakun.app
  if [ $uninstall = 1 ]; then
    rm -rf "$app"
    rm -f "$prefix/bin/arakun"
    echo "install: removed $app (settings are kept)"
    exit 0
  fi
  cd "$here"
  profile=release
  [ $debug = 1 ] && profile=debug
  flags=
  [ $debug = 1 ] || flags=--release
  [ $browser = 1 ] || flags="$flags --no-default-features"
  # shellcheck disable=SC2086
  cargo build $flags $features

  tmp="$app.tmp.$$"
  trap 'rm -rf "$tmp"' EXIT
  set --
  [ $debug = 1 ] || set -- --strip
  if [ $browser = 1 ]; then
    framework=$(ls -d .cef-sdk/*/cef_macos_*/"Chromium Embedded Framework.framework" 2>/dev/null | tail -1)
    [ -n "$framework" ] || { echo "install: the build left no CEF framework in .cef-sdk" >&2; exit 1; }
    set -- "$@" --framework "$framework"
  fi
  sh tools/macos-app.sh "$@" "target/$profile/arakun" "$tmp"

  rm -rf "$app"
  mkdir -p "$(dirname "$app")"
  mv "$tmp" "$app"
  trap - EXIT
  mkdir -p "$prefix/bin"
  ln -sfn "$app/Contents/MacOS/arakun" "$prefix/bin/arakun"
  echo "install: Arakun.app installed in $(dirname "$app"), arakun linked in $prefix/bin"
  case :$PATH: in
    *:"$prefix/bin":*) ;;
    *) echo "install: $prefix/bin is not on PATH" ;;
  esac
  exit 0
fi

apps=$prefix/share/applications
icons=$prefix/share/icons/hicolor
sizes="16 24 32 48 64 128 256 512"

# Icons from before the raccoon were named arakun; a new name also keeps
# notification daemons and docks from showing a cached old one.
old_icons() {
  for s in $sizes; do rm -f "$icons/${s}x$s/apps/arakun.png"; done
}

refresh() {
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$icons" 2>/dev/null || true
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$apps" 2>/dev/null || true
}

if [ $uninstall = 1 ]; then
  rm -f "$prefix/bin/arakun" "$apps/arakun.desktop"
  for s in $sizes; do rm -f "$icons/${s}x$s/apps/arakun.png"; done
  rm -f "$icons/scalable/apps/arakun.svg"
  old_icons
  refresh
  echo "install: removed arakun from $prefix (settings and the CEF runtime are kept)"
  exit 0
fi

cd "$here"
profile=release
[ $debug = 1 ] && profile=dev

if [ $browser = 1 ]; then
  cef=${XDG_DATA_HOME:-$HOME/.local/share}/arakun/cef
  # The binary is built against one CEF release (the `cef` crate's +x.y.z) and must load
  # exactly that libcef.so: a runtime left from an older crate crashes the first tab.
  want=$(sed -n '/^name = "cef"$/{n;s/^version = ".*+\(.*\)"$/\1/p;}' Cargo.lock)
  have=$(sed -n 's/.*"cef_binary_\([^+]*\)+.*/\1/p' "$cef/archive.json" 2>/dev/null)
  if [ ! -f "$cef/libcef.so" ] || [ ! -d "$cef/sdk/include" ] || [ "$have" != "$want" ]; then
    dist=${CEF_DIST:-}
    if [ -n "$dist" ] && [ -f "$dist/libcef.so" ]; then
      sh tools/install-cef.sh "$dist" "$cef"
    else
      echo "install: $cef has CEF ${have:-nothing}, this build needs CEF $want" >&2
      echo "         set CEF_DIST to an unpacked CEF $want linux64 build, or pass --no-browser" >&2
      exit 1
    fi
  fi
  # shellcheck disable=SC2086
  if [ $debug = 1 ]; then cargo build $features; else cargo build --release $features; fi
else
  # shellcheck disable=SC2086
  if [ $debug = 1 ]; then cargo build --no-default-features $features; else cargo build --release --no-default-features $features; fi
fi

install -Dm755 "target/$profile/arakun" "$prefix/bin/arakun"
# The release profile keeps debug info for profiling; symbols stay for backtraces.
if [ $debug = 0 ]; then
  command -v strip >/dev/null && strip --strip-debug "$prefix/bin/arakun"
fi

for s in $sizes; do
  dir=$icons/${s}x$s/apps
  mkdir -p "$dir"
  # Up to 48 px the chunkier small icon reads better (tools/brand-logo.py).
  src=assets/brand/arakun.png
  [ "$s" -le 48 ] && src=assets/brand/arakun-small.png
  if command -v magick >/dev/null; then
    magick "$src" -resize "${s}x$s" "$dir/arakun.png"
  elif command -v convert >/dev/null; then
    convert "$src" -resize "${s}x$s" "$dir/arakun.png"
  elif [ "$s" = 512 ]; then
    # Without ImageMagick only one size is installed; the desktop scales it.
    install -m644 assets/brand/arakun.png "$dir/arakun.png"
  fi
done
install -Dm644 assets/brand/arakun.svg "$icons/scalable/apps/arakun.svg"
old_icons

mkdir -p "$apps"
cat > "$apps/arakun.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Arakun
Comment=Coding agents, terminals and a browser in one window
Exec=$prefix/bin/arakun
Icon=arakun
Terminal=false
Categories=Development;IDE;
StartupWMClass=arakun
EOF
command -v desktop-file-validate >/dev/null && desktop-file-validate "$apps/arakun.desktop" || true
refresh

echo "install: arakun installed in $prefix/bin"
case :$PATH: in
  *:"$prefix/bin":*) ;;
  *) echo "install: $prefix/bin is not on PATH" ;;
esac
