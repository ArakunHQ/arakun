#!/usr/bin/env python3
"""Pack an arakun iso scene into ONE animated SVG: every frame as vector, shown
in turn by a CSS animation. Stays sharp at any scale (non-scaling strokes),
unlike a WebP. Frames are framed by their own bounds, so each is shifted to
line up its first path (the scene's base) with frame 0's.

usage: iso-anim-svg.py scene.rhai seconds fps out.svg [start_t]
"""
import re, subprocess, sys, tempfile, os

scene, seconds, fps, out = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4]
start = float(sys.argv[5]) if len(sys.argv) > 5 else 0.0
n = round(seconds * fps)
tmp = tempfile.mkdtemp()

style = cls = None
frames = []
for i in range(n):
    t = (start + i / n) % 1.0
    p = os.path.join(tmp, f"{i}.svg")
    subprocess.run(["arakun", "iso", "svg", scene, "--at", repr(t), "--tau", repr(t * seconds), "--out", p],
                   check=True, capture_output=True)
    s = open(p).read()
    if style is None:
        cls = re.search(r'<svg class="([^"]+)"', s).group(1)
        style = re.search(r"<style>.*?</style>", s, re.S).group(0)
    w, h = (float(x) for x in re.search(r'viewBox="0 0 ([\d.]+) ([\d.]+)"', s).groups())
    body = s[s.index("</style>") + len("</style>"):s.rindex("</svg>")]
    m = re.search(r'd="M([-\d.]+) ([-\d.]+)', body)
    frames.append((float(m.group(1)), float(m.group(2)), w, h, body.strip()))

x0, y0 = frames[0][0], frames[0][1]
shifted = [(x0 - fx, y0 - fy, w, h, b) for fx, fy, w, h, b in frames]
minx = min(dx for dx, *_ in shifted); miny = min(dy for _, dy, *_ in shifted)
maxx = max(dx + w for dx, _, w, _, _ in shifted); maxy = max(dy + h for _, dy, _, h, _ in shifted)
W, H = maxx - minx, maxy - miny

dur = seconds
anim = (f"<style>.f{{visibility:hidden;animation:fr {dur}s steps(1,end) infinite}}"
        f"@keyframes fr{{0%{{visibility:visible}}{100 / n:.4f}%,100%{{visibility:hidden}}}}</style>")
parts = [f'<svg class="{cls}" xmlns="http://www.w3.org/2000/svg" width="{W:.2f}" height="{H:.2f}" '
         f'viewBox="{minx:.2f} {miny:.2f} {W:.2f} {H:.2f}" fill="none" role="img">', style, anim]
for i, (dx, dy, _, _, b) in enumerate(shifted):
    parts.append(f'<g class="f" style="animation-delay:{i * dur / n:.4f}s" transform="translate({dx:.2f} {dy:.2f})">{b}</g>')
parts.append("</svg>")
open(out, "w").write("".join(parts))
print(f"{out}: {n} frames, {os.path.getsize(out) / 1e6:.2f} MB")
