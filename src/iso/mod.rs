//! Isometric drawings: boxes, cylinders and plates in a 3D world, projected
//! at 30° into flat 2D paths that GPUI paints and `svg` writes out.
//!
//! The projection is affine, so a circle on any face of the world becomes an
//! exact ellipse on screen: rounded corners are elliptical arcs (`Seg::A`),
//! never polylines, and stay smooth at any zoom. Depth is the painter's order
//! a scene lists its nodes in; things that sink into something are clipped
//! in world space before projecting, so no 2D clip paths are needed.
//!
//! World axes: x runs down-right on screen, y down-left, z up. One world
//! unit is one screen unit along the vertical.

pub mod anim;
pub mod cli;
pub mod raster;
pub mod scenes;
pub mod script;
// TEMP: the Isometric tab lists these in the next commit.
#[allow(dead_code)]
pub mod starters;
pub mod svg;
pub mod text;

/// cos 30°
const C30: f32 = 0.866_025_4;

pub type P2 = [f32; 2];
pub type V3 = [f32; 3];

/// A world point on screen (in screen units, y down).
pub fn proj([x, y, z]: V3) -> P2 {
    [(x - y) * C30, (x + y) * 0.5 - z]
}

// ---- 2D output ---------------------------------------------------------------

/// A path segment in screen units. `A` is an SVG elliptical arc.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    M(P2),
    L(P2),
    A {
        r: P2,
        rot: f32,
        large: bool,
        sweep: bool,
        to: P2,
    },
    /// A cubic Bézier: two controls, then the end. Glyphs use these.
    C(P2, P2, P2),
    Z,
}

/// The fills of the line-iso style: three tones make the volume, with no light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    Top,
    Left,
    Right,
    Recess,
    AccentTop,
    AccentLeft,
    AccentRight,
    /// Mid gray: fur, stone, anything neither paper nor ink.
    MidTop,
    MidRight,
    /// Dark fur, rubber, a mask: ink-heavy faces that still show volume.
    DarkTop,
    DarkLeft,
    DarkRight,
    /// Solid ink, for dots and marks.
    Ink,
    /// Solid muted ink, for secondary text and marks.
    Mute,
    /// The page color: cards that text sits on.
    Ground,
    /// A color of the scene's own (`.color("#rrggbb")`), on face 0 (lit
    /// top), 1 (left wall) or 2 (right wall, the shade).
    Custom([u8; 3], u8),
    /// Solid accent.
    Accent,
}

/// The strokes: full ink for edges, a muted ink for details, the accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ink {
    Edge,
    Mute,
    Accent,
    AccentMute,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Fill(Tone),
    /// A hard shadow tone, drawn only in the line style (the soft style
    /// shades with `Shade` instead).
    LineOnly(Tone),
    /// Soft style: light over a curved body, a gradient in the frame of its
    /// outline ellipse (`[ox, oy, ux, uy, vx, vy]`: the unit circle's center
    /// and axes on screen), brightest at `hi` in that frame.
    Shade {
        frame: [f32; 6],
        hi: P2,
    },
    /// Soft style: ambient occlusion, darkening linearly from `a` (clear)
    /// to `b` (dark), screen points: walls deepening toward the ground.
    Fade {
        a: P2,
        b: P2,
    },
    /// Soft style: a blurred dark spot (a contact shadow) filling the frame's
    /// unit circle with `alpha` at its middle.
    Soft {
        frame: [f32; 6],
        alpha: f32,
    },
    /// `flow` slides a dashed line along its path, in dash periods: animate
    /// it and the dashes travel the way the line was drawn.
    /// `wide` strokes are drawn twice as wide under a merged group's fills,
    /// so only the outer half shows: the group's union outline.
    Stroke {
        ink: Ink,
        dashed: bool,
        flow: f32,
        wide: bool,
    },
}

/// One painted path.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub path: Vec<Seg>,
    pub paint: Paint,
    /// The opacity of the item's layer, not of the item: a layer's items are
    /// painted opaque together and then faded as one, so a half-faded box
    /// doesn't show its own back edges through its faces.
    pub alpha: f32,
    /// Items in a row with the same layer fade together.
    pub layer: u32,
    /// How the camera moves it.
    pub anchor: Anchor,
    /// The outline of an organic body: in a merged group it goes under the
    /// fills, so the group reads as one body.
    pub silhouette: bool,
}

/// How an item follows the camera (`Shape::Camera`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    /// Part of the world: moves and scales with it.
    World,
    /// Text pinned to a world point: it moves with the point but keeps its
    /// size, so labels stay readable however close the camera comes.
    Pinned(P2),
    /// Fixed on screen: captions and legends.
    Screen,
}

/// Screen bounds as `[min_x, min_y, max_x, max_y]`.
pub type Rect = [f32; 4];

pub fn bounds(items: &[Item]) -> Option<Rect> {
    let mut b: Option<Rect> = None;
    let mut add = |[x, y]: P2| {
        let r = b.get_or_insert([x, y, x, y]);
        r[0] = r[0].min(x);
        r[1] = r[1].min(y);
        r[2] = r[2].max(x);
        r[3] = r[3].max(y);
    };
    for item in items.iter().filter(|i| i.alpha > 0.) {
        let mut at = [0., 0.];
        for seg in &item.path {
            match *seg {
                Seg::M(p) | Seg::L(p) => {
                    add(p);
                    at = p;
                }
                Seg::A {
                    r,
                    rot,
                    large,
                    sweep,
                    to,
                } => {
                    for p in arc_points(at, r, rot, large, sweep, to, 8) {
                        add(p);
                    }
                    at = to;
                }
                Seg::C(c1, c2, to) => {
                    // The control hull holds the curve: close enough for bounds.
                    for p in [c1, c2, to] {
                        add(p);
                    }
                    at = to;
                }
                Seg::Z => {}
            }
        }
    }
    b
}

/// An SVG arc in center form: the ellipse `(cx, cy, rx, ry)` turned by
/// `rot`, run from parameter `t0` through `dt` radians.
struct ArcCenter {
    c: P2,
    rx: f32,
    ry: f32,
    sin: f32,
    cos: f32,
    t0: f32,
    dt: f32,
}

impl ArcCenter {
    /// SVG implementation notes, F.6.5: endpoint to center parameterization.
    /// `None` when a radius is zero: the arc is a straight line.
    fn of(from: P2, r: P2, rot: f32, large: bool, sweep: bool, to: P2) -> Option<Self> {
        let (sin, cos) = rot.to_radians().sin_cos();
        let dx = (from[0] - to[0]) / 2.;
        let dy = (from[1] - to[1]) / 2.;
        let x1 = cos * dx + sin * dy;
        let y1 = -sin * dx + cos * dy;
        let (mut rx, mut ry) = (r[0].abs(), r[1].abs());
        if rx < 1e-6 || ry < 1e-6 {
            return None;
        }
        let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
        if lambda > 1. {
            rx *= lambda.sqrt();
            ry *= lambda.sqrt();
        }
        let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
        let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
        let mut k = (num / den).max(0.).sqrt();
        if large == sweep {
            k = -k;
        }
        let cx1 = k * rx * y1 / ry;
        let cy1 = -k * ry * x1 / rx;
        let c = [
            cos * cx1 - sin * cy1 + (from[0] + to[0]) / 2.,
            sin * cx1 + cos * cy1 + (from[1] + to[1]) / 2.,
        ];
        let angle = |ux: f32, uy: f32| uy.atan2(ux);
        let t0 = angle((x1 - cx1) / rx, (y1 - cy1) / ry);
        let mut dt = angle((-x1 - cx1) / rx, (-y1 - cy1) / ry) - t0;
        if sweep && dt < 0. {
            dt += std::f32::consts::TAU;
        } else if !sweep && dt > 0. {
            dt -= std::f32::consts::TAU;
        }
        Some(ArcCenter {
            c,
            rx,
            ry,
            sin,
            cos,
            t0,
            dt,
        })
    }

    fn at(&self, t: f32) -> P2 {
        let (ex, ey) = (self.rx * t.cos(), self.ry * t.sin());
        [
            self.cos * ex - self.sin * ey + self.c[0],
            self.sin * ex + self.cos * ey + self.c[1],
        ]
    }

    /// The tangent at `t`, per radian.
    fn tangent(&self, t: f32) -> P2 {
        let (ex, ey) = (-self.rx * t.sin(), self.ry * t.cos());
        [self.cos * ex - self.sin * ey, self.sin * ex + self.cos * ey]
    }
}

/// Points along an SVG arc (endpoint form), for bounds and tests.
pub fn arc_points(
    from: P2,
    r: P2,
    rot: f32,
    large: bool,
    sweep: bool,
    to: P2,
    steps: usize,
) -> Vec<P2> {
    let Some(arc) = ArcCenter::of(from, r, rot, large, sweep, to) else {
        return vec![from, to];
    };
    (0..=steps)
        .map(|i| arc.at(arc.t0 + arc.dt * i as f32 / steps as f32))
        .collect()
}

/// An SVG arc as cubic Béziers `[control, control, end]`, a piece per
/// quarter turn at most: off the true ellipse by under 0.03% of its radius.
pub fn arc_cubics(from: P2, r: P2, rot: f32, large: bool, sweep: bool, to: P2) -> Vec<[P2; 3]> {
    let Some(arc) = ArcCenter::of(from, r, rot, large, sweep, to) else {
        return vec![[from, to, to]];
    };
    let n = (arc.dt.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.) as usize;
    let step = arc.dt / n as f32;
    let k = 4. / 3. * (step / 4.).tan();
    (0..n)
        .map(|i| {
            let (a, b) = (arc.t0 + step * i as f32, arc.t0 + step * (i + 1) as f32);
            let (pa, pb) = (arc.at(a), arc.at(b));
            let (ta, tb) = (arc.tangent(a), arc.tangent(b));
            let end = if i + 1 == n { to } else { pb };
            [
                [pa[0] + k * ta[0], pa[1] + k * ta[1]],
                [pb[0] - k * tb[0], pb[1] - k * tb[1]],
                end,
            ]
        })
        .collect()
}

// ---- planes and pens -----------------------------------------------------------

/// A flat 2D coordinate system on a world plane: `origin + a·e1 + b·e2`.
#[derive(Clone, Copy, Debug)]
struct Plane {
    origin: V3,
    e1: V3,
    e2: V3,
}

/// The faces a flat thing can lie on. Only the three faces the camera sees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Face {
    /// The horizontal plane at this z; `a` is x, `b` is y.
    Top(f32),
    /// The vertical plane at this y, facing down-left; `a` is x, `b` is z.
    Left(f32),
    /// The vertical plane at this x, facing down-right; `a` is y, `b` is z.
    Right(f32),
}

impl Face {
    fn plane(self) -> Plane {
        match self {
            Face::Top(z) => Plane {
                origin: [0., 0., z],
                e1: [1., 0., 0.],
                e2: [0., 1., 0.],
            },
            Face::Left(y) => Plane {
                origin: [0., y, 0.],
                e1: [1., 0., 0.],
                e2: [0., 0., 1.],
            },
            Face::Right(x) => Plane {
                origin: [x, 0., 0.],
                e1: [0., 1., 0.],
                e2: [0., 0., 1.],
            },
        }
    }
}

impl Plane {
    fn at(&self, a: f32, b: f32) -> V3 {
        let o = self.origin;
        [
            o[0] + a * self.e1[0] + b * self.e2[0],
            o[1] + a * self.e1[1] + b * self.e2[1],
            o[2] + a * self.e1[2] + b * self.e2[2],
        ]
    }

    /// The screen ellipse a unit circle of this plane becomes: radii, the
    /// rotation of its first axis in degrees, and whether the plane keeps its
    /// winding on screen.
    fn ellipse(&self) -> (P2, f32, bool) {
        let u = proj(self.e1);
        let v = proj(self.e2);
        // The image of the unit circle under M = [u v] is the ellipse of M·Mᵀ.
        let s11 = u[0] * u[0] + v[0] * v[0];
        let s22 = u[1] * u[1] + v[1] * v[1];
        let s12 = u[0] * u[1] + v[0] * v[1];
        let mid = (s11 + s22) / 2.;
        let spread = (((s11 - s22) / 2.).powi(2) + s12 * s12).sqrt();
        let rot = 0.5 * (2. * s12).atan2(s11 - s22);
        let keeps = u[0] * v[1] - u[1] * v[0] > 0.;
        (
            [(mid + spread).sqrt(), (mid - spread).max(0.).sqrt()],
            rot.to_degrees(),
            keeps,
        )
    }
}

/// A piece of a flat outline, in plane coordinates; angles in degrees.
#[derive(Clone, Copy, Debug)]
enum Piece {
    Arc { c: P2, r: f32, a0: f32, a1: f32 },
}

impl Piece {
    fn point(c: P2, r: f32, a: f32) -> P2 {
        let (s, co) = a.to_radians().sin_cos();
        [c[0] + r * co, c[1] + r * s]
    }

    fn reversed(self) -> Self {
        let Piece::Arc { c, r, a0, a1 } = self;
        Piece::Arc {
            c,
            r,
            a0: a1,
            a1: a0,
        }
    }
}

/// Builds one screen path out of pieces that live on world planes.
#[derive(Default)]
struct Pen {
    path: Vec<Seg>,
    at: Option<P2>,
}

impl Pen {
    fn to(&mut self, p: P2) {
        match self.at {
            None => self.path.push(Seg::M(p)),
            Some(q) if (q[0] - p[0]).abs() < 1e-4 && (q[1] - p[1]).abs() < 1e-4 => {}
            Some(_) => self.path.push(Seg::L(p)),
        }
        self.at = Some(p);
    }

    fn lift(&mut self) {
        self.at = None;
    }

    fn close(&mut self) {
        self.path.push(Seg::Z);
        self.at = None;
    }

    fn pieces(&mut self, plane: &Plane, pieces: &[Piece]) {
        let (unit, rot, keeps) = plane.ellipse();
        for piece in pieces {
            match *piece {
                Piece::Arc { c, r, a0, a1 } => {
                    let s = Piece::point(c, r, a0);
                    self.to(proj(plane.at(s[0], s[1])));
                    if r < 1e-4 {
                        continue;
                    }
                    // SVG arcs can't close a full turn and are ambiguous at
                    // half of one: keep each at a quarter or less.
                    let n = ((a1 - a0).abs() / 90.).ceil().max(1.) as usize;
                    for i in 1..=n {
                        let a = a0 + (a1 - a0) * i as f32 / n as f32;
                        let e = Piece::point(c, r, a);
                        let to = proj(plane.at(e[0], e[1]));
                        self.path.push(Seg::A {
                            r: [unit[0] * r, unit[1] * r],
                            rot,
                            large: false,
                            sweep: (a1 > a0) == keeps,
                            to,
                        });
                        self.at = Some(to);
                    }
                }
            }
        }
    }
}

// ---- outlines of rounded rectangles and circles ---------------------------------

/// A rounded rectangle `[a0, b0, a1, b1]` as a closed loop.
fn rr_loop([a0, b0, a1, b1]: [f32; 4], r: f32) -> Vec<Piece> {
    let r = r.min((a1 - a0) / 2.).min((b1 - b0) / 2.).max(0.);
    vec![
        Piece::Arc {
            c: [a1 - r, b1 - r],
            r,
            a0: 0.,
            a1: 90.,
        },
        Piece::Arc {
            c: [a0 + r, b1 - r],
            r,
            a0: 90.,
            a1: 180.,
        },
        Piece::Arc {
            c: [a0 + r, b0 + r],
            r,
            a0: 180.,
            a1: 270.,
        },
        Piece::Arc {
            c: [a1 - r, b0 + r],
            r,
            a0: 270.,
            a1: 360.,
        },
    ]
}

/// The front of a rounded rectangle on the ground plane, split where its
/// two walls meet: from the leftmost point on screen to the nearest one, and
/// from there to the rightmost. The left part runs on to `seam` degrees.
fn rr_front([x0, y0, x1, y1]: [f32; 4], r: f32, seam: f32) -> (Vec<Piece>, Vec<Piece>) {
    let r = r.min((x1 - x0) / 2.).min((y1 - y0) / 2.).max(0.);
    let left = vec![
        Piece::Arc {
            c: [x0 + r, y1 - r],
            r,
            a0: 135.,
            a1: 90.,
        },
        Piece::Arc {
            c: [x1 - r, y1 - r],
            r,
            a0: 90.,
            a1: seam,
        },
    ];
    let right = vec![
        Piece::Arc {
            c: [x1 - r, y1 - r],
            r,
            a0: 45.,
            a1: 0.,
        },
        Piece::Arc {
            c: [x1 - r, y0 + r],
            r,
            a0: 360.,
            a1: 315.,
        },
    ];
    (left, right)
}

fn circle(c: P2, r: f32) -> Vec<Piece> {
    vec![Piece::Arc {
        c,
        r,
        a0: 0.,
        a1: 360.,
    }]
}

/// The two front walls of an upright shape between `z0` and `z1`, as filled
/// paths: the outline at the top, then the same outline back at the bottom.
fn wall(front: &[Piece], z0: f32, z1: f32) -> Vec<Seg> {
    let mut pen = Pen::default();
    pen.pieces(&Face::Top(z1).plane(), front);
    let back: Vec<Piece> = front.iter().rev().map(|p| p.reversed()).collect();
    pen.pieces(&Face::Top(z0).plane(), &back);
    pen.close();
    pen.path
}

fn flat(face: Face, pieces: &[Piece], closed: bool) -> Vec<Seg> {
    let mut pen = Pen::default();
    pen.pieces(&face.plane(), pieces);
    if closed {
        pen.close();
    }
    pen.path
}

// ---- the things a scene is made of -----------------------------------------------

/// A world-space box that clips a node: what lies outside it is cut away.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub min: V3,
    pub max: V3,
}

impl Clip {
    pub const NONE: Clip = Clip {
        min: [f32::MIN; 3],
        max: [f32::MAX; 3],
    };

    pub fn above(z: f32) -> Clip {
        Clip {
            min: [f32::MIN, f32::MIN, z],
            ..Self::NONE
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A box with rounded vertical edges: `min` corner, `size`, corner radius.
    Slab { min: V3, size: V3, r: f32 },
    /// An upright cylinder standing on `base` (the center of its bottom).
    Cylinder { base: V3, r: f32, h: f32 },
    /// The far half of a tube with an open top: what lies behind whatever
    /// drops into it. Paint `TubeFront` after the contents.
    TubeBack {
        base: V3,
        r: f32,
        inner: f32,
        h: f32,
    },
    TubeFront {
        base: V3,
        r: f32,
        inner: f32,
        h: f32,
    },
    /// A flat rounded rectangle `[a0, b0, a1, b1]` on a face.
    Plate { face: Face, rect: [f32; 4], r: f32 },
    /// A flat circle on a face.
    Disc { face: Face, c: P2, r: f32 },
    /// A world polyline.
    Line { points: Vec<V3> },
    /// Flat text facing the viewer, centered vertically on `at`; `align` 0,
    /// 0.5 or 1 puts its left, middle or right end there.
    Text {
        at: V3,
        text: String,
        size: f32,
        align: f32,
    },
    /// A rounded rectangle flat on the screen, `size` wide and tall from its
    /// top-left at `at`: the card a caption sits on.
    Card { at: V3, size: P2, r: f32 },
    /// An ellipsoid: `c` plus its three semi-axes as world vectors (their
    /// lengths are the radii), so it can be turned and tilted.
    Ellipsoid { c: V3, axes: [V3; 3] },
    /// A patch painted on an ellipsoid's surface: around the local direction
    /// `dir` (in the ellipsoid's own axes), spanning `[lon0, lat0, lon1,
    /// lat1]` degrees, corners rounded by `r` degrees. It turns with the body.
    Decal {
        c: V3,
        axes: [V3; 3],
        dir: V3,
        rect: [f32; 4],
        r: f32,
    },
    /// Where the camera looks: `focus` comes to the middle of the frame,
    /// `zoom` times as large. Draws nothing.
    Camera { focus: V3, zoom: f32 },
    /// A soft contact shadow on the ground under `c`, `r` wide in x and y.
    /// Only the soft style shows it.
    Shadow { c: V3, r: P2, alpha: f32 },
    /// A callout: a dot on `from`, a level leader `length` long (to the
    /// right; negative runs it left) and the text at its far end. `reveal`
    /// (0..1) draws the leader from the text toward the thing it names, and
    /// the dot lands last, so the eye follows the line to it.
    Label {
        from: V3,
        length: f32,
        text: String,
        size: f32,
        reveal: f32,
    },
}

/// A shape with its look.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub shape: Shape,
    /// Accent fills for solids; the fill of plates and discs.
    pub accent: bool,
    /// Dark tones for solids (`.dark()`).
    pub dark: bool,
    /// Mid gray tones for solids (`.mid()`).
    pub mid: bool,
    /// A color of its own, overriding the palette's tones.
    pub color: Option<[u8; 3]>,
    pub fill: Option<Tone>,
    pub ink: Option<Ink>,
    pub dashed: bool,
    /// How far the dashes have travelled along the line, in dash periods.
    pub flow: f32,
    pub alpha: f32,
    pub clip: Clip,
    /// Nodes listed together with the same layer fade as one picture.
    pub layer: Option<u32>,
    /// The ink of the edges below the top outline (sides and bottom).
    pub side_ink: Option<Ink>,
    /// Stays put on screen when the camera moves.
    pub fixed: bool,
    /// Nodes in a row with the same merge id draw as one body: their
    /// outlines go under all their fills (`render`).
    pub merge: Option<u32>,
}

impl Node {
    pub fn new(shape: Shape) -> Self {
        let flat = matches!(
            shape,
            Shape::Plate { .. } | Shape::Disc { .. } | Shape::Line { .. }
        );
        let text = matches!(
            shape,
            Shape::Text { .. } | Shape::Label { .. } | Shape::Card { .. }
        );
        let flat = flat || matches!(shape, Shape::Decal { .. });
        Node {
            shape,
            accent: false,
            dark: false,
            mid: false,
            color: None,
            fill: None,
            ink: Some(if flat && !text { Ink::Mute } else { Ink::Edge }),
            dashed: false,
            flow: 0.,
            alpha: 1.,
            clip: Clip::NONE,
            layer: None,
            side_ink: None,
            fixed: false,
            merge: None,
        }
    }

    pub fn accent(mut self) -> Self {
        self.accent = true;
        if self.ink == Some(Ink::Edge) {
            self.ink = Some(Ink::Accent);
        }
        self
    }
    pub fn fill(mut self, tone: Tone) -> Self {
        self.fill = Some(tone);
        self
    }
    pub fn ink(mut self, ink: Ink) -> Self {
        self.ink = Some(ink);
        self
    }
    pub fn dashed(mut self) -> Self {
        self.dashed = true;
        self
    }
    pub fn alpha(mut self, a: f32) -> Self {
        self.alpha = a.clamp(0., 1.);
        self
    }
    pub fn clip(mut self, clip: Clip) -> Self {
        self.clip = clip;
        self
    }
    pub fn layer(mut self, layer: u32) -> Self {
        self.layer = Some(layer);
        self
    }
    pub fn side_ink(mut self, ink: Ink) -> Self {
        self.side_ink = Some(ink);
        self
    }

    /// The fill of text: the accent, muted ink or ink, after the node's look.
    fn text_tone(&self) -> Tone {
        if self.accent {
            Tone::Accent
        } else if matches!(self.ink, Some(Ink::Mute)) {
            Tone::Mute
        } else {
            Tone::Ink
        }
    }

    /// The node as painted paths, back to front.
    pub fn items(&self) -> Vec<Item> {
        if self.alpha <= 0. {
            return Vec::new();
        }
        let tones = if let Some(c) = self.color {
            [Tone::Custom(c, 0), Tone::Custom(c, 1), Tone::Custom(c, 2)]
        } else if self.accent {
            [Tone::AccentTop, Tone::AccentLeft, Tone::AccentRight]
        } else if self.dark {
            [Tone::DarkTop, Tone::DarkLeft, Tone::DarkRight]
        } else if self.mid {
            [Tone::MidTop, Tone::MidTop, Tone::MidRight]
        } else {
            [Tone::Top, Tone::Left, Tone::Right]
        };
        let mut out = Vec::new();
        // Paints other than plain fills, in order (organic bodies use only these).
        let mut extra: Vec<(Vec<Seg>, Paint)> = Vec::new();
        let mut fill = |path: Vec<Seg>, tone: Tone| out.push((path, Paint::Fill(tone)));
        // Outlines, and whether each is a side edge (`side_ink`).
        let mut lines: Vec<(Vec<Seg>, bool)> = Vec::new();
        let c = self.clip;
        match &self.shape {
            Shape::Slab { min, size, r } => {
                let lo = [
                    min[0].max(c.min[0]),
                    min[1].max(c.min[1]),
                    min[2].max(c.min[2]),
                ];
                let hi = [
                    (min[0] + size[0]).min(c.max[0]),
                    (min[1] + size[1]).min(c.max[1]),
                    (min[2] + size[2]).min(c.max[2]),
                ];
                // A box clipped to no height is gone (a flat one drawn on purpose,
                // size 0, keeps its top).
                let squashed = size[2] > 0. && hi[2] - lo[2] <= 1e-3;
                if hi[0] - lo[0] <= 1e-3 || hi[1] - lo[1] <= 1e-3 || hi[2] - lo[2] < 0. || squashed
                {
                    return Vec::new();
                }
                let rect = [lo[0], lo[1], hi[0], hi[1]];
                let r = r.min((hi[0] - lo[0]) / 2.).min((hi[1] - lo[1]) / 2.);
                let (left, right) = rr_front(rect, r, 45.);
                let top = Face::Top(hi[2]);
                if hi[2] - lo[2] > 1e-3 {
                    // Both walls in the right wall's tone, then the left one
                    // exactly to the corner over it: the tone changes right at
                    // the front, with no antialiased seam between two fills.
                    let front: Vec<Piece> = left.iter().chain(&right).copied().collect();
                    fill(wall(&front, lo[2], hi[2]), tones[2]);
                    fill(wall(&left, lo[2], hi[2]), tones[1]);
                    let mid = [(lo[0] + hi[0]) / 2., (lo[1] + hi[1]) / 2.];
                    extra.push((
                        wall(&front, lo[2], hi[2]),
                        Paint::Fade {
                            a: proj([mid[0], mid[1], hi[2]]),
                            b: proj([mid[0], mid[1], lo[2]]),
                        },
                    ));
                }
                fill(flat(top, &rr_loop(rect, r), true), tones[0]);
                lines.push((flat(top, &rr_loop(rect, r), true), false));
                if hi[2] - lo[2] > 1e-3 {
                    let front: Vec<Piece> = left.iter().chain(&right).copied().collect();
                    let mut pen = Pen::default();
                    pen.pieces(&Face::Top(lo[2]).plane(), &front);
                    // Upright edges: the silhouette, and the front corner
                    // when it is sharp enough to read as an edge.
                    let mut edges = vec![
                        Piece::point([lo[0] + r, hi[1] - r], r, 135.),
                        Piece::point([hi[0] - r, lo[1] + r], r, 315.),
                    ];
                    if r < 0.5 {
                        edges.push([hi[0], hi[1]]);
                    }
                    for [x, y] in edges {
                        pen.lift();
                        pen.to(proj([x, y, hi[2]]));
                        pen.to(proj([x, y, lo[2]]));
                    }
                    lines.push((pen.path, true));
                }
            }
            Shape::Cylinder { base, r, h } => {
                let z0 = base[2].max(c.min[2]);
                let z1 = (base[2] + h).min(c.max[2]);
                if z1 < z0 || (*h > 0. && z1 - z0 <= 1e-3) {
                    return Vec::new();
                }
                let ctr = [base[0], base[1]];
                let left = [Piece::Arc {
                    c: ctr,
                    r: *r,
                    a0: 135.,
                    a1: 45.,
                }];
                let front = [Piece::Arc {
                    c: ctr,
                    r: *r,
                    a0: 135.,
                    a1: -45.,
                }];
                if z1 - z0 > 1e-3 {
                    fill(wall(&front, z0, z1), tones[2]);
                    fill(wall(&left, z0, z1), tones[1]);
                    extra.push((
                        wall(&front, z0, z1),
                        Paint::Fade {
                            a: proj([ctr[0], ctr[1], z1]),
                            b: proj([ctr[0], ctr[1], z0]),
                        },
                    ));
                }
                fill(flat(Face::Top(z1), &circle(ctr, *r), true), tones[0]);
                lines.push((flat(Face::Top(z1), &circle(ctr, *r), true), false));
                if z1 - z0 > 1e-3 {
                    lines.push((cyl_sides(ctr, *r, z0, z1), true));
                }
            }
            Shape::TubeBack { base, r, inner, h } => {
                let ctr = [base[0], base[1]];
                let z1 = base[2] + h;
                let top = Face::Top(z1);
                let mut ring = flat(top, &circle(ctr, *r), true);
                ring.extend(flat(top, &circle(ctr, *inner), true));
                fill(ring, tones[0]);
                // The hollow: a floor down at the base, and the inside of
                // the far wall rising from it to the rim. The near half of
                // the floor runs under the lip; `TubeFront` covers it.
                let z0 = base[2];
                fill(
                    flat(Face::Top(z0), &circle(ctr, *inner), true),
                    Tone::Recess,
                );
                for (a0, a1, tone) in [(135., 225., Tone::Right), (225., 315., Tone::Left)] {
                    let arc = [Piece::Arc {
                        c: ctr,
                        r: *inner,
                        a0,
                        a1,
                    }];
                    fill(wall(&arc, z0, z1), tone);
                }
                lines.push((
                    flat(
                        Face::Top(z0),
                        &[Piece::Arc {
                            c: ctr,
                            r: *inner,
                            a0: 135.,
                            a1: 315.,
                        }],
                        false,
                    ),
                    true,
                ));
                lines.push((
                    flat(
                        top,
                        &[Piece::Arc {
                            c: ctr,
                            r: *r,
                            a0: 135.,
                            a1: 315.,
                        }],
                        false,
                    ),
                    false,
                ));
                lines.push((
                    flat(
                        top,
                        &[Piece::Arc {
                            c: ctr,
                            r: *inner,
                            a0: 135.,
                            a1: 315.,
                        }],
                        false,
                    ),
                    false,
                ));
            }
            Shape::TubeFront { base, r, inner, h } => {
                let ctr = [base[0], base[1]];
                let (z0, z1) = (base[2], base[2] + h);
                let left = [Piece::Arc {
                    c: ctr,
                    r: *r,
                    a0: 135.,
                    a1: 45.,
                }];
                let front = [Piece::Arc {
                    c: ctr,
                    r: *r,
                    a0: 135.,
                    a1: -45.,
                }];
                fill(wall(&front, z0, z1), tones[2]);
                fill(wall(&left, z0, z1), tones[1]);
                let top = Face::Top(z1);
                let lip = [
                    Piece::Arc {
                        c: ctr,
                        r: *r,
                        a0: 135.,
                        a1: -45.,
                    },
                    Piece::Arc {
                        c: ctr,
                        r: *inner,
                        a0: -45.,
                        a1: 135.,
                    },
                ];
                fill(flat(top, &lip, true), tones[0]);
                lines.push((
                    flat(
                        top,
                        &[Piece::Arc {
                            c: ctr,
                            r: *r,
                            a0: 135.,
                            a1: -45.,
                        }],
                        false,
                    ),
                    false,
                ));
                lines.push((
                    flat(
                        top,
                        &[Piece::Arc {
                            c: ctr,
                            r: *inner,
                            a0: 135.,
                            a1: -45.,
                        }],
                        false,
                    ),
                    false,
                ));
                lines.push((cyl_sides(ctr, *r, z0, z1), true));
            }
            Shape::Plate { face, rect, r } => {
                let Some(rect) = clip_flat(*face, *rect, &c) else {
                    return Vec::new();
                };
                let (rect, r) = (
                    &rect,
                    &r.min((rect[2] - rect[0]) / 2.)
                        .min((rect[3] - rect[1]) / 2.),
                );
                let tone = self.fill.unwrap_or(if self.accent {
                    Tone::AccentTop
                } else {
                    Tone::Recess
                });
                if self.fill.is_some() || self.accent {
                    fill(flat(*face, &rr_loop(*rect, *r), true), tone);
                }
                lines.push((flat(*face, &rr_loop(*rect, *r), true), false));
            }
            Shape::Disc { face, c: ctr, r } => {
                // A disc shows whole or not at all.
                let whole = [ctr[0] - r, ctr[1] - r, ctr[0] + r, ctr[1] + r];
                if clip_flat(*face, whole, &c) != Some(whole) {
                    return Vec::new();
                }
                let tone = self
                    .fill
                    .unwrap_or(if self.accent { Tone::Accent } else { Tone::Ink });
                if self.fill.is_some() || self.accent {
                    fill(flat(*face, &circle(*ctr, *r), true), tone);
                }
                lines.push((flat(*face, &circle(*ctr, *r), true), false));
            }
            Shape::Text {
                at,
                text,
                size,
                align,
            } => {
                if (0..3).any(|i| at[i] < c.min[i] || at[i] > c.max[i]) {
                    return Vec::new();
                }
                fill(
                    text::outline(text, proj(*at), *size, text::tracking(text, *size), *align),
                    self.text_tone(),
                );
            }
            Shape::Label {
                from,
                length,
                text,
                size,
                reveal,
            } => {
                let p = proj(*from);
                let end = [p[0] + length, p[1]];
                let reveal = reveal.clamp(0., 1.);
                // The leader grows from the text toward `from`.
                let tip = [end[0] - length * reveal, p[1]];
                if reveal > 0. {
                    lines.push((vec![Seg::M(end), Seg::L(tip)], false));
                }
                let dot = 1.6 * Ease::Out.at(((reveal - 0.85) / 0.15).clamp(0., 1.));
                if dot > 0.05 {
                    let a = |dx: f32| Seg::A {
                        r: [dot, dot],
                        rot: 0.,
                        large: false,
                        sweep: true,
                        to: [p[0] + dx, p[1]],
                    };
                    fill(
                        vec![Seg::M([p[0] + dot, p[1]]), a(-dot), a(dot), Seg::Z],
                        self.text_tone(),
                    );
                }
                let gap = size * 0.8 * length.signum();
                let align = if *length < 0. { 1. } else { 0. };
                // The page color behind the words, so a label that lands on
                // the drawing still reads.
                let tracking = text::tracking(text, *size);
                let w = text::width(text, *size, tracking);
                let x0 = if *length < 0. {
                    end[0] + gap - w
                } else {
                    end[0] + gap
                };
                let (pad, h) = (size * 0.35, size * 1.1);
                fill(
                    vec![
                        Seg::M([x0 - pad, end[1] - h / 2.]),
                        Seg::L([x0 + w + pad, end[1] - h / 2.]),
                        Seg::L([x0 + w + pad, end[1] + h / 2.]),
                        Seg::L([x0 - pad, end[1] + h / 2.]),
                        Seg::Z,
                    ],
                    Tone::Ground,
                );
                fill(
                    text::outline(text, [end[0] + gap, end[1]], *size, tracking, align),
                    self.text_tone(),
                );
            }
            Shape::Camera { .. } => {}
            Shape::Shadow { c: at, r, alpha } => {
                let o = proj(*at);
                let (u, v) = (proj([r[0], 0., 0.]), proj([0., r[1], 0.]));
                let ring: Vec<P2> = (0..48)
                    .map(|i| {
                        let a = i as f32 / 48. * std::f32::consts::TAU;
                        [
                            o[0] + u[0] * a.cos() + v[0] * a.sin(),
                            o[1] + u[1] * a.cos() + v[1] * a.sin(),
                        ]
                    })
                    .collect();
                extra.push((
                    poly(&ring),
                    Paint::Soft {
                        frame: [o[0], o[1], u[0], u[1], v[0], v[1]],
                        alpha: *alpha,
                    },
                ));
            }
            Shape::Ellipsoid { c: ctr, axes } => {
                let Some(e) = Ell::new(*ctr, *axes) else {
                    return Vec::new();
                };
                let tones = if let Some(c) = self.color {
                    [Tone::Custom(c, 0), Tone::Custom(c, 2)]
                } else if self.accent {
                    [Tone::AccentTop, Tone::AccentRight]
                } else if self.dark {
                    [Tone::DarkTop, Tone::DarkRight]
                } else if self.mid {
                    [Tone::MidTop, Tone::MidRight]
                } else {
                    [Tone::Top, Tone::Right]
                };
                let outline = e.silhouette();
                extra.push((poly(&outline), Paint::Fill(tones[0])));
                if let Some(shade) = e.shadow() {
                    extra.push((poly(&shade), Paint::LineOnly(tones[1])));
                }
                let (frame, hi) = e.light_frame();
                extra.push((poly(&outline), Paint::Shade { frame, hi }));
                lines.push((poly(&outline), true));
            }
            Shape::Decal {
                c: ctr,
                axes,
                dir,
                rect,
                r,
            } => {
                let Some(e) = Ell::new(*ctr, *axes) else {
                    return Vec::new();
                };
                let Some(points) = e.decal(*dir, *rect, *r) else {
                    return Vec::new();
                };
                let tone = self.fill.unwrap_or(if let Some(c) = self.color {
                    Tone::Custom(c, 0)
                } else if self.accent {
                    Tone::Accent
                } else if self.dark {
                    Tone::DarkTop
                } else if self.mid {
                    Tone::MidTop
                } else {
                    Tone::Top
                });
                extra.push((poly(&points), Paint::Fill(tone)));
                let (frame, hi) = e.light_frame();
                extra.push((poly(&points), Paint::Shade { frame, hi }));
                lines.push((poly(&points), false));
            }
            Shape::Card { at, size, r } => {
                let [x, y] = proj(*at);
                let (w, h) = (size[0], size[1]);
                let r = r.min(w / 2.).min(h / 2.).max(0.);
                let a = |to: P2| Seg::A {
                    r: [r, r],
                    rot: 0.,
                    large: false,
                    sweep: true,
                    to,
                };
                let card = vec![
                    Seg::M([x + r, y]),
                    Seg::L([x + w - r, y]),
                    a([x + w, y + r]),
                    Seg::L([x + w, y + h - r]),
                    a([x + w - r, y + h]),
                    Seg::L([x + r, y + h]),
                    a([x, y + h - r]),
                    Seg::L([x, y + r]),
                    a([x + r, y]),
                    Seg::Z,
                ];
                fill(card.clone(), self.fill.unwrap_or(Tone::Ground));
                lines.push((card, false));
            }
            Shape::Line { points } => {
                let mut pen = Pen::default();
                for p in points
                    .iter()
                    .filter(|p| (0..3).all(|i| p[i] >= c.min[i] && p[i] <= c.max[i]))
                {
                    pen.to(proj(*p));
                }
                lines.push((pen.path, false));
            }
        }
        let anchor = match &self.shape {
            _ if self.fixed => Anchor::Screen,
            Shape::Text { at, .. } => Anchor::Pinned(proj(*at)),
            Shape::Label { from, .. } => Anchor::Pinned(proj(*from)),
            Shape::Card { at, .. } => Anchor::Pinned(proj(*at)),
            _ => Anchor::World,
        };
        out.extend(extra);
        // An ellipsoid's outline is all silhouette; it goes in as a side line.
        let solid_body = matches!(self.shape, Shape::Ellipsoid { .. });
        let mut items: Vec<Item> = out
            .into_iter()
            .map(|(path, paint)| Item {
                path,
                paint,
                alpha: self.alpha,
                layer: 0,
                anchor,
                silhouette: false,
            })
            .collect();
        for (path, side) in lines.into_iter().filter(|(p, _)| p.len() > 1) {
            let ink = if side {
                self.side_ink.or(self.ink)
            } else {
                self.ink
            };
            if let Some(ink) = ink {
                items.push(Item {
                    path,
                    paint: Paint::Stroke {
                        ink,
                        dashed: self.dashed,
                        flow: self.flow.rem_euclid(1.),
                        wide: false,
                    },
                    alpha: self.alpha,
                    layer: 0,
                    anchor,
                    silhouette: side && solid_body,
                });
            }
        }
        items
    }
}

/// The bottom front arc and the two silhouette edges of a cylinder.
fn cyl_sides(c: P2, r: f32, z0: f32, z1: f32) -> Vec<Seg> {
    let mut pen = Pen::default();
    pen.pieces(
        &Face::Top(z0).plane(),
        &[Piece::Arc {
            c,
            r,
            a0: 135.,
            a1: -45.,
        }],
    );
    for a in [135., -45.] {
        let [x, y] = Piece::point(c, r, a);
        pen.lift();
        pen.to(proj([x, y, z1]));
        pen.to(proj([x, y, z0]));
    }
    pen.path
}

/// A rectangle `[a0, b0, a1, b1]` on `face` cut to `clip`, or `None` when
/// nothing of it is left.
fn clip_flat(face: Face, [a0, b0, a1, b1]: [f32; 4], c: &Clip) -> Option<[f32; 4]> {
    // Which world axes the face's plane sits on and its a, b run along.
    let (at, plane, a, b) = match face {
        Face::Top(z) => (z, 2, 0, 1),
        Face::Left(y) => (y, 1, 0, 2),
        Face::Right(x) => (x, 0, 1, 2),
    };
    if at < c.min[plane] || at > c.max[plane] {
        return None;
    }
    let r = [
        a0.max(c.min[a]),
        b0.max(c.min[b]),
        a1.min(c.max[a]),
        b1.min(c.max[b]),
    ];
    (r[2] - r[0] > 1e-3 && r[3] - r[1] > 1e-3).then_some(r)
}

// ---- organic bodies ----------------------------------------------------------------

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-9);
    [a[0] / l, a[1] / l, a[2] / l]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

/// Toward the viewer: what the projection flattens away.
const VIEW: V3 = [0.577_350_3, 0.577_350_3, 0.577_350_3];
/// Where the light comes from: high, from the screen's upper left.
fn light() -> V3 {
    norm([-0.35, 0.45, 1.])
}

/// An ellipsoid in the frame of its axes: a point `q` of the unit sphere
/// sits at `c + A q`, and its normal is `A⁻ᵀ q`, so whether it faces a
/// direction `v` is the sign of `q · A⁻¹ v`.
struct Ell {
    c: V3,
    axes: [V3; 3],
    inv: [V3; 3],
}

impl Ell {
    fn new(c: V3, axes: [V3; 3]) -> Option<Ell> {
        // Rows of A⁻¹ are the cross products of A's columns over det A.
        let [a, b, d] = axes;
        let det = dot(a, cross(b, d));
        if det.abs() < 1e-6 {
            return None;
        }
        let inv = [
            scale(cross(b, d), 1. / det),
            scale(cross(d, a), 1. / det),
            scale(cross(a, b), 1. / det),
        ];
        Some(Ell { c, axes, inv })
    }

    /// A⁻¹ v: the unit-sphere direction whose points face `v` most.
    fn facing(&self, v: V3) -> V3 {
        [
            dot(self.inv[0], v),
            dot(self.inv[1], v),
            dot(self.inv[2], v),
        ]
    }

    fn at(&self, q: V3) -> P2 {
        let [a, b, d] = self.axes;
        proj(add(
            self.c,
            add(scale(a, q[0]), add(scale(b, q[1]), scale(d, q[2]))),
        ))
    }

    /// The outline: the great circle of points edge-on to the viewer.
    fn silhouette(&self) -> Vec<P2> {
        let m = norm(self.facing(VIEW));
        let e1 = norm(cross(
            m,
            if m[2].abs() < 0.9 {
                [0., 0., 1.]
            } else {
                [1., 0., 0.]
            },
        ));
        let e2 = cross(m, e1);
        (0..96)
            .map(|i| {
                let a = i as f32 / 96. * std::f32::consts::TAU;
                self.at(add(scale(e1, a.cos()), scale(e2, a.sin())))
            })
            .collect()
    }

    /// The outline ellipse as a frame on screen (center and the images of its
    /// two unit axes) and where in it the light lands brightest.
    fn light_frame(&self) -> ([f32; 6], P2) {
        let m = norm(self.facing(VIEW));
        let e1 = norm(cross(
            m,
            if m[2].abs() < 0.9 {
                [0., 0., 1.]
            } else {
                [1., 0., 0.]
            },
        ));
        let e2 = cross(m, e1);
        let [a, b, d] = self.axes;
        let image = |q: V3| proj(add(scale(a, q[0]), add(scale(b, q[1]), scale(d, q[2]))));
        let o = proj(self.c);
        let (u, v) = (image(e1), image(e2));
        // Halfway between facing the light and facing us: where it shines.
        let h = norm(add(norm(self.facing(light())), m));
        (
            [o[0], o[1], u[0], u[1], v[0], v[1]],
            [dot(h, e1), dot(h, e2)],
        )
    }

    /// The part in shadow that the viewer sees: the lune between the
    /// silhouette and the terminator (where the surface turns from the light).
    fn shadow(&self) -> Option<Vec<P2>> {
        let md = norm(self.facing(VIEW));
        let ml = norm(self.facing(light()));
        let k = cross(md, ml);
        if dot(k, k) < 1e-6 {
            return None;
        }
        let k = norm(k);
        let half = |m: V3, other: V3, keep_positive: bool, from: V3| -> Vec<V3> {
            // Half of the great circle ⟂ m from `from` to -`from`, on the side
            // of `other` asked for.
            let mut e2 = norm(cross(m, from));
            if (dot(e2, other) > 0.) != keep_positive {
                e2 = scale(e2, -1.);
            }
            (0..=48)
                .map(|i| {
                    let a = i as f32 / 48. * std::f32::consts::PI;
                    add(scale(from, a.cos()), scale(e2, a.sin()))
                })
                .collect()
        };
        // Along the silhouette where it is unlit, then back along the
        // terminator where it is seen.
        let mut q = half(md, ml, false, k);
        q.extend(half(ml, md, true, scale(k, -1.)));
        Some(q.into_iter().map(|q| self.at(q)).collect())
    }

    /// A patch around the local direction `dir`: `[lon0, lat0, lon1, lat1]`
    /// degrees with corners rounded `r` degrees. None when it faces away.
    fn decal(&self, dir: V3, [l0, b0, l1, b1]: [f32; 4], r: f32) -> Option<Vec<P2>> {
        let q0 = norm(dir);
        let up = if q0[2].abs() > 0.95 {
            [0., 1., 0.]
        } else {
            [0., 0., 1.]
        };
        let east = norm(cross(up, q0));
        let north = cross(q0, east);
        let md = self.facing(VIEW);
        let point = |lon: f32, lat: f32| {
            let (lo, la) = (lon.to_radians(), lat.to_radians());
            norm(add(
                scale(q0, la.cos() * lo.cos()),
                add(scale(east, la.cos() * lo.sin()), scale(north, la.sin())),
            ))
        };
        // Rounded rectangle in lon/lat, walked corner by corner.
        let r = r.min((l1 - l0) / 2.).min((b1 - b0) / 2.).max(0.);
        let mut pts = Vec::new();
        let corners = [
            (l1 - r, b0 + r, -90.),
            (l1 - r, b1 - r, 0.),
            (l0 + r, b1 - r, 90.),
            (l0 + r, b0 + r, 180.),
        ];
        for (cx, cy, start) in corners {
            for i in 0..=8 {
                let a = (start + i as f32 * 90. / 8.).to_radians();
                pts.push(point(cx + r * a.cos(), cy + r * a.sin()));
            }
        }
        if dot(point((l0 + l1) / 2., (b0 + b1) / 2.), md) <= 0. {
            return None;
        }
        // What wraps past the horizon is pressed onto it, so the patch ends at
        // the outline instead of folding back over the front.
        let m = norm(md);
        Some(
            pts.into_iter()
                .map(|q| {
                    let d = dot(q, m);
                    let q = if d < 0. {
                        norm(add(q, scale(m, -d)))
                    } else {
                        q
                    };
                    self.at(q)
                })
                .collect(),
        )
    }
}

fn poly(points: &[P2]) -> Vec<Seg> {
    let mut path: Vec<Seg> = points
        .iter()
        .enumerate()
        .map(|(i, &p)| if i == 0 { Seg::M(p) } else { Seg::L(p) })
        .collect();
    path.push(Seg::Z);
    path
}

/// The rotation turning unit vector `from` onto `to` (Rodrigues), applied to `v`.
pub fn turn_onto(v: V3, from: V3, to: V3) -> V3 {
    let (f, t) = (norm(from), norm(to));
    let k = cross(f, t);
    let s = dot(k, k).sqrt();
    let c = dot(f, t);
    if s < 1e-6 {
        return if c > 0. { v } else { scale(v, -1.) };
    }
    let k = scale(k, 1. / s);
    // v cosθ + (k × v) sinθ + k (k·v)(1 − cosθ)
    add(
        add(scale(v, c), scale(cross(k, v), s)),
        scale(k, dot(k, v) * (1. - c)),
    )
}

/// `v` turned `deg` degrees about the world's up axis.
pub fn yaw(v: V3, deg: f32) -> V3 {
    let (s, c) = deg.to_radians().sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c, v[2]]
}

/// Lays out the items of each node; nodes in a row that share a merge id
/// put their outlines, drawn wide, under all their fills.
fn assemble(per_node: Vec<(Option<u32>, u32, Vec<Item>)>) -> Vec<Item> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < per_node.len() {
        let Some(m) = per_node[i].0 else {
            let (_, layer, items) = &per_node[i];
            out.extend(items.iter().cloned().map(|item| Item {
                layer: *layer,
                ..item
            }));
            i += 1;
            continue;
        };
        let run: Vec<&(Option<u32>, u32, Vec<Item>)> = per_node[i..]
            .iter()
            .take_while(|n| n.0 == Some(m))
            .collect();
        let layer = run[0].1;
        for (_, _, items) in &run {
            for item in items.iter().filter(|it| it.silhouette) {
                let mut item = item.clone();
                if let Paint::Stroke { wide, .. } = &mut item.paint {
                    *wide = true;
                }
                out.push(Item { layer, ..item });
            }
        }
        for (_, _, items) in &run {
            out.extend(
                items
                    .iter()
                    .filter(|it| !it.silhouette)
                    .cloned()
                    .map(|item| Item { layer, ..item }),
            );
        }
        i += run.len();
    }
    out
}

/// Every node's items, back to front, each tagged with its fading layer.
pub fn render(nodes: &[Node]) -> Vec<Item> {
    assemble(
        nodes
            .iter()
            .enumerate()
            // Named layers sit above the per-node ones so the two never collide.
            .map(|(i, node)| {
                (
                    node.merge,
                    node.layer.map_or(i as u32, |l| 1 << 20 | l),
                    node.items(),
                )
            })
            .collect(),
    )
}

/// The scene's camera, if it has one, as `screen = offset + zoom · world`
/// (in `render`'s units): what `shoot` does to world-anchored items.
pub fn lens(nodes: &[Node], frame: Rect) -> Option<(f32, P2)> {
    let center = middle(frame);
    let (focus, zoom) = nodes.iter().rev().find_map(|n| match n.shape {
        Shape::Camera { focus, zoom } => Some((proj(focus), zoom.max(0.05))),
        _ => None,
    })?;
    // At zoom 1 the camera leaves the view as it rests; the closer it
    // comes, the more the focus moves to the middle (all the way by 1.4×).
    let pull = ((zoom - 1.) / 0.4).clamp(0., 1.);
    let lands = [
        focus[0] + (center[0] - focus[0]) * pull,
        focus[1] + (center[1] - focus[1]) * pull,
    ];
    Some((
        zoom,
        [lands[0] - focus[0] * zoom, lands[1] - focus[1] * zoom],
    ))
}

/// The frame as the camera sees it: like `render`, then the world zoomed
/// about the camera's focus, which lands in the middle of `frame` (the
/// scene's resting view, `rest_bounds` over its loop). Labels keep their
/// size and follow their point, and shorten their leader when their text
/// would leave the frame.
pub fn shoot(nodes: &[Node], frame: Rect) -> Vec<Item> {
    let cam = lens(nodes, frame);
    let (zoom, off) = cam.unwrap_or((1., [0., 0.]));
    let map = |p: P2| [off[0] + p[0] * zoom, off[1] + p[1] * zoom];
    let margin = 6.;
    let mut per_node = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        let layer = node.layer.map_or(i as u32, |l| 1 << 20 | l);
        let fitted;
        let node = match &node.shape {
            Shape::Label {
                from,
                length,
                text: t,
                size,
                ..
            } if !node.fixed => {
                let q = map(proj(*from));
                let need = text::width(t, *size, text::tracking(t, *size)) + size * 0.8;
                let right = frame[2] - margin - q[0] - need;
                let left = q[0] - frame[0] - margin - need;
                let (room, side) = if *length >= 0. {
                    (right, 1.)
                } else {
                    (left, -1.)
                };
                // Too tight on its side: the label turns to the other one.
                let (room, side) = if room < 12. && (if side > 0. { left } else { right }) > room {
                    (if side > 0. { left } else { right }, -side)
                } else {
                    (room, side)
                };
                let len = length.abs().min(room).max(12.) * side;
                let mut n = node.clone();
                if let Shape::Label { length, .. } = &mut n.shape {
                    *length = len;
                }
                fitted = n;
                &fitted
            }
            _ => node,
        };
        per_node.push((node.merge, layer, node.items()));
    }
    let mut out = assemble(per_node);
    if cam.is_none() {
        return out;
    }
    for item in &mut out {
        let (scale, shift): (f32, Option<P2>) = match item.anchor {
            Anchor::Screen => continue,
            Anchor::World => (zoom, None),
            Anchor::Pinned(p) => {
                let q = map(p);
                (1., Some([q[0] - p[0], q[1] - p[1]]))
            }
        };
        let tf = |p: P2| match shift {
            Some(d) => [p[0] + d[0], p[1] + d[1]],
            None => map(p),
        };
        for seg in &mut item.path {
            *seg = match *seg {
                Seg::M(p) => Seg::M(tf(p)),
                Seg::L(p) => Seg::L(tf(p)),
                Seg::C(a, b, p) => Seg::C(tf(a), tf(b), tf(p)),
                Seg::A {
                    r,
                    rot,
                    large,
                    sweep,
                    to,
                } => Seg::A {
                    r: [r[0] * scale, r[1] * scale],
                    rot,
                    large,
                    sweep,
                    to: tf(to),
                },
                Seg::Z => Seg::Z,
            };
        }
    }
    out
}

/// The view a scene rests in: the bounds of its world, captions and labels
/// (at their shortest; they lengthen into the room they find) over the loop,
/// grown evenly around the world so the drawing sits in the middle.
pub fn rest_bounds(frames: impl Iterator<Item = Vec<Node>>) -> Option<Rect> {
    let grow = |a: Option<Rect>, b: Option<Rect>| match (a, b) {
        (Some(a), Some(b)) => Some([
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]),
        (a, b) => a.or(b),
    };
    let (mut world, mut all) = (None, None);
    for nodes in frames {
        // Labels count at their shortest, so the view leaves each room to
        // show its words on its own side.
        let nodes: Vec<Node> = nodes
            .into_iter()
            .map(|mut n| {
                if let Shape::Label { length, .. } = &mut n.shape {
                    // The shortest leader plus the frame's margin in `shoot`.
                    *length = 18. * length.signum();
                }
                n
            })
            .collect();
        let items = render(&nodes);
        let solid: Vec<Item> = items
            .iter()
            .filter(|i| matches!(i.anchor, Anchor::World))
            .cloned()
            .collect();
        world = grow(world, bounds(&solid));
        all = grow(all, bounds(&items));
    }
    let all = all?;
    let Some(world) = world else { return Some(all) };
    let [cx, cy] = middle(world);
    let (hw, hh) = (
        (cx - all[0]).max(all[2] - cx),
        (cy - all[1]).max(all[3] - cy),
    );
    Some([cx - hw, cy - hh, cx + hw, cy + hh])
}

/// The middle of a rect.
pub fn middle(r: Rect) -> P2 {
    [(r[0] + r[2]) / 2., (r[1] + r[3]) / 2.]
}

// ---- motion ----------------------------------------------------------------------

/// CSS `cubic-bezier(x1, y1, x2, y2)`: the progress at time `t` in 0..1.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    let bez = |a: f32, b: f32, s: f32| {
        3. * a * s * (1. - s).powi(2) + 3. * b * s * s * (1. - s) + s.powi(3)
    };
    let dbez = |a: f32, b: f32, s: f32| {
        3. * a * (1. - s).powi(2) + 6. * (b - a) * s * (1. - s) + 3. * (1. - b) * s * s
    };
    let mut s = t;
    for _ in 0..8 {
        let d = dbez(x1, x2, s);
        if d.abs() < 1e-5 {
            break;
        }
        s = (s - (bez(x1, x2, s) - t) / d).clamp(0., 1.);
    }
    // Newton can stall on flat stretches: finish with bisection.
    let (mut lo, mut hi) = (0f32, 1f32);
    for _ in 0..20 {
        let x = bez(x1, x2, s);
        if (x - t).abs() < 1e-5 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.;
    }
    bez(y1, y2, s)
}

/// The easings illos uses: a strong ease-out and an ease-in-out.
#[derive(Clone, Copy, Debug)]
pub enum Ease {
    Linear,
    Out,
    InOut,
}

impl Ease {
    pub fn at(self, t: f32) -> f32 {
        match self {
            Ease::Linear => t.clamp(0., 1.),
            Ease::Out => cubic_bezier(0.23, 1., 0.32, 1., t),
            Ease::InOut => cubic_bezier(0.77, 0., 0.175, 1., t),
        }
    }
}

/// A value held at `keys[0]` before the first key and at the last after it,
/// eased between keys by the easing of the key it leaves.
pub fn keyed(t: f32, keys: &[(f32, f32, Ease)]) -> f32 {
    let Some(first) = keys.first() else { return 0. };
    if t <= first.0 {
        return first.1;
    }
    for w in keys.windows(2) {
        let (t0, v0, ease) = w[0];
        let (t1, v1, _) = w[1];
        if t <= t1 {
            let q = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1. };
            return v0 + (v1 - v0) * ease.at(q);
        }
    }
    keys[keys.len() - 1].1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn ground_circles_are_upright_ellipses() {
        let (r, rot, keeps) = Face::Top(0.).plane().ellipse();
        assert!(
            close(r[0], 1.5f32.sqrt()) && close(r[1], 0.5f32.sqrt()),
            "{r:?}"
        );
        assert!(close(rot, 0.));
        assert!(keeps);
    }

    #[test]
    fn arcs_land_on_the_projected_circle() {
        // Every point of every arc of a circle on each face must be the
        // projection of a point at distance r from the center.
        for face in [Face::Top(3.), Face::Left(10.), Face::Right(-4.)] {
            let plane = face.plane();
            let path = flat(face, &circle([5., 7.], 4.), true);
            let mut at = [0., 0.];
            for seg in path {
                match seg {
                    Seg::M(p) => at = p,
                    Seg::A {
                        r,
                        rot,
                        large,
                        sweep,
                        to,
                    } => {
                        for p in arc_points(at, r, rot, large, sweep, to, 6) {
                            // Find the world angle whose projection is closest.
                            let best = (0..3600)
                                .map(|i| {
                                    let q = Piece::point([5., 7.], 4., i as f32 / 10.);
                                    let s = proj(plane.at(q[0], q[1]));
                                    (s[0] - p[0]).hypot(s[1] - p[1])
                                })
                                .fold(f32::MAX, f32::min);
                            assert!(best < 0.02, "{face:?}: {p:?} is {best} off the circle");
                        }
                        at = to;
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn easing_matches_css_ends() {
        assert!(close(Ease::Out.at(0.), 0.) && close(Ease::Out.at(1.), 1.));
        // Reference values from bisecting the curves to 1e-15.
        assert!((Ease::Out.at(0.3) - 0.843_01).abs() < 1e-3);
        assert!((Ease::InOut.at(0.5) - 0.595_97).abs() < 1e-3);
    }

    #[test]
    fn flat_shapes_obey_the_clip() {
        // A disc below the clip is gone; a plate across it is cut, not dropped.
        let disc = Node::new(Shape::Disc {
            face: Face::Top(2.),
            c: [0., 0.],
            r: 3.,
        })
        .clip(Clip::above(5.));
        assert!(disc.items().is_empty());
        let wall = Face::Left(0.);
        let plate = Node::new(Shape::Plate {
            face: wall,
            rect: [0., 0., 10., 10.],
            r: 2.,
        })
        .clip(Clip::above(5.));
        let b = bounds(&plate.items()).unwrap();
        let full = bounds(
            &Node::new(Shape::Plate {
                face: wall,
                rect: [0., 0., 10., 10.],
                r: 2.,
            })
            .items(),
        )
        .unwrap();
        assert!(b[3] < full[3] - 1., "the part below z 5 is cut away");
    }

    #[test]
    fn named_layers_fade_together() {
        let coin = |l| {
            Node::new(Shape::Cylinder {
                base: [0., 0., 0.],
                r: 4.,
                h: 2.,
            })
            .alpha(0.5)
            .layer(l)
        };
        let items = render(&[
            coin(3),
            coin(3),
            Node::new(Shape::Line {
                points: vec![[0.; 3], [5., 0., 0.]],
            }),
        ]);
        let layers: Vec<u32> = items.iter().map(|i| i.layer).collect();
        let first = layers[0];
        let shared = layers.iter().take_while(|&&l| l == first).count();
        assert_eq!(
            shared,
            items.len() - 1,
            "both coins share a layer; the line has its own"
        );
    }

    #[test]
    fn the_camera_rests_at_zoom_one_and_keeps_text_size() {
        let box_ = Node::new(Shape::Slab {
            min: [0., 0., 0.],
            size: [40., 40., 10.],
            r: 4.,
        });
        let tag = Node::new(Shape::Text {
            at: [40., 40., 10.],
            text: "A".into(),
            size: 10.,
            align: 0.,
        });
        let frame = rest_bounds(std::iter::once(vec![box_.clone(), tag.clone()])).unwrap();
        // Zoom 1 looking anywhere: the view as it rests.
        let rest = shoot(&[box_.clone(), tag.clone()], frame);
        let still = shoot(
            &[
                box_.clone(),
                tag.clone(),
                Node::new(Shape::Camera {
                    focus: [0., 0., 0.],
                    zoom: 1.,
                }),
            ],
            frame,
        );
        assert_eq!(rest, still);
        // Closer: the box grows, the pinned text keeps its size.
        let close = shoot(
            &[
                box_,
                tag,
                Node::new(Shape::Camera {
                    focus: [20., 20., 5.],
                    zoom: 2.,
                }),
            ],
            frame,
        );
        let size = |items: &[Item], pinned: bool| {
            let only: Vec<Item> = items
                .iter()
                .filter(|i| matches!(i.anchor, Anchor::Pinned(_)) == pinned)
                .cloned()
                .collect();
            let b = bounds(&only).unwrap();
            b[2] - b[0]
        };
        assert!((size(&close, false) / size(&rest, false) - 2.).abs() < 0.01);
        assert!((size(&close, true) - size(&rest, true)).abs() < 0.01);
    }

    #[test]
    fn the_resting_view_centers_the_drawing() {
        // A long label off to the right must not push the box off-center.
        let box_ = Node::new(Shape::Slab {
            min: [0., 0., 0.],
            size: [40., 40., 10.],
            r: 4.,
        });
        let label = Node::new(Shape::Label {
            from: [40., 0., 10.],
            length: 80.,
            text: "A LONG LABEL".into(),
            size: 9.,
            reveal: 1.,
        });
        let frame = rest_bounds(std::iter::once(vec![box_.clone(), label])).unwrap();
        let world = bounds(&render(&[box_])).unwrap();
        let (f, w) = (middle(frame), middle(world));
        assert!((f[0] - w[0]).abs() < 1e-3 && (f[1] - w[1]).abs() < 1e-3);
    }

    #[test]
    fn labels_stay_inside_the_frame() {
        let label = Node::new(Shape::Label {
            from: [0., 0., 0.],
            length: 500.,
            text: "FAR AWAY".into(),
            size: 9.,
            reveal: 1.,
        });
        let frame = [-100., -50., 100., 50.];
        let b = bounds(&shoot(&[label], frame)).unwrap();
        assert!(b[2] <= frame[2] + 0.5, "{b:?}");
    }

    #[test]
    fn clipped_to_no_height_paints_nothing() {
        // Sunk exactly to the clip: no top face left lying on the surface.
        let n = Node::new(Shape::Slab {
            min: [0., 0., -5.],
            size: [10., 10., 5.],
            r: 2.,
        })
        .clip(Clip::above(0.));
        assert!(n.items().is_empty());
        let c = Node::new(Shape::Cylinder {
            base: [0., 0., -5.],
            r: 3.,
            h: 5.,
        })
        .clip(Clip::above(0.));
        assert!(c.items().is_empty());
    }

    #[test]
    fn clipped_away_slab_paints_nothing() {
        let n = Node::new(Shape::Slab {
            min: [0., 0., 0.],
            size: [10., 10., 5.],
            r: 2.,
        })
        .clip(Clip::above(6.));
        assert!(n.items().is_empty());
    }
}
