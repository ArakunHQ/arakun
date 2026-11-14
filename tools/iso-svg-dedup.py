#!/usr/bin/env python3
"""Shrink an animated iso SVG (from tools/iso-anim-svg.py) without changing how
it renders: every top-level element of a frame that shows up in more than one
frame is stored once in <defs> and referenced with <use>.

usage: iso-dedup.py in.svg out.svg
"""
import sys, os, re
import xml.etree.ElementTree as ET
from collections import Counter

SVG = "http://www.w3.org/2000/svg"
ET.register_namespace("", SVG)
src, out = sys.argv[1], sys.argv[2]
tree = ET.parse(src)
root = tree.getroot()
frames = [g for g in root if g.tag == f"{{{SVG}}}g" and g.get("class") == "f"]

def key(el):
    return ET.tostring(el, encoding="unicode")

count = Counter()
for f in frames:
    for k in set(key(c) for c in f):
        count[k] += 1

defs = ET.Element(f"{{{SVG}}}defs")
ids = {}
for f in frames:
    kids = list(f)
    for c in kids:
        k = key(c)
        if count[k] < 2:
            continue
        if k not in ids:
            ids[k] = f"u{len(ids):x}"
            c2 = ET.fromstring(k)
            c2.set("id", ids[k])
            defs.append(c2)
        i = list(f).index(c)
        f.remove(c)
        u = ET.Element(f"{{{SVG}}}use", {"href": "#" + ids[k]})
        f.insert(i, u)

# defs go right after the style blocks
pos = max(i for i, c in enumerate(root) if c.tag == f"{{{SVG}}}style") + 1
root.insert(pos, defs)
# <use> clones live in a shadow tree that @scope does not reach: unwrap it
for st in root.iter(f"{{{SVG}}}style"):
    css = st.text or ""
    m = re.search(r"@scope \((svg\.[\w-]+)\) \{", css)
    if m:
        sel = m.group(1)
        body = css[m.end():css.rindex("}")]
        body = body.replace(":scope{", sel + "{", 1)
        st.text = css[:m.start()] + body
s = ET.tostring(root, encoding="unicode")
s = re.sub(r">\s+<", "><", s)
open(out, "w").write(s)
print(f"{out}: {len(frames)} frames, {len(ids)} shared parts, {os.path.getsize(src) / 1e6:.2f} MB -> {os.path.getsize(out) / 1e6:.2f} MB")
