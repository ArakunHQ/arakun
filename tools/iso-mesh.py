#!/usr/bin/env python3
"""Render a real 3D model (glTF) in the flat iso style of tools/iso-real.py, as SVG.

Hand-built primitives miss organic shapes (a near-perfect circle came out square), so for those we take a licensed mesh instead:

  1. load the glTF scene (all nodes, their transforms, each primitive's material),
  2. project it in true 30° isometric with the same light as iso-real,
  3. paint every triangle back to front at high resolution, shaded per triangle
     and snapped to a few tones per material (the reference's flat palette),
  4. trace each tone with potrace into vector paths.

Tones are traced as cumulative masks (tone k = everything from k up), drawn in
order, so neighbouring regions overlap and no hairline gap shows between them.

    iso-mesh.py inspect scene.gltf                # list materials and bounds
    iso-mesh.py render scene.gltf out.svg --map mat=role,... [--turn 30] [--height 187]
"""
import argparse
import base64
import json
import math
import os
import struct
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image, ImageDraw

POTRACE = os.environ.get("POTRACE", "/nix/store/przl9bg6mj1zqx5qzrzpswh8niicm8c5-potrace-1.16/bin/potrace")

C30 = math.cos(math.pi / 6)
CAM = np.array([1, 1, 1]) / math.sqrt(3)
LIGHT = np.array([-0.15, 0.25, 1.0])
LIGHT /= np.linalg.norm(LIGHT)

# role -> (dark, light, levels): the reference's flat greys (iso-real F_*)
ROLES = {
    "shell": ("#2a2b2f", "#6e6f75", 7),
    "cushion": ("#1a1b1e", "#38393d", 5),
    "mesh": ("#17181a", "#2c2d31", 4),
    "band": ("#1f2023", "#46474c", 6),
    "steel": ("#5f6065", "#dcdde1", 7),
    "dark": ("#141517", "#2a2b2e", 3),
    "light": ("#9a9ba0", "#f4f4f6", 5),
    "red": ("#5a1010", "#ff5050", 3),
    "green": ("#0f4a10", "#50ff50", 3),
    "blue": ("#10205a", "#5080ff", 3),
    "yellow": ("#5a5010", "#ffe050", 3),
    "hide": None,
}


# --- glTF ------------------------------------------------------------------------
COMP = {5120: "b", 5121: "B", 5122: "h", 5123: "H", 5125: "I", 5126: "f"}
NCOMP = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}


def load_gltf(path):
    if path.endswith(".glb"):
        data = open(path, "rb").read()
        jlen = struct.unpack_from("<I", data, 12)[0]
        g = json.loads(data[20:20 + jlen])
        blen = struct.unpack_from("<I", data, 20 + jlen)[0]
        bins = [data[28 + jlen:28 + jlen + blen]]
    else:
        g = json.load(open(path))
        base = os.path.dirname(path)
        bins = []
        for b in g["buffers"]:
            uri = b["uri"]
            if uri.startswith("data:"):
                bins.append(base64.b64decode(uri.split(",", 1)[1]))
            else:
                bins.append(open(os.path.join(base, uri), "rb").read())

    def accessor(i):
        a = g["accessors"][i]
        bv = g["bufferViews"][a["bufferView"]]
        buf = bins[bv["buffer"]]
        n, k = a["count"], NCOMP[a["type"]]
        dt = np.dtype("<" + COMP[a["componentType"]])
        off = bv.get("byteOffset", 0) + a.get("byteOffset", 0)
        stride = bv.get("byteStride", 0) or dt.itemsize * k
        raw = np.frombuffer(buf, dtype=np.uint8, count=stride * (n - 1) + dt.itemsize * k, offset=off)
        out = np.lib.stride_tricks.as_strided(raw, shape=(n, dt.itemsize * k), strides=(stride, 1)).copy()
        return out.view(dt).reshape(n, k)

    def node_matrix(nd):
        if "matrix" in nd:
            return np.array(nd["matrix"], dtype=float).reshape(4, 4).T
        t = np.eye(4)
        if "scale" in nd:
            t = np.diag(list(nd["scale"]) + [1.0]) @ t
        if "rotation" in nd:
            x, y, z, w = nd["rotation"]
            r = np.array([[1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w)],
                          [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w)],
                          [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y)]])
            m = np.eye(4)
            m[:3, :3] = r
            t = m @ t
        if "translation" in nd:
            m = np.eye(4)
            m[:3, 3] = nd["translation"]
            t = m @ t
        return t

    mats = [m.get("name") or f"material_{i}" for i, m in enumerate(g.get("materials", []))]
    colors = []
    for m in g.get("materials", []):
        c = m.get("pbrMetallicRoughness", {}).get("baseColorFactor", [1, 1, 1, 1])
        colors.append(c)
    tris = []  # (verts (n,3,3), material index)

    def walk(ni, parent):
        nd = g["nodes"][ni]
        m = parent @ node_matrix(nd)
        if "mesh" in nd:
            for prim in g["meshes"][nd["mesh"]]["primitives"]:
                if prim.get("mode", 4) != 4:
                    continue
                pos = accessor(prim["attributes"]["POSITION"]).astype(float)
                pos = (m @ np.c_[pos, np.ones(len(pos))].T).T[:, :3]
                idx = accessor(prim["indices"]).ravel() if "indices" in prim else np.arange(len(pos))
                tris.append((pos[idx.reshape(-1, 3)], prim.get("material", -1)))
        for c in nd.get("children", []):
            walk(c, m)

    scene = g["scenes"][g.get("scene", 0)]
    for r in scene["nodes"]:
        walk(r, np.eye(4))
    return tris, mats, colors


# --- render ----------------------------------------------------------------------
def hx(c):
    c = c.lstrip("#")
    return np.array([int(c[i:i + 2], 16) for i in (0, 2, 4)], dtype=float)


def project(p):
    x, y, z = p[..., 0], p[..., 1], p[..., 2]
    return np.stack([(x - y) * C30, (x + y) * 0.5 - z], axis=-1)


def render(path, out, mapping, turn, height, px, flip_up, smooth=True, above=(), clean=5,
           levels=12, rim=0.38, sigma=3.4, shadow=True, inward=()):
    """Per-pixel shading: triangles are painted back to front into id and normal
    buffers, the normals are blurred within each mesh part (so light follows the
    surface, not the triangles), lit with the iso-real light plus a rim term that
    brightens grazing edges like the reference's drawn rims, snapped to `levels`
    tones per role, and traced."""
    from scipy import ndimage

    tris, mats, _ = load_gltf(path)
    verts = np.concatenate([t[0] for t in tris])
    mids = np.concatenate([np.full(len(t[0]), t[1]) for t in tris])
    part = np.concatenate([np.full(len(t[0]), k) for k, t in enumerate(tris)])
    v = verts[..., [0, 2, 1]] * np.array([1, -1, 1]) if flip_up else verts
    lo, hi = v.reshape(-1, 3).min(0), v.reshape(-1, 3).max(0)
    sc = height / (hi[2] - lo[2])
    v = (v - (lo + hi) / 2 * np.array([1, 1, 0]) - np.array([0, 0, lo[2]])) * sc
    a = math.radians(turn)
    rot = np.array([[math.cos(a), -math.sin(a), 0], [math.sin(a), math.cos(a), 0], [0, 0, 1]])
    v = v @ rot.T

    roles = [mapping.get(m, "shell") for m in mats] + ["shell"]
    keep = np.array([ROLES.get(roles[m]) is not None for m in mids])
    v, mids, part = v[keep], mids[keep], part[keep]
    n = np.cross(v[:, 1] - v[:, 0], v[:, 2] - v[:, 0])
    ln = np.linalg.norm(n, axis=1)
    ok = ln > 1e-12
    v, mids, part, n, ln = v[ok], mids[ok], part[ok], n[ok] / ln[ok, None], ln[ok]
    n[(n @ CAM) < 0] *= -1
    if smooth:
        key = np.round(v.reshape(-1, 3) / (height * 2e-4)).astype(np.int64)
        _, inv = np.unique(key, axis=0, return_inverse=True)
        acc = np.zeros((inv.max() + 1, 3))
        np.add.at(acc, inv, np.repeat(n * ln[:, None], 3, axis=0))
        vn = acc[inv].reshape(-1, 3, 3).sum(1)
        vn /= np.linalg.norm(vn, axis=1, keepdims=True) + 1e-12
        vn[(vn * n).sum(1) < 0] *= -1
        n = vn
    tri_role = [roles[m] for m in mids]
    for rule in above:
        mat, rest = rule.split("@", 1)
        frac, role = rest.split("=", 1)
        if mat in mats:
            zc = v[:, :, 2].mean(1)
            for i in np.nonzero((mids == mats.index(mat)) & (zc > float(frac) * height))[0]:
                tri_role[i] = role
                part[i] += 10000  # its own part, so normals do not blur across

    for rule in inward:  # material=role: faces of that material looking at the vertical axis
        mat, role = rule.split("=", 1)
        if mat in mats:
            c = v.mean(1)
            radial = c * np.array([1, 1, 0])
            radial /= np.linalg.norm(radial, axis=1, keepdims=True) + 1e-9
            facing = (n * -radial).sum(1)
            for i in np.nonzero((mids == mats.index(mat)) & (facing > 0.86))[0]:
                tri_role[i] = role
                part[i] += 20000

    # frame: the object plus its shadow on the ground
    p2 = project(v)
    g2 = project(v * np.array([1, 1, 0]))
    allp = np.concatenate([p2.reshape(-1, 2), g2.reshape(-1, 2)])
    mn, mx = allp.min(0), allp.max(0)
    pad = 0.04 * (mx - mn).max()
    scale = px / ((mx - mn).max() + 2 * pad)
    tf = lambda q: (q - mn + pad) * scale
    p2, g2 = tf(p2), tf(g2)
    W = int(math.ceil((mx[0] - mn[0] + 2 * pad) * scale))
    H = int(math.ceil((mx[1] - mn[1] + 2 * pad) * scale))
    order = np.argsort(v.mean(1) @ CAM)

    pid_img = Image.new("I", (W, H), -1)
    nimg = [Image.new("I", (W, H), 0) for _ in range(3)]
    dp = ImageDraw.Draw(pid_img)
    dn = [ImageDraw.Draw(im) for im in nimg]
    uparts = {p_: k for k, p_ in enumerate(np.unique(part))}
    nq = np.round(n * 30000).astype(int)
    for i in order:
        poly = [tuple(q) for q in p2[i]]
        dp.polygon(poly, fill=uparts[part[i]])
        for c in range(3):
            dn[c].polygon(poly, fill=int(nq[i, c]))
    pid = np.array(pid_img)
    N = np.stack([np.array(im, dtype=float) / 30000 for im in nimg], -1)
    # blur normals inside each part only (normalized convolution)
    Ns = np.zeros_like(N)
    for k in range(len(uparts)):
        mk = (pid == k).astype(float)
        if not mk.any():
            continue
        den = ndimage.gaussian_filter(mk, sigma)
        for c in range(3):
            num = ndimage.gaussian_filter(N[..., c] * mk, sigma)
            Ns[..., c] += np.where(mk > 0, num / np.maximum(den, 1e-6), 0)
    Ns /= np.linalg.norm(Ns, axis=-1, keepdims=True) + 1e-9
    lam = (Ns @ LIGHT + 1) / 2
    graze = np.clip(1 - Ns @ CAM, 0, 1) ** 2.2
    val = np.clip(lam * 0.9 + graze * rim, 0, 1)

    # tones: role block x levels
    role_names = [r for r in ROLES if ROLES[r]]
    tone_colors, base = [], {}
    for r in role_names:
        dark, light, _ = ROLES[r]
        base[r] = len(tone_colors)
        for k in range(levels):
            f = k / (levels - 1)
            tone_colors.append(hx(dark) + (hx(light) - hx(dark)) * f)
    part_role = {}
    for i in range(len(part)):
        part_role.setdefault(uparts[part[i]], tri_role[i])
    lab = np.full(pid.shape, -1, dtype=np.int32)
    q = np.minimum(levels - 1, (val * levels).astype(int))
    for k, r in part_role.items():
        mk = pid == k
        lab[mk] = base[r] + q[mk]
    if clean:
        from PIL import ImageFilter
        l8 = np.where(lab < 0, 255, lab).astype(np.uint8)
        l8 = np.array(Image.fromarray(l8, "L").filter(ImageFilter.ModeFilter(clean)))
        lab = np.where(l8 == 255, -1, l8.astype(np.int32))
    print(f"{len(order)} triangles -> {W}x{H}, {len(set(np.unique(lab)) - {-1})} tones used", file=sys.stderr)

    def trace(mask, td):
        pbm = os.path.join(td, "m.pbm")
        Image.fromarray(((~mask) * 255).astype(np.uint8)).convert("1").save(pbm)
        svg = subprocess.run([POTRACE, "-s", "--flat", "-t", "8", "-a", "1.2", "-O", "0.6", "-o", "-", pbm],
                             capture_output=True, text=True, check=True).stdout
        d = " ".join(seg.split('d="', 1)[1].split('"', 1)[0] for seg in svg.split("<path")[1:])
        return svg.split('<g transform="', 1)[1].split('"', 1)[0], d

    parts_svg, defs = [], ""
    with tempfile.TemporaryDirectory() as td:
        if shadow:
            # soft ground shadow: the footprint of whatever is near the floor
            sh = Image.new("1", (W, H), 0)
            ds = ImageDraw.Draw(sh)
            low = v[:, :, 2].min(1) < 0.12 * height
            for i in np.nonzero(low)[0]:
                ds.polygon([tuple(q_) for q_ in g2[i]], fill=1)
            tr, d = trace(np.array(sh, dtype=bool), td)
            blur = 0.012 * px
            defs = (f'<defs><filter id="sh" x="-20%" y="-20%" width="140%" height="140%">'
                    f'<feGaussianBlur stdDeviation="{blur:.1f}"/></filter></defs>')
            parts_svg.append(f'<g filter="url(#sh)" opacity="0.6"><path transform="{tr}" fill="#000" d="{d}"/></g>')
        present = sorted(set(np.unique(lab)) - {-1})
        for k, tid in enumerate(present):
            tr, d = trace(np.isin(lab, present[k:]), td)
            c = tone_colors[tid]
            parts_svg.append(f'<path transform="{tr}" fill="#{int(c[0]):02x}{int(c[1]):02x}{int(c[2]):02x}" d="{d}"/>')
    with open(out, "w") as f:
        f.write(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}">'
                + defs + "".join(parts_svg) + "</svg>")
    print(out, os.path.getsize(out) // 1024, "KB", file=sys.stderr)


# --- paint: continuous shading, raster -------------------------------------------
# Posterised tones traced to vector look like contour lines up close. For a polished product render we shade every pixel continuously and
# write an image at ~3x its display size instead.
LOOK = {  # albedo (sRGB 0-1), diffuse, specular, shininess, rim, texture
    "shell": ((0.30, 0.305, 0.325), 0.85, 0.16, 9, 0.42, 0.0),
    "cushion": ((0.155, 0.158, 0.172), 0.9, 0.03, 5, 0.30, 0.06),
    "mesh": ((0.09, 0.092, 0.1), 0.9, 0.02, 4, 0.12, 0.08),
    "band": ((0.175, 0.178, 0.19), 0.9, 0.12, 9, 0.32, 0.0),
    "steel": ((0.66, 0.67, 0.70), 0.7, 0.8, 40, 0.2, 0.0),
    "dark": ((0.06, 0.062, 0.07), 0.9, 0.05, 8, 0.1, 0.0),
    "light": ((0.9, 0.9, 0.92), 0.9, 0.3, 20, 0.1, 0.0),
}


def paint(path, out, mapping, turn, height, px, flip_up, above=(), inward=(), ss=2, sigma=5.0, seed=7):
    from scipy import ndimage

    tris, mats, _ = load_gltf(path)
    verts = np.concatenate([t[0] for t in tris])
    mids = np.concatenate([np.full(len(t[0]), t[1]) for t in tris])
    part = np.concatenate([np.full(len(t[0]), k) for k, t in enumerate(tris)])
    v = verts[..., [0, 2, 1]] * np.array([1, -1, 1]) if flip_up else verts
    lo, hi = v.reshape(-1, 3).min(0), v.reshape(-1, 3).max(0)
    sc = height / (hi[2] - lo[2])
    v = (v - (lo + hi) / 2 * np.array([1, 1, 0]) - np.array([0, 0, lo[2]])) * sc
    a = math.radians(turn)
    rot = np.array([[math.cos(a), -math.sin(a), 0], [math.sin(a), math.cos(a), 0], [0, 0, 1]])
    v = v @ rot.T
    roles = [mapping.get(m, "shell") for m in mats] + ["shell"]
    keep = np.array([roles[m] in LOOK for m in mids])
    v, mids, part = v[keep], mids[keep], part[keep]
    n = np.cross(v[:, 1] - v[:, 0], v[:, 2] - v[:, 0])
    ln = np.linalg.norm(n, axis=1)
    ok = ln > 1e-12
    v, mids, part, n, ln = v[ok], mids[ok], part[ok], n[ok] / ln[ok, None], ln[ok]
    n[(n @ CAM) < 0] *= -1
    key = np.round(v.reshape(-1, 3) / (height * 2e-4)).astype(np.int64)
    _, inv = np.unique(key, axis=0, return_inverse=True)
    acc = np.zeros((inv.max() + 1, 3))
    np.add.at(acc, inv, np.repeat(n * ln[:, None], 3, axis=0))
    vn = acc[inv].reshape(-1, 3, 3).sum(1)
    vn /= np.linalg.norm(vn, axis=1, keepdims=True) + 1e-12
    vn[(vn * n).sum(1) < 0] *= -1
    n = vn
    tri_role = [roles[m] for m in mids]
    for rule in above:
        mat, rest = rule.split("@", 1)
        frac, role = rest.split("=", 1)
        if mat in mats:
            zc = v[:, :, 2].mean(1)
            for i in np.nonzero((mids == mats.index(mat)) & (zc > float(frac) * height))[0]:
                tri_role[i] = role
                part[i] += 10000
    for rule in inward:
        mat, role = rule.split("=", 1)
        if mat in mats:
            c = v.mean(1)
            radial = c * np.array([1, 1, 0])
            radial /= np.linalg.norm(radial, axis=1, keepdims=True) + 1e-9
            for i in np.nonzero((mids == mats.index(mat)) & ((n * -radial).sum(1) > 0.86))[0]:
                tri_role[i] = role
                part[i] += 20000

    p2 = project(v)
    g2 = project(v * np.array([1, 1, 0]))
    allp = np.concatenate([p2.reshape(-1, 2), g2.reshape(-1, 2)])
    mn, mx = allp.min(0), allp.max(0)
    pad = 0.05 * (mx - mn).max()
    S = px * ss
    scale = S / ((mx - mn).max() + 2 * pad)
    p2 = (p2 - mn + pad) * scale
    g2 = (g2 - mn + pad) * scale
    W = int(math.ceil((mx[0] - mn[0] + 2 * pad) * scale / ss)) * ss
    H = int(math.ceil((mx[1] - mn[1] + 2 * pad) * scale / ss)) * ss
    depth = v.mean(1) @ CAM
    order = np.argsort(depth)
    uparts = {p_: k for k, p_ in enumerate(np.unique(part))}
    ims = {k: Image.new("I", (W, H), -1 if k == "id" else 0) for k in ("id", "nx", "ny", "nz", "z")}
    dr = {k: ImageDraw.Draw(im) for k, im in ims.items()}
    nq = np.round(n * 30000).astype(int)
    zq = np.round((depth - depth.min()) / (np.ptp(depth) + 1e-9) * 1e6).astype(int)
    for i in order:
        poly = [tuple(q) for q in p2[i]]
        dr["id"].polygon(poly, fill=uparts[part[i]])
        dr["nx"].polygon(poly, fill=int(nq[i, 0]))
        dr["ny"].polygon(poly, fill=int(nq[i, 1]))
        dr["nz"].polygon(poly, fill=int(nq[i, 2]))
        dr["z"].polygon(poly, fill=int(zq[i]))
    pid = np.array(ims["id"])
    N = np.stack([np.array(ims[c], dtype=np.float32) / 30000 for c in ("nx", "ny", "nz")], -1)
    Z = np.array(ims["z"], dtype=np.float32) / 1e6 * float(np.ptp(depth)) * scale  # in pixels
    cover = pid >= 0
    sg = sigma * ss
    Ns = np.zeros_like(N)
    for k in range(len(uparts)):
        mk = (pid == k).astype(np.float32)
        if not mk.any():
            continue
        den = ndimage.gaussian_filter(mk, sg)
        for c in range(3):
            Ns[..., c] += np.where(mk > 0, ndimage.gaussian_filter(N[..., c] * mk, sg) / np.maximum(den, 1e-6), 0)
    Ns /= np.linalg.norm(Ns, axis=-1, keepdims=True) + 1e-9

    # per-pixel material
    part_role = {}
    for i in range(len(part)):
        part_role.setdefault(uparts[part[i]], tri_role[i])
    alb = np.zeros((H, W, 3), np.float32)
    kd, ks, sh_, kr, tex = (np.zeros((H, W), np.float32) for _ in range(5))
    for k, r in part_role.items():
        mk = pid == k
        al, d_, s_, shin, rim_, t_ = LOOK[r]
        alb[mk] = al
        kd[mk], ks[mk], sh_[mk], kr[mk], tex[mk] = d_, s_, shin, rim_, t_

    L2 = np.array([0.9, -0.35, 0.45])
    L2 /= np.linalg.norm(L2)  # soft fill from the right
    Hh = (LIGHT + CAM) / np.linalg.norm(LIGHT + CAM)
    ndl = np.clip(Ns @ LIGHT, 0, 1)
    wrap = (Ns @ LIGHT + 0.35) / 1.35  # wrapped diffuse keeps the dark side readable
    fill = np.clip(Ns @ L2, 0, 1) * 0.25
    spec = np.clip(Ns @ Hh, 0, 1) ** np.maximum(sh_, 1) * ks
    graze = np.clip(1 - Ns @ CAM, 0, 1) ** 3 * kr
    # contact shadow: pixels well behind their neighbourhood (crevices) get darker
    # depth is flat per triangle here, so a screen-space AO would print the mesh:
    # only a gentle darkening where a part is overlapped by a nearer one
    ao = np.ones_like(Z)
    rng = np.random.default_rng(seed)
    grain = 1 + tex * (ndimage.gaussian_filter(rng.standard_normal((H, W)).astype(np.float32), 0.7 * ss) * 2.2)
    light = (0.36 + 0.66 * np.clip(wrap, 0, 1) * kd + fill) * ao
    col = alb * light[..., None] * grain[..., None] + (spec + graze)[..., None] * 0.55 * ao[..., None]
    col = np.clip(col, 0, 1)

    # ground shadow (soft, from what touches the floor) under everything
    sh = Image.new("L", (W, H), 0)
    ds = ImageDraw.Draw(sh)
    for i in np.nonzero(v[:, :, 2].min(1) < 0.14 * height)[0]:
        ds.polygon([tuple(q) for q in g2[i]], fill=255)
    shadow = ndimage.gaussian_filter(np.array(sh, np.float32) / 255, 0.012 * S) * 0.62
    alpha = np.where(cover, 1.0, shadow)
    if out.endswith(".svg"):
        light = (0.36 + 0.66 * np.clip(wrap, 0, 1) * kd + fill) * ao  # no grain in vector
        col = np.clip(alb * light[..., None] + (spec + graze)[..., None] * 0.55, 0, 1)
        vectorize(out, col, pid, Z, Ns, cover, np.array(sh, np.float32) > 0, 0.012 * S, W, H)
        return
    rgb = np.where(cover[..., None], col, 0.0)
    # premultiplied downsample = antialiased edges
    pre = np.concatenate([rgb * alpha[..., None], alpha[..., None]], -1)
    pre = pre.reshape(H // ss, ss, W // ss, ss, 4).mean((1, 3))
    a_ = pre[..., 3:4]
    out_rgb = np.where(a_ > 1e-4, pre[..., :3] / np.maximum(a_, 1e-4), 0)
    out_rgb += rng.uniform(-0.5, 0.5, out_rgb.shape) / 255  # dither: no 8-bit banding
    img = np.concatenate([np.clip(out_rgb, 0, 1), np.clip(a_, 0, 1)], -1)
    Image.fromarray((img * 255 + 0.5).astype(np.uint8), "RGBA").save(out, quality=95, method=6) \
        if out.endswith(".webp") else Image.fromarray((img * 255 + 0.5).astype(np.uint8), "RGBA").save(out)
    print(out, f"{W // ss}x{H // ss}", os.path.getsize(out) // 1024, "KB", file=sys.stderr)


def _kmeans(x, k, iters=20, seed=0):
    rng = np.random.default_rng(seed)
    c = x[rng.choice(len(x), k, replace=False)]
    for _ in range(iters):
        lab = np.argmin(((x[:, None] - c[None]) ** 2).sum(-1), 1)
        for j in range(k):
            if (lab == j).any():
                c[j] = x[lab == j].mean(0)
    return lab


def vectorize(out, col, pid, Z, Ns, cover, shadow_mask, shadow_blur, W, H, levels=6):
    """Vector the way illustrators shade in Illustrator: every part is one crisp
    silhouette used as a clip, filled with its darkest tone, and the lighter tones
    are stacked inside as feathered (Gaussian-blurred) shapes. The blur turns the
    tone steps into smooth gradients, the clip keeps the outer edge sharp, and no
    contour lines or face seams show. (Layered fills over a base, as in ARDECO /
    Adobe's gradient-reconstruction vectorizer, with the feather doing the fit.)"""
    from scipy import ndimage

    def trace(mask, td, turd=10, alpha=1.3):
        pbm = os.path.join(td, "m.pbm")
        Image.fromarray(((~mask) * 255).astype(np.uint8)).convert("1").save(pbm)
        svg = subprocess.run([POTRACE, "-s", "--flat", "-t", str(turd), "-a", str(alpha), "-O", "0.8",
                              "-u", "10", "-o", "-", pbm], capture_output=True, text=True, check=True).stdout
        return " ".join(seg.split('d="', 1)[1].split('"', 1)[0] for seg in svg.split("<path")[1:])

    hexc = lambda c: "#%02x%02x%02x" % tuple(int(np.clip(v, 0, 1) * 255 + 0.5) for v in c)
    G = f'transform="translate(0,{H}) scale(0.1,-0.1)"'
    lumw = np.array([0.3, 0.59, 0.11])
    parts = [k for k in np.unique(pid[pid >= 0]) if (pid == k).sum() >= 40]
    parts.sort(key=lambda k: Z[pid == k].mean())
    defs, body = [], []
    with tempfile.TemporaryDirectory() as td:
        if shadow_mask.any():
            defs.append(f'<filter id="sh" x="-20%" y="-20%" width="140%" height="140%">'
                        f'<feGaussianBlur stdDeviation="{shadow_blur:.1f}"/></filter>')
            body.append(f'<g filter="url(#sh)" opacity="0.6"><path {G} fill="#000" d="{trace(shadow_mask, td, 40)}"/></g>')
        for k in parts:
            m = pid == k
            area = m.sum()
            sil = trace(ndimage.binary_dilation(m, iterations=1), td)
            cid = f"c{k}"
            defs.append(f'<clipPath id="{cid}"><path {G} d="{sil}"/></clipPath>')
            lum = col @ lumw
            pl = lum[m]
            spread = np.quantile(pl, 0.97) - np.quantile(pl, 0.03)
            nl = levels if area > 3000 else (3 if area > 300 else 1)
            nl = max(1, min(nl, int(spread / 0.018)))  # one layer per ~5/255 of change
            qs = np.quantile(pl, np.linspace(0, 1, nl + 1))
            # base: the darkest band's colour over the whole silhouette
            base = col[m & (lum <= qs[1])].mean(0) if nl > 1 else col[m].mean(0)
            layers = [f'<path {G} fill="{hexc(base)}" d="{sil}"/>']
            feather = max(2.0, math.sqrt(area) * 0.045)
            fid = f"f{k}"
            if nl > 1:
                defs.append(f'<filter id="{fid}" x="-30%" y="-30%" width="160%" height="160%">'
                            f'<feGaussianBlur stdDeviation="{feather:.1f}"/></filter>')
            soft = ndimage.gaussian_filter(np.where(m, lum, 0).astype(np.float32), feather * 1.6)
            norm_ = ndimage.gaussian_filter(m.astype(np.float32), feather * 1.6)
            lums = np.where(norm_ > 1e-3, soft / np.maximum(norm_, 1e-3), 0)
            outside = ndimage.binary_dilation(m, iterations=int(2 * feather) + 2) & ~m
            for j in range(1, nl):
                mk = m & (lums >= qs[j])
                if mk.sum() < 30:
                    continue
                # carry the layer past the silhouette so the feather never fades at the clip edge
                mk = mk | (ndimage.binary_dilation(mk, iterations=int(2 * feather) + 2) & outside)
                band = (lum >= qs[j]) & (lum <= qs[j + 1]) & m
                c = col[band].mean(0) if band.any() else base
                # the filter sits on an untransformed group, so stdDeviation is in pixels
                layers.append(f'<g filter="url(#{fid})"><path {G} fill="{hexc(c)}" d="{trace(mk, td, 20, 1.33)}"/></g>')
            body.append(f'<g clip-path="url(#{cid})">{"".join(layers)}</g>')
    with open(out, "w") as f:
        f.write(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}">'
                f'<defs>{"".join(defs)}</defs>{"".join(body)}</svg>')
    print(out, f"{W}x{H}", len(parts), "parts", os.path.getsize(out) // 1024, "KB", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    pp = sub.add_parser("paint", help="continuous shading to PNG/WebP")
    pp.add_argument("model")
    pp.add_argument("out")
    pp.add_argument("--map", default="")
    pp.add_argument("--turn", type=float, default=0.0)
    pp.add_argument("--height", type=float, default=187.0)
    pp.add_argument("--px", type=int, default=1800)
    pp.add_argument("--ss", type=int, default=2)
    pp.add_argument("--z-up", action="store_true")
    pp.add_argument("--above", action="append", default=[])
    pp.add_argument("--inward", action="append", default=[])
    i = sub.add_parser("inspect")
    i.add_argument("model")
    r = sub.add_parser("render")
    r.add_argument("model")
    r.add_argument("out")
    r.add_argument("--map", default="", help="material=role,... roles: " + ",".join(ROLES))
    r.add_argument("--turn", type=float, default=0.0, help="degrees around the vertical")
    r.add_argument("--height", type=float, default=187.0)
    r.add_argument("--px", type=int, default=2400, help="trace resolution (longest side)")
    r.add_argument("--z-up", action="store_true", help="model is already Z-up")
    r.add_argument("--flat-normals", action="store_true", help="shade each triangle by its own normal")
    r.add_argument("--above", action="append", default=[], help="material@frac=role, e.g. fabric@0.6=mesh")
    r.add_argument("--clean", type=int, default=5, help="mode filter size before tracing (0 = off)")
    r.add_argument("--levels", type=int, default=12, help="tones per role")
    r.add_argument("--rim", type=float, default=0.38, help="brightening of grazing edges")
    r.add_argument("--inward", action="append", default=[], help="material=role for faces looking at the centre axis")
    a = ap.parse_args()
    if a.cmd == "paint":
        mapping = dict(kv.split("=", 1) for kv in a.map.split(",") if kv)
        paint(a.model, a.out, mapping, a.turn, a.height, a.px, not a.z_up, a.above, a.inward, a.ss)
        return
    if a.cmd == "inspect":
        tris, mats, colors = load_gltf(a.model)
        allv = np.concatenate([t[0] for t in tris]).reshape(-1, 3)
        print("bounds", allv.min(0).round(3), allv.max(0).round(3))
        count = {}
        for t, m in tris:
            count[m] = count.get(m, 0) + len(t)
        for k, name in enumerate(mats):
            print(f"{k:3d} {name:40s} tris={count.get(k, 0):7d} color={[round(c, 2) for c in colors[k]]}")
    else:
        mapping = dict(kv.split("=", 1) for kv in a.map.split(",") if kv)
        render(a.model, a.out, mapping, a.turn, a.height, a.px, not a.z_up, not a.flat_normals, a.above, a.clean,
               a.levels, a.rim, inward=a.inward)


if __name__ == "__main__":
    main()
