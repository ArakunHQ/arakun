#!/usr/bin/env python3
"""Realistic isometric renderer: soft-shaded objects in true 30° isometric, as plain SVG.

Unlike arakun iso (ink outlines, three flat tones), form here comes only from value:
every side face is cut into thin quads around its outline and each quad is lit from
its own normal (Lambert + a little specular), so rounded corners and bevels get a
smooth gradient the way a product render does. No scripts in the output: Doop morphs
script-drawn SVG away (mem #303).

    iso-real.py phone    out.svg   # iPhone + USB-C cable (study 01)
    iso-real.py swatches out.svg   # materials sheet (study 02)
"""
import math
import sys

# --- projection -------------------------------------------------------------
C30 = math.cos(math.pi / 6)
K = 6.0  # px per mm


def P(p):
    x, y, z = p
    return ((x - y) * C30 * K, ((x + y) * 0.5 - z) * K)


def Pv(v):
    return P(v)


def add(a, b):
    return tuple(i + j for i, j in zip(a, b))


def mul(a, s):
    return tuple(i * s for i in a)


def dot(a, b):
    return sum(i * j for i, j in zip(a, b))


def norm(a):
    l = math.sqrt(dot(a, a)) or 1.0
    return mul(a, 1 / l)


CAM = norm((1, 1, 1))  # toward the viewer
LIGHT = norm((-0.15, 0.25, 1.0))  # from above, a touch from the viewer's left
HALF = norm(add(LIGHT, CAM))


# --- colour -----------------------------------------------------------------
def hx(c):
    c = c.lstrip("#")
    return tuple(int(c[i:i + 2], 16) for i in (0, 2, 4))


def css(rgb):
    return "#%02x%02x%02x" % tuple(max(0, min(255, round(v))) for v in rgb)


def mix(a, b, t):
    return tuple(x + (y - x) * t for x, y in zip(a, b))


class Mat:
    """dark..light ramp over n·L, plus a white specular lobe."""

    def __init__(self, dark, light, ks=0.15, shin=24, gamma=1.0):
        self.dark, self.light = hx(dark), hx(light)
        self.ks, self.shin, self.gamma = ks, shin, gamma

    def shade(self, n):
        t = (dot(n, LIGHT) + 1) / 2
        c = mix(self.dark, self.light, t ** self.gamma)
        s = max(dot(n, HALF), 0) ** self.shin * self.ks
        return css(mix(c, (255, 255, 255), s))


class RimMat:
    """Flat illustration tone: `center` where the surface faces you, easing to a
    lighter `edge` toward the silhouette, like the drawn rim on the reference plug."""

    def __init__(self, center, edge, width=0.5):
        self.center, self.edge, self.width = hx(center), hx(edge), width

    def shade(self, n):
        f = max(0.0, min(1.0, dot(n, CAM) / self.width))
        return css(mix(self.edge, self.center, f * f * (3 - 2 * f)))


# --- outlines ---------------------------------------------------------------
def rrect(w, h, r, seg=14, smooth=False):
    """Rounded rect in local (u, v), u right v down, with outward vertex normals.

    smooth: Apple-style continuous corner instead of a circular arc. Each corner is
    a superellipse quarter spanning 1.55 r of the edge, so the curve leaves the
    straight side gently instead of a circle being stuck onto it."""
    n = 3.6
    R = min(r * 1.55 if smooth else r, w / 2, h / 2)
    pts = []
    corners = [(w - R, R, -90), (w - R, h - R, 0), (R, h - R, 90), (R, R, 180)]
    for cx, cy, a0 in corners:
        for i in range(seg + 1):
            a = math.radians(a0 + 90 * i / seg)
            c, sn = math.cos(a), math.sin(a)
            if not smooth:
                pts.append((cx + R * c, cy + R * sn, c, sn))
                continue
            x = math.copysign(abs(c) ** (2 / n), c)
            y = math.copysign(abs(sn) ** (2 / n), sn)
            nu = math.copysign(abs(x) ** (n - 1), x)
            nv = math.copysign(abs(y) ** (n - 1), y)
            l = math.hypot(nu, nv)
            pts.append((cx + R * x, cy + R * y, nu / l, nv / l))
    return pts


def inset_path(outline, d):
    """SVG path (local coords) of the outline pushed in by d along its normals."""
    return "M" + "L".join(f"{u - a * d:.3f},{v - b * d:.3f}" for u, v, a, b in outline) + "Z"


# --- svg accumulation -------------------------------------------------------
# --- output -----------------------------------------------------------------
import os
# ISO_REAL_QUADS=1: the old output, a flat-lit facet per quad, to compare.
QUADS = bool(os.environ.get("ISO_REAL_QUADS"))
# How far (in 0-255 levels) a blended gradient may stray from the lit facets.
RAMP = float(os.environ.get("ISO_REAL_RAMP", "1.0"))


def num(v, places):
    s = f"{v:.{places}f}".rstrip("0").rstrip(".")
    if s.startswith("0."):
        s = s[1:]
    elif s.startswith("-0."):
        s = "-" + s[2:]
    return "0" if s in ("", "-0", "-") else s


def short(c):
    """#aabbcc as #abc when it can be."""
    return "#" + c[1] + c[3] + c[5] if c[1] == c[2] and c[3] == c[4] and c[5] == c[6] else c


def tight(pts):
    """A closed polygon as short as path data gets at a tenth of a pixel:
    rounded first, then relative moves (so rounding never adds up)."""
    q = [(round(x * 10), round(y * 10)) for x, y in pts]
    out, cx, cy, last = ["m"], 0, 0, ""
    for k, (x, y) in enumerate(q):
        for v in (x - cx, y - cy):
            t = num(v / 10, 1)
            if last and not t.startswith("-") and not (t.startswith(".") and "." in last):
                out.append(" ")
            out.append(t)
            last = t
        cx, cy = x, y
    out.append("z")
    return "".join(out)


def ramp(stops, tol):
    """Fewest gradient stops whose straight blend stays within `tol` of every
    level of the given ones (Ramer-Douglas-Peucker along the offset)."""
    if len(stops) <= 2:
        return stops
    keep = {0, len(stops) - 1}
    work = [(0, len(stops) - 1)]
    while work:
        a, b = work.pop()
        (oa, ca), (ob, cb) = stops[a], stops[b]
        worst, at = 0.0, None
        for i in range(a + 1, b):
            o, c = stops[i]
            f = 0.0 if ob - oa < 1e-9 else (o - oa) / (ob - oa)
            e = max(abs(c[k] - (ca[k] + (cb[k] - ca[k]) * f)) for k in range(3))
            if e > worst:
                worst, at = e, i
        if at is not None and worst > tol:
            keep.add(at)
            work += [(a, at), (at, b)]
    return [stops[i] for i in sorted(keep)]


def across(d):
    """Unit screen vector perpendicular to `d`."""
    l = math.hypot(*d) or 1.0
    return (-d[1] / l, d[0] / l)


def sub2(a, b):
    return (a[0] - b[0], a[1] - b[1])


def dot2(a, b):
    return a[0] * b[0] + a[1] * b[1]


class Svg:
    def __init__(self):
        self.parts, self.defs = [], []
        self.minx = self.miny = 1e9
        self.maxx = self.maxy = -1e9
        self._id = 0
        self._user = False

    def uid(self, p="g"):
        self._id += 1
        return f"{p}{self._id}"

    def grow(self, pts):
        for x, y in pts:
            self.minx, self.maxx = min(self.minx, x), max(self.maxx, x)
            self.miny, self.maxy = min(self.miny, y), max(self.maxy, y)

    def poly(self, pts, fill, extra="", seam=True):
        self.grow(pts)
        d = tight(pts) if not QUADS else "M" + "L".join(f"{x:.1f},{y:.1f}" for x, y in pts) + "Z"
        st = f' stroke="{fill}" stroke-width="0.35" stroke-linejoin="round"' if seam else ""
        self.parts.append(f'<path d="{d}" fill="{fill}"{st}{extra}/>')

    def shaded(self, pts, origin, axis, stops, hard=False):
        """A polygon filled with a linear gradient running along `axis` (a unit
        screen vector) from `origin`: stops are (distance along axis, rgb). One
        path instead of a strip of flat-lit facets, so no seams and no banding."""
        if len(stops) == 1 or max(stops, key=lambda q: q[0])[0] - min(stops, key=lambda q: q[0])[0] < 0.05:
            self.poly(pts, css(stops[0][1]), seam=False)
            return
        p0 = min(q[0] for q in stops)
        L = max(q[0] for q in stops) - p0
        offs = [((d - p0) / L, c) for d, c in stops]
        offs.sort(key=lambda q: q[0])
        if hard:
            # a run of facets in one colour needs only its two ends
            offs = [q for i, q in enumerate(offs)
                    if not (0 < i < len(offs) - 1 and offs[i - 1][1] == q[1] == offs[i + 1][1])]
        else:
            offs = ramp(offs, RAMP)
        gid = self.uid("s")
        x1, y1 = origin[0] + axis[0] * p0, origin[1] + axis[1] * p0
        x2, y2 = x1 + axis[0] * L, y1 + axis[1] * L
        st = "".join(f'<stop offset="{num(o, 4)}" stop-color="{short(css(c))}"/>' for o, c in offs)
        if not self._user:
            # every shaded gradient is in user space: say so once, inherit it
            self._user = True
            self.defs.append('<linearGradient id="u" gradientUnits="userSpaceOnUse"/>')
        self.defs.append(f'<linearGradient id="{gid}" href="#u" x1="{num(x1, 2)}" y1="{num(y1, 2)}" '
                         f'x2="{num(x2, 2)}" y2="{num(y2, 2)}">{st}</linearGradient>')
        self.poly(pts, f"url(#{gid})", seam=False)

    def raw(self, s):
        self.parts.append(s)

    def out(self, pad=40, bg=None):
        x0, y0 = self.minx - pad, self.miny - pad
        w, h = self.maxx - self.minx + 2 * pad, self.maxy - self.miny + 2 * pad
        b = f'<rect x="{x0:.1f}" y="{y0:.1f}" width="{w:.1f}" height="{h:.1f}" fill="{bg}"/>' if bg else ""
        return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x0:.1f} {y0:.1f} {w:.1f} {h:.1f}" '
                f'width="{w:.0f}" height="{h:.0f}"><defs>{"".join(self.defs)}</defs>{b}{"".join(self.parts)}</svg>')


def matrix(origin, U, V):
    a, b = Pv(U)
    c, d = Pv(V)
    e, f = P(origin)
    return f"matrix({a:.4f},{b:.4f},{c:.4f},{d:.4f},{e:.2f},{f:.2f})"


def fillet(depth, e, steps=20):
    """Real rounded edges of radius e on both ends of the wall: [(w0, w1, in0, in1, deg)].

    The outline shrinks by `in` as the wall curves over, so the silhouette is round,
    not a box with a painted gradient."""
    e = min(e, depth / 2)
    out = []

    def arc(cw, a0, a1):
        for k in range(steps):
            t0 = math.radians(a0 + (a1 - a0) * k / steps)
            t1 = math.radians(a0 + (a1 - a0) * (k + 1) / steps)
            out.append((cw + e * math.sin(t0), cw + e * math.sin(t1),
                        e - e * math.cos(t0), e - e * math.cos(t1),
                        math.degrees((t0 + t1) / 2)))

    arc(e, -90, 0)
    if depth - 2 * e > 1e-6:
        out.append((e, depth - e, 0, 0, 0))
    arc(depth - e, 0, 90)
    return out


def extrude(svg, O, U, V, W, outline, depth, mat, bands=None, cap=None, edge=None, steps=20, knurl=0.0):
    """Prism: outline in the (U, V) plane at O, pushed `depth` along W.

    bands: [(w0, w1, tilt_deg)] split a straight wall; tilt only bends the shading
    normal toward +W or -W (fine for thin chamfers).
    edge: radius of a true rounded edge on both ends (see fillet), overrides bands.
    cap: fill for the visible end cap (default: shaded flat).
    Returns the SVG transform of the visible cap, for drawing details on it.
    """
    if edge:
        walls, real = fillet(depth, edge, steps), True
    else:
        walls, real = [(w0, w1, 0, 0, t) for w0, w1, t in (bands or [(0, depth, 0)])], False
    loc = lambda u, v, w: add(add(add(O, mul(U, u)), mul(V, v)), mul(W, w))
    pt = lambda o, w, d: P(loc(o[0] - o[2] * d, o[1] - o[3] * d, w))
    n = len(outline)
    quads = []
    # Each piece reaches over the next segment and the next band; whichever is drawn
    # later covers the overlap, so adjacent faces never leave an anti-aliased seam.
    for bi, (w0, w1, i0, i1, tilt) in enumerate(walls):
        w1x, i1x = (walls[bi + 1][1], walls[bi + 1][3]) if bi + 1 < len(walls) else (w1, i1)
        t = math.radians(tilt)
        for i in range(n):
            o0, o1, o2 = outline[i], outline[(i + 1) % n], outline[(i + 2) % n]
            ns = norm(add(mul(U, o0[2] + o1[2]), mul(V, o0[3] + o1[3])))
            nn = norm(add(mul(ns, math.cos(t)), mul(W, math.sin(t))))
            if dot(nn if real else ns, CAM) <= 1e-4:
                continue
            pts = [pt(o0, w0, i0), pt(o1, w0, i0), pt(o2, w0, i0),
                   pt(o2, w1x, i1x), pt(o1, w1x, i1x), pt(o0, w1x, i1x)]
            mid = loc((o0[0] + o1[0]) / 2, (o0[1] + o1[1]) / 2, (w0 + w1) / 2)
            fill = mat.shade(nn)
            if knurl and i % 2:  # ribbed edge, like a Digital Crown
                fill = css(mix(hx(fill), (0, 0, 0), knurl))
            quads.append((dot(mid, CAM), pts, fill))
    if QUADS:
        quads.sort(key=lambda q: q[0])
        for _, pts, fill in quads:
            svg.poly(pts, fill, seam=False)
    else:
        _walls_shaded(svg, O, U, V, W, outline, walls, real, mat, knurl, loc, pt)
    top = dot(W, CAM) > 0
    wc = depth if top else 0
    ic = walls[-1][3] if top else walls[0][2]
    capn = W if top else mul(W, -1)
    svg.poly([pt(o, wc, ic) for o in outline], cap or mat.shade(capn), seam=False)
    return matrix(loc(0, 0, wc), U, V)


def _walls_shaded(svg, O, U, V, W, outline, walls, real, mat, knurl, loc, pt):
    """`extrude`'s walls as one gradient-filled column per outline segment
    (all its bands, back to front), runs of identical columns (a straight
    side) merged into one. Band edges are parallel to the segment on screen,
    so a gradient across it puts every band's colour exactly where it was."""
    n = len(outline)
    cols = []
    for i in range(n):
        o0, o1 = outline[i], outline[(i + 1) % n]
        ns = norm(add(mul(U, o0[2] + o1[2]), mul(V, o0[3] + o1[3])))
        bands = []
        for bi, (w0, w1, i0, i1, tilt) in enumerate(walls):
            t = math.radians(tilt)
            nn = norm(add(mul(ns, math.cos(t)), mul(W, math.sin(t))))
            vis = dot(nn if real else ns, CAM) > 1e-4
            fill = mat.shade(nn)
            if knurl and i % 2:
                fill = css(mix(hx(fill), (0, 0, 0), knurl))
            bands.append((bi, vis, hx(fill)))
        # contiguous visible runs of bands
        run = []
        for b in bands + [(None, False, None)]:
            if b[1]:
                run.append(b)
            elif run:
                cols.append((i, ns, tuple(r[0] for r in run), tuple(r[2] for r in run)))
                run = []
    # merge neighbours with the same normal, bands and colours (a straight side)
    merged = []
    for c in cols:
        if merged and not knurl and merged[-1][1] + 1 == c[0] and merged[-1][2] == c[2] and merged[-1][3] == c[3] \
                and dot(merged[-1][4], c[1]) > 1 - 1e-9:
            m = merged[-1]
            merged[-1] = (m[0], c[0], m[2], m[3], m[4])
        else:
            merged.append((c[0], c[0], c[2], c[3], c[1]))
    pieces = []
    for i0_, i1_, bis, colors, ns in merged:
        idx = [(i0_ + k) % n for k in range(i1_ - i0_ + 3)]  # reach one segment past, like the facets
        first, last = bis[0], bis[-1]
        wl = walls[last]
        wtop, itop = (walls[last + 1][1], walls[last + 1][3]) if last + 1 < len(walls) else (wl[1], wl[3])
        ob = outline[idx[0]]
        left = [pt(ob, walls[first][0], walls[first][2])] + [pt(ob, walls[b][1], walls[b][3]) for b in bis[:-1]] + [pt(ob, wtop, itop)]
        oe = outline[idx[-1]]
        idx_pts = [outline[k] for k in idx]
        right = [pt(oe, walls[first][0], walls[first][2])] + [pt(oe, walls[b][1], walls[b][3]) for b in bis[:-1]] + [pt(oe, wtop, itop)]
        bottom = [pt(o, walls[first][0], walls[first][2]) for o in idx_pts]
        top = [pt(o, wtop, itop) for o in idx_pts]
        poly = bottom + right[1:-1][::1] + top[::-1] + left[1:-1][::-1]
        o0, o1 = outline[idx[0]], outline[idx[1]]
        d = sub2(P(loc(o1[0], o1[1], 0)), P(loc(o0[0], o0[1], 0)))
        ax = across(d)
        org = left[0]
        stops = []
        for b, c in zip(bis, colors):
            w0, w1, i0, i1, _ = walls[b]
            m = mul(add(add(pt(ob, w0, i0), (0, 0)), pt(ob, w1, i1)), 0.5)
            stops.append((dot2(sub2(m, org), ax), c))
        if len(stops) > 1 and stops[-1][0] < stops[0][0]:
            ax = (-ax[0], -ax[1])
            stops = [(-q, c) for q, c in stops]
        # depth of the whole run (a merged straight side spans many segments)
        oz = outline[(i1_ + 1) % n]
        mid = loc((o0[0] + oz[0]) / 2, (o0[1] + oz[1]) / 2, (walls[first][0] + wtop) / 2)
        pieces.append((dot(mid, CAM), poly, org, ax, stops, bool(knurl)))
    # Drawn along the outline, from just after a gap: each column reaches a
    # segment over the next to hide the seam, and the next, drawn later,
    # covers that reach (where this gradient no longer fits the bands).
    start = 0
    for k in range(len(merged)):
        if (merged[k - 1][1] + 1) % n != merged[k][0]:
            start = k
            break
    for _, poly, org, ax, stops, hard in pieces[start:] + pieces[:start]:
        svg.shaded(poly, org, ax, stops, hard)


def circle(r, seg=48):
    return [(r * math.cos(a), r * math.sin(a), math.cos(a), math.sin(a))
            for a in (2 * math.pi * k / seg for k in range(seg))]


def ground_shadow(svg, O, U, V, outline, blur=6, alpha=0.55, dy=0.0, spread=0.0):
    fid = svg.uid("sh")
    svg.defs.append(f'<filter id="{fid}" x="-30%" y="-30%" width="160%" height="160%">'
                    f'<feGaussianBlur stdDeviation="{blur}"/></filter>')
    pts = []
    for u, v, a, b in outline:
        p = add(add(O, mul(U, u + a * spread)), mul(V, v + b * spread))
        x, y = P(p)
        pts.append((x, y + dy))
    d = "M" + "L".join(f"{x:.2f},{y:.2f}" for x, y in pts) + "Z"
    svg.grow([(x + sx * blur * 2.5, y + sy * blur * 2.5) for x, y in pts for sx in (-1, 1) for sy in (-1, 1)])
    svg.raw(f'<path d="{d}" fill="#000" opacity="{alpha}" filter="url(#{fid})"/>')


def tube(svg, path3d, r, base, mid, hi, braid=None, shadow=True):
    """Cable: a 3D centre polyline drawn as stacked screen-space strokes."""
    pts = [P(p) for p in path3d]
    svg.grow([(x - r * K, y - r * K) for x, y in pts] + [(x + r * K, y + r * K) for x, y in pts])
    d = "M" + "L".join(f"{x:.2f},{y:.2f}" for x, y in pts)
    w = 2 * r * K
    if shadow:
        fid = svg.uid("cs")
        svg.defs.append(f'<filter id="{fid}" x="-20%" y="-20%" width="140%" height="140%">'
                        f'<feGaussianBlur stdDeviation="{r * K * 0.55:.2f}"/></filter>')
        gp = [P((x, y, 0)) for x, y, _ in path3d]
        gd = "M" + "L".join(f"{x:.2f},{y + r * K * 0.25:.2f}" for x, y in gp)
        svg.raw(f'<path d="{gd}" fill="none" stroke="#000" stroke-width="{w * 1.1:.2f}" '
                f'stroke-linecap="round" opacity="0.6" filter="url(#{fid})"/>')
    lc = 'fill="none" stroke-linecap="butt" stroke-linejoin="round"'
    svg.raw(f'<path d="{d}" {lc} stroke="{base}" stroke-width="{w:.2f}"/>')
    off = lambda k: "M" + "L".join(f"{x:.2f},{y - r * K * k:.2f}" for x, y in pts)
    svg.raw(f'<path d="{off(0.18)}" {lc} stroke="{mid}" stroke-width="{w * 0.62:.2f}"/>')
    if braid:
        svg.raw(f'<path d="{off(0.1)}" {lc} stroke="{braid}" stroke-width="{w * 0.86:.2f}" '
                f'stroke-dasharray="0.9 1.1" opacity="0.32"/>')
        svg.raw(f'<path d="{off(0.1)}" {lc} stroke="#000" stroke-width="{w * 0.86:.2f}" '
                f'stroke-dasharray="0.5 1.5" stroke-dashoffset="1" opacity="0.28"/>')
    svg.raw(f'<path d="{off(0.42)}" {lc} stroke="{hi}" stroke-width="{w * 0.12:.2f}" opacity="0.35"/>')


def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def resample(path, ds):
    """Points every ds along the polyline (by arc length)."""
    cum = [0.0]
    for a, b in zip(path, path[1:]):
        cum.append(cum[-1] + math.dist(a, b))
    out, i, t = [], 0, 0.0
    while t <= cum[-1]:
        while cum[i + 1] < t:
            i += 1
        f = (t - cum[i]) / ((cum[i + 1] - cum[i]) or 1)
        out.append(add(path[i], mul(add(path[i + 1], mul(path[i], -1)), f)))
        t += ds
    return out


def sweep(svg, path, radius, mat, sides=36, ds=0.6, braid=None, N0=None, mesh=None):
    """Real 3D tube: a ring swept along `path` (parallel-transport frame).

    radius(s) and mat(s) take the arc length, so one sweep can be a tapered strain
    relief that turns into cable. braid=(start_s, amount) darkens a diagonal
    over-under checker on the surface from start_s on, like woven nylon.
    radius may return (rN, rB) for an elliptical section (a flat band); N0 fixes
    the starting normal; mesh=(centre, amount) dots the side facing `centre` like a
    knit canopy."""
    pts = resample(path, ds)
    T = [norm(add(pts[min(k + 1, len(pts) - 1)], mul(pts[max(k - 1, 0)], -1))) for k in range(len(pts))]
    N = N0 or (norm(cross(T[0], Z)) if abs(dot(T[0], Z)) < 0.95 else norm(cross(T[0], X)))
    rings, dirs = [], []
    for k, (p, t) in enumerate(zip(pts, T)):
        N = norm(add(N, mul(t, -dot(N, t))))
        B = cross(t, N)
        r = radius(k * ds)
        rn, rb = r if isinstance(r, tuple) else (r, r)
        ring, nd = [], []
        for j in range(sides):
            c_, s_ = math.cos(2 * math.pi * j / sides), math.sin(2 * math.pi * j / sides)
            ring.append(add(p, add(mul(N, c_ * rn), mul(B, s_ * rb))))
            nd.append(norm(add(mul(N, c_ / rn), mul(B, s_ / rb))))
        dirs.append(nd)
        rings.append(ring)
    quads = []
    lerp = lambda a, b, f: add(a, mul(add(b, mul(a, -1)), f))
    for k in range(len(pts) - 1):
        k2 = min(k + 2, len(pts) - 1)
        m = mat(k * ds)
        for j in range(sides):
            j1, j2 = (j + 1) % sides, (j + 2) % sides
            n = norm(add(add(dirs[k][j], dirs[k][j1]), add(dirs[k + 1][j], dirs[k + 1][j1])))
            if dot(n, CAM) <= 1e-4:
                continue
            # reach 40% into the next cell both ways so neighbours never leave a seam
            a0, a1 = rings[k][j], lerp(rings[k][j1], rings[k][j2], 0.4)
            b0 = lerp(rings[k + 1][j], rings[k2][j], 0.4)
            b1 = lerp(lerp(rings[k + 1][j1], rings[k2][j1], 0.4), lerp(rings[k + 1][j2], rings[k2][j2], 0.4), 0.4)
            c = hx(m.shade(n))
            if mesh and dot(n, add(mesh[0], mul(pts[k], -1))) > 0 and (k + j) % 2:
                c = mix(c, (0, 0, 0), mesh[1])
            if braid and k * ds >= braid[0]:
                # basket weave: P×P cells alternate which helix is on top; inside a
                # cell the top strand reads as diagonal ribs leaning its own way
                Pc = 3
                lean = (k + j) if (k // Pc + j // Pc) % 2 else (k - j)
                c = mix(c, (0, 0, 0), braid[1] * (lean % Pc) / (Pc - 1))
            mid = mul(add(rings[k][j], rings[k + 1][j1]), 0.5)
            quads.append((dot(mid, CAM), [P(a0), P(a1), P(b1), P(b0)], css(c), k, j))
    if not QUADS:
        quads = _rings_shaded(quads, rings, pts, sides, bool(braid or mesh))
    for end, k, sgn in ((0, 0, -1), (1, len(pts) - 1, 1)):
        n = mul(T[k], sgn)
        if dot(n, CAM) > 0:
            quads.append((dot(pts[k], CAM) + 9, [P(q) for q in rings[k]], mat(k * ds).shade(n)))
    quads.sort(key=lambda q: q[0])
    for q in quads:
        if len(q) == 4:  # a shaded ring: (depth, poly, None, (origin, axis, stops, hard))
            org, ax, stops, hard = q[3]
            svg.shaded(q[1], org, ax, stops, hard)
        else:
            svg.poly(q[1], q[2], seam=False)
    return pts


def _rings_shaded(quads, rings, pts, sides, hard):
    """`sweep`'s facets as one gradient-filled band per ring step: the facets
    of a step meet along lines parallel to the tube's axis on screen, so a
    gradient across the axis puts each one's colour where it was. Steps along
    a straight run with the same colours merge into one band."""
    by_k = {}
    for q in quads:
        by_k.setdefault(q[3], []).append(q)
    bands = []
    for k in sorted(by_k):
        fac = sorted(by_k[k], key=lambda q: q[4])
        # visible facets of a step: one run around the ring (split if not)
        js = [q[4] for q in fac]
        runs, cur = [], [fac[0]]
        for a, b in zip(fac, fac[1:]):
            if b[4] == (a[4] + 1) % sides:
                cur.append(b)
            else:
                runs.append(cur)
                cur = [b]
        runs.append(cur)
        if len(runs) > 1 and runs[0][0][4] == 0 and runs[-1][-1][4] == sides - 1:
            runs[0] = runs.pop() + runs[0]
        for run in runs:
            bands.append((k, run))
    out = []
    for k, run in bands:
        axis2 = sub2(P(pts[k + 1]), P(pts[k]))
        if math.hypot(*axis2) < 1e-6:
            out += [q[:3] for q in run]
            continue
        ax = across(axis2)
        # outline: the run's facets' near edges (a0 a1) then far edges (b1 b0) back
        near = [run[0][1][0]] + [q[1][1] for q in run]
        far = [run[0][1][3]] + [q[1][2] for q in run]
        poly = near + far[::-1]
        org = near[0]
        stops = []
        for q in run:
            c = hx(q[2])
            # the facet's true edges on the ring (its polygon reaches past them)
            a0, a1 = P(rings[k][q[4]]), P(rings[k][(q[4] + 1) % sides])
            if hard:
                stops += [(dot2(sub2(a0, org), ax), c), (dot2(sub2(a1, org), ax), c)]
            else:
                stops.append((dot2(sub2(((a0[0] + a1[0]) / 2, (a0[1] + a1[1]) / 2), org), ax), c))
        if stops[-1][0] < stops[0][0]:
            ax = (-ax[0], -ax[1])
            stops = [(-d, c) for d, c in stops]
        # not monotonic (the band folds over itself on screen): keep the facets
        if any(b[0] < a[0] - 0.05 for a, b in zip(stops, stops[1:])):
            out += [q[:3] for q in run]
            continue
        depth = sum(q[0] for q in run) / len(run)
        out.append((depth, poly, None, (org, ax, stops, hard)))
    return out


def cable_shadow(svg, path, r):
    fid = svg.uid("cs")
    svg.defs.append(f'<filter id="{fid}" x="-20%" y="-20%" width="140%" height="140%">'
                    f'<feGaussianBlur stdDeviation="{r * K * 0.55:.2f}"/></filter>')
    gp = [P((x, y, 0)) for x, y, _ in path]
    gd = "M" + "L".join(f"{x:.2f},{y + r * K * 0.25:.2f}" for x, y in gp)
    svg.raw(f'<path d="{gd}" fill="none" stroke="#000" stroke-width="{2.2 * r * K:.2f}" '
            f'stroke-linecap="round" opacity="0.6" filter="url(#{fid})"/>')


def bezier(p0, p1, p2, p3, n=48):
    out = []
    for i in range(n + 1):
        t = i / n
        a, b, c, d = (1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t ** 2, t ** 3
        out.append(tuple(a * p0[k] + b * p1[k] + c * p2[k] + d * p3[k] for k in range(3)))
    return out


# --- materials ----------------------------------------------------------------
TITANIUM = Mat("#1d1d20", "#6d6e75", ks=0.35, shin=18)
PLASTIC = Mat("#222225", "#5b5c62", ks=0.12, shin=10)
WHITEMETAL = Mat("#5f6167", "#f7f7f9", ks=0.55, shin=26, gamma=1.8)
GLASS = Mat("#0c0c0e", "#3a3b40", ks=0.6, shin=40)
PLUG = Mat("#161618", "#4b4c51", ks=0.3, shin=12)
NYLON = Mat("#111113", "#47484d", ks=0.18, shin=8)
ALU = Mat("#55575d", "#d9dade", ks=0.45, shin=20)

X, Y, Z = (1, 0, 0), (0, 1, 0), (0, 0, 1)
NX, NY, NZ = (-1, 0, 0), (0, -1, 0), (0, 0, -1)

# Rounded side profile: chamfer catches light on top, rolls into shadow at the base.
def pill(h, steps=18):
    return [(h * i / steps, h * (i + 1) / steps, -70 + 140 * (i + 0.5) / steps) for i in range(steps)]


def flat_side(h, ch=0.7):
    return [(0, ch, -45), (ch, h - ch, 0), (h - ch, h, 50)]


def grille(cx, cz, n, step, lip, hole, r=0.62, side=1):
    """A row of drilled holes in face coords (page-down = -z). Each hole is dark
    with a thin lit crescent on its lower inner wall and a soft shadow up top, so
    it reads as a hole cut into the metal rather than a painted dot."""
    out = []
    for i in range(n):
        x = cx + side * i * step
        out.append(f'<circle cx="{x:.2f}" cy="{cz + 0.1 * r:.2f}" r="{r * 1.06:.2f}" fill="{lip}"/>'
                   f'<circle cx="{x:.2f}" cy="{cz - 0.12 * r:.2f}" r="{r * 0.96:.2f}" fill="{hole}"/>')
    return "".join(out)


def usb_port(cx, cz, rim, metal, metal_dark, hole):
    """USB-C receptacle in face coords (page-down = -z): chamfered opening, the
    metal shell lining it, and the centre tongue the plug slides over. The dark
    cavity sits a bit high so the lower lip of the shell shows, as seen from above."""
    st = lambda w, h, dy, fill: (f'<rect x="{cx - w / 2:.2f}" y="{cz - h / 2 + dy:.2f}" width="{w}" '
                                 f'height="{h}" rx="{h / 2:.2f}" fill="{fill}"/>')
    return (st(9.2, 3.3, 0, rim) + st(8.5, 2.7, 0, metal) + st(8.0, 2.3, -0.1, hole)
            + f'<rect x="{cx - 2.7:.2f}" y="{cz - 0.27:.2f}" width="5.4" height="0.5" rx="0.25" fill="{metal_dark}"/>')


# --- study 01: iPhone 15 Pro face up + USB-C plug ---------------------------
def phone(svg):
    Wp, Lp, Hp, Rp = 71.6, 147.6, 8.25, 11.5
    # The projection is mirror-handed (x lower-right, y lower-left), so a face reads
    # right way round with page-right = +x / page-down = +y on top.
    O = (0, 0, 0)
    out = rrect(Wp, Lp, Rp, seg=40, smooth=True)
    ground_shadow(svg, O, X, Y, out, blur=10, alpha=0.6, dy=6, spread=1.5)
    ground_shadow(svg, O, X, Y, out, blur=2, alpha=0.8, dy=1.5)
    g = svg.uid("frm")
    svg.defs.append(f'<linearGradient id="{g}" x1="0" y1="0" x2="1" y2="1">'
                    f'<stop offset="0" stop-color="#6b6c72"/><stop offset="1" stop-color="#55565c"/></linearGradient>')
    m = extrude(svg, O, X, Y, Z, out, Hp, TITANIUM, edge=1.4, steps=12, cap=f"url(#{g})")
    glass = svg.uid("gl")
    sheen = svg.uid("sn")
    svg.defs.append(f'<linearGradient id="{glass}" x1="0" y1="0" x2="0" y2="1">'
                    f'<stop offset="0" stop-color="#232427"/><stop offset="1" stop-color="#1b1c1f"/></linearGradient>'
                    f'<linearGradient id="{sheen}" x1="0" y1="0" x2="1" y2="0.55">'
                    f'<stop offset="0.3" stop-color="#fff" stop-opacity="0"/>'
                    f'<stop offset="0.48" stop-color="#fff" stop-opacity="0.07"/>'
                    f'<stop offset="0.62" stop-color="#fff" stop-opacity="0"/></linearGradient>')
    b = 1.75  # glass starts just inside the rounded rim of the frame
    s = 3.1  # thin border of the display
    tile = "#2e2f33"
    ui = (
        f'<path d="{inset_path(out, b)}" fill="#101012"/>'
        f'<path d="{inset_path(out, s)}" fill="url(#{glass})"/>'
        f'<rect x="{Wp / 2 - 11}" y="{s + 4.5}" width="22" height="6.6" rx="3.3" fill="#020203"/>'
        f'<circle cx="{Wp / 2 + 7.2}" cy="{s + 7.8}" r="1.5" fill="#101420"/>'
        f'<rect x="9" y="27" width="25.5" height="25.5" rx="5" fill="{tile}"/>'
        f'<rect x="37.1" y="27" width="25.5" height="25.5" rx="5" fill="{tile}"/>'
        f'<circle cx="15" cy="62.5" r="6" fill="{tile}"/>'
        f'<circle cx="29.6" cy="62.5" r="6" fill="#f3f3f5"/>'
        f'<rect x="26.5" y="60.2" width="5.6" height="4.4" rx="0.9" fill="none" stroke="#3a3a3c" stroke-width="0.7"/>'
        f'<rect x="25.2" y="59" width="5.6" height="4.4" rx="0.9" fill="none" stroke="#9a9a9e" stroke-width="0.6"/>'
        f'<rect x="37.1" y="56.5" width="11.8" height="25.5" rx="5.9" fill="{tile}"/>'
        f'<rect x="50.8" y="56.5" width="11.8" height="25.5" rx="5.9" fill="{tile}"/>'
        f'<rect x="9" y="72" width="25.5" height="11.8" rx="5" fill="{tile}"/>'
        f'<path d="{inset_path(out, s)}" fill="url(#{sheen})"/>'
    )
    svg.raw(f'<g transform="{m}">{ui}</g>')
    # bottom face (+y): page-right = +x, page-down = -z
    mb = matrix((0, Lp, Hp), X, NZ)
    cz = Hp / 2
    holes = (grille(Wp / 2 - 8.6, cz, 4, 2.0, "#6c6d73", "#121214", side=-1)
             + grille(Wp / 2 + 8.6, cz, 7, 2.0, "#6c6d73", "#121214"))
    port = usb_port(Wp / 2, cz, "#2a2a2e", "#5a5c62", "#3a3c41", "#0d0d0f")
    svg.raw(f'<g transform="{mb}">{holes}{port}</g>')
    return Wp, Lp, Hp


def plug(svg, cx, y0, cz):
    """USB-C plug lying flat, tip toward -y, tip starting at y0."""
    tw, tl, th = 8.3, 6.6, 2.5     # metal shell: stadium section
    bw, bl, bh = 12.4, 21.0, 6.6   # overmold
    oy_body = y0 + tl - 0.4
    oy_neck = oy_body + bl - 1.5
    fb = rrect(bw, bl, 3.6)
    ground_shadow(svg, (cx - bw / 2, oy_body, 0), X, Y, fb, blur=5, alpha=0.65, dy=3)
    extrude(svg, (cx - tw / 2, y0, cz - th / 2), X, Y, Z, rrect(tw, tl + 0.6, 1.2), th,
            WHITEMETAL, edge=th / 2 * 0.95)
    mb = extrude(svg, (cx - bw / 2, oy_body, cz - bh / 2), X, Y, Z, fb, bh, PLUG, edge=2.6)
    k = 3.1
    bolt = "M7.4,7.3 L4.6,11.3 L6.6,11.3 L5.1,14.7 L8.1,10.4 L6.1,10.4 Z"
    svg.raw(f'<g transform="{mb}"><rect x="{k}" y="{k}" width="{bw - 2 * k}" height="{bl - 2 * k}" rx="1.4" '
            f'fill="none" stroke="#535459" stroke-width="0.3" opacity="0.8"/>'
            f'<g transform="translate(-0.15 -0.5)"><path d="{bolt}" fill="#2a2a2d" transform="translate(0.22 0.28)"/>'
            f'<path d="{bolt}" fill="#5a5b60"/></g></g>')
    return oy_body + bl


def study_phone():
    svg = Svg()
    Wp, Lp, Hp = phone(svg)
    cx, cz, r = Wp / 2, Hp / 2, 2.2
    end = plug(svg, cx, Lp + 9.0, cz)
    y = end - 0.3
    path = [(cx, y, cz), (cx, y + 11, cz)]
    path += bezier((cx, y + 11, cz), (cx, y + 22, cz), (cx - 2, y + 32, r), (cx + 18, y + 58, r))[1:]
    path += bezier((cx + 18, y + 58, r), (cx + 30, y + 76, r), (cx + 70, y + 70, r), (cx + 96, y + 96, r))[1:]
    cable_shadow(svg, path[12:], r)

    def radius(s):  # strain relief: 3.0 at the body, eases down to the cable
        if s < 2.5:
            return 3.0
        if s < 10.5:
            f = (s - 2.5) / 8
            return 3.0 - 0.8 * (f * f * (3 - 2 * f))
        return r

    sweep(svg, path, radius, lambda s: PLUG if s < 10.5 else NYLON, braid=(10.5, 0.28))
    return svg


# --- study 03: the reference's flat palette, in iso ------------------------------
# Sampled from the reference: no metal, no specular, two or three greys per object.
F_PAGE, F_CARD, F_RING, F_SIDE, F_TILE, F_HOLE = "#18191b", "#232426", "#66676c", "#444444", "#303133", "#1f1f1f"
F_WALL = Mat("#404042", "#48484a", ks=0)
F_PLUG = RimMat(F_SIDE, "#6a6b70", width=0.45)
F_TIP = Mat("#d9d9dc", "#d9d9dc", ks=0)


AIRPLAY = "M3.412 1.32c-1.178 0-1.584.122-2.031.366A2.45 2.45 0 0 0 .365 2.7C.122 3.148 0 3.553 0 4.771v9.137c0 1.178.122 1.585.365 2.031c.244.447.57.772 1.016 1.016c.406.244.813.365 2.031.365h2.72l1.138-1.34H3.006a1.8 1.8 0 0 1-1.016-.164c-.203-.08-.366-.283-.488-.486c-.122-.325-.203-.65-.162-1.016V4.406c-.04-.325 0-.69.162-1.015c.081-.203.285-.365.488-.487c.325-.122.65-.204 1.016-.164h17.867c.325-.04.69.002 1.016.164c.203.082.364.284.486.487c.122.324.203.65.162 1.015v9.95c.04.324 0 .69-.162 1.015c-.081.203-.283.365-.486.486c-.325.122-.65.203-1.016.163h-4.264l1.137 1.341l2.803-.04c1.218 0 1.623-.122 2.07-.366a2.45 2.45 0 0 0 1.016-1.016c.243-.406.365-.813.365-2.03V4.77c0-1.218-.122-1.623-.365-2.07a2.45 2.45 0 0 0-1.016-1.015c-.447-.244-.852-.366-2.07-.366H3.412zm8.451 12.198a.5.5 0 0 0-.37.187l-7.106 8.162a.47.47 0 0 0-.123.326a.47.47 0 0 0 .488.487h14.293c.122 0 .245-.04.326-.121c.203-.163.204-.489.041-.692l-7.107-8.162l-.041-.04a.6.6 0 0 0-.4-.147z"


def phone_flat(svg, O=(0, 0, 0), U=X, V=Y, screen=True):
    """Face up at O; U = the phone's right, V = from its top toward the port."""
    Wp, Lp, Hp, Rp = 71.6, 147.6, 8.25, 11.5
    out = rrect(Wp, Lp, Rp, seg=40, smooth=True)
    ground_shadow(svg, O, U, V, out, blur=12, alpha=0.35, dy=6, spread=1.5)
    m = extrude(svg, O, U, V, Z, out, Hp, F_WALL, cap=F_RING)
    b = 3.0  # the light ring of the reference, then the screen
    u = lambda x: 3.2 + (x - 783) * 0.1096  # reference px -> mm on the screen
    v = lambda y: 3.2 + (y - 215) * 0.1096
    if not screen:  # asleep: dark glass and the island
        ui = (f'<path d="{inset_path(out, b)}" fill="#1c1d1f"/>'
              f'<rect x="{u(985):.2f}" y="{v(250):.2f}" width="{u(1173) - u(985):.2f}" height="{v(312) - v(250):.2f}" rx="3.4" fill="#111113"/>')
    else:
      ui = (
        f'<path d="{inset_path(out, b)}" fill="{F_CARD}"/>'
        f'<rect x="{u(985):.2f}" y="{v(250):.2f}" width="{u(1173) - u(985):.2f}" height="{v(312) - v(250):.2f}" rx="3.4" fill="{F_RING}"/>'
        f'<rect x="{u(820):.2f}" y="{v(372):.2f}" width="{u(1065) - u(820):.2f}" height="{v(617) - v(372):.2f}" rx="4.9" fill="{F_TILE}"/>'
        f'<rect x="{u(1093):.2f}" y="{v(372):.2f}" width="{u(1338) - u(1093):.2f}" height="{v(617) - v(372):.2f}" rx="4.9" fill="{F_TILE}"/>'
        f'<circle cx="{u(876):.2f}" cy="{v(699):.2f}" r="6.1" fill="{F_TILE}"/>'
        f'<rect x="{u(1093):.2f}" y="{v(643):.2f}" width="{u(1205) - u(1093):.2f}" height="27" rx="6.1" fill="{F_TILE}"/>'
        f'<rect x="{u(1226):.2f}" y="{v(643):.2f}" width="{u(1338) - u(1226):.2f}" height="27" rx="6.1" fill="{F_TILE}"/>'
        f'<rect x="{u(820):.2f}" y="{v(783):.2f}" width="{u(1065) - u(820):.2f}" height="12.3" rx="6.1" fill="{F_TILE}"/>'
        f'<circle cx="{u(1009):.2f}" cy="{v(699):.2f}" r="6.1" fill="#fefefe"/>'
        # Apple's AirPlay video glyph (screen + triangle), 24-unit box, from simple-icons
        f'<path d="{AIRPLAY}" fill="#3a3a3c" transform="translate({u(1009) - 3.6:.2f} {v(699) - 3.62:.2f}) scale(0.30)"/>'
    )
    svg.raw(f'<g transform="{m}">{ui}</g>')
    if dot(V, CAM) <= 0:
        return Wp, Lp, Hp
    mb = matrix(add(O, add(mul(V, Lp), mul(Z, Hp))), U, NZ)
    cz = Hp / 2
    holes = (grille(Wp / 2 - 8.6, cz, 4, 2.0, "#5d5e63", F_HOLE, side=-1)
             + grille(Wp / 2 + 8.6, cz, 7, 2.0, "#5d5e63", F_HOLE))
    port = usb_port(Wp / 2, cz, "#4c4c4f", "#76777c", "#4a4b4f", F_HOLE)
    svg.raw(f'<g transform="{mb}">{holes}{port}</g>')
    return Wp, Lp, Hp


def plug_flat(svg, cx, y0, cz):
    tw, tl, th = 8.3, 6.6, 2.5
    bw, bl, bh = 12.4, 21.0, 6.6
    oy_body = y0 + tl - 0.4
    fb = rrect(bw, bl, 3.6)
    ground_shadow(svg, (cx - bw / 2, oy_body, 0), X, Y, fb, blur=5, alpha=0.35, dy=3)
    mt = extrude(svg, (cx - tw / 2, y0, cz - th / 2), X, Y, Z, rrect(tw, tl + 0.6, 1.2), th,
                 F_TIP, edge=th / 2 * 0.95, steps=8, cap="#fefefe")
    mb = extrude(svg, (cx - bw / 2, oy_body, cz - bh / 2), X, Y, Z, fb, bh, Mat("#5d5e63", "#636469", ks=0),
                 edge=2.2, steps=10, cap=F_SIDE)
    bolt = "M7.4,7.3 L4.6,11.3 L6.6,11.3 L5.1,14.7 L8.1,10.4 L6.1,10.4 Z"
    svg.raw(f'<g transform="{mb}"><path d="{bolt}" fill="{F_RING}" transform="translate(-0.15 -0.5)"/></g>')
    return oy_body + bl


def study_flat():
    svg = Svg()
    Wp, Lp, Hp = phone_flat(svg)
    cx, cz, r = Wp / 2, Hp / 2, 2.2
    y = plug_flat(svg, cx, Lp + 9.0, cz) - 0.3
    path = [(cx, y, cz), (cx, y + 11, cz)]
    # leaves the plug along its axis, then bends to fall straight down the screen (+x+y)
    path += bezier((cx, y + 11, cz), (cx, y + 20, cz), (cx + 3, y + 27, r), (cx + 12, y + 36, r))[1:]
    path += bezier((cx + 12, y + 36, r), (cx + 30, y + 54, r), (cx + 50, y + 70, r), (cx + 80, y + 100, r))[1:]
    cable_shadow(svg, [p for p in path if p[2] < r + 0.3], r)

    def radius(s):
        if s < 2.5:
            return 3.0
        if s < 10.5:
            f = (s - 2.5) / 8
            return 3.0 - 0.8 * (f * f * (3 - 2 * f))
        return r

    sweep(svg, path, radius, lambda s: F_PLUG, sides=28, ds=1.0)
    return svg


# --- study 04: MacBook Pro with PROLED XDR (Oct 2026), same flat palette ---------
# Thin bezels and the Dynamic Island replacing the notch (Apple newsroom, 28-oct-2026).
def keyboard(u):
    """Key rects (x, y, w, h) in mm for a MacBook layout, u = key pitch."""
    g, keys, y = 1.6, [], 0.0
    fn_h = 0.55 * u
    fw = 14.5 * u / 14
    keys += [(i * fw, y, fw - g, fn_h - g) for i in range(14)]
    y += fn_h
    rows = [[1] * 13 + [1.5], [1.5] + [1] * 13, [1.8] + [1] * 11 + [1.7],
            [2.35] + [1] * 10 + [2.15], [1, 1, 1, 1.25, 5, 1.25, 1]]
    for r in rows:
        x = 0.0
        for w in r:
            keys.append((x * u, y, w * u - g, u - g))
            x += w
        if r is rows[-1]:  # inverted-T arrows
            ax = x * u
            keys += [(ax, y + u / 2, u - g, u / 2 - g / 2), (ax + u, y, u - g, u / 2 - g / 2),
                     (ax + u, y + u / 2, u - g, u / 2 - g / 2), (ax + 2 * u, y + u / 2, u - g, u / 2 - g / 2)]
        y += u
    return keys, y


def deck(svg, W, deck_c, well, key, pad):
    """Keyboard and trackpad in deck coords (v = 0 at the hinge). No side speaker
    grilles: the PROLED deck beside the keys is plain in Apple's photos."""
    u = 18.6
    keys, kh = keyboard(u)
    kw = 14.5 * u
    kx, ky = (W - kw) / 2, 13.0
    dk = "".join(f'<rect x="{kx + x:.2f}" y="{ky + y:.2f}" width="{w:.2f}" height="{h:.2f}" rx="1.6" fill="{key}"/>'
                 for x, y, w, h in keys)
    pw, ph = 150.0, 80.0
    py = ky + kh + 10
    return (
        f'<rect x="{kx - 2.2:.2f}" y="{ky - 2.2:.2f}" width="{kw + 2.8:.2f}" height="{kh + 2.8:.2f}" rx="3.2" fill="{well}"/>'
        + dk
        + f'<rect x="{(W - pw) / 2:.2f}" y="{py:.2f}" width="{pw}" height="{ph}" rx="6" fill="{pad}"/>'
    )


def study_macbook():
    svg = Svg()
    W, D, H = 312.6, 221.2, 8.6          # base, 14-inch
    LH, LT, tilt = 214.0, 4.2, 18.0      # lid height, thickness, lean back from vertical
    deck_c, well, key, pad = "#4a4b4f", "#2a2b2e", "#1d1e20", "#505156"
    out = rrect(W, D, 9, seg=24, smooth=True)
    ground_shadow(svg, (0, 0, 0), X, Y, out, blur=16, alpha=0.4, dy=10, spread=2)
    m = extrude(svg, (0, 0, 0), X, Y, Z, out, H, F_WALL, cap=deck_c)
    svg.raw(f'<g transform="{m}">{deck(svg, W, deck_c, well, key, pad)}</g>')
    # front edge: the thumb scoop; right side, back to front: HDMI, Thunderbolt 5, SDXC
    # (Apple's PROLED specs; spacing from the newsroom photos)
    mf = matrix((0, D, H), X, NZ)
    svg.raw(f'<g transform="{mf}"><path d="M{W / 2 - 34},0 C{W / 2 - 20},0 {W / 2 - 18},2.1 {W / 2},2.1 '
            f'C{W / 2 + 18},2.1 {W / 2 + 20},0 {W / 2 + 34},0 Z" fill="#404043"/></g>')
    ms = matrix((W, 0, H), Y, NZ)
    port = lambda y0, w, h, rx: (f'<rect x="{y0 - 0.5:.2f}" y="{H / 2 - h / 2 - 0.5:.2f}" width="{w + 1}" height="{h + 1}" rx="{rx + 0.5}" fill="#4c4c4f"/>'
                                 f'<rect x="{y0:.2f}" y="{H / 2 - h / 2:.2f}" width="{w}" height="{h}" rx="{rx}" fill="{F_HOLE}"/>')
    svg.raw(f'<g transform="{ms}">{port(34, 14.5, 4.6, 0.8)}{port(56, 8.6, 2.8, 1.4)}{port(76, 24, 1.6, 0.6)}</g>')

    # lid: front face is the glass; page-right = +x, page-down runs down the lid to the hinge
    t = math.radians(tilt)
    up = (0, -math.sin(t), math.cos(t))
    Vd = mul(up, -1)
    nrm = (0, math.cos(t), math.sin(t))
    hinge = (0, 5.0, H + 0.4)
    O = add(hinge, mul(up, LH))
    lo = rrect(W, LH, 9, seg=24, smooth=True)
    ml = extrude(svg, O, X, Vd, mul(nrm, -1), lo, LT, F_WALL, cap="#66676c")
    rim, bz_s, bz_t, bz_b = 1.1, 4.6, 4.6, 4.6  # PROLED: even bezels, no thicker chin
    sx, sy, sw, sh = bz_s, bz_t, W - 2 * bz_s, LH - bz_t - bz_b
    cx = W / 2
    win, bar, dot = "#2c2d30", "#333438", "#4b4c50"
    iw, ih = 66.0, 33.0
    screen = (
        f'<path d="{inset_path(lo, rim)}" fill="#141516"/>'
        f'<rect x="{sx}" y="{sy}" width="{sw}" height="{sh}" rx="7" fill="#232426"/>'
        # menu bar
        f'<rect x="{sx}" y="{sy}" width="{sw}" height="6.4" rx="0" fill="#26272a"/>'
        f'<circle cx="{sx + 7}" cy="{sy + 3.2}" r="1.3" fill="#8e8e93"/>'
        + "".join(f'<rect x="{sx + 12 + i * 11}" y="{sy + 2.5}" width="{7 if i else 9}" height="1.4" rx="0.7" fill="#5a5b60"/>' for i in range(5))
        + "".join(f'<rect x="{sx + sw - 9 - i * 8}" y="{sy + 2.3}" width="4.6" height="1.8" rx="0.6" fill="#5a5b60"/>' for i in range(4))
        # windows
        + f'<rect x="{sx + 14}" y="{sy + 44}" width="128" height="98" rx="4" fill="{win}"/>'
        f'<rect x="{sx + 14}" y="{sy + 44}" width="128" height="9" rx="4" fill="{bar}"/>'
        + "".join(f'<circle cx="{sx + 20 + i * 4.2}" cy="{sy + 48.5}" r="1.3" fill="{dot}"/>' for i in range(3))
        + "".join(f'<rect x="{sx + 22}" y="{sy + 62 + i * 9}" width="{[92, 70, 84, 56, 76, 40, 64, 50][i]}" height="3" rx="1.5" fill="#3a3b3f"/>' for i in range(8))
        + f'<rect x="{sx + 156}" y="{sy + 28}" width="122" height="86" rx="4" fill="{win}"/>'
        f'<rect x="{sx + 156}" y="{sy + 28}" width="122" height="9" rx="4" fill="{bar}"/>'
        + "".join(f'<circle cx="{sx + 162 + i * 4.2}" cy="{sy + 32.5}" r="1.3" fill="{dot}"/>' for i in range(3))
        + f'<rect x="{sx + 164}" y="{sy + 43}" width="106" height="62" rx="2.5" fill="#303133"/>'
        # dock
        # 9 icons centred on the screen, the white one in the middle, 2 mm padding all round
        + f'<rect x="{cx - 4.5 * 10.6 - 0.6:.2f}" y="{sy + sh - 15}" width="{9 * 10.6 + 1.2:.2f}" height="12" rx="4.5" fill="{win}"/>'
        + "".join(f'<rect x="{cx + (i - 4) * 10.6 - 4:.2f}" y="{sy + sh - 13}" width="8" height="8" rx="2" fill="{"#fefefe" if i == 4 else "#3a3b3f"}"/>' for i in range(9))
        # Dynamic Island, expanded with a Live Activity (now playing)
        + f'<rect x="{cx - iw / 2}" y="{sy + 1.2}" width="{iw}" height="{ih}" rx="8" fill="#0c0c0d"/>'
        f'<rect x="{cx - iw / 2 + 5}" y="{sy + 6}" width="11" height="11" rx="2.2" fill="#66676c"/>'
        f'<rect x="{cx - iw / 2 + 19}" y="{sy + 7.6}" width="26" height="2.6" rx="1.3" fill="#e6e6e8"/>'
        f'<rect x="{cx - iw / 2 + 19}" y="{sy + 12.2}" width="17" height="2.2" rx="1.1" fill="#6e6f74"/>'
        f'<rect x="{cx - iw / 2 + 5}" y="{sy + 21}" width="{iw - 10}" height="1.2" rx="0.6" fill="#3a3b3f"/>'
        f'<rect x="{cx - iw / 2 + 5}" y="{sy + 21}" width="{(iw - 10) * 0.62:.1f}" height="1.2" rx="0.6" fill="#b4b5ba"/>'
        f'<path d="M{cx - 11},{sy + 25.2} l-3,2 l3,2 Z M{cx - 14},{sy + 25.2} l-3,2 l3,2 Z" fill="#e6e6e8"/>'
        f'<rect x="{cx - 2.2}" y="{sy + 24.9}" width="1.5" height="4.6" rx="0.4" fill="#fefefe"/>'
        f'<rect x="{cx + 0.7}" y="{sy + 24.9}" width="1.5" height="4.6" rx="0.4" fill="#fefefe"/>'
        f'<path d="M{cx + 11},{sy + 25.2} l3,2 l-3,2 Z M{cx + 14},{sy + 25.2} l3,2 l-3,2 Z" fill="#e6e6e8"/>'
    )
    svg.raw(f'<g transform="{ml}">{screen}</g>')
    return svg


APPLE = "M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04c-2.04.027-3.91 1.183-4.961 3.014c-2.117 3.675-.546 9.103 1.519 12.09c1.013 1.454 2.208 3.09 3.792 3.039c1.52-.065 2.09-.987 3.935-.987c1.831 0 2.35.987 3.96.948c1.637-.026 2.676-1.48 3.676-2.948c1.156-1.688 1.636-3.325 1.662-3.415c-.039-.013-3.182-1.221-3.22-4.857c-.026-3.04 2.48-4.494 2.597-4.559c-1.429-2.09-3.623-2.324-4.39-2.376c-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83c-1.207.052-2.662.805-3.532 1.818c-.78.896-1.454 2.338-1.273 3.714c1.338.104 2.715-.688 3.559-1.701"  # simple-icons, 24-unit box


def study_macbook_back(turn=30.0):
    """The open MacBook Pro seen from behind: the lid's back faces the camera and its
    Apple logo glows again (Oct 2026), lighting up and breathing on a loop. CSS
    animation inside the SVG, so it plays in an <img> with no script.

    turn: degrees the laptop is rotated on the table toward the camera, so the lid's
    back (and the logo) is not seen edge-on."""
    svg = Svg()
    W, D, H = 312.6, 221.2, 8.6
    LH, LT, tilt = 214.0, 4.2, 14.0
    a = math.radians(turn)
    F = (math.sin(a), math.cos(a), 0.0)     # from the user toward the hinge
    R = cross(F, Z)                         # the user's right, along the width
    O0 = mul(R, -W / 2)                     # centre the base on the origin's x
    out = rrect(W, D, 9, seg=24, smooth=True)
    ground_shadow(svg, O0, R, F, out, blur=16, alpha=0.4, dy=10, spread=2)
    extrude(svg, O0, R, F, Z, out, H, F_WALL, cap="#4a4b4f")
    # the user sits on -F, so the deck reads from the far corner: hinge at v = 0
    md = matrix(add(add(O0, mul(R, W)), add(mul(F, D), mul(Z, H))), mul(R, -1), mul(F, -1))
    svg.raw(f'<g transform="{md}">{deck(svg, W, "#4a4b4f", "#2a2b2e", "#1d1e20", "#505156")}</g>')
    t = math.radians(tilt)
    up = add(mul(F, math.sin(t)), mul(Z, math.cos(t)))   # leans away from the user
    n = add(mul(F, math.cos(t)), mul(Z, -math.sin(t)))   # back of the lid
    O = add(add(O0, add(mul(F, D - 2.0), mul(Z, H + 0.4))), mul(up, LH))
    lo = rrect(W, LH, 9, seg=24, smooth=True)
    g = svg.uid("lid")
    svg.defs.append(f'<linearGradient id="{g}" x1="0" y1="0" x2="1" y2="1">'
                    f'<stop offset="0" stop-color="#4b4c50"/><stop offset="1" stop-color="#424347"/></linearGradient>')
    m = extrude(svg, O, R, mul(up, -1), mul(n, -1), lo, LT, F_WALL, cap=f"url(#{g})")
    sc = 34 / 24
    cx, cy = W / 2, LH / 2 - 4
    tr = f"translate({cx - 12 * sc:.2f} {cy - 12 * sc:.2f}) scale({sc:.4f})"
    bl, sp = svg.uid("bloom"), svg.uid("spill")
    svg.defs.append(
        f'<filter id="{bl}" x="-60%" y="-60%" width="220%" height="220%"><feGaussianBlur stdDeviation="1.6"/></filter>'
        f'<radialGradient id="{sp}"><stop offset="0" stop-color="#fff" stop-opacity="0.16"/>'
        f'<stop offset="1" stop-color="#fff" stop-opacity="0"/></radialGradient>'
        "<style>"
        "@keyframes glow{0%{opacity:.08}18%{opacity:1}55%{opacity:.62}100%{opacity:.08}}"
        "@keyframes halo{0%{opacity:0}18%{opacity:.9}55%{opacity:.45}100%{opacity:0}}"
        ".lit{animation:glow 5s cubic-bezier(.45,0,.3,1) -0.9s infinite}"
        ".halo{animation:halo 5s cubic-bezier(.45,0,.3,1) -0.9s infinite}"
        "</style>")
    # CSS on purpose (measured): the compositor fades the blurred glow without
    # re-blurring it every frame (SMIL: 149% CPU vs 94%), and nothing is
    # painted over it, so its layer shifts nothing (0.002% of pixels).
    logo = (
        f'<ellipse class="halo" cx="{cx}" cy="{cy + 2}" rx="62" ry="62" fill="url(#{sp})"/>'
        f'<path d="{APPLE}" transform="{tr}" fill="#38393d"/>'
        f'<path class="halo" d="{APPLE}" transform="{tr}" fill="#fff" filter="url(#{bl})"/>'
        f'<path class="lit" d="{APPLE}" transform="{tr}" fill="#fbfbfd"/>'
    )
    svg.raw(f'<g transform="{m}">{logo}</g>')
    return svg


def magic_mouse(svg, c, F, Rp):
    """Magic Mouse (USB-C, 2024; Apple's store photos): a thin black multi-touch
    shell arched along its length, peak a little toward the palm, over a narrower
    silver aluminium body that tucks under it. 113.5 x 57.1 x 21.6 mm. The shell is
    a real height field: every facet is lit from its own normal.

    c: centre on the desk; F: toward the clicking end; Rp: across."""
    L, Wd, Hm, n = 113.5, 57.1, 21.6, 3.2
    a_, b_ = Wd / 2, L / 2
    hw = lambda t: a_ * max(0.0, 1 - abs(t) ** n) ** (1 / n)
    k = math.log(0.5) / math.log(0.42)

    def arch(t):  # 0 at the tips, 1 at the peak (40% from the back)
        u = max(0.0, min(1.0, (t + 1) / 2))
        return math.sin(math.pi * u ** k) ** 0.62

    ztop = lambda t: 2.0 + (Hm - 2.0) * arch(t)
    zrim = lambda t: 1.4 + 7.4 * arch(t)

    def S(t, w):  # point on the shell
        z = zrim(t) + (ztop(t) - zrim(t)) * math.sqrt(max(0.0, 1 - w * w))
        return add(add(c, mul(F, t * b_)), add(mul(Rp, w * hw(t)), mul(Z, z)))

    ring = []
    for i in range(96):
        th = 2 * math.pi * i / 96
        cx, sy = math.cos(th), math.sin(th)
        x = math.copysign(abs(cx) ** (2 / n), cx)
        y = math.copysign(abs(sy) ** (2 / n), sy)
        nx = math.copysign(abs(x) ** (n - 1), x) / a_
        ny = math.copysign(abs(y) ** (n - 1), y) / b_
        l = math.hypot(nx, ny)
        ring.append((x * a_, y * b_, nx / l, ny / l, y))
    foot = [(x * 0.78, y * 0.86, nx, ny) for x, y, nx, ny, _ in ring]
    ground_shadow(svg, c, Rp, F, foot, blur=7, alpha=0.7, dy=3)
    silver = Mat("#6a6b70", "#c4c5ca", ks=0.25, shin=10)
    rim = Mat("#121315", "#2c2d31", ks=0.2, shin=14)
    shell = Mat("#121315", "#33343a", ks=0.22, shin=26, gamma=1.5)
    quads = []
    at = lambda x, y, z: add(add(c, mul(Rp, x)), add(mul(F, y), mul(Z, z)))
    N = len(ring)
    for i in range(N):
        x0, y0, nx0, ny0, t0 = ring[i]
        x1, y1, nx1, ny1, t1 = ring[(i + 1) % N]
        x2, y2, _, _, t2 = ring[(i + 2) % N]
        ns = norm(add(mul(Rp, nx0 + nx1), mul(F, ny0 + ny1)))
        # silver body, just inside the shell's edge and leaning in at the bottom
        sb = 0.985
        top0, top1, top2 = zrim(t0) - 0.9, zrim(t1) - 0.9, zrim(t2) - 0.9
        nn = norm(add(mul(ns, math.cos(-0.45)), mul(Z, math.sin(-0.45))))
        if dot(nn, CAM) > 0 and top0 > 1.2:
            pts = [at(x0 * sb * 0.93, y0 * sb * 0.95, 0.9), at(x1 * sb * 0.93, y1 * sb * 0.95, 0.9),
                   at(x2 * sb * 0.93, y2 * sb * 0.95, 0.9), at(x2 * sb, y2 * sb, top2),
                   at(x1 * sb, y1 * sb, top1), at(x0 * sb, y0 * sb, top0)]
            quads.append((dot(at(x0, y0, 0), CAM) - 50, pts, silver.shade(nn)))
        # black rim of the shell
        nr = norm(add(mul(ns, math.cos(0.2)), mul(Z, math.sin(0.2))))
        if dot(nr, CAM) > 0:
            pts = [at(x0, y0, zrim(t0) - 1.0), at(x1, y1, zrim(t1) - 1.0), at(x2, y2, zrim(t2) - 1.0),
                   at(x2, y2, zrim(t2) + 0.15), at(x1, y1, zrim(t1) + 0.15), at(x0, y0, zrim(t0) + 0.15)]
            quads.append((dot(at(x0, y0, 0), CAM) - 25, pts, rim.shade(nr)))
    NT, NW = 110, 34
    ts = [-1 + 2 * i / NT for i in range(NT + 1)]
    ws = [-1 + 2 * j / NW for j in range(NW + 1)]
    lerp = lambda p0, p1, f: add(p0, mul(add(p1, mul(p0, -1)), f))
    for i in range(NT):
        for j in range(NW):
            t0, t1 = ts[i], ts[i + 1]
            w0, w1 = ws[j], ws[j + 1]
            tm, wm = (t0 + t1) / 2, (w0 + w1) / 2
            dt = add(S(min(tm + 0.01, 1), wm), mul(S(max(tm - 0.01, -1), wm), -1))
            dw = add(S(tm, min(wm + 0.01, 1)), mul(S(tm, max(wm - 0.01, -1)), -1))
            nrm = norm(cross(dw, dt))
            if nrm[2] < 0:
                nrm = mul(nrm, -1)
            if dot(nrm, CAM) <= 0:
                continue
            t2, w2 = min(t1 + 0.4 * (t1 - t0), 1), min(w1 + 0.4 * (w1 - w0), 1)
            pts = [S(t0, w0), S(t2, w0), S(t2, w2), S(t0, w2)]
            quads.append((dot(S(tm, wm), CAM), pts, shell.shade(nrm)))
    quads.sort(key=lambda q: q[0])
    for _, pts, fill in quads:
        svg.poly([P(q) for q in pts], fill, seam=False)
    # grey Apple logo near the palm end, on the tangent plane of the shell
    tl, sc = -0.62, 10.5 / 24
    o = S(tl, 0)
    U_ = norm(add(S(tl, 0.02), mul(S(tl, -0.02), -1)))
    V_ = norm(add(S(tl - 0.02, 0), mul(S(tl + 0.02, 0), -1)))
    o = add(o, mul(Z, 0.05))
    m = matrix(o, U_, V_)
    svg.raw(f'<g transform="{m}"><path d="{APPLE}" fill="#4a4b50" transform="translate({-12 * sc:.2f} {-12 * sc:.2f}) scale({sc:.4f})"/></g>')


def study_desk():
    """Closed MacBook Pro on a big desk mat with the iPhone asleep on the left and a
    mug (the Magic Mouse was dropped; magic_mouse() is still here to reuse). Closed, the Apple logo does not glow (it
    reads as polished metal, per 9to5Mac's review), so the light here is a slow
    reflection gliding over the lid and catching the logo. CSS only."""
    svg = Svg()
    MW, MD, MH = 760.0, 400.0, 3.0
    mat = rrect(MW, MD, 16)
    ground_shadow(svg, (0, 0, 0), X, Y, mat, blur=14, alpha=0.3, dy=6)
    extrude(svg, (0, 0, 0), X, Y, Z, mat, MH, Mat("#38393d", "#3e3f43", ks=0), cap="#2b2c30")

    a = math.radians(20)
    F = (-math.sin(a), -math.cos(a), 0.0)      # from the user toward the hinge
    Rp = cross(Z, F)                           # the user's right, as drawn
    W, D, HB, HL = 312.6, 221.2, 9.8, 5.2      # 14-inch, 15 mm closed
    C = (350.0, 205.0, MH)
    at = lambda c, r=0.0, f=0.0, z=0.0: add(add(c, mul(Rp, r)), add(mul(F, f), mul(Z, z)))
    items = []

    def macbook():
        O = at(C, W / 2, -D / 2)               # u runs along -Rp, v from the front edge
        U, V = mul(Rp, -1), F
        out = rrect(W, D, 9, seg=24, smooth=True)
        ground_shadow(svg, O, U, V, out, blur=10, alpha=0.55, dy=5, spread=1)
        extrude(svg, O, U, V, Z, out, HB, Mat("#3a3a3d", "#444447", ks=0), edge=1.2, steps=6)
        g = svg.uid("lid")
        svg.defs.append(f'<linearGradient id="{g}" x1="0" y1="0" x2="1" y2="1">'
                        f'<stop offset="0" stop-color="#4d4e52"/><stop offset="1" stop-color="#45464a"/></linearGradient>')
        m = extrude(svg, add(O, mul(Z, HB)), U, V, Z, out, HL, Mat("#424346", "#4c4d51", ks=0),
                    edge=1.6, steps=6, cap=f"url(#{g})")
        sc = 30 / 24
        tr = f"translate({W / 2 - 12 * sc:.2f} {D / 2 - 12 * sc:.2f}) scale({sc:.4f})"
        lg, cl, sh, lc = svg.uid("lg"), svg.uid("clip"), svg.uid("sh"), svg.uid("lidclip")
        svg.defs.append(
            f'<linearGradient id="{lg}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#8b8c91"/>'
            f'<stop offset="1" stop-color="#5c5d62"/></linearGradient>'
            f'<clipPath id="{cl}"><path d="{APPLE}"/></clipPath>'
            f'<clipPath id="{lc}"><path d="{inset_path(out, 1.6)}"/></clipPath>'
            f'<linearGradient id="{sh}" x1="0" y1="0" x2="1" y2="0">'
            f'<stop offset="0" stop-color="#fff" stop-opacity="0"/><stop offset="0.5" stop-color="#fff" stop-opacity="1"/>'
            f'<stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient>'
            "<style>"
            "@keyframes sweep{0%{transform:translateX(-420px) skewX(-24deg)}55%,100%{transform:translateX(420px) skewX(-24deg)}}"
            "@keyframes glint{0%,18%{transform:translateX(-30px) skewX(-24deg)}42%,100%{transform:translateX(34px) skewX(-24deg)}}"
            ".sheen{animation:sweep 6s cubic-bezier(.45,0,.25,1) -1.4s infinite}"
            ".glint{animation:glint 6s cubic-bezier(.45,0,.25,1) -1.4s infinite}"
            "</style>")
        # CSS on purpose: cheaper than SMIL here, same pixels (see the logo glow).
        lid = (
            f'<g clip-path="url(#{lc})"><rect class="sheen" x="{W / 2 - 60}" y="-40" width="120" height="{D + 80}" '
            f'fill="url(#{sh})" opacity="0.07"/></g>'
            f'<g transform="{tr}"><path d="{APPLE}" fill="url(#{lg})"/>'
            f'<g clip-path="url(#{cl})"><rect class="glint" x="2" y="-6" width="9" height="36" fill="url(#{sh})" opacity="0.85"/></g></g>'
        )
        svg.raw(f'<g transform="{m}">{lid}</g>')
        # front: the thumb scoop across the seam
        mf = matrix(at(C, W / 2, -D / 2, HB + HL), mul(Rp, -1), NZ)
        svg.raw(f'<g transform="{mf}"><path d="M{W / 2 - 34},{HL} C{W / 2 - 20},{HL} {W / 2 - 18},{HL + 1.6} {W / 2},{HL + 1.6} '
                f'C{W / 2 + 18},{HL + 1.6} {W / 2 + 20},{HL} {W / 2 + 34},{HL} '
                f'C{W / 2 + 20},{HL} {W / 2 + 18},{HL - 1.4} {W / 2},{HL - 1.4} C{W / 2 - 18},{HL - 1.4} {W / 2 - 20},{HL} {W / 2 - 34},{HL} Z" fill="#3b3b3e"/></g>')
        # the user's right: SDXC, Thunderbolt 5, HDMI from front to back (subtle)
        ms = matrix(at(C, W / 2, -D / 2, HB), F, NZ)
        port = lambda d0, w, h, rx: (f'<rect x="{d0 - 0.5:.2f}" y="{HB / 2 - h / 2 - 0.5:.2f}" width="{w + 1}" height="{h + 1}" rx="{rx + 0.5}" fill="#48484b"/>'
                                     f'<rect x="{d0:.2f}" y="{HB / 2 - h / 2:.2f}" width="{w}" height="{h}" rx="{rx}" fill="{F_HOLE}"/>')
        svg.raw(f'<g transform="{ms}">{port(D - 100, 24, 1.6, 0.6)}{port(D - 64.6, 8.6, 2.8, 1.4)}{port(D - 48.5, 14.5, 4.6, 0.8)}</g>')

    def mouse():
        magic_mouse(svg, at(C, W / 2 + 95, -18, 0), F, Rp)

    def phone():
        c = at(C, -(W / 2 + 80), 10)
        phone_flat(svg, at(c, -71.6 / 2, 147.6 / 2), Rp, mul(F, -1), screen=False)

    def mug():
        c = at(C, -(W / 2 + 60), D / 2 + 25)
        ground_shadow(svg, c, X, Y, circle(41, 48), blur=7, alpha=0.55, dy=4)
        m = extrude(svg, c, X, Y, Z, circle(40, 72), 92, Mat("#202124", "#55565b", ks=0, gamma=1.2), cap="#4f5055")
        svg.raw(f'<g transform="{m}"><circle r="35.5" fill="#151516"/><ellipse cx="-8" cy="-9" rx="13" ry="9" fill="#1d1e20"/></g>')
        hx_, hy_ = c[0], c[1]
        path = bezier((hx_ + 36, hy_, c[2] + 76), (hx_ + 66, hy_, c[2] + 76), (hx_ + 66, hy_, c[2] + 24), (hx_ + 36, hy_, c[2] + 24), n=40)
        sweep(svg, path, lambda s: 5.0, lambda s: Mat("#202124", "#55565b", ks=0, gamma=1.2), sides=20, ds=1.2)

    for depth, fn in sorted([(dot(at(C), CAM), macbook),
                             (dot(at(C, -(W / 2 + 80), 10), CAM), phone),
                             (dot(at(C, -(W / 2 + 60), D / 2 + 25), CAM), mug)], key=lambda t: t[0]):
        fn()
    return svg


def _draw_sorted(svg, quads):
    quads.sort(key=lambda q: q[0])
    for _, pts, fill in quads:
        svg.poly([P(q) for q in pts], fill, seam=False)


def _surface(f, mat, nu, nv, closed_u=True, reach=0.4, tint=None):
    """Quads of a parametric surface f(u, v) -> point, u, v in [0, 1], each lit
    from its own normal. Facets reach into their neighbours so no seam shows."""
    quads = []
    lerp = lambda p0, p1, k: add(p0, mul(add(p1, mul(p0, -1)), k))
    for i in range(nu if closed_u else nu - 1):
        u0, u1 = i / nu if closed_u else i / (nu - 1), (i + 1) / nu if closed_u else (i + 1) / (nu - 1)
        for j in range(nv):
            v0, v1 = j / nv, (j + 1) / nv
            um, vm = (u0 + u1) / 2, (v0 + v1) / 2
            e = 1e-3
            du = add(f(um + e, vm), mul(f(um - e, vm), -1))
            dv = add(f(um, min(vm + e, 1)), mul(f(um, max(vm - e, 0)), -1))
            n = norm(cross(du, dv))
            c = f(um, vm)
            if dot(n, CAM) <= 1e-4:
                n2 = mul(n, -1)
                continue
            u2 = u1 + reach * (u1 - u0)
            v2 = min(1.0, v1 + reach * (v1 - v0))
            pts = [f(u0, v0), f(u2, v0), f(u2, v2), f(u0, v2)]
            col = mat.shade(n)
            if tint:
                col = tint(i, j, col)
            quads.append((dot(c, CAM), pts, col))
    return quads


def sellipse(a, b, n, seg=96):
    """Superellipse outline centred on (0, 0) with outward normals, for extrude."""
    out = []
    for i in range(seg):
        th = 2 * math.pi * i / seg
        c, s_ = math.cos(th), math.sin(th)
        x = math.copysign(abs(c) ** (2 / n), c)
        y = math.copysign(abs(s_) ** (2 / n), s_)
        nx = math.copysign(abs(x) ** (n - 1), x) / a
        ny = math.copysign(abs(y) ** (n - 1), y) / b
        l = math.hypot(nx, ny)
        out.append((x * a, y * b, nx / l, ny / l))
    return out


# AirPods Max (1 and 2 share the body), measured on Apple's spec silhouettes
# (apple.com/airpods-max/specs, front + side, scaled by their 168.6 / 83.4 /
# 187.3 mm dimension lines; fits in /tmp/apm-ref during the 5-nov-2026 session):
APM = dict(
    H=187.3,               # band top to cup bottom (side view)
    cup_a=37.75, cup_b=47.6, cup_n=2.62,   # cup side outline: superellipse, rms 0.8 %
    shell=23.6, cushion=17.8, cushion_k=0.97,  # front view slabs (constant thickness)
    stem_x=80.8,           # stem axis from the centre (front view)
    arm_z=78.0, stem_len=13.5, cup_top=89.9,   # from the band top, side view
    band_t=6.3, rail_w=7.4, arm_w=10.0,         # band section
    yoke_w=39.6, yoke_flat=30.0, yoke_join=45.0,  # side view Y: width, flat until, joined at
    canopy_half=62.4, canopy_gap=24.9, canopy_sag=12.0, canopy_top=7.7,
)


def study_airpods_max(tilt=30.5, turn_A=(0.643, -0.766, 0.0), extend=14.0):  # -50°: hero, near-front view
    """AirPods Max 2 (Mar 2026; same body as the first), midnight, flat palette,
    built from measured primitives so it is exactly symmetric: superellipse cups
    (aluminium slab + cushion with real filleted edges), a half-ellipse band made
    of two rails that join into the arm (the side view's Y), a knit canopy hung
    between the rails like a hammock, steel stems, Digital Crown and button.
    tilt rolls the cups bottoms-in (Apple's front photo hangs them at 28.8°);
    extend pulls the telescoping arms out (that photo: ~15 mm)."""
    m = APM
    svg = Svg()
    A = norm(turn_A)
    Fw = cross(Z, A)
    zoff = [0.0]  # lowers everything so the tilted cups still rest on the floor
    at = lambda x, f=0.0, z=0.0: add(add(mul(A, x), mul(Fw, f)), mul(Z, z - zoff[0]))
    Ztop = m["H"]
    shell = Mat("#2a2b2f", "#5c5d63", ks=0.0, gamma=1.05)
    cush = Mat("#303135", "#4a4b50", ks=0.0)
    band_m = Mat("#202124", "#4a4b50", ks=0.0)
    canopy_m = Mat("#18191b", "#36373b", ks=0.0)
    steel = Mat("#66676c", "#d8d9dd", ks=0.35, shin=16)
    out = sellipse(m["cup_a"], m["cup_b"], m["cup_n"])
    cin = sellipse(m["cup_a"] * m["cushion_k"], m["cup_b"] * m["cushion_k"], m["cup_n"])
    hole = sellipse(m["cup_a"] * 0.5, m["cup_b"] * 0.6, 2.3, 64)
    zpiv = Ztop - m["cup_top"] - extend        # cup top / stem foot
    t = -math.radians(tilt)  # positive tilt = bottoms in, as in Apple's front photo

    def cup_frame(side):
        o = add(mul(A, side * math.cos(t)), mul(Z, math.sin(t)))   # outward, rolled
        d = add(mul(A, side * -math.sin(t) * -1 * -1), mul(Z, -math.cos(t)))
        d = norm(cross(Fw, o)) if side > 0 else norm(cross(o, Fw))  # down, perpendicular
        if dot(d, Z) > 0:
            d = mul(d, -1)
        piv = at(side * m["stem_x"], 0, zpiv)
        return o, d, piv

    low = []
    for side in (-1, 1):
        o, d, piv = cup_frame(side)
        c = add(piv, mul(d, m["cup_b"]))
        for x, y, _, _ in out:
            for oo in (m["shell"] / 2, -m["shell"] / 2 - m["cushion"]):
                low.append(add(add(c, mul(Fw, x)), add(mul(d, y), mul(o, oo)))[2])
    zoff[0] = min(low)
    def hull(pts):
        pts = sorted(set(pts))
        if len(pts) < 3:
            return pts
        cr = lambda o_, a_, b_: (a_[0] - o_[0]) * (b_[1] - o_[1]) - (a_[1] - o_[1]) * (b_[0] - o_[0])
        lo_, hi_ = [], []
        for p_ in pts:
            while len(lo_) >= 2 and cr(lo_[-2], lo_[-1], p_) <= 0:
                lo_.pop()
            lo_.append(p_)
        for p_ in reversed(pts):
            while len(hi_) >= 2 and cr(hi_[-2], hi_[-1], p_) <= 0:
                hi_.pop()
            hi_.append(p_)
        return lo_[:-1] + hi_[:-1]

    for side in (-1, 1):  # shadow under the part of each cup that rests near the floor
        o, d, piv = cup_frame(side)
        c = add(piv, mul(d, m["cup_b"]))
        near = []
        for x, y, _, _ in out:
            for oo in (m["shell"] / 2, 0.0, -m["shell"] / 2, -m["shell"] / 2 - m["cushion"]):
                q = add(add(c, mul(Fw, x)), add(mul(d, y), mul(o, oo)))
                if q[2] < 24:
                    near.append((round(q[0], 2), round(q[1], 2)))
        h = hull(near)
        if len(h) >= 3:
            ground_shadow(svg, (0, 0, 0), X, Y, [(x, y, 0, 0) for x, y in h], blur=16, alpha=0.42, dy=4)

    def cup(side, only):
        """only = 'shell' (with the crown and button) or 'cushion', so the painter can
        slot the stem between them: the stem rises from the shell's top, outside
        the cushion."""
        o, d, piv = cup_frame(side)
        c = add(piv, mul(d, m["cup_b"]))
        so = -m["shell"] / 2                    # stem sits over the shell's middle
        parts = []
        # shell: outer face away from the head
        parts.append(("shell", lambda: extrude(svg, add(c, mul(o, so)), Fw, d, o, out, m["shell"], shell,
                                               edge=9.5, steps=12, cap="#47484d" if dot(o, CAM) > 0 else None)))

        def cushion():
            mm = extrude(svg, add(c, mul(o, so)), Fw, d, mul(o, -1), cin, m["cushion"], cush, edge=7.5, steps=10,
                         cap="#2e2f33")
            if dot(mul(o, -1), CAM) > 0:  # we see the inside: knit ring, mesh and the letter
                pid = svg.uid("knit")
                svg.defs.append(f'<pattern id="{pid}" width="1.5" height="1.5" patternUnits="userSpaceOnUse">'
                                f'<circle cx="0.75" cy="0.75" r="0.32" fill="#1c1d20"/></pattern>')
                hp = "M" + "L".join(f"{x:.2f},{y:.2f}" for x, y, _, _ in hole) + "Z"
                ring = "M" + "L".join(f"{x:.2f},{y:.2f}" for x, y, _, _ in sellipse(m["cup_a"] * 0.64, m["cup_b"] * 0.72, 2.4, 64)) + "Z"
                letter = "L" if side < 0 else "R"
                # U = Fw, V = d read mirrored on screen when their 2D cross is negative
                u2, v2 = Pv(Fw), Pv(d)
                flip = (u2[0] * v2[1] - u2[1] * v2[0]) < 0
                tx = ' transform="scale(-1 1)"' if flip else ""
                svg.raw(f'<g transform="{mm}"><path d="{ring}" fill="#28292c"/><path d="{ring}" fill="url(#{pid})"/>'
                        f'<path d="{hp}" fill="#141517"/>'
                        f'<text{tx} x="0" y="9" text-anchor="middle" font-family="Geist, Helvetica, sans-serif" '
                        f'font-size="25" font-weight="600" fill="#2b2c30">{letter}</text></g>')
        parts.append(("cushion", cushion))
        dict(parts)[only]()
        if side > 0 and only == "shell":  # Digital Crown (front) and noise control button (back), on the cup's top edge
            def on_top(x):
                """Point on the shell's top edge at front offset x, its outward normal and tangent."""
                a_, b_, n_ = m["cup_a"], m["cup_b"], m["cup_n"]
                v = -b_ * max(0.0, 1 - abs(x / a_) ** n_) ** (1 / n_)
                nx = math.copysign(abs(x / a_) ** (n_ - 1), x) / a_
                ny = -abs(v / b_) ** (n_ - 1) / b_
                nrm = norm(add(mul(Fw, nx), mul(d, ny)))
                return add(add(c, mul(Fw, x)), mul(d, v)), nrm, norm(cross(nrm, o))

            # measured on Apple's side photo: crown Ø9.5 mm 15 mm in front of the stem,
            # button 10 mm long 18 mm behind it; both sit on the curve, partly sunk in
            p_, nrm, tg = on_top(15.0)
            extrude(svg, add(p_, mul(nrm, -0.8)), o, tg, nrm, circle(6.0, 48), 1.4,
                    Mat("#3a3b3f", "#55565b", ks=0), cap="#4a4b50")           # skirt
            mcr = extrude(svg, add(p_, mul(nrm, -0.6)), o, tg, nrm, circle(4.75, 64), 3.2,
                          Mat("#2b2c30", "#5a5b60", ks=0), knurl=0.4, cap="#3c3d41")
            svg.raw(f'<g transform="{mcr}"><circle r="3.6" fill="#46474b"/></g>')
            p_, nrm, tg = on_top(-18.0)
            w_, h_ = 3.6, 10.0
            extrude(svg, add(add(p_, mul(nrm, -0.5)), add(mul(o, -w_ / 2), mul(tg, -h_ / 2))), o, tg, nrm,
                    rrect(w_, h_, 1.8, seg=10), 1.5, Mat("#33343a", "#505156", ks=0), cap="#45464b")

    def stem(side):
        x = side * m["stem_x"]
        z0, z1 = zpiv + 0.3, Ztop - m["arm_z"]
        sweep(svg, [at(x, 0, z0), at(x, 0, z1)], lambda s_: 3.0, lambda s_: steel, sides=28, ds=0.8, N0=Fw)
        sweep(svg, [at(x, 0, z1 - 2.6), at(x, 0, z1 + 0.4)], lambda s_: 4.3, lambda s_: steel, sides=28, ds=0.6, N0=Fw)

    # band centreline: half ellipse from arm to arm, vertical at both ends
    a_e, z_end = m["stem_x"], Ztop - m["arm_z"]
    b_e = Ztop - m["band_t"] / 2 - z_end

    def band_pt(ph):  # ph in [-pi/2, pi/2], 0 at the top
        return math.sin(ph) * a_e, z_end + math.cos(ph) * b_e

    def offset(zc_):  # rail offset front/back from the side view's Y
        depth = Ztop - zc_
        f0 = (m["yoke_w"] - m["rail_w"]) / 2
        if depth <= m["yoke_flat"]:
            return f0
        if depth >= m["yoke_join"]:
            return 0.0
        k = (depth - m["yoke_flat"]) / (m["yoke_join"] - m["yoke_flat"])
        return f0 * (1 - k * k * (3 - 2 * k))

    def rail(sgn):
        path = []
        for i in range(161):
            ph = -math.pi / 2 + math.pi * i / 160
            x, z = band_pt(ph)
            path.append(at(x, sgn * offset(z), z))
        path = [at(-a_e, 0, z_end - 0.5)] + path + [at(a_e, 0, z_end - 0.5)]
        total = sum(math.dist(p0, p1) for p0, p1 in zip(path, path[1:]))

        def radius(s_):
            # thickness along the band's normal, width across; widen into the single arm
            i = min(len(path) - 1, int(s_ / total * (len(path) - 1)))
            joined = 1 - offset(path[i][2] if True else 0) / ((m["yoke_w"] - m["rail_w"]) / 2)
            return (m["band_t"] / 2, m["rail_w"] / 2 + (m["arm_w"] - m["rail_w"]) / 2 * joined)
        sweep(svg, path, radius, lambda s_: band_m, sides=20, ds=1.4, N0=Fw)

    def canopy():
        ph_max = math.asin(min(1.0, m["canopy_half"] / a_e))
        g = m["canopy_gap"] / 2

        def f(u, v):  # u along the band, v across
            u, v = min(max(u, 0.0), 1.0), min(max(v, 0.0), 1.0)
            ph = -ph_max + 2 * ph_max * u
            x, z = band_pt(ph)
            w = max(0.0, math.cos(ph / ph_max * math.pi / 2)) ** 0.6     # 1 in the middle, 0 at the ends
            vv = -1 + 2 * v
            zz = z - m["band_t"] / 2 - (m["canopy_top"] - m["band_t"]) - m["canopy_sag"] * w * (1 - vv * vv)
            return at(x, vv * g, zz)
        knit = lambda i, j, c_: css(mix(hx(c_), (0, 0, 0), 0.08)) if (i + j) % 2 else c_
        _draw_sorted(svg, _surface(f, canopy_m, 90, 14, closed_u=False, tint=knit))

    # Painter's order, decided per cup by which side faces the camera: the stem
    # rises from the shell's top, so it goes after the shell; the band goes after
    # both stems (the arm sits in the collar); a cushion that faces the camera is
    # in front of its shell and stem, so it goes last.
    def depth(side):
        _, d_, piv_ = cup_frame(side)
        return dot(add(piv_, mul(d_, m["cup_b"])), CAM)

    later = []
    for side in sorted((-1, 1), key=depth):
        o_, _, _ = cup_frame(side)
        if dot(mul(o_, -1), CAM) > 0:      # we look into this cup: cushion in front
            cup(side, "shell")
            stem(side)
            later.append(side)
        else:                              # we see its outer shell
            cup(side, "cushion")
            cup(side, "shell")
            stem(side)
    back = -1 if dot(Fw, CAM) > 0 else 1
    rail(back)
    canopy()
    rail(-back)
    for side in later:
        cup(side, "cushion")
    return svg


# --- study 02: material sheet -------------------------------------------------
def study_swatches():
    svg = Svg()
    items = [
        ("Titanio", TITANIUM, 1.4, "#606167"),
        ("Plástico mate", PLASTIC, 4.5, None),
        ("Metal blanco", WHITEMETAL, 3.0, None),
        ("Aluminio", ALU, 1.2, None),
        ("Vidrio", GLASS, 0.8, "#1d1e21"),
    ]
    step = 46
    for i, (_, mat, edge, cap) in enumerate(items):
        x = i * step
        o = rrect(32, 32, 9)
        ground_shadow(svg, (x, -x, 0), X, Y, o, blur=6, alpha=0.6, dy=4, spread=1)
        extrude(svg, (x, -x, 0), X, Y, Z, o, 10, mat, edge=edge, cap=cap)
    return svg


if __name__ == "__main__":
    which, dst = sys.argv[1], sys.argv[2]
    s = {"phone": study_phone, "swatches": study_swatches, "flat": study_flat, "macbook": study_macbook, "macbook-back": study_macbook_back, "desk": study_desk, "airpods-max": study_airpods_max}[which]()
    open(dst, "w").write(s.out(pad=30))
    print(dst, f"{s.maxx - s.minx:.0f}x{s.maxy - s.miny:.0f}")
