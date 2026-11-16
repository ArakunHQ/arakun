#!/usr/bin/env bash
# Fails if arakun-server would need a desktop library to start. It is built
# against GPUI without Wayland and X11 so that it runs on a machine with no
# display; a dependency that links one of those libraries again would only show
# on such a machine, as a program that does not start. CI runs this on the
# binary it builds (Linux only: it reads the ELF's list of needed libraries,
# and follows it through the libraries themselves).
#
#   cargo build --locked -p arakun-server
#   tools/check-server-libs.sh target/debug/arakun-server
set -euo pipefail

bin=${1:?usage: tools/check-server-libs.sh <arakun-server binary>}
[[ -f $bin ]] || { echo "check-server-libs: $bin is not a file" >&2; exit 2; }

# What the loader must find before the program runs. A library opened later,
# on demand, is not in this list and does not keep the program from starting.
needed=$(readelf -d "$bin" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
[[ -n $needed ]] || { echo "check-server-libs: could not read what $bin needs" >&2; exit 2; }
# And what those need in turn: a library the binary links can bring a desktop
# one with it. ldd follows them on the machine that built the binary, where
# they are installed; its first word on each line is the library's name.
if closure=$(ldd "$bin" 2>/dev/null); then
  indirect=$(awk '{print $1}' <<< "$closure" | sed 's|.*/||' | grep -E '^lib' || true)
  needed=$(printf '%s\n%s\n' "$needed" "$indirect" | awk 'NF && !seen[$0]++')
fi

desktop='^lib(xkbcommon|xcb|X11|Xcursor|Xi|Xrandr|wayland|fontconfig|freetype|vulkan|EGL|GL|GLX|gbm|drm)([-.]|$)'
found=$(grep -E "$desktop" <<< "$needed" || true)

echo "$bin needs:"
sed 's/^/  /' <<< "$needed"
if [[ -n $found ]]; then
  echo "check-server-libs: these are desktop libraries, which a server does not have:" >&2
  sed 's/^/  /' <<< "$found" >&2
  echo "Something arakun-server depends on links them; see server/Cargo.toml." >&2
  exit 1
fi
echo "No desktop library among them."
