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
Needs rsvg-convert.
"""
import json
import os
import subprocess
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
TILE, PAPER = "#151515", "#f1efe8"


def svg(parts, small):
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
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
<rect x="2" y="2" width="196" height="196" rx="46" fill="{TILE}"/>
<g transform="translate({100 - 100 * k:.2f} {100 - 77.5 * k:.3f}) scale({k})" fill="{PAPER}">
<path fill-rule="evenodd" d="{head} {band} {holes}"/>{eyes}</g></svg>'''


def main():
    parts = json.load(open(os.path.join(ROOT, "assets", "brand", "mascot.json")))
    brand = os.path.join(ROOT, "assets", "brand")
    with open(os.path.join(brand, "arakun.svg"), "w") as f:
        f.write(svg(parts, False) + "\n")
    for small, name, size in [(False, "arakun.png", 960), (True, "arakun-small.png", 256)]:
        out = os.path.join(brand, name)
        subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", out],
                       input=svg(parts, small).encode(), check=True)
        print(f"brand-logo: wrote {os.path.relpath(out, ROOT)} ({size} px)", file=sys.stderr)


if __name__ == "__main__":
    main()
