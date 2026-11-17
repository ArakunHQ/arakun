#!/usr/bin/env python3
"""Draws the app icon: the Arakun raccoon in paper white with amber eyes on a
near-black rounded tile.

    python3 tools/brand-logo.py

Writes assets/brand/arakun.png (960 px), arakun.svg (any size) and
arakun-small.png (256 px, bigger eyes and nose that stay readable at 16-48 px;
install.sh uses it for the small sizes). The face is the mascot's own drawing,
assets/brand/raccoon.svg, so the logo and the app's animated raccoon are one
face: the ear insides, the mask with its thin forehead stripe and the nose are
holes, and the eyes are the brand's one amber.

For macOS it also writes:
- arakun-macos.png (1024 px) and arakun-macos-small.png (256 px): the tile on
  Apple's icon grid, 824 px of 1024 with a drop shadow, so it is as big as the
  other icons in the Dock. tools/macos-app.sh makes the .icns from them, and an
  unbundled `cargo run` shows the large one in the Dock.
- tools/Arakun.icon: an Icon Composer icon, which macOS 26 draws in Liquid Glass
  and tints or clears when the user picks those styles. Light and dark mode both
  get the brand's paper raccoon on ink: actool draws the images older macOS shows
  from the light look, so an inverted light look would replace the brand there.
  Icon Composer designs on the full 1024 pt square; macOS adds the margin.

For the phone remote (phone/) it writes icon.svg, icon-192.png and icon-512.png
(the tile), and apple-touch-icon.png and icon-maskable-512.png (full-bleed ink,
which iOS and Android round or mask themselves).

For the macOS disk image (tools/macos-dmg.py) it writes dmg-background.png and
dmg-background@2x.png: the wordmark on ink, and a stage the app and the
Applications link sit on, with an arrow from one to the other. Finder draws
the icon names black in light mode and white in dark mode, whatever the
background, so the stage is a grey both read on (about 4.5:1 each).
Needs rsvg-convert.
"""
import json
import os
import subprocess
import sys
import tempfile

import re

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
TILE, PAPER, AMBER = "#151515", "#f1efe8", "#ffb547"
# The ASCII face image's width; src/ascii_face.rs scales it to the screen.
ASCII_W = 268
# raccoon.svg's units are the logo's 200-unit box moved by this much; the
# head's (100, 77.5) then sits at the tile's centre.
SHIFT = (10.73, 10.68)
# Apple's macOS icon grid: on a 1024 px canvas the tile is 824 px with a
# 185.4 px corner radius, and a shadow below it (10 px down, 10 px blur, 30 %).
MAC_CANVAS, MAC_TILE, MAC_RADIUS = 1024, 824, 185.4
# The disk image window's content, in points, and where the centres of the app
# and the Applications link go. tools/macos-dmg.py reads these.
DMG_SIZE = (640, 400)
DMG_APP, DMG_APPLICATIONS = (168, 252), (472, 252)
DMG_ICON_SIZE = 128
# The stage: from light at the top to dark at the bottom, around the grey
# where black and white names both read about 4.5:1 (#78746b).
DMG_STAGE = ("#85807a", "#6f6b63")


def face():
    """raccoon.svg's parts in order: head, two ear insides, mask, nose, then the
    two eye rects (x, y, w, h)."""
    src = open(os.path.join(ROOT, "assets", "brand", "raccoon.svg")).read()
    paths = re.findall(r'<path[^>]*\sd="([^"]+)"', src)
    rects = [tuple(float(v) for v in m) for m in re.findall(
        r'<rect[^>]*x="([\d.]+)"[^>]*y="([\d.]+)"[^>]*width="([\d.]+)"[^>]*height="([\d.]+)"', src)]
    assert len(paths) >= 5 and len(rects) >= 2, "raccoon.svg: head, ears, mask, nose, eyes"
    return paths, rects


def glyph(parts, small, fill, eyes=AMBER):
    """The raccoon in the brand tile's 200-unit box, where the tile spans 2-198.
    `eyes=None` leaves them out (the Icon Composer fur layer)."""
    paths, rects = parts
    head, holes, nose = paths[0], paths[1:4], paths[4]
    # Small icons get a bigger nose, so it survives 16 px.
    if small:
        nose = f'<path transform="translate(89.27 98) scale(1.35) translate(-89.27 -98)" d="{nose}"/>'
        fur = f'<path fill-rule="evenodd" d="{head} {" ".join(holes)}"/>'
        # The nose is a hole too: drawn in the tile's colour over the fur.
        nose = nose.replace("<path ", f'<path fill="{TILE}" ')
    else:
        fur = f'<path fill-rule="evenodd" d="{head} {" ".join(holes)} {nose}"/>'
        nose = ""
    marks = ""
    for x, y, w, h in rects[:2] if eyes else []:
        cx, cy = x + w / 2, y + h / 2
        # Match the app's small eyes: wider and taller, so they survive 16 px.
        w, h = (18.3, 32.7) if small else (w, h)
        marks += (f'<rect x="{cx - w / 2:.2f}" y="{cy - h / 2:.2f}" width="{w:.2f}" '
                  f'height="{h:.2f}" rx="{w / 2:.2f}" fill="{eyes}"/>')
    k = 0.84 if small else 0.74
    return (f'<g transform="translate({100 - 100 * k:.2f} {100 - 77.5 * k:.3f}) scale({k}) '
            f'translate({SHIFT[0]} {SHIFT[1]})" fill="{fill}">{fur}{nose}{marks}</g>')


def svg(parts, small):
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
<rect x="2" y="2" width="196" height="196" rx="46" fill="{TILE}"/>
{glyph(parts, small, PAPER)}</svg>'''


def bleed(parts, small):
    """Full-bleed ink with the raccoon as on the tile: for icons the platform
    rounds or masks itself."""
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
<rect width="200" height="200" fill="{TILE}"/>
{glyph(parts, small, PAPER)}</svg>'''


def fit(box, at):
    """Maps the brand tile (2-198 of 200) onto a `box` px square at (`at`, `at`)."""
    k = box / 196
    return f'translate({at - 2 * k:.3f} {at - 2 * k:.3f}) scale({k:.5f})'


def macos_svg(parts, small):
    at = (MAC_CANVAS - MAC_TILE) / 2
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {MAC_CANVAS} {MAC_CANVAS}">
<defs><filter id="shadow" x="-10%" y="-10%" width="120%" height="125%">
<feGaussianBlur in="SourceAlpha" stdDeviation="10"/><feOffset dy="10"/>
<feComponentTransfer><feFuncA type="linear" slope="0.3"/></feComponentTransfer>
<feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge></filter></defs>
<rect x="{at:g}" y="{at:g}" width="{MAC_TILE}" height="{MAC_TILE}" rx="{MAC_RADIUS}" fill="{TILE}" filter="url(#shadow)"/>
<g transform="{fit(MAC_TILE, at)}">{glyph(parts, small, PAPER)}</g></svg>'''


def ascii_face(parts):
    """The raccoon alone on clear ground for the ASCII face. Black holes (not
    cut out) keep the mask apart from the clear around the head: dark mode
    leaves both empty, light mode draws the mask in ink."""
    paths, _ = parts
    head, holes, nose = paths[0], paths[1:4], paths[4]
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 178.54 137.16">
<defs><radialGradient id="lit" gradientUnits="userSpaceOnUse" cx="89.27" cy="70" r="95">
<stop offset="0" stop-color="{PAPER}"/><stop offset="0.55" stop-color="#d9d6ce"/>
<stop offset="1" stop-color="#7a776f"/></radialGradient></defs>
<path fill="url(#lit)" d="{head}"/><path fill="#000" d="{" ".join(holes)} {nose}"/></svg>'''


def color(hex_):
    r, g, b = (int(hex_[i:i + 2], 16) / 255 for i in (1, 3, 5))
    return {"solid": f"srgb:{r:.5f},{g:.5f},{b:.5f},1.00000"}


def icon_composer(parts):
    """tools/Arakun.icon: the fur and the eyes as two layers on the brand's tile
    colour, so Liquid Glass keeps the eyes amber."""
    def layer(body):
        return f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g transform="{fit(1024, 0)}">{body}</g></svg>
'''
    fur = layer(glyph(parts, False, PAPER, eyes=None))
    # The eyes alone: the fur drawn invisible keeps their place.
    eyes = layer(glyph(parts, False, "none", eyes=AMBER))
    spec = {
        "fill": color(TILE),
        "groups": [{
            "layers": [{
                "name": "eyes",
                "image-name": "eyes.svg",
                "fill": color(AMBER),
            }, {
                "name": "raccoon",
                "image-name": "raccoon.svg",
                "fill": color(PAPER),
            }],
            "shadow": {"kind": "neutral", "opacity": 0.5},
        }],
        "supported-platforms": {"squares": "shared"},
    }
    icon = os.path.join(ROOT, "tools", "Arakun.icon")
    os.makedirs(os.path.join(icon, "Assets"), exist_ok=True)
    with open(os.path.join(icon, "Assets", "raccoon.svg"), "w") as f:
        f.write(fur)
    with open(os.path.join(icon, "Assets", "eyes.svg"), "w") as f:
        f.write(eyes)
    with open(os.path.join(icon, "icon.json"), "w") as f:
        f.write(json.dumps(spec, indent=2) + "\n")
    print(f"brand-logo: wrote {os.path.relpath(icon, ROOT)}", file=sys.stderr)


def dmg_svg(parts):
    """The disk image background, in points. The stage runs past the bottom
    edge: the window's title bar takes some of its height, and how much
    differs between macOS versions, so the bottom must look right cut short."""
    w, h = DMG_SIZE
    (ax, cy), (bx, _) = DMG_APP, DMG_APPLICATIONS
    mark = 44
    k = mark / 196
    # The arrow runs between the icons' edges, with room for the tiles' shadow.
    start, end = ax + DMG_ICON_SIZE / 2 + 16, bx - DMG_ICON_SIZE / 2 - 22
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">
<defs><linearGradient id="stage" x1="0" y1="0" x2="0" y2="1">
<stop offset="0" stop-color="{DMG_STAGE[0]}"/><stop offset="1" stop-color="{DMG_STAGE[1]}"/></linearGradient></defs>
<rect width="{w}" height="{h}" fill="{TILE}"/>
<g transform="translate({222 - 2 * k:.3f} {34 - 2 * k:.3f}) scale({k:.5f})">{glyph(parts, True, PAPER)}</g>
<text x="270" y="66" font-family="Geist" font-size="30" font-weight="600" fill="{PAPER}" letter-spacing="-0.6">Arakun</text>
<text x="{w / 2:g}" y="104" font-family="Geist" font-size="13" fill="#8a867d" text-anchor="middle">Drag Arakun into Applications to install it</text>
<rect x="16" y="132" width="{w - 32}" height="{h}" rx="22" fill="url(#stage)"/>
<rect x="16.5" y="132.5" width="{w - 33}" height="{h}" rx="21.5" fill="none" stroke="#ffffff" stroke-opacity="0.12"/>
<line x1="{start:g}" y1="{cy}" x2="{end - 6:g}" y2="{cy}" stroke="{PAPER}" stroke-width="3" stroke-linecap="round" stroke-dasharray="0.1 9"/>
<path d="M{end - 12:g} {cy - 11} L{end:g} {cy} L{end - 12:g} {cy + 11}" fill="none" stroke="{AMBER}" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>
</svg>'''


def dmg_background(parts, brand):
    """Renders dmg_svg at 1x and 2x with the repository's Geist, not whatever
    fonts this machine has."""
    with tempfile.TemporaryDirectory() as tmp:
        conf = os.path.join(tmp, "fonts.conf")
        with open(conf, "w") as f:
            f.write(f'<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "fonts.dtd"><fontconfig>'
                    f'<dir>{os.path.abspath(os.path.join(ROOT, "assets", "fonts"))}</dir>'
                    f'<cachedir>{tmp}</cachedir></fontconfig>')
        env = dict(os.environ, FONTCONFIG_FILE=conf)
        for zoom, name in [(1, "dmg-background.png"), (2, "dmg-background@2x.png")]:
            out = os.path.join(brand, name)
            subprocess.run(["rsvg-convert", "-z", str(zoom), "-o", out],
                           input=dmg_svg(parts).encode(), env=env, check=True)
            print(f"brand-logo: wrote {os.path.relpath(out, ROOT)} ({zoom}x)", file=sys.stderr)


def main():
    parts = face()
    brand = os.path.join(ROOT, "assets", "brand")
    with open(os.path.join(brand, "arakun.svg"), "w") as f:
        f.write(svg(parts, False) + "\n")
    phone = os.path.join(ROOT, "phone")
    with open(os.path.join(phone, "icon.svg"), "w") as f:
        f.write(svg(parts, False) + "\n")
    for draw, small, out, size in [
        (svg, False, os.path.join(brand, "arakun.png"), 960),
        (svg, True, os.path.join(brand, "arakun-small.png"), 256),
        (macos_svg, False, os.path.join(brand, "arakun-macos.png"), 1024),
        (macos_svg, True, os.path.join(brand, "arakun-macos-small.png"), 256),
        (svg, True, os.path.join(phone, "icon-192.png"), 192),
        (svg, False, os.path.join(phone, "icon-512.png"), 512),
        (bleed, False, os.path.join(phone, "apple-touch-icon.png"), 180),
        (bleed, False, os.path.join(phone, "icon-maskable-512.png"), 512),
    ]:
        subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", out],
                       input=draw(parts, small).encode(), check=True)
        print(f"brand-logo: wrote {os.path.relpath(out, ROOT)} ({size} px)", file=sys.stderr)
    out = os.path.join(brand, "ascii-face.png")
    subprocess.run(["rsvg-convert", "-w", str(ASCII_W), "-o", out],
                   input=ascii_face(parts).encode(), check=True)
    print(f"brand-logo: wrote {os.path.relpath(out, ROOT)} ({ASCII_W} px wide)", file=sys.stderr)
    icon_composer(parts)
    dmg_background(parts, brand)


if __name__ == "__main__":
    main()
