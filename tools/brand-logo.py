#!/usr/bin/env python3
"""Draws the app icon: the Arakun raccoon (the R2 mark) in paper white on a
near-black rounded tile.

    python3 tools/brand-logo.py

Writes assets/brand/arakun.png (960 px), arakun.svg (any size) and
arakun-small.png (256 px, the chunkier parts that stay readable at 16-48 px;
install.sh uses it for the small sizes). The parts are
assets/brand/mascot.json (the app's animated raccoon is a newer drawing,
assets/brand/raccoon.svg), with the eyes open and centred; the mask band, ear insides and forehead stripe
are holes. Placement follows shots/arakun/logo-bw/final/build.py.

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
Needs rsvg-convert.
"""
import json
import os
import subprocess
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
TILE, PAPER = "#151515", "#f1efe8"
# Apple's macOS icon grid: on a 1024 px canvas the tile is 824 px with a
# 185.4 px corner radius, and a shadow below it (10 px down, 10 px blur, 30 %).
MAC_CANVAS, MAC_TILE, MAC_RADIUS = 1024, 824, 185.4


def glyph(parts, small, fill):
    """The raccoon in the brand tile's 200-unit box, where the tile spans 2-198."""
    sfx = "_small" if small else ""
    head = parts["head_small"]["d"] if small else parts["head"][0]["d"]
    band = parts["band_small"]["d"] if small else parts["band"][0]["d"]
    holes = " ".join(e["d"] for e in parts["ear_in" + sfx]) + " " + parts["stripe" + sfx]["d"]
    eyes = ""
    for e in parts["eyes"]:
        # Match the app's small eyes: wider and taller, so they survive 16 px.
        w, h = (18.3, 32.7) if small else (e["w"], e["h"])
        eyes += (f'<rect x="{e["cx"] - w / 2:.2f}" y="{e["cy"] - h / 2:.2f}" width="{w:.2f}" '
                 f'height="{h:.2f}" rx="{w / 2:.2f}"/>')
    # The head's (100, 77.5) sits at the tile's centre; small icons get more of the tile.
    k = 0.84 if small else 0.74
    return f'''<g transform="translate({100 - 100 * k:.2f} {100 - 77.5 * k:.3f}) scale({k})" fill="{fill}">
<path fill-rule="evenodd" d="{head} {band} {holes}"/>{eyes}</g>'''


def svg(parts, small):
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
<rect x="2" y="2" width="196" height="196" rx="46" fill="{TILE}"/>
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


def color(hex_):
    r, g, b = (int(hex_[i:i + 2], 16) / 255 for i in (1, 3, 5))
    return {"solid": f"srgb:{r:.5f},{g:.5f},{b:.5f},1.00000"}


def icon_composer(parts):
    """tools/Arakun.icon: the raccoon as one layer on the brand's tile colour."""
    layer = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g transform="{fit(1024, 0)}">{glyph(parts, False, PAPER)}</g></svg>
'''
    spec = {
        "fill": color(TILE),
        "groups": [{
            "layers": [{
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
        f.write(layer)
    with open(os.path.join(icon, "icon.json"), "w") as f:
        f.write(json.dumps(spec, indent=2) + "\n")
    print(f"brand-logo: wrote {os.path.relpath(icon, ROOT)}", file=sys.stderr)


def main():
    parts = json.load(open(os.path.join(ROOT, "assets", "brand", "mascot.json")))
    brand = os.path.join(ROOT, "assets", "brand")
    with open(os.path.join(brand, "arakun.svg"), "w") as f:
        f.write(svg(parts, False) + "\n")
    for draw, small, name, size in [
        (svg, False, "arakun.png", 960),
        (svg, True, "arakun-small.png", 256),
        (macos_svg, False, "arakun-macos.png", 1024),
        (macos_svg, True, "arakun-macos-small.png", 256),
    ]:
        out = os.path.join(brand, name)
        subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", out],
                       input=draw(parts, small).encode(), check=True)
        print(f"brand-logo: wrote {os.path.relpath(out, ROOT)} ({size} px)", file=sys.stderr)
    icon_composer(parts)


if __name__ == "__main__":
    main()
