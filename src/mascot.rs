//! Arakun's raccoon, alive: the brand drawing on springs, with eyes that
//! follow the cursor and blink, and ears that bend past their base.
//! A small brain turns what the agent is doing into poses over time, and
//! each state reads at a glance: one gaze, one rhythm and one symbol over the
//! head per state.
//!
//! The drawing is `assets/brand/raccoon.svg` as the designer made it. The
//! prototype page `shots/mascota-preview/` runs the same motion in a browser;
//! keep the two in step.

use std::cell::Cell;
use std::collections::HashSet;
use std::f32::consts::{PI, TAU};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    Bounds, ContentMask, Context, DispatchPhase, Hsla, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, PathBuilder, PathStyle, Pixels, Point, Render,
    StrokeOptions, Styled as _, Subscription, Task, Window, canvas, div, point, px,
};
use lyon_tessellation::{LineCap, LineJoin};

use crate::theme;

/// What the agent behind a mascot is doing. `Done` and `Sleeping` also come
/// on their own: a busy mood turning idle plays `Done`, and a long idle dozes off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mood {
    Idle,
    Thinking,
    Working,
    Waiting,
    Done,
    Error,
    Sleeping,
}

/// What the raccoon is showing: the agent's mood, or you typing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Idle,
    Typing,
    Thinking,
    Working,
    Waiting,
    Done,
    Error,
    Sleeping,
}

/// The drawing's box in the SVG's units (the head is 178.54 wide), with room
/// for hops and perked ears; symbols float above it.
const VB_X: f32 = -5.;
const VB_Y: f32 = -8.;
const VB_W: f32 = 188.54;
const VB_H: f32 = 152.;
/// The head's centre line and chin; it squashes and turns about the chin.
const CX: f32 = 89.27;
const FLOOR: f32 = 137.16;
/// The prompter's top edge under a shy raccoon (the chat's): at rest it sits
/// whole on it, and anything that sinks below is cut there.
const LEDGE_Y: f32 = 135.;
/// How far it sinks to spy over that edge (eyes half over it), and to hide
/// behind it altogether while it moves on top of the card above.
const AWAY_Y: f32 = 61.;
const HIDE_Y: f32 = 140.;
/// How long a spy over the edge lasts before it comes back up, and a hide.
const SPY_FOR: f64 = 3.;
const HIDE_FOR: f64 = 0.5;
/// No input for this long and the raccoon starts nodding off, then sleeps.
const DOZE_AFTER: f64 = 90.;
const SLEEP_AFTER: f64 = 150.;
/// Below this width the eyes are drawn bigger and the symbols left out.
const SMALL_W: f32 = 48.;
/// Typing pauses this long and the raccoon looks up again.
const TYPING_FOR: f64 = 1.1;

static ANIMATED: AtomicBool = AtomicBool::new(true);

/// Off, every raccoon settles with its eyes open and ahead, and holds still.
pub fn set_animated(on: bool) {
    ANIMATED.store(on, Ordering::Relaxed);
}

pub(crate) fn animated() -> bool {
    ANIMATED.load(Ordering::Relaxed)
}

static DANCING: AtomicBool = AtomicBool::new(false);

/// Flips the chat raccoons' dance mode; returns the new mode.
pub fn toggle_dancing() -> bool {
    !DANCING.fetch_xor(true, Ordering::Relaxed)
}

fn dancing() -> bool {
    DANCING.load(Ordering::Relaxed)
}

/// One beat at 118 BPM.
const BEAT: f64 = 60. / 118.;

/// Only raccoons allowed to dance do, and not while they show that you are
/// needed (a question, an approval) or that something broke.
fn dances(can: bool, mode: bool, state: State) -> bool {
    can && mode && !matches!(state, State::Waiting | State::Error)
}

fn smoothstep(a: f32, b: f32, v: f32) -> f32 {
    let t = ((v - a) / (b - a)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

// ---------- Drawing ----------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    M([f32; 2]),
    L([f32; 2]),
    C([f32; 6]),
    Z,
}

impl Seg {
    fn points(&self) -> &[f32] {
        match self {
            Seg::M(p) | Seg::L(p) => p,
            Seg::C(p) => p,
            Seg::Z => &[],
        }
    }
}

enum Tok {
    Cmd(char),
    Num(f32),
}

/// Path data split into commands and numbers. Numbers may run together the
/// way exporters write them: `-.46.21` is `-0.46 0.21`.
fn tokens(d: &str) -> Vec<Tok> {
    let (mut out, mut num, mut dot) = (Vec::new(), String::new(), false);
    let flush = |num: &mut String, dot: &mut bool, out: &mut Vec<Tok>| {
        if let Ok(v) = num.parse() {
            out.push(Tok::Num(v));
        }
        num.clear();
        *dot = false;
    };
    for ch in d.chars() {
        match ch {
            c if c.is_ascii_alphabetic() => {
                flush(&mut num, &mut dot, &mut out);
                out.push(Tok::Cmd(c));
            }
            '-' | '+' => {
                flush(&mut num, &mut dot, &mut out);
                num.push(ch);
            }
            '.' => {
                if dot {
                    flush(&mut num, &mut dot, &mut out);
                }
                dot = true;
                num.push('.');
            }
            c if c.is_ascii_digit() => num.push(c),
            _ => flush(&mut num, &mut dot, &mut out),
        }
    }
    flush(&mut num, &mut dot, &mut out);
    out
}

/// SVG path data (`M L H V C S Z`, absolute or relative) as absolute
/// moves, lines and cubics.
fn parse_path(d: &str) -> Vec<Seg> {
    let arity = |c: char| match c.to_ascii_uppercase() {
        'M' | 'L' => 2,
        'H' | 'V' => 1,
        'C' => 6,
        'S' => 4,
        _ => 0,
    };
    let (mut out, mut nums) = (Vec::new(), Vec::<f32>::new());
    let (mut cmd, mut cur, mut start, mut ctrl) = ('M', [0f32; 2], [0f32; 2], None::<[f32; 2]>);
    for tok in tokens(d) {
        match tok {
            Tok::Cmd(c) => {
                cmd = c;
                nums.clear();
                if c.eq_ignore_ascii_case(&'z') {
                    out.push(Seg::Z);
                    cur = start;
                    ctrl = None;
                }
            }
            Tok::Num(v) => {
                nums.push(v);
                if arity(cmd) == 0 || nums.len() < arity(cmd) {
                    continue;
                }
                let rel = cmd.is_ascii_lowercase();
                let o = if rel { cur } else { [0., 0.] };
                let at = |i: usize| [o[0] + nums[i], o[1] + nums[i + 1]];
                match cmd.to_ascii_uppercase() {
                    'M' => {
                        cur = at(0);
                        start = cur;
                        out.push(Seg::M(cur));
                        ctrl = None;
                        // Further pairs after a move are lines.
                        cmd = if rel { 'l' } else { 'L' };
                    }
                    'L' => {
                        cur = at(0);
                        out.push(Seg::L(cur));
                        ctrl = None;
                    }
                    'H' => {
                        cur[0] = o[0] + nums[0];
                        out.push(Seg::L(cur));
                        ctrl = None;
                    }
                    'V' => {
                        cur[1] = o[1] + nums[0];
                        out.push(Seg::L(cur));
                        ctrl = None;
                    }
                    'C' => {
                        let (a, b, e) = (at(0), at(2), at(4));
                        out.push(Seg::C([a[0], a[1], b[0], b[1], e[0], e[1]]));
                        ctrl = Some(b);
                        cur = e;
                    }
                    _ => {
                        // S: the first control mirrors the last one.
                        let a = ctrl.map_or(cur, |c| [2. * cur[0] - c[0], 2. * cur[1] - c[1]]);
                        let (b, e) = (at(0), at(2));
                        out.push(Seg::C([a[0], a[1], b[0], b[1], e[0], e[1]]));
                        ctrl = Some(b);
                        cur = e;
                    }
                }
                nums.clear();
            }
        }
    }
    out
}

/// Every path's `d` and every rect's box (x, y, w, h), in document order.
fn svg_parts(svg: &str) -> (Vec<String>, Vec<[f32; 4]>) {
    let attr = |tag: &str, name: &str| -> Option<String> {
        let key = format!(" {name}=\"");
        let i = tag.find(&key)? + key.len();
        tag[i..].split('"').next().map(str::to_string)
    };
    let (mut paths, mut rects) = (Vec::new(), Vec::new());
    for tag in svg.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or_default();
        if tag.starts_with("path") {
            paths.extend(attr(tag, "d"));
        } else if tag.starts_with("rect") {
            let f = |n: &str| attr(tag, n).and_then(|v| v.parse().ok()).unwrap_or(0.);
            rects.push([f("x"), f("y"), f("width"), f("height")]);
        }
    }
    (paths, rects)
}

/// An ear's base line through `o` along `d`, and `n` from it toward the tip;
/// the right ear mirrors the left about the centre line.
struct Ear {
    o: [f32; 2],
    d: [f32; 2],
    n: [f32; 2],
}

const EARS: [Ear; 2] = [
    Ear {
        o: [39.5, 28.5],
        d: [0.783, -0.622],
        n: [-0.622, -0.783],
    },
    Ear {
        o: [139.04, 28.5],
        d: [-0.783, -0.622],
        n: [0.622, -0.783],
    },
];

fn side_of(q: [f32; 2]) -> usize {
    usize::from(q[0] >= CX)
}

/// How far past its ear's base a point sits.
fn past_base(q: [f32; 2]) -> f32 {
    let e = &EARS[side_of(q)];
    (q[0] - e.o[0]) * e.n[0] + (q[1] - e.o[1]) * e.n[1]
}

/// An ear bent by `ks`: `k` shears it sideways along its base, `s` stretches
/// it up from it. `w` is how much of it a point takes, so the outline bends
/// at the base as one curve and no seam can open there.
fn ear_bend(q: [f32; 2], w: f32, ks: [f32; 2]) -> [f32; 2] {
    if w <= 0. {
        return q;
    }
    let e = &EARS[side_of(q)];
    let v = [q[0] - e.o[0], q[1] - e.o[1]];
    let (along, up) = (v[0] * e.d[0] + v[1] * e.d[1], v[0] * e.n[0] + v[1] * e.n[1]);
    let (along, up) = (along + ks[0] * up, ks[1] * up);
    let m = [
        e.o[0] + e.d[0] * along + e.n[0] * up,
        e.o[1] + e.d[1] * along + e.n[1] * up,
    ];
    [q[0] + (m[0] - q[0]) * w, q[1] + (m[1] - q[1]) * w]
}

/// A path with, per point, how much it bends with its ear.
struct Shape {
    segs: Vec<Seg>,
    bend: Vec<f32>,
}

impl Shape {
    fn new(d: &str) -> Self {
        let segs = parse_path(d);
        let bend = segs
            .iter()
            .flat_map(|s| {
                s.points()
                    .chunks(2)
                    .map(|p| smoothstep(-1.5, 10., past_base([p[0], p[1]])))
                    .collect::<Vec<_>>()
            })
            .collect();
        Shape { segs, bend }
    }
}

struct Geo {
    head: Shape,
    ear_in: [Shape; 2],
    mask: Vec<Seg>,
    nose: Vec<Seg>,
    eye_at: [[f32; 2]; 2],
    eye_size: [f32; 2],
}

/// The SVG's head (ears included), the two ear insides, the mask with its
/// stripe and the nose, in that order, then the two eyes.
fn geo() -> &'static Geo {
    static GEO: OnceLock<Geo> = OnceLock::new();
    GEO.get_or_init(|| {
        let (paths, rects) = svg_parts(include_str!("../assets/brand/raccoon.svg"));
        assert!(
            paths.len() >= 5 && rects.len() >= 2,
            "raccoon.svg: head, ear insides, mask, nose and two eyes"
        );
        let centre = |r: [f32; 4]| [r[0] + r[2] / 2., r[1] + r[3] / 2.];
        Geo {
            head: Shape::new(&paths[0]),
            ear_in: [Shape::new(&paths[1]), Shape::new(&paths[2])],
            mask: parse_path(&paths[3]),
            nose: parse_path(&paths[4]),
            eye_at: [centre(rects[0]), centre(rects[1])],
            eye_size: [rects[0][2], rects[0][3]],
        }
    })
}

// ---------- Springs ----------

#[derive(Clone, Copy)]
pub(crate) struct Spring {
    pub(crate) x: f32,
    v: f32,
    /// Natural frequency in Hz and damping ratio.
    f: f32,
    z: f32,
}

impl Spring {
    pub(crate) const fn new(x: f32, f: f32, z: f32) -> Self {
        Spring { x, v: 0., f, z }
    }

    /// From a stiffness and a damping, the way the prototype page tunes them.
    fn kc(x: f32, k: f32, c: f32) -> Self {
        let w = k.sqrt();
        Spring::new(x, w / TAU, c / (2. * w))
    }

    pub(crate) fn step(&mut self, target: f32, dt: f32) {
        let w = TAU * self.f;
        self.v += (w * w * (target - self.x) - 2. * self.z * w * self.v) * dt;
        self.x += self.v * dt;
    }

    fn settled(&self, target: f32, eps: f32) -> bool {
        (self.x - target).abs() < eps && self.v.abs() < eps * 4.
    }
}

// ---------- Brain ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Twitch,
    Sniff,
    Tilt,
    Boop,
    Wake,
    Wink,
}

impl Act {
    fn dur(self) -> f64 {
        match self {
            Act::Twitch => 0.2,
            Act::Sniff => 1.1,
            Act::Tilt => 1.5,
            Act::Boop => 1.2,
            Act::Wake => 0.8,
            Act::Wink => 0.75,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Eyes {
    #[default]
    Open,
    Happy,
    Cross,
}

/// The symbol over the head; one per state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prop {
    Dots,
    Code,
    Ask,
    Bang,
    Check,
    Pencil,
    Zzz,
    Notes,
}

const PROPS: [Prop; 8] = [
    Prop::Dots,
    Prop::Code,
    Prop::Ask,
    Prop::Bang,
    Prop::Check,
    Prop::Pencil,
    Prop::Zzz,
    Prop::Notes,
];

/// Indent and length of the lines the working raccoon writes, in a loop.
const CODE_LINES: [(f32, f32); 7] = [
    (0., 34.),
    (8., 22.),
    (8., 40.),
    (16., 16.),
    (8., 30.),
    (0., 26.),
    (0., 44.),
];
/// Seconds each of those lines takes; the eyes sweep once per line.
const CODE_PERIOD: f64 = 1.25;

/// Which line is being written `t` seconds into the work, and how far along it is.
fn code_line(t: f64) -> (i64, f32) {
    let t = t.max(0.);
    (
        (t / CODE_PERIOD).floor() as i64,
        ((t % CODE_PERIOD) / CODE_PERIOD) as f32,
    )
}

/// One frame's intent.
struct Pose {
    /// x, y, rotation (degrees), scale x, scale y.
    head: [f32; 5],
    /// Per ear: shear along its base and stretch up from it.
    ears: [[f32; 2]; 2],
    look: [f32; 2],
    /// Eye width and height factors, and tilt in degrees (mirrored per eye).
    eye: [f32; 3],
    eyes: Eyes,
    /// The eye (0 left, 1 right) that smiles on its own.
    wink: Option<usize>,
    nose: f32,
    blink: bool,
    prop: Option<Prop>,
}

impl Default for Pose {
    fn default() -> Self {
        Pose {
            head: [0., 0., 0., 1., 1.],
            ears: [[0., 1.]; 2],
            look: [0., 0.],
            eye: [1., 1., 0.],
            eyes: Eyes::Open,
            wink: None,
            nose: 1.,
            blink: true,
            prop: None,
        }
    }
}

/// Something painted in the raccoon's colour at an opacity: a filled outline,
/// or a round-capped line `width` units thick.
enum Mark {
    Fill(Vec<[f32; 2]>, f32),
    Line(Vec<[f32; 2]>, f32, f32),
}

/// What gets painted, in the drawing's units with the motion applied.
struct Frame {
    /// The head, with the ear insides, the mask and the nose as holes.
    body: Vec<Seg>,
    /// How many of `body`'s segments are the head's outline alone: painted
    /// dark underneath, so the holes never show what is behind the raccoon.
    outline: usize,
    marks: Vec<Mark>,
    /// Paint is cut below this line (a shy raccoon behind the prompter).
    ledge: Option<f32>,
    /// Px the whole drawing sits above its box (on the card over the prompter).
    lift: f32,
}

pub struct Mascot {
    width: f32,
    small: bool,
    start: Instant,
    last_frame: Option<f64>,
    rendered: bool,
    active: bool,
    moving: bool,

    mood: Mood,
    state: State,
    t0: f64,
    auto_sleep: bool,
    act: Option<(Act, f64, f32)>,
    act_first: bool,
    next_act: f64,
    fired: HashSet<u64>,
    rng: u64,

    pointer: Option<Point<Pixels>>,
    pointer_dirty: bool,
    last_move: f64,
    last_input: f64,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    wander: [f32; 2],
    next_wander: f64,

    last_key: f64,
    keys: u32,
    caret: f32,
    /// Your attention is elsewhere, this way; it spies there until `spy_until`.
    away: Option<[f32; 2]>,
    spy_until: f64,
    /// Sits on the prompter's edge and clips there.
    shy: bool,
    /// Px it sits above the prompter (on the card there), the height it is
    /// moving to, and until when it hides to get there.
    perch: f32,
    perch_want: f32,
    hide_until: f64,

    next_blink: f64,
    blink_t: Option<f64>,

    head: [Spring; 5],
    ears: [[Spring; 2]; 2],
    gaze: [Spring; 2],
    eye: [Spring; 3],
    nose: Spring,
    prop: Option<Prop>,
    prop_t0: f64,
    prop_a: [f32; PROPS.len()],

    can_dance: bool,
    /// Moves even in a window that never takes focus (the island's layer).
    awake: bool,
    /// Painted in this instead of the theme's accent.
    color: Option<Hsla>,
    dance_t0: Option<f64>,

    _tick: Task<()>,
    _keys: Subscription,
}

impl Mascot {
    /// A mascot `width` px wide; its height follows the drawing (about 0.8).
    pub fn new(width: f32, cx: &mut Context<Self>) -> Self {
        // Wakes often but only redraws while something moves or is due, so a
        // settled raccoon costs no frames.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let alive = this.update(cx, |m, cx| {
                    if m.wants_frame() {
                        m.rendered = false;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let keys = cx.observe_keystrokes(|m, _, _, _| m.input());
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9, |d| d.as_nanos() as u64)
            | 1;
        let head = [
            Spring::kc(0., 300., 24.),
            Spring::kc(0., 300., 22.),
            Spring::kc(0., 200., 20.),
            Spring::kc(1., 300., 20.),
            Spring::kc(1., 300., 20.),
        ];
        let ear = [Spring::kc(0., 200., 11.), Spring::kc(1., 200., 11.)];
        Mascot {
            width,
            small: width < SMALL_W,
            start: Instant::now(),
            last_frame: None,
            rendered: true,
            active: true,
            moving: true,
            mood: Mood::Idle,
            state: State::Idle,
            t0: 0.,
            auto_sleep: false,
            act: None,
            act_first: false,
            next_act: 3.,
            fired: HashSet::new(),
            rng: seed,
            pointer: None,
            pointer_dirty: false,
            last_move: -1e9,
            last_input: 0.,
            bounds: Rc::default(),
            wander: [0.; 2],
            next_wander: 0.,
            last_key: -1e9,
            keys: 0,
            caret: 0.,
            away: None,
            spy_until: 0.,
            shy: false,
            perch: 0.,
            perch_want: 0.,
            hide_until: 0.,
            next_blink: 1.5,
            blink_t: None,
            head,
            ears: [ear; 2],
            gaze: [Spring::kc(0., 170., 22.); 2],
            eye: [
                Spring::kc(1., 400., 32.),
                Spring::kc(1., 400., 32.),
                Spring::kc(0., 300., 28.),
            ],
            nose: Spring::kc(1., 500., 30.),
            prop: None,
            prop_t0: 0.,
            prop_a: [0.; PROPS.len()],
            can_dance: false,
            awake: false,
            color: None,
            dance_t0: None,
            _tick: tick,
            _keys: keys,
        }
    }

    /// Lets this raccoon dance when the dance mode is on (the chat's does).
    pub fn dancer(mut self) -> Self {
        self.can_dance = true;
        self
    }

    /// Sits on the prompter's edge: it hides its chin behind it, spies over
    /// the edge for a moment when your attention goes elsewhere, and moves
    /// on top of the card above the prompter when one shows.
    pub fn shy(mut self) -> Self {
        self.shy = true;
        self
    }

    /// Keeps moving while its window is not focused.
    #[allow(dead_code, reason = "only the island demo has an unfocused window")]
    pub fn awake(mut self) -> Self {
        self.awake = true;
        self
    }

    /// Paints it in `color` instead of the theme's accent.
    #[allow(dead_code, reason = "only the island uses it")]
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Px from the top of the raccoon's box down to the prompter's edge, for
    /// placing a shy one.
    pub fn ledge(&self) -> f32 {
        (LEDGE_Y - VB_Y) * self.width / VB_W
    }

    /// Follows the agent. Turning idle after busy work plays `Done` first.
    pub fn set_mood(&mut self, mood: Mood) {
        if mood == self.mood {
            return;
        }
        let was_busy = matches!(self.mood, Mood::Thinking | Mood::Working | Mood::Waiting);
        self.mood = mood;
        let now = self.now();
        self.last_input = now;
        let state = match mood {
            Mood::Idle if was_busy => State::Done,
            Mood::Idle => State::Idle,
            Mood::Thinking => State::Thinking,
            Mood::Working => State::Working,
            Mood::Waiting => State::Waiting,
            Mood::Done => State::Done,
            Mood::Error => State::Error,
            Mood::Sleeping => State::Sleeping,
        };
        self.enter(state, now);
    }

    /// You typed in the prompter; `caret` is how far along its line the caret
    /// is, 0 to 1. While idle it watches the text and scribbles along.
    pub fn key(&mut self, caret: f32) {
        let now = self.now();
        self.last_key = now;
        self.input();
        if !matches!(self.state, State::Idle | State::Typing) {
            return;
        }
        if self.state == State::Idle {
            self.state = State::Typing;
            self.t0 = now;
            self.act = None;
        }
        let c = caret.clamp(0., 1.) * 1.5 - 0.75;
        if c < self.caret - 0.5 {
            self.kick_ears(3.);
        }
        self.caret = c;
        self.head[1].v += 85.;
        self.head[4].v -= 0.9;
        self.ears[(self.keys % 2) as usize][0].v += 3.;
        self.keys = self.keys.wrapping_add(1);
    }

    /// You sent a message: a wink, with the ear on that side.
    pub fn sent(&mut self) {
        let now = self.now();
        self.input();
        if self.state == State::Typing {
            self.enter(State::Idle, now);
        }
        self.start_act(Act::Wink, now);
    }

    /// Your attention went elsewhere (`dir` points that way, y down) or came
    /// back (`None`). A shy idle raccoon ducks and spies that way for a
    /// moment, then comes back up looking there; if you return while it is
    /// still down, it hops back up.
    pub fn set_away(&mut self, dir: Option<[f32; 2]>) {
        if !self.shy {
            return;
        }
        let now = self.now();
        let left = dir.is_some() && self.away.is_none();
        self.away = dir;
        self.moving = true;
        // Not while the app is still starting and focus settles.
        if left && self.state == State::Idle && animated() && now > 3. {
            self.spy_until = now + SPY_FOR;
        }
        if dir.is_none() && now < self.spy_until {
            self.spy_until = 0.;
            if self.state == State::Idle && animated() {
                self.start_act(Act::Wake, now);
            }
        }
    }

    /// How many px above the prompter a shy raccoon sits: the card there, or
    /// 0. It hides behind the edge for a moment and comes out on top.
    pub fn set_perch(&mut self, px: f32) {
        if (px - self.perch_want).abs() < 0.5 {
            return;
        }
        self.perch_want = px;
        let now = self.now();
        if animated() && self.last_frame.is_some() {
            self.hide_until = now + HIDE_FOR;
        } else {
            self.perch = px;
        }
        self.moving = true;
    }

    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn rnd(&mut self, a: f64, b: f64) -> f64 {
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        a + (self.rng >> 11) as f64 / (1u64 << 53) as f64 * (b - a)
    }

    fn chance(&mut self, p: f64) -> bool {
        self.rnd(0., 1.) < p
    }

    fn once(&mut self, tag: u32, n: u32) -> bool {
        self.fired.insert(((tag as u64) << 32) | n as u64)
    }

    fn kick_ears(&mut self, v: f32) {
        self.ears[0][1].v += v;
        self.ears[1][1].v += v;
    }

    fn enter(&mut self, state: State, now: f64) {
        self.state = state;
        self.t0 = now;
        self.act = None;
        self.auto_sleep = false;
        self.fired.clear();
    }

    fn start_act(&mut self, act: Act, now: f64) {
        let side = if self.chance(0.5) { -1. } else { 1. };
        self.act = Some((act, now, side));
        self.act_first = true;
    }

    fn input(&mut self) {
        let now = self.now();
        self.last_input = now;
        if self.auto_sleep && self.state == State::Sleeping {
            self.enter(State::Idle, now);
            self.start_act(Act::Wake, now);
        }
    }

    fn pointer_moved(&mut self, at: Point<Pixels>) {
        self.pointer = Some(at);
        self.pointer_dirty = true;
        self.last_move = self.now();
        self.input();
    }

    fn boop(&mut self) {
        let now = self.now();
        self.input();
        if self.state == State::Sleeping {
            self.enter(State::Idle, now);
        }
        self.start_act(Act::Boop, now);
    }

    fn cursor(&self, now: f64) -> Option<[f32; 2]> {
        let (p, b) = (self.pointer?, self.bounds.get()?);
        if now - self.last_move > 2.5 {
            return None;
        }
        let (cx, cy) = (
            b.center().x.as_f32(),
            b.origin.y.as_f32() - self.perch + b.size.height.as_f32() * 0.65,
        );
        let (vx, vy) = ((p.x.as_f32() - cx) / 320., (p.y.as_f32() - cy) / 260.);
        let m = vx.hypot(vy);
        Some(if m > 1. { [vx / m, vy / m] } else { [vx, vy] })
    }

    fn wander_at(&mut self, now: f64) -> [f32; 2] {
        if now > self.next_wander {
            let a = self.rnd(0., std::f64::consts::TAU) as f32;
            let m = if self.chance(0.4) {
                0.
            } else {
                self.rnd(0.3, 0.75) as f32
            };
            self.wander = [a.cos() * m, a.sin() * m * 0.7];
            self.next_wander = now + self.rnd(1.4, 3.6);
        }
        self.wander
    }

    fn plan(&mut self, now: f64) -> Pose {
        let mut p = Pose::default();
        if !animated() {
            self.act = None;
            p.blink = false;
            if self.state == State::Sleeping {
                p.eye[1] = 0.13;
            }
        } else if dances(self.can_dance, dancing(), self.state) {
            self.dance(&mut p, now);
        } else {
            self.dance_t0 = None;
            let t = now - self.t0;
            match self.state {
                State::Idle => self.idle(&mut p, now),
                State::Typing => self.typing(&mut p, now),
                State::Thinking => self.thinking(&mut p, t, now),
                State::Working => self.working(&mut p, t, now),
                State::Waiting => self.waiting(&mut p, t, now),
                State::Done => self.done(&mut p, t, now),
                State::Error => self.error(&mut p, t, now),
                State::Sleeping => self.sleeping(&mut p, t),
            }
            if let Some((act, t0, side)) = self.act {
                let at = now - t0;
                if at > act.dur() {
                    self.act = None;
                } else {
                    self.run_act(act, at, side, now, &mut p);
                }
            }
        }
        // Moving on top of the card above the prompter: down behind the edge,
        // over, and up again once there.
        if self.perch != self.perch_want {
            if now < self.hide_until {
                p.head[0] = 0.;
                p.head[1] = HIDE_Y;
                p.prop = None;
            } else {
                self.perch = self.perch_want;
                self.kick_ears(3.);
            }
        }
        p
    }

    fn idle(&mut self, p: &mut Pose, now: f64) {
        let spying = self.away.filter(|_| now < self.spy_until);
        match (spying, self.away) {
            (Some(d), _) => self.spy(p, now, d),
            // Back up, it keeps an eye on where you went.
            (None, Some(d)) => {
                p.look = self
                    .cursor(now)
                    .unwrap_or([d[0] * 0.7, d[1].min(0.4) * 0.7])
            }
            (None, None) => p.look = self.cursor(now).unwrap_or_else(|| self.wander_at(now)),
        }
        let quiet = now - self.last_input;
        if quiet > SLEEP_AFTER {
            self.enter(State::Sleeping, now);
            self.auto_sleep = true;
            return;
        }
        if quiet > DOZE_AFTER {
            p.eye[1] = 0.45;
            p.ears = [[-0.2, 0.92]; 2];
            return;
        }
        if spying.is_none() && self.act.is_none() && now > self.next_act {
            let pool = [Act::Twitch, Act::Sniff, Act::Tilt, Act::Twitch, Act::Sniff];
            let act = pool[self.rnd(0., pool.len() as f64) as usize % pool.len()];
            self.start_act(act, now);
            self.next_act = now + self.rnd(3.5, 7.);
        }
    }

    /// Down behind the edge, spying toward where you went, once a little
    /// higher for a better look.
    fn spy(&mut self, p: &mut Pose, now: f64, d: [f32; 2]) {
        let t = now - (self.spy_until - SPY_FOR);
        p.head[1] = if (1.5..2.1).contains(&t) {
            AWAY_Y - 14.
        } else {
            AWAY_Y
        };
        p.head[2] = d[0] * 4.;
        p.look = [d[0], d[1].min(0.4)];
        p.eye = [1.08, 1.1, 0.];
        p.ears[0] = if d[0] < -0.3 { [0.1, 1.18] } else { [0., 1.02] };
        p.ears[1] = if d[0] > 0.3 { [0.1, 1.18] } else { [0., 1.02] };
    }

    /// Eyes on the text, following the caret, a bob per key and a pencil
    /// scribbling the line over its head.
    fn typing(&mut self, p: &mut Pose, now: f64) {
        if now - self.last_key > TYPING_FOR {
            self.enter(State::Idle, now);
            self.start_act(Act::Tilt, now);
            self.next_act = now + self.rnd(4., 7.);
            return;
        }
        p.ears = [[0.12, 1.14]; 2];
        p.look = [self.caret * 1.2, 1.];
        p.eye = [1.06, 0.9, 0.];
        p.head[1] = 6.;
        p.head[2] = self.caret * 8.;
        p.prop = Some(Prop::Pencil);
    }

    /// Slow and tilted, eyes up and away: the only state that looks away from
    /// you and the text.
    fn thinking(&mut self, p: &mut Pose, t: f64, now: f64) {
        let side = if (t / 3.2).floor() as i64 % 2 == 1 {
            -1.
        } else {
            1.
        };
        p.head[2] = 8. * side;
        p.head[1] = -1.;
        p.eye[1] = 0.92;
        p.look = [0.75 * side + 0.1 * (now * 1.3).sin() as f32, -0.95];
        p.ears = if side > 0. {
            [[-0.1, 0.96], [0.14, 1.12]]
        } else {
            [[0.14, 1.12], [-0.1, 0.96]]
        };
        p.prop = Some(Prop::Dots);
    }

    /// Head down, narrowed eyes following each line of code as it is written
    /// overhead, glancing up at you every fifth line.
    fn working(&mut self, p: &mut Pose, t: f64, now: f64) {
        let (n, c) = code_line(t);
        let n = n as u32;
        let lx = -0.75 + 1.5 * (c / 0.85).clamp(0., 1.);
        if self.once(1, n) {
            self.head[1].v += 40.;
        }
        p.prop = Some(Prop::Code);
        if n % 5 == 4 && c > 0.2 {
            p.look = self.cursor(now).unwrap_or([0., -0.1]);
            return;
        }
        p.look = [lx, 0.85];
        p.eye = [1., 0.55, -9.];
        p.head[1] = 4.;
        p.head[2] = lx * 1.5;
        p.ears = [[-0.12, 0.92]; 2];
    }

    /// Big eyes locked on you and a hop that keeps coming back: it cannot go
    /// on without you.
    fn waiting(&mut self, p: &mut Pose, t: f64, now: f64) {
        let (c, n) = (t % 2., (t / 2.).floor() as u32);
        p.head[2] = if n % 2 == 1 { -5. } else { 5. };
        if c < 0.12 {
            p.head[1] = 2.;
            (p.head[3], p.head[4]) = (1.05, 0.92);
        } else if c < 0.45 {
            p.head[1] = -12.;
            (p.head[3], p.head[4]) = (0.97, 1.05);
        } else if c < 0.58 {
            p.head[1] = 1.;
            (p.head[3], p.head[4]) = (1.04, 0.94);
        } else if self.once(2, n) {
            self.kick_ears(2.2);
        }
        p.look = self.cursor(now).unwrap_or([0., 0.]);
        p.eye = [1.12, 1.15, 0.];
        p.ears = [[0.1, 1.16]; 2];
        p.prop = Some(Prop::Ask);
    }

    /// One hop, smiling eyes and a tick, then back to rest.
    fn done(&mut self, p: &mut Pose, t: f64, now: f64) {
        if t > 2.8 {
            self.enter(State::Idle, now);
            return;
        }
        p.prop = Some(Prop::Check);
        p.ears = [[0.1, 1.12]; 2];
        if t < 0.18 {
            p.head[1] = 3.;
            (p.head[3], p.head[4]) = (1.07, 0.88);
            p.eye = [1.15, 1.2, 0.];
        } else if t < 0.55 {
            p.head[1] = -16.;
            (p.head[3], p.head[4]) = (0.96, 1.06);
            p.eyes = Eyes::Happy;
        } else if t < 0.7 {
            p.head[1] = 1.;
            (p.head[3], p.head[4]) = (1.05, 0.93);
            p.eyes = Eyes::Happy;
            if self.once(3, 0) {
                self.kick_ears(2.6);
            }
        } else if t < 2.5 {
            p.eyes = Eyes::Happy;
            p.head[2] = 5. * ((t - 0.7) * 6.).sin() as f32;
        }
    }

    /// A shake with crossed eyes, then drooped ears and a sad look down.
    fn error(&mut self, p: &mut Pose, t: f64, now: f64) {
        p.prop = Some(Prop::Bang);
        p.ears = [[-0.45, 0.72]; 2];
        if t < 0.8 {
            p.head[0] = 8. * ((t * 42.).sin() * (-3.5 * t).exp()) as f32;
            p.eyes = Eyes::Cross;
            return;
        }
        let c = (t - 0.8) % 4.5;
        p.eye = [1., 0.72, 17.];
        p.head[1] = 6.;
        p.head[4] = 1. + 0.02 * (t * 1.2).sin() as f32;
        p.look = if c > 3.4 {
            self.cursor(now).unwrap_or([0., -0.2])
        } else {
            [0., 0.75]
        };
    }

    fn sleeping(&mut self, p: &mut Pose, t: f64) {
        let b = (t * 1.3).sin() as f32;
        p.eye = [1.2, 0.13, 0.];
        p.head[1] = 7. + 1.5 * b;
        p.head[2] = -3.;
        p.head[4] = 1. + 0.03 * b;
        p.look = [0., 0.5];
        p.blink = false;
        p.ears = [[-0.38, 0.78]; 2];
        p.prop = Some(Prop::Zzz);
    }

    /// Bounces on the beat and sways every two, smiling half the phrases.
    /// Dancing keeps it awake.
    fn dance(&mut self, p: &mut Pose, now: f64) {
        let t = now - *self.dance_t0.get_or_insert(now);
        let b = t / BEAT;
        let n = b.floor() as u32;
        let pb = std::f64::consts::PI * b;
        p.head[1] = -11. * pb.sin().abs() as f32;
        p.head[2] = 7. * (pb / 2.).sin() as f32;
        p.eyes = if (b / 8.).floor() as i64 % 2 == 1 {
            Eyes::Open
        } else {
            Eyes::Happy
        };
        p.look = [0.5 * (pb / 2.).sin() as f32, 0.];
        if self.once(4, n) {
            self.ears[(n % 2) as usize][1].v += 2.4;
        }
        p.prop = Some(Prop::Notes);
        self.last_input = now;
    }

    fn run_act(&mut self, act: Act, t: f64, side: f32, now: f64, p: &mut Pose) {
        let first = std::mem::take(&mut self.act_first);
        let tilt = |p: &mut Pose| {
            p.ears = if side > 0. {
                [[-0.1, 0.97], [0.12, 1.1]]
            } else {
                [[0.12, 1.1], [-0.1, 0.97]]
            };
        };
        match act {
            Act::Twitch => {
                if first {
                    self.ears[usize::from(side < 0.)][0].v += 4.;
                }
            }
            Act::Sniff => {
                p.nose = 1. + 0.2 * (t * 17.).sin().abs() as f32;
                p.head[1] = -2.;
                p.ears = [[0.05, 1.07]; 2];
            }
            Act::Tilt => {
                p.head[2] = 8. * side;
                p.eye[1] = 1.08;
                tilt(p);
            }
            Act::Boop => {
                if first {
                    self.head[4].v -= 3.;
                    self.head[3].v += 1.6;
                    self.kick_ears(3.);
                }
                if t < 0.3 {
                    p.eye = [1.15, 1.2, 0.];
                } else {
                    p.eyes = Eyes::Happy;
                }
                p.ears = [[0.1, 1.1]; 2];
            }
            Act::Wake => {
                if first {
                    self.head[1].v -= 130.;
                    self.kick_ears(3.);
                }
                if t < 0.45 {
                    p.eye = [1.15, 1.22, 0.];
                }
                p.look = self.cursor(now).unwrap_or([0., 0.]);
            }
            Act::Wink => {
                let eye = usize::from(side > 0.);
                if first {
                    self.ears[eye][0].v += 4.5;
                    self.head[1].v += 45.;
                }
                p.wink = Some(eye);
                p.head[2] = 6. * side;
                p.ears = [[0.08, 1.1]; 2];
            }
        }
    }

    /// When an idle raccoon next has something to do, so frames can stop until then.
    fn next_event(&self) -> f64 {
        let mut t = self
            .next_blink
            .min(self.next_act)
            .min(self.last_input + DOZE_AFTER)
            .min(self.last_input + SLEEP_AFTER);
        if self.pointer.is_none() || self.now() - self.last_move > 2.5 {
            t = t.min(self.next_wander);
        }
        t
    }

    fn wants_frame(&self) -> bool {
        if !self.rendered {
            return false;
        }
        let now = self.now();
        // A window in the background gets no new motion, but a blink under
        // way finishes so the raccoon never freezes with its eyes shut.
        if !self.active {
            return self.blink_t.is_some_and(|b| now - b < 0.25);
        }
        if !animated() {
            return self.moving || self.pointer_dirty || self.blink_t.is_some();
        }
        if dances(self.can_dance, dancing(), self.state) {
            return true;
        }
        if self.state == State::Sleeping && !self.moving && !self.pointer_dirty {
            return now - self.last_frame.unwrap_or(0.) > 0.1;
        }
        self.moving
            || self.pointer_dirty
            || now < self.spy_until + 0.1
            || self.perch != self.perch_want
            || self.act.is_some()
            || self.state != State::Idle
            || self.blink_t.is_some_and(|b| now - b < 0.25)
            || now >= self.next_event()
    }

    fn step(&mut self, now: f64, dt: f32) -> Frame {
        self.pointer_dirty = false;
        let p = self.plan(now);
        let sub = (dt / 0.006).ceil().max(1.) as usize;
        let h = dt / sub as f32;
        for _ in 0..sub {
            for (s, &t) in self.head.iter_mut().zip(&p.head) {
                s.step(t, h);
            }
            for i in 0..2 {
                self.ears[i][0].step(p.ears[i][0], h);
                self.ears[i][1].step(p.ears[i][1], h);
                self.gaze[i].step(p.look[i], h);
            }
            for (s, &t) in self.eye.iter_mut().zip(&p.eye) {
                s.step(t, h);
            }
            self.nose.step(p.nose, h);
        }
        let mut fading = false;
        for (a, prop) in self.prop_a.iter_mut().zip(PROPS) {
            let want = if p.prop == Some(prop) { 1. } else { 0. };
            *a += (want - *a) * (dt * 14.).min(1.);
            fading |= (want - *a).abs() > 0.02;
        }
        if p.prop != self.prop {
            self.prop = p.prop;
            self.prop_t0 = now;
        }
        self.moving = fading
            || !(self
                .head
                .iter()
                .zip(&p.head)
                .enumerate()
                .all(|(k, (s, &t))| s.settled(t, if k >= 3 { 0.001 } else { 0.02 }))
                && (0..2).all(|i| {
                    self.ears[i][0].settled(p.ears[i][0], 0.002)
                        && self.ears[i][1].settled(p.ears[i][1], 0.002)
                        && self.gaze[i].settled(p.look[i], 0.003)
                })
                && self
                    .eye
                    .iter()
                    .zip(&p.eye)
                    .all(|(s, &t)| s.settled(t, 0.002))
                && self.nose.settled(p.nose, 0.002));

        if p.blink && p.eyes == Eyes::Open && p.eye[1] > 0.4 && now > self.next_blink {
            self.blink_t = Some(now);
            self.next_blink = now + self.rnd(2.2, 5.5);
        }
        let mut blink = 1.;
        if let Some(b) = self.blink_t {
            let u = ((now - b) / 0.2) as f32;
            if u >= 1. {
                self.blink_t = None;
            } else {
                blink = 1. - 0.9 * (PI * u).sin();
            }
        }
        self.frame(&p, blink, now)
    }

    fn frame(&self, p: &Pose, blink: f32, now: f64) -> Frame {
        let geo = geo();
        let [x, y, rot, sx, sy] = self.head.map(|s| s.x);
        let (sr, cr) = rot.to_radians().sin_cos();
        // The head's motion: squash and turn about the chin, then move.
        let rig = |q: [f32; 2]| {
            let (dx, dy) = ((q[0] - CX) * sx, (q[1] - FLOOR) * sy);
            [CX + cr * dx - sr * dy + x, FLOOR + sr * dx + cr * dy + y]
        };
        let ears = self.ears.map(|e| [e[0].x, e[1].x]);
        let bend = |s: &Shape| -> Vec<Seg> {
            let mut w = s.bend.iter();
            s.segs
                .iter()
                .map(|seg| {
                    map_seg(seg, &mut |q| {
                        let wt = w.next().copied().unwrap_or(0.);
                        rig(ear_bend(q, wt, ears[side_of(q)]))
                    })
                })
                .collect()
        };
        let [lx, ly] = self.gaze.map(|s| s.x);
        let nose = self.nose.x;
        let mut body = bend(&geo.head);
        let outline = body.len();
        body.extend(bend(&geo.ear_in[0]));
        body.extend(bend(&geo.ear_in[1]));
        body.extend(
            geo.mask
                .iter()
                .map(|s| map_seg(s, &mut |q| rig([q[0] + lx * 2.5, q[1] + ly * 2.]))),
        );
        body.extend(geo.nose.iter().map(|s| {
            map_seg(s, &mut |q| {
                rig([
                    CX + (q[0] - CX) * nose + lx * 4.5,
                    97.5 + (q[1] - 97.5) * nose + ly * 3.5,
                ])
            })
        }));

        let mut marks = Vec::new();
        let [ew, eh, tilt] = self.eye.map(|s| s.x);
        let big = if self.small { [1.5, 1.3] } else { [1., 1.] };
        for (i, c) in geo.eye_at.iter().enumerate() {
            let side = if i == 0 { 1. } else { -1. };
            let (ts, tc) = (tilt * side).to_radians().sin_cos();
            let at = [c[0] + lx * 6., c[1] + ly * 5.];
            let place = |q: [f32; 2]| {
                rig([
                    at[0] + (tc * q[0] - ts * q[1]) * big[0],
                    at[1] + (ts * q[0] + tc * q[1]) * big[1],
                ])
            };
            match if p.wink == Some(i) {
                Eyes::Happy
            } else {
                p.eyes
            } {
                Eyes::Open => {
                    let (w, h) = (
                        geo.eye_size[0] * ew,
                        (geo.eye_size[1] * eh * blink).max(2.4),
                    );
                    marks.push(Mark::Fill(
                        capsule(w, h).into_iter().map(place).collect(),
                        1.,
                    ));
                }
                Eyes::Happy => {
                    let arc = (0..=12).map(|k| {
                        let t = k as f32 / 12.;
                        let u = 1. - t;
                        place([
                            -8.5 * u * u + 8.5 * t * t,
                            5. * u * u - 22. * u * t + 5. * t * t,
                        ])
                    });
                    marks.push(Mark::Line(arc.collect(), 6. * big[0], 1.));
                }
                Eyes::Cross => {
                    marks.push(Mark::Line(
                        vec![place([-7., -7.]), place([7., 7.])],
                        5.5 * big[0],
                        1.,
                    ));
                    marks.push(Mark::Line(
                        vec![place([7., -7.]), place([-7., 7.])],
                        5.5 * big[0],
                        1.,
                    ));
                }
            }
        }
        if !self.small {
            for (&a, prop) in self.prop_a.iter().zip(PROPS) {
                if a >= 0.02 {
                    self.prop_marks(prop, a, [CX + x * 0.5, -30. + y * 0.6], now, &mut marks);
                }
            }
        }
        Frame {
            body,
            outline,
            marks,
            ledge: self.shy.then_some(LEDGE_Y),
            lift: self.perch,
        }
    }

    /// One symbol at `anchor`, `a` of the way faded in (it grows as it does).
    fn prop_marks(&self, prop: Prop, a: f32, anchor: [f32; 2], now: f64, out: &mut Vec<Mark>) {
        let s = 0.5 + 0.5 * a;
        let tp = (now
            - if self.prop == Some(prop) {
                self.prop_t0
            } else {
                now - 10.
            }) as f32;
        let now32 = now as f32;
        // Local units: translate by `tr` after turning by `deg`, all scaled.
        let at = |q: [f32; 2], tr: [f32; 2], deg: f32| {
            let (sn, cs) = deg.to_radians().sin_cos();
            [
                anchor[0] + s * (tr[0] + cs * q[0] - sn * q[1]),
                anchor[1] + s * (tr[1] + sn * q[0] + cs * q[1]),
            ]
        };
        let pts = |v: &[[f32; 2]], tr: [f32; 2], deg: f32| {
            v.iter().map(|&q| at(q, tr, deg)).collect::<Vec<_>>()
        };
        match prop {
            Prop::Dots => {
                for i in 0..3 {
                    let u = (now32 * 5.2 - i as f32 * 0.95).sin().max(0.);
                    out.push(Mark::Fill(
                        pts(&circle([-21. + 21. * i as f32, -9. * u], 7.), [0.; 2], 0.),
                        a * (0.4 + 0.6 * u),
                    ));
                }
            }
            Prop::Code => {
                // Lines of code written one after another, in step with the
                // eyes; the block scrolls up as each new line starts.
                let (k, f) = code_line(now - self.t0);
                let scroll = (f / 0.2).clamp(0., 1.);
                for i in 0..4 {
                    let j = k + i - 3;
                    if j < 0 {
                        continue;
                    }
                    let (indent, len) = CODE_LINES[j as usize % CODE_LINES.len()];
                    let len = if i == 3 {
                        len * ((f - 0.15) / 0.7).clamp(0., 1.)
                    } else {
                        len
                    };
                    let fade = if i == 0 { 1. - scroll } else { 1. };
                    if len < 6. || fade < 0.02 {
                        continue;
                    }
                    let (x, y) = (-27. + indent, -23. + (i as f32 - scroll) * 12.);
                    out.push(Mark::Line(
                        pts(&[[x + 3., y], [x + len - 3., y]], [0.; 2], 0.),
                        6. * s,
                        a * fade,
                    ));
                }
            }
            Prop::Ask => {
                let tr = [0., -7. * (now32 * PI).sin().abs()];
                let deg = 6. * (now32 * 3.1).sin();
                let mut hook: Vec<_> = (0..=14)
                    .map(|k| {
                        let t = -PI + k as f32 / 14. * 1.3 * PI;
                        [9. * t.cos(), -9. + 9. * t.sin()]
                    })
                    .collect();
                hook.push([0., 3.]);
                out.push(Mark::Line(pts(&hook, tr, deg), 7. * s, a));
                out.push(Mark::Fill(pts(&circle([0., 13.], 4.5), tr, deg), a));
            }
            Prop::Bang => {
                let (tr, k) = if tp < 0.8 {
                    ([5. * (tp * 42.).sin() * (-3. * tp).exp(), 0.], 1.)
                } else {
                    ([0.; 2], 1. + 0.07 * (now32 * 3.).sin())
                };
                let bar = [[0., -18. * k], [0., 1. * k]];
                out.push(Mark::Line(pts(&bar, tr, 0.), 8. * s * k, a));
                out.push(Mark::Fill(pts(&circle([0., 12. * k], 4.5 * k), tr, 0.), a));
            }
            Prop::Check => {
                let tick = [[-15., 0.], [-5., 11.], [16., -12.]];
                out.push(Mark::Line(
                    pts(&partial(&tick, (tp / 0.3).clamp(0., 1.)), [0.; 2], 0.),
                    9. * s,
                    a,
                ));
                let v = ((tp - 0.25) / 0.55).clamp(0., 1.);
                if v > 0. && v < 1. {
                    let k = 0.6 + 0.7 * v;
                    for [p0, p1] in [
                        [[-30., -14.], [-38., -19.]],
                        [[30., -16.], [38., -22.]],
                        [[-32., 10.], [-41., 13.]],
                        [[33., 8.], [42., 10.]],
                    ] {
                        let seg = [[p0[0] * k, p0[1] * k], [p1[0] * k, p1[1] * k]];
                        out.push(Mark::Line(pts(&seg, [0.; 2], 0.), 5. * s, a * (1. - v * v)));
                    }
                }
            }
            Prop::Pencil => {
                let since = (now - self.last_key) as f32;
                let wd = 6. + (self.caret + 0.75) / 1.5 * 40.;
                let k = (-since * 9.).exp();
                out.push(Mark::Line(
                    pts(&[[-25., 11.], [-28. + wd - 3., 11.]], [0.; 2], 0.),
                    6. * s,
                    a,
                ));
                let (tr, deg) = (
                    [-28. + wd + 2., 9. - 3. * k],
                    28. + 12. * (now32 * 34.).sin() * k,
                );
                out.push(Mark::Fill(
                    pts(&[[-5., -10.], [0., 0.], [5., -10.]], tr, deg),
                    a,
                ));
                out.push(Mark::Fill(
                    pts(&[[-5., -36.], [5., -36.], [5., -13.], [-5., -13.]], tr, deg),
                    a,
                ));
            }
            Prop::Zzz | Prop::Notes => {
                let zz = prop == Prop::Zzz;
                for i in 0..3 {
                    let speed = if zz { 0.42 } else { 0.6 };
                    let ph = ((now * speed + i as f64 / 3.) % 1.) as f32;
                    let dir = if zz || i % 2 == 0 { 1. } else { -1. };
                    let (x, y) = (dir * (42. + ph * 24.), 26. - ph * 46.);
                    let fs = (if zz { 16. } else { 22. }) + ph * 16.;
                    let alpha = a * (PI * ph).sin();
                    if zz {
                        let (w, h) = (fs * 0.5, fs * 0.55);
                        let z = [
                            [x - w / 2., y - h],
                            [x + w / 2., y - h],
                            [x - w / 2., y],
                            [x + w / 2., y],
                        ];
                        out.push(Mark::Line(pts(&z, [0.; 2], 0.), fs * 0.16 * s, alpha));
                    } else {
                        let k = fs / 16.;
                        let head: Vec<_> = (0..16)
                            .map(|j| {
                                let t = j as f32 / 16. * TAU;
                                [
                                    x + k * (4.6 * t.cos() * 0.94 - 1.2 * t.sin()),
                                    y + k * (3.4 * t.sin() + 0.8 * t.cos()),
                                ]
                            })
                            .collect();
                        out.push(Mark::Fill(pts(&head, [0.; 2], 0.), alpha));
                        let stem = [
                            [x + 3.4 * k, y - k],
                            [x + 5.2 * k, y - k],
                            [x + 5.2 * k, y - 15. * k],
                            [x + 3.4 * k, y - 15. * k],
                        ];
                        out.push(Mark::Fill(pts(&stem, [0.; 2], 0.), alpha));
                        let flag = [
                            [x + 5.2 * k, y - 15. * k],
                            [x + 10. * k, y - 11. * k],
                            [x + 9. * k, y - 9. * k],
                            [x + 5.2 * k, y - 11.5 * k],
                        ];
                        out.push(Mark::Fill(pts(&flag, [0.; 2], 0.), alpha));
                    }
                }
            }
        }
    }
}

fn polar(r: f32, a: f32) -> [f32; 2] {
    [r * a.cos(), r * a.sin()]
}

fn circle(c: [f32; 2], r: f32) -> Vec<[f32; 2]> {
    (0..=24)
        .map(|k| {
            let p = polar(r, k as f32 / 24. * TAU);
            [c[0] + p[0], c[1] + p[1]]
        })
        .collect()
}

/// A rounded box `w` by `h` centred on 0,0, its ends fully round.
fn capsule(w: f32, h: f32) -> Vec<[f32; 2]> {
    let r = w.min(h) / 2.;
    let (hx, hy) = (w / 2. - r, h / 2. - r);
    let mut out = Vec::with_capacity(44);
    for (cx, cy, a0) in [
        (hx, hy, 0.),
        (-hx, hy, 0.5 * PI),
        (-hx, -hy, PI),
        (hx, -hy, 1.5 * PI),
    ] {
        for k in 0..=10 {
            let p = polar(r, a0 + k as f32 / 10. * 0.5 * PI);
            out.push([cx + p[0], cy + p[1]]);
        }
    }
    out
}

/// The first `f` of a polyline's length.
fn partial(pts: &[[f32; 2]], f: f32) -> Vec<[f32; 2]> {
    let lens: Vec<f32> = pts
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .collect();
    let mut left = f * lens.iter().sum::<f32>();
    let mut out = vec![pts[0]];
    for (w, l) in pts.windows(2).zip(lens) {
        if left >= l {
            out.push(w[1]);
            left -= l;
        } else {
            let t = left / l.max(1e-6);
            out.push([
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ]);
            break;
        }
    }
    out
}

fn map_seg(s: &Seg, f: &mut impl FnMut([f32; 2]) -> [f32; 2]) -> Seg {
    match *s {
        Seg::M(p) => Seg::M(f(p)),
        Seg::L(p) => Seg::L(f(p)),
        Seg::C(c) => {
            let (a, b, e) = (f([c[0], c[1]]), f([c[2], c[3]]), f([c[4], c[5]]));
            Seg::C([a[0], a[1], b[0], b[1], e[0], e[1]])
        }
        Seg::Z => Seg::Z,
    }
}

fn paint(frame: &Frame, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let k = bounds.size.width.as_f32() / VB_W;
    let ox = bounds.origin.x.as_f32() - VB_X * k;
    let oy = bounds.origin.y.as_f32() - frame.lift - VB_Y * k
        + (bounds.size.height.as_f32() - VB_H * k) / 2.;
    let pt = |q: [f32; 2]| point(px(ox + q[0] * k), px(oy + q[1] * k));

    let draw = |window: &mut Window| {
        let fill = |segs: &[Seg], color: Hsla, window: &mut Window| {
            let mut b = PathBuilder::fill();
            for seg in segs {
                match *seg {
                    Seg::M(p) => b.move_to(pt(p)),
                    Seg::L(p) => b.line_to(pt(p)),
                    Seg::C(c) => {
                        b.cubic_bezier_to(pt([c[4], c[5]]), pt([c[0], c[1]]), pt([c[2], c[3]]))
                    }
                    Seg::Z => b.close(),
                }
            }
            if let Ok(path) = b.build() {
                window.paint_path(path, color);
            }
        };
        // The dark of the ear insides, the mask and the nose: the head's
        // outline in the app's background, opaque even with glass on.
        fill(&frame.body[..frame.outline], theme::background(), window);
        // The head in one even-odd path: the ear insides, the mask and the
        // nose are holes over that dark.
        fill(&frame.body, color, window);
        for mark in &frame.marks {
            let (path, alpha) = match mark {
                Mark::Fill(q, alpha) if q.len() >= 3 => {
                    let mut b = PathBuilder::fill();
                    b.add_polygon(&q.iter().map(|&q| pt(q)).collect::<Vec<_>>(), true);
                    (b.build(), *alpha)
                }
                Mark::Line(q, width, alpha) if q.len() >= 2 => {
                    let opts = StrokeOptions::default()
                        .with_line_width(width * k)
                        .with_line_join(LineJoin::Round)
                        .with_line_cap(LineCap::Round);
                    let mut b = PathBuilder::fill().with_style(PathStyle::Stroke(opts));
                    b.add_polygon(&q.iter().map(|&q| pt(q)).collect::<Vec<_>>(), false);
                    (b.build(), *alpha)
                }
                _ => continue,
            };
            if let (Ok(path), true) = (path, alpha > 0.01) {
                window.paint_path(path, color.opacity(alpha.min(1.)));
            }
        }
    };
    match frame.ledge {
        Some(ledge) => {
            let edge = oy + ledge * k;
            let keep = Bounds::from_corners(
                point(px(ox - 2000.), px(edge - 2000.)),
                point(px(ox + 2000.), px(edge)),
            );
            window.with_content_mask(Some(ContentMask { bounds: keep }), draw);
        }
        None => draw(window),
    }
}

impl Render for Mascot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = self.now();
        let dt = self.last_frame.map_or(1. / 60., |l| (now - l).min(0.05)) as f32;
        self.last_frame = Some(now);
        self.rendered = true;
        self.active = self.awake || window.is_window_active();
        let frame = self.step(now, dt);
        let color = self.color.unwrap_or_else(theme::accent);
        let (weak, click) = (cx.weak_entity(), cx.weak_entity());
        let bounds = self.bounds.clone();
        // Where it is drawn, not its box: it may sit higher, on the card over
        // the prompter, and a shy one ends at the prompter's edge.
        let (lift, reach) = (
            self.perch,
            if self.shy {
                self.ledge()
            } else {
                self.width * VB_H / VB_W
            },
        );
        div()
            .flex_none()
            .w(px(self.width))
            .h(px(self.width * VB_H / VB_W))
            .child(
                canvas(
                    |_, _, _| {},
                    move |b, _, window, _| {
                        bounds.set(Some(b));
                        paint(&frame, b, color, window);
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                let _ = weak.update(cx, |m, _| m.pointer_moved(e.position));
                            }
                        });
                        let top = b.origin.y.as_f32() - lift;
                        let (l, r) = (
                            b.origin.x.as_f32(),
                            b.origin.x.as_f32() + b.size.width.as_f32(),
                        );
                        window.on_mouse_event(move |e: &MouseDownEvent, phase, _, cx| {
                            let (x, y) = (e.position.x.as_f32(), e.position.y.as_f32());
                            let hit = x >= l && x <= r && y >= top && y <= top + reach;
                            if phase == DispatchPhase::Bubble
                                && e.button == MouseButton::Left
                                && hit
                            {
                                let _ = click.update(cx, |m, cx| {
                                    m.boop();
                                    cx.notify();
                                });
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

    #[test]
    fn parses_absolute_paths() {
        let segs = parse_path("M1.5 2C3 4 5 6 7 8L-9 10.25Z");
        assert_eq!(
            segs,
            vec![
                Seg::M([1.5, 2.]),
                Seg::C([3., 4., 5., 6., 7., 8.]),
                Seg::L([-9., 10.25]),
                Seg::Z
            ]
        );
    }

    #[test]
    fn parses_relative_paths_the_way_exporters_write_them() {
        // Run-together numbers, relative curves, a mirrored `s` and `h`/`v`.
        let segs = parse_path("M10,10c1,2,3,4,5,6s1-.5.5.5h2v-1.5l1,1z");
        assert_eq!(
            segs,
            vec![
                Seg::M([10., 10.]),
                Seg::C([11., 12., 13., 14., 15., 16.]),
                Seg::C([17., 18., 16., 15.5, 15.5, 16.5]),
                Seg::L([17.5, 16.5]),
                Seg::L([17.5, 15.]),
                Seg::L([18.5, 16.]),
                Seg::Z,
            ]
        );
    }

    #[test]
    fn reads_the_drawing_from_the_svg() {
        let g = geo();
        assert!(g.head.segs.len() > 40 && !g.mask.is_empty() && !g.nose.is_empty());
        assert!((g.eye_at[0][0] - 58.345).abs() < 0.01 && (g.eye_at[1][0] - 120.185).abs() < 0.01);
        assert!((g.eye_size[1] - 25.55).abs() < 0.01);
        // Every subpath of the head closes back where it started.
        assert!(matches!(g.head.segs.last(), Some(Seg::Z)));
    }

    #[test]
    fn ears_bend_only_past_their_base() {
        let g = geo();
        let at = |q: [f32; 2]| smoothstep(-1.5, 10., past_base(q));
        // Ear tips go all the way, the chin and cheeks never.
        assert_eq!(at([22.61, 1.48]), 1.);
        assert_eq!(at([156., 1.5]), 1.);
        assert_eq!(at([CX, FLOOR]), 0.);
        assert_eq!(at([0., 85.43]), 0.);
        assert_eq!(
            g.head.bend.len(),
            g.head
                .segs
                .iter()
                .map(|s| s.points().len() / 2)
                .sum::<usize>()
        );
        // A point that takes no bend stays put however the ear moves.
        assert_eq!(ear_bend([CX, FLOOR], 0., [0.5, 1.3]), [CX, FLOOR]);
    }

    #[test]
    fn spying_shows_the_eyes_over_the_edge_and_hiding_hides_it_all() {
        let g = geo();
        let eye_top = g.eye_at[0][1] - g.eye_size[1] / 2.;
        let nose_top = 90.49;
        // At rest it shows whole, chin on the edge.
        const { assert!(LEDGE_Y >= FLOOR - 3.) };
        // Spying: the eyes rise over the edge, the nose stays behind it.
        assert!(eye_top + AWAY_Y < LEDGE_Y && eye_top + AWAY_Y + g.eye_size[1] > LEDGE_Y);
        assert!(nose_top + AWAY_Y > LEDGE_Y);
        // Hidden: not even the ear tips (the drawing's top) show.
        const { assert!(1.48 + HIDE_Y > LEDGE_Y) };
    }

    #[test]
    fn dancing_only_for_dancers_and_not_while_you_are_needed() {
        assert!(dances(true, true, State::Idle));
        assert!(dances(true, true, State::Working));
        assert!(!dances(false, true, State::Idle));
        assert!(!dances(true, false, State::Idle));
        assert!(!dances(true, true, State::Waiting));
        assert!(!dances(true, true, State::Error));
    }

    #[test]
    fn a_partial_tick_grows_along_its_length() {
        let tick = [[0., 0.], [10., 0.], [10., 10.]];
        assert_eq!(partial(&tick, 0.25), vec![[0., 0.], [5., 0.]]);
        assert_eq!(partial(&tick, 1.), tick.to_vec());
    }

    #[test]
    fn code_lines_advance_one_per_sweep_of_the_eyes() {
        assert_eq!(code_line(0.), (0, 0.));
        assert_eq!(code_line(CODE_PERIOD * 2.5), (2, 0.5));
        // A clock that runs backwards never asks for a line before the first.
        assert_eq!(code_line(-3.), (0, 0.));
    }
}
