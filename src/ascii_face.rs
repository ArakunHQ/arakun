//! The raccoon's ink face drawn in ASCII, for the start and welcome screens.
//!
//! Each frame samples the head image through the head's inverse transform, nine
//! points per character cell, with the amber eye capsules worked out in code;
//! brightness picks a character, amber becomes block glyphs. The first face of a
//! run plays an intro: eyes in the dark, then the face lit from the eyes out.
//! The browser prototype is `shots/arakun/intro/rig/welcome.html`.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    Bounds, Context, DispatchPhase, FontId, GlyphId, Hsla, IntoElement, MouseMoveEvent,
    ParentElement as _, Pixels, Point, Render, Rgba, Styled as _, Subscription, Task, TextRun,
    Window, canvas, div, point, px,
};

use crate::mascot::{Spring, animated};
use crate::theme;

pub const HEIGHT: f32 = 360.;
const CELL_W: f32 = 7.;
const CELL_H: f32 = 12.;
const FONT_SIZE: f32 = CELL_H * 0.92;
/// The head's width on screen.
const FACE_W: f32 = 372.;
/// The prototype's distances were tuned for a head 1.75x the image; they scale
/// with the face.
const PROTO_K: f32 = 1.75;
const RAMP: [char; 10] = [' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];
/// Light mode: below this brightness a cell is the mask, an ear or the nose.
const LIGHT_SPLIT: f32 = 0.35;
const DOT: char = '·';
const CHARS: &str = " .:-=+*#%@·░▓█";

const INTRO_LEN: f32 = 4.8;
const FADE_LEN: f32 = 0.6;
/// Keys during the intro play the rest this much faster.
const RUSH: f32 = 4.;

/// The eyes in the image: centres, then width and height of the capsules.
const EYES: [[f32; 2]; 2] = [[97.2, 124.5], [170.2, 124.5]];
const EYE_SIZE: [f32; 2] = [12.5, 35.];
const AMBER: [f32; 3] = [255., 181., 71.];
/// Over the eyes painted into the image, so a blink shows no old eye beneath.
const EYE_CAP: [f32; 3] = [22., 22., 27.];
const DOTS: [f32; 3] = [134., 166., 242.];

static INTRO_SEEN: AtomicBool = AtomicBool::new(false);

/// True only the first time in a run: that face plays the intro.
fn claim_intro() -> bool {
    !INTRO_SEEN.swap(true, Ordering::Relaxed)
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

// ---------- The image ----------

struct Img {
    w: usize,
    h: usize,
    /// Premultiplied RGB, 0-255.
    px: Vec<[f32; 3]>,
    /// Coverage, 0-1: tells the black mask (opaque) from the empty around
    /// the head (clear), which premultiplied both read as black.
    alpha: Vec<f32>,
}

fn img() -> &'static Img {
    static IMG: OnceLock<Img> = OnceLock::new();
    IMG.get_or_init(|| {
        let png = image::load_from_memory_with_format(
            include_bytes!("../assets/brand/ascii-face.png"),
            image::ImageFormat::Png,
        )
        .expect("ascii-face.png")
        .to_rgba8();
        let (w, h) = (png.width() as usize, png.height() as usize);
        let px = png
            .pixels()
            .map(|p| {
                let a = p[3] as f32 / 255.;
                [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a]
            })
            .collect();
        let alpha = png.pixels().map(|p| p[3] as f32 / 255.).collect();
        Img { w, h, px, alpha }
    })
}

impl Img {
    fn at(&self, x: isize, y: isize) -> [f32; 3] {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return [0.; 3];
        }
        self.px[y as usize * self.w + x as usize]
    }

    /// Coverage at a pixel, the nearest one: only compared against a half.
    fn alpha_at(&self, u: f32, v: f32) -> f32 {
        let (x, y) = (u.floor() as isize, v.floor() as isize);
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return 0.;
        }
        self.alpha[y as usize * self.w + x as usize]
    }

    /// Bilinear, so characters don't flicker as the head moves a fraction of a pixel.
    fn sample(&self, u: f32, v: f32) -> [f32; 3] {
        let (x, y) = (u - 0.5, v - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let (a, b, c, d) = (
            self.at(x0, y0),
            self.at(x0 + 1, y0),
            self.at(x0, y0 + 1),
            self.at(x0 + 1, y0 + 1),
        );
        [0, 1, 2].map(|i| lerp(lerp(a[i], b[i], fx), lerp(c[i], d[i], fx), fy))
    }
}

fn in_capsule(x: f32, y: f32, cx: f32, cy: f32, w: f32, h: f32) -> bool {
    let r = w / 2.;
    let l = (h / 2. - r).max(0.);
    let dy = ((y - cy).abs() - l).max(0.);
    (x - cx).powi(2) + dy * dy <= r * r
}

fn is_amber(c: [f32; 3]) -> bool {
    let [r, g, b] = c;
    r > 70. && (r - b) / r > 0.55 && g > b * 1.3
}

// ---------- One frame ----------

/// Where the head is and what its eyes do, plus how lit the intro has it.
#[derive(Clone, Copy, Debug)]
struct Pose {
    /// Centre of the head on the canvas, its turn (radians) and scale.
    at: [f32; 2],
    turn: f32,
    k: f32,
    stretch: f32,
    gaze: [f32; 2],
    blink: f32,
    glow: f32,
    /// Intro: how lit the eyes are, how far the light has reached from them
    /// and how bright it is; `None` once the intro is over.
    reveal: Option<(f32, f32, f32)>,
    /// Everything at once, for the quick fade-in.
    fade: f32,
}

impl Pose {
    fn still(w: f32) -> Self {
        Pose {
            at: [w / 2., HEIGHT * 0.53],
            turn: 0.,
            k: FACE_W / img().w as f32,
            stretch: 1.,
            gaze: [0.; 2],
            blink: 1.,
            glow: 0.2,
            reveal: None,
            fade: 1.,
        }
    }

    /// Image point to canvas point.
    fn forward(&self, u: f32, v: f32) -> [f32; 2] {
        let img = img();
        let (x, y) = (
            (u - img.w as f32 / 2.) * self.k,
            (v - img.h as f32 / 2.) * self.k * self.stretch,
        );
        let (s, c) = self.turn.sin_cos();
        [self.at[0] + c * x - s * y, self.at[1] + s * x + c * y]
    }

    fn eyes_at(&self) -> [f32; 2] {
        self.forward((EYES[0][0] + EYES[1][0]) / 2., EYES[0][1])
    }

    /// How much of the head covers canvas point `x`, `y`.
    fn coverage(&self, x: f32, y: f32) -> f32 {
        let img = img();
        let (dx, dy) = (x - self.at[0], y - self.at[1]);
        let (s, c) = self.turn.sin_cos();
        let u = (c * dx + s * dy) / self.k + img.w as f32 / 2.;
        let v = (-s * dx + c * dy) / (self.k * self.stretch) + img.h as f32 / 2.;
        img.alpha_at(u, v)
    }

    /// The colour at canvas point `x`, `y`: the image with the eyes on top.
    fn color(&self, x: f32, y: f32) -> [f32; 3] {
        let img = img();
        let (dx, dy) = (x - self.at[0], y - self.at[1]);
        let (s, c) = self.turn.sin_cos();
        let u = (c * dx + s * dy) / self.k + img.w as f32 / 2.;
        let v = (-s * dx + c * dy) / (self.k * self.stretch) + img.h as f32 / 2.;
        let mut col = img.sample(u, v);
        let [ew, eh] = EYE_SIZE;
        for e in EYES {
            if in_capsule(u, v, e[0], e[1], ew + 5., eh + 5.) {
                col = EYE_CAP;
            }
        }
        let h = (ew * 0.55).max(eh * self.blink);
        for e in EYES {
            let (cx, cy) = (e[0] + self.gaze[0], e[1] + self.gaze[1] + (eh - h) * 0.3);
            if self.glow > 0. {
                let r = eh * 0.85;
                let d = (u - cx).hypot(v - cy);
                if d < r {
                    let a = 0.4 * self.glow * (1. - d / r);
                    col = [0, 1, 2].map(|i| col[i] * (1. - a) + AMBER[i] * a);
                }
            }
            if in_capsule(u, v, cx, cy, ew, h) {
                col = AMBER;
            }
        }
        col
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Grid {
    cols: usize,
    rows: usize,
    /// Left edge of the first column, centring the grid.
    x0: f32,
}

impl Grid {
    fn new(w: f32) -> Self {
        let cols = (w / CELL_W).floor().max(0.) as usize;
        Grid {
            cols,
            rows: (HEIGHT / CELL_H).floor() as usize,
            x0: (w - cols as f32 * CELL_W) / 2.,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FaceCell {
    col: usize,
    row: usize,
    ch: char,
    rgb: [f32; 3],
    part: Part,
}

/// What a character draws, so it takes the mascot's color for that part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    /// Paper in dark mode, a warm gray in light (paper would vanish on white).
    Fur,
    /// The mask, ear insides and nose: drawn in ink on white only; on black
    /// the ground shows through, the way the logo's holes do.
    Ink,
    /// Amber.
    Eye,
}

/// The characters of the face: only cells near the head are looked at, and
/// cells too dark to read are left empty.
fn face_cells(pose: &Pose, grid: Grid) -> Vec<FaceCell> {
    let img = img();
    let u = pose.k / PROTO_K;
    let r = img.w.max(img.h) as f32 * pose.k * 0.62 + 30. * u;
    let span = |lo: f32, hi: f32, size: f32, n: usize| {
        ((lo / size).floor().max(0.) as usize).min(n)..((hi / size).ceil().max(0.) as usize).min(n)
    };
    let cols = span(
        pose.at[0] - r - grid.x0,
        pose.at[0] + r - grid.x0,
        CELL_W,
        grid.cols,
    );
    let rows = span(pose.at[1] - r, pose.at[1] + r, CELL_H, grid.rows);
    let eyes = pose.eyes_at();
    let mut out = Vec::new();
    for row in rows {
        for col in cols.clone() {
            let (x, y) = (grid.x0 + col as f32 * CELL_W, row as f32 * CELL_H);
            let mut sum = [0.; 3];
            for fy in [0.25, 0.5, 0.75] {
                for fx in [0.25, 0.5, 0.75] {
                    let c = pose.color(x + CELL_W * fx, y + CELL_H * fy);
                    sum = [0, 1, 2].map(|i| sum[i] + c[i]);
                }
            }
            let c = sum.map(|v| v / 9.);
            let amber = is_amber(c);
            let mut k = pose.fade;
            if let Some((eyes_k, radius, light)) = pose.reveal {
                k *= if amber {
                    eyes_k
                } else {
                    let d = (x + CELL_W / 2. - eyes[0]).hypot(y + CELL_H / 2. - eyes[1]);
                    ((radius - d) / (200. * u)).clamp(0., 1.) * light
                };
            }
            let l = (0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]) / 255. * k;
            // On white the face is drawn the way the mascot looks: the dark
            // half of the drawing (mask, ear insides, nose; under 0.35 of
            // full brightness, where the image splits in two) as dense ink,
            // the fur as lighter, sparser characters.
            if theme::is_light() && !amber {
                if k < 0.05 || pose.coverage(x + CELL_W / 2., y + CELL_H / 2.) < 0.5 {
                    continue;
                }
                let raw = l / k;
                let (density, part) = if raw < LIGHT_SPLIT {
                    (0.55 + 0.45 * (1. - raw / LIGHT_SPLIT), Part::Ink)
                } else {
                    (0.15 + 0.4 * (1. - raw), Part::Fur)
                };
                let i = (density * k * RAMP.len() as f32) as usize;
                if i == 0 {
                    continue;
                }
                out.push(FaceCell {
                    col,
                    row,
                    ch: RAMP[i.min(RAMP.len() - 1)],
                    rgb: [0.; 3],
                    part,
                });
                continue;
            }
            if l < 0.07 {
                continue;
            }
            let ch = if amber {
                if l > 0.38 {
                    '█'
                } else if l > 0.2 {
                    '▓'
                } else {
                    '░'
                }
            } else {
                RAMP[((l.powf(0.75) * RAMP.len() as f32) as usize).min(RAMP.len() - 1)]
            };
            let gain = (0.55 / l.max(0.12) + 0.9).min(2.2) * k;
            out.push(FaceCell {
                col,
                row,
                ch,
                rgb: c.map(|v| (v * gain).min(255.)),
                part: if amber { Part::Eye } else { Part::Fur },
            });
        }
    }
    out
}

/// How visible the faint dot behind a cell is: soft at the canvas edges.
fn dot_alpha(grid: Grid, col: usize, row: usize) -> f32 {
    let x = (col as f32 + 0.5) / grid.cols.max(1) as f32 * 2. - 1.;
    let y = (row as f32 + 0.5) / grid.rows.max(1) as f32 * 2. - 1.;
    let d = x.hypot(y * 0.9);
    0.07 * (1. - smooth((d - 0.55) / 0.45))
}

fn rgb(c: [f32; 3], a: f32) -> Hsla {
    Rgba {
        r: c[0] / 255.,
        g: c[1] / 255.,
        b: c[2] / 255.,
        a,
    }
    .into()
}

struct Glyphs {
    ids: HashMap<char, (FontId, GlyphId)>,
    advance: f32,
    baseline: f32,
}

/// Glyph ids for every character the face uses, from one laid-out line, so
/// frames paint glyphs directly with no shaping.
fn glyphs(window: &Window) -> Glyphs {
    let font = gpui::Font {
        family: theme::FONT_MONO.into(),
        features: gpui::FontFeatures::disable_ligatures(),
        fallbacks: None,
        weight: gpui::FontWeight::SEMIBOLD,
        style: gpui::FontStyle::Normal,
    };
    let run = TextRun {
        len: CHARS.len(),
        font,
        color: theme::content(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .layout_line(CHARS, px(FONT_SIZE), &[run], None);
    let at: HashMap<usize, char> = CHARS.char_indices().collect();
    let mut ids = HashMap::new();
    for r in &line.runs {
        for g in &r.glyphs {
            if let Some(&ch) = at.get(&g.index) {
                ids.insert(ch, (r.font_id, g.id));
            }
        }
    }
    let n = CHARS.chars().count() as f32;
    let (ascent, descent) = (line.ascent.as_f32(), line.descent.as_f32().abs());
    Glyphs {
        ids,
        advance: line.width.as_f32() / n,
        baseline: (CELL_H - ascent - descent) / 2. + ascent,
    }
}

struct Frame {
    grid: Grid,
    cells: Vec<FaceCell>,
    dots: f32,
    glyphs: Rc<Glyphs>,
    scale: f32,
}

// ---------- The live face ----------

struct Blinker {
    next: f32,
    at: f32,
}

pub struct AsciiFace {
    start: Instant,
    last: Option<f64>,
    started: bool,
    rendered: bool,
    asked: f64,
    hidden: bool,
    active: bool,
    rng: u64,

    intro: Option<f32>,
    rush: bool,
    fade: Option<f32>,
    time: f32,
    look: [Spring; 2],
    tilt: Spring,
    blink: Blinker,

    pointer: Option<(Point<Pixels>, f64)>,
    typing_until: f64,
    /// The composer's caret in window coordinates, set by whoever shows the face.
    pub caret: Rc<Cell<Option<Point<Pixels>>>>,
    origin: Point<Pixels>,
    eyes: [f32; 2],
    glyphs: Option<Rc<Glyphs>>,
    /// How large it is drawn, 1 for the start screen's size (`scaled`).
    scale: f32,

    _tick: Task<()>,
    _keys: Subscription,
}

impl AsciiFace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Only asks for a frame once the last one was painted: a face that is
        // not on screen never paints, so it is known hidden and costs nothing.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let alive = this.update(cx, |f, cx| {
                    let now = f.now();
                    if !f.rendered {
                        if now - f.asked > 0.3 {
                            f.hidden = true;
                        }
                    } else if f.wants_frame() {
                        f.rendered = false;
                        f.asked = now;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let keys = cx.observe_keystrokes(|f, _, _, _| f.typed());
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9, |d| d.as_nanos() as u64)
            | 1;
        let mut face = AsciiFace {
            start: Instant::now(),
            last: None,
            started: false,
            rendered: true,
            asked: 0.,
            hidden: false,
            active: true,
            rng: seed,
            intro: None,
            rush: false,
            fade: None,
            time: 0.,
            look: [Spring::new(0., 2.6, 0.7); 2],
            tilt: Spring::new(0., 2.2, 0.55),
            blink: Blinker { next: 0., at: -9. },
            pointer: None,
            typing_until: -9.,
            caret: Rc::default(),
            origin: Point::default(),
            eyes: [0.; 2],
            glyphs: None,
            scale: 1.,
            _tick: tick,
            _keys: keys,
        };
        face.blink.next = face.rnd(1., 3.);
        face
    }

    /// A smaller face beside something else, such as the setup's steps: the
    /// same drawing with every size times `scale`. It skips the intro, which
    /// is the start screen's.
    pub fn scaled(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// The title and composer under the face come in late in the intro.
    pub fn ui_alpha(&self) -> f32 {
        self.intro.map_or(1., |t| smooth((t - 3.4) / 1.2))
    }

    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn rnd(&mut self, a: f32, b: f32) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        a + (self.rng >> 11) as f32 / (1u64 << 53) as f32 * (b - a)
    }

    fn typed(&mut self) {
        self.typing_until = self.now() + 1.4;
        if self.intro.is_some() {
            self.rush = true;
        }
    }

    fn wants_frame(&self) -> bool {
        animated() && (self.intro.is_some() || self.fade.is_some() || self.active)
    }

    fn blink_at(&mut self) -> f32 {
        let t = self.time;
        if t > self.blink.next {
            self.blink.at = t;
            self.blink.next = t + if self.rnd(0., 1.) < 0.2 {
                0.25
            } else {
                self.rnd(2.2, 5.5)
            };
        }
        let q = (t - self.blink.at) / 0.16;
        if q >= 1. {
            return 1.;
        }
        let v = if q < 0.45 {
            1. - q / 0.45
        } else {
            (q - 0.45) / 0.55
        };
        smooth(v).max(0.06)
    }

    /// Where the eyes want to be, -1..1 each way.
    fn target(&self, now: f64) -> ([f32; 2], f32) {
        let toward = |p: Point<Pixels>, sx: f32, sy: f32| {
            // Into the face's own units, where it has its usual size.
            let (x, y) = (
                (p.x - self.origin.x).as_f32() / self.scale,
                (p.y - self.origin.y).as_f32() / self.scale,
            );
            [
                ((x - self.eyes[0]) / sx).clamp(-1., 1.),
                ((y - self.eyes[1]) / sy).clamp(-1., 1.),
            ]
        };
        let u = FACE_W / img().w as f32 / PROTO_K;
        if self.intro.is_some() {
            return ([0., 0.05], 0.);
        }
        if now < self.typing_until {
            return match self.caret.get() {
                Some(c) => {
                    let t = toward(c, 450. * u, 260. * u);
                    (t, 4. * t[0].signum())
                }
                None => ([0., 0.8], 0.),
            };
        }
        if let Some((p, _)) = self.pointer.filter(|&(_, at)| now - at < 3.) {
            return (toward(p, 450. * u, 300. * u), 0.);
        }
        const IDLE: [[f32; 2]; 5] = [[0., 0.], [-0.7, 0.1], [0.2, -0.3], [0.8, 0.1], [-0.2, 0.5]];
        (IDLE[(self.time / 2.6) as usize % IDLE.len()], 0.)
    }

    fn pose(&mut self, w: f32, dt: f32) -> Pose {
        let mut pose = Pose::still(w);
        let now = self.now();
        if !animated() {
            self.intro = None;
            self.fade = None;
            return pose;
        }
        if let Some(t) = self.intro.as_mut() {
            *t += dt * if self.rush { RUSH } else { 1. };
            if *t >= INTRO_LEN {
                self.intro = None;
            }
        }
        if let Some(t) = self.fade.as_mut() {
            *t += dt;
            if *t >= FADE_LEN {
                self.fade = None;
            }
        }
        self.time += dt;
        let (want, lean) = self.target(now);
        let n = (dt / 0.004).ceil().max(1.) as usize;
        let h = dt / n as f32;
        for _ in 0..n {
            self.look[0].step(want[0], h);
            self.look[1].step(want[1], h);
            let lx = self.look[0].x;
            self.tilt.step(lx * 7. + lean, h);
        }
        let [lx, ly] = [self.look[0].x, self.look[1].x];
        let br = (self.time * 2.4).sin();
        let u = pose.k / PROTO_K;
        pose.at = [
            pose.at[0] + lx * 14. * u,
            pose.at[1] + (ly * 8. + 3. * br) * u,
        ];
        pose.turn = self.tilt.x.to_radians();
        pose.stretch = 1. + 0.006 * br;
        pose.gaze = [lx * 7., ly * 5.];
        pose.blink = self.blink_at();
        if let Some(t) = self.intro {
            let lit = smooth((t - 2.) / 2.4);
            pose.k *= lerp(0.84, 1., smooth((t - 1.8) / 2.8));
            pose.glow = lerp(1., 0.2, smooth((t - 2.4) / 2.));
            pose.reveal = Some((
                smooth((t - 0.5) / 0.9),
                lerp(0., 700., lit) * u,
                lerp(0.35, 1., lit),
            ));
        }
        pose.fade = self.fade.map_or(1., |t| smooth(t / FADE_LEN));
        pose
    }

    fn frame(&mut self, bounds: Bounds<Pixels>, window: &Window) -> Frame {
        let now = self.now();
        let dt = self.last.map_or(1. / 60., |l| (now - l).min(0.05)) as f32;
        self.last = Some(now);
        let w = bounds.size.width.as_f32() / self.scale;
        self.origin = bounds.origin;
        let pose = self.pose(w, dt);
        self.eyes = pose.eyes_at();
        let grid = Grid::new(w);
        let glyphs = self
            .glyphs
            .get_or_insert_with(|| Rc::new(glyphs(window)))
            .clone();
        let dots = match self.intro {
            Some(t) => smooth((t - 3.) / 1.4),
            None => pose.fade,
        };
        Frame {
            grid,
            cells: face_cells(&pose, grid),
            dots,
            glyphs,
            scale: self.scale,
        }
    }
}

fn paint(frame: &Frame, b: Bounds<Pixels>, window: &mut Window) {
    let g = &frame.glyphs;
    let grid = frame.grid;
    let k = frame.scale;
    let size = px(FONT_SIZE * k);
    let at = |col: usize, row: usize| {
        point(
            b.origin.x + px((grid.x0 + col as f32 * CELL_W + (CELL_W - g.advance) / 2.) * k),
            b.origin.y + px((row as f32 * CELL_H + g.baseline) * k),
        )
    };
    let mut taken = vec![false; grid.cols * grid.rows];
    for c in &frame.cells {
        taken[c.row * grid.cols + c.col] = true;
        if let Some(&(font, id)) = g.ids.get(&c.ch) {
            // The mascot's colors: paper fur, ink mask, amber eyes. The fur
            // keeps the drawing's shading through how bright it is.
            let color = match (theme::is_light(), c.part) {
                (false, Part::Eye) => rgb(c.rgb, 1.),
                (true, Part::Eye) => theme::accent(),
                (false, Part::Fur) => {
                    let shade = (0.299 * c.rgb[0] + 0.587 * c.rgb[1] + 0.114 * c.rgb[2]) / 255.;
                    theme::brand_paper().opacity(shade.clamp(0.35, 1.))
                }
                (true, Part::Fur) => theme::brand_fur_on_light(),
                (_, Part::Ink) => theme::brand_ink(),
            };
            let _ = window.paint_glyph(at(c.col, c.row), font, id, size, color);
        }
    }
    if frame.dots <= 0. {
        return;
    }
    let Some(&(font, id)) = g.ids.get(&DOT) else {
        return;
    };
    for row in 0..grid.rows {
        for col in 0..grid.cols {
            let a = dot_alpha(grid, col, row) * frame.dots;
            if a > 0.004 && !taken[row * grid.cols + col] {
                let _ = window.paint_glyph(at(col, row), font, id, size, rgb(DOTS, a));
            }
        }
    }
}

impl Render for AsciiFace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.started {
            self.started = true;
            if self.scale == 1. && animated() && claim_intro() {
                self.intro = Some(0.);
            }
        }
        if std::mem::take(&mut self.hidden) && self.intro.is_none() && animated() {
            self.fade = Some(0.);
            self.last = None;
        }
        self.rendered = true;
        self.active = window.is_window_active();
        let (weak, moves) = (cx.weak_entity(), cx.weak_entity());
        div().w_full().h(px(HEIGHT * self.scale)).child(
            canvas(
                move |b, window, cx| weak.update(cx, |f, _| f.frame(b, window)).ok(),
                move |b, frame, window, _| {
                    if let Some(frame) = frame {
                        paint(&frame, b, window);
                    }
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            let _ =
                                moves.update(cx, |f, _| f.pointer = Some((e.position, f.now())));
                        }
                    });
                },
            )
            .size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 860.;

    fn lit(pose: &Pose) -> (usize, usize) {
        let cells = face_cells(pose, Grid::new(W));
        let eyes = cells
            .iter()
            .filter(|c| matches!(c.ch, '█' | '▓' | '░'))
            .count();
        (eyes, cells.len() - eyes)
    }

    #[test]
    fn intro_is_claimed_once() {
        let first = claim_intro();
        assert!(!claim_intro());
        assert!(INTRO_SEEN.load(Ordering::Relaxed) || first);
    }

    #[test]
    fn amber_is_told_from_fur_and_ink() {
        assert!(is_amber(AMBER));
        assert!(is_amber(AMBER.map(|v| v * 0.5)));
        assert!(!is_amber([150., 150., 155.]));
        assert!(!is_amber(EYE_CAP));
        assert!(!is_amber([0.; 3]));
    }

    #[test]
    fn the_face_reads_and_stays_in_its_grid() {
        let pose = Pose::still(W);
        let grid = Grid::new(W);
        let cells = face_cells(&pose, grid);
        assert!(cells.iter().all(|c| c.col < grid.cols && c.row < grid.rows));
        let (eyes, fur) = lit(&pose);
        assert!(eyes > 10, "the eyes show as blocks: {eyes}");
        assert!(fur > 300, "and the fur as characters: {fur}");
    }

    #[test]
    fn the_intro_starts_with_only_the_eyes() {
        let pose = Pose {
            reveal: Some((1., 0., 0.35)),
            glow: 1.,
            ..Pose::still(W)
        };
        let (eyes, fur) = lit(&pose);
        assert!(eyes > 10);
        assert!(fur < eyes / 2, "almost nothing but the eyes: {fur}");
        let dark = Pose {
            reveal: Some((0., 0., 0.35)),
            ..pose
        };
        assert_eq!(lit(&dark), (0, 0));
    }
}
