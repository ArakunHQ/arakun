#!/usr/bin/env python3
"""Puts Arakun.app in a disk image that opens as the brand's install window:
the app, a link to /Applications and an arrow between them on
assets/brand/dmg-background.png. The release workflow uses it; macOS only.

    python3 tools/macos-dmg.py APP DMG

DMG is replaced if it exists. It needs dmgbuild, which writes Finder's window
layout (.DS_Store) itself, so no Finder has to run, as on a CI runner. Install
the versions the release uses in a virtual environment:

    python3 -m venv .venv-dmg
    .venv-dmg/bin/pip install --require-hashes --no-deps --only-binary :all: \\
        -r tools/macos-dmg-requirements.txt
    .venv-dmg/bin/python tools/macos-dmg.py Arakun.app Arakun.dmg

The image has free space left in it, so the release workflow can sign the app
again inside it and staple the notarization ticket to it.
"""
import importlib.util
import os
import subprocess
import sys
import tempfile

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
# Room for the Developer ID signature and the stapled ticket, which are bigger
# than the ad-hoc signature the app has here. Empty, so it compresses away.
SLACK_SECTORS = 64 * 1024 * 1024 // 512
# The window's height counts its title bar; the background is the content.
TITLE_BAR = 28


def brand():
    """The layout the background was drawn for, from tools/brand-logo.py."""
    spec = importlib.util.spec_from_file_location(
        "brand_logo", os.path.join(ROOT, "tools", "brand-logo.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def hdiutil(*args):
    return subprocess.run(["hdiutil", *args], check=True, capture_output=True, text=True).stdout


def main():
    if len(sys.argv) != 3:
        sys.exit("usage: tools/macos-dmg.py APP DMG")
    app, dmg = sys.argv[1].rstrip("/"), sys.argv[2]
    try:
        import dmgbuild
    except ImportError:
        sys.exit("macos-dmg: dmgbuild is not installed; see the top of tools/macos-dmg.py")
    if not os.path.isdir(app):
        sys.exit(f"macos-dmg: {app} is not an app bundle")

    b = brand()
    name = os.path.basename(app)
    w, h = b.DMG_SIZE
    settings = {
        "files": [app],
        "symlinks": {"Applications": "/Applications"},
        "hide_extensions": [name],
        # The volume's icon, in the Finder sidebar and on the desktop.
        "icon": os.path.join(app, "Contents", "Resources", "arakun.icns"),
        # dmgbuild finds dmg-background@2x.png next to it.
        "background": os.path.join(ROOT, "assets", "brand", "dmg-background.png"),
        "window_rect": ((200, 120), (w, h + TITLE_BAR)),
        "default_view": "icon-view",
        "icon_size": b.DMG_ICON_SIZE,
        "text_size": 13,
        "icon_locations": {name: b.DMG_APP, "Applications": b.DMG_APPLICATIONS},
        # Read-write, so it can be grown below before it is compressed.
        "format": "UDRW",
    }
    with tempfile.TemporaryDirectory() as tmp:
        rw = os.path.join(tmp, "rw.dmg")
        dmgbuild.build_dmg(rw, "Arakun", settings=settings)
        # dmgbuild shrinks the file system to what it holds; grow it again.
        # -limits prints the smallest, current and largest size, in sectors.
        limits = [int(v) for v in hdiutil("resize", "-limits", rw).split() if v.isdigit()]
        if len(limits) != 3:
            sys.exit(f"macos-dmg: cannot read hdiutil resize -limits: {limits}")
        current = limits[1]
        hdiutil("resize", "-sectors", str(current + SLACK_SECTORS), rw)
        # LZFSE: smaller than zlib, and opens on every macOS the app runs on.
        if os.path.exists(dmg):
            os.remove(dmg)
        hdiutil("convert", rw, "-format", "ULFO", "-o", dmg)
    print(f"macos-dmg: wrote {dmg}", file=sys.stderr)


if __name__ == "__main__":
    main()
