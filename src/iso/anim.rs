//! A whole loop as ONE animated SVG the way illos draws theirs: every path
//! once, and only what changes animated, so the browser tweens it at the
//! screen's own rate instead of flipping through stored frames.
//!
//! The loop is sampled, the samples are lined up into slots (one path that
//! lives across frames), and each slot gets the cheapest motion that
//! reproduces it: none, a carry (translate, uniform scale), a spin in one of
//! the iso planes, or (for parts that really change shape) a morph of `d`.
//! Parts that move together share one `<g>` and one animation. Keyframes
//! the tween would land on anyway are dropped.
//!
//! All motion is SMIL, not CSS: Chrome hands CSS `opacity`/`transform`
//! animations to the compositor, which redraws the lines soft (measured:
//! 1.3% of the drawing off against the exact frame, 0% with SMIL), and
//! Safari can't animate `d` from CSS. One clock for everything, too. A page
//! that honours reduced motion calls `svg.pauseAnimations()`.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::svg::{Palette, dash_offset, num, paint_of, style, svg_id};
use super::{Anchor, Item, P2, Rect, Seg, bounds};

/// Named layers (`.layer(n)`) keep their id across frames; per-node ones
/// are the node's index, which shifts when a script skips a node.
const NAMED: u32 = 1 << 20;

#[derive(Clone)]
struct Val {
    nums: Vec<f32>,
    alpha: f32,
    dash: Option<f32>,
}

struct Tok {
    key: u32,
    hash: u64,
    shape: Vec<Seg>,
    val: Val,
}

struct Slot {
    key: u32,
    last: u64,
    class: String,
    fill: String,
    group: Option<u32>,
    /// Drawn in the world's units, inside the camera's group.
    world: bool,
    shape: Vec<Seg>,
    vals: Vec<Option<Val>>,
}

/// What each number of a path is: a point's x or y, an arc radius or angle.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    X,
    Y,
    R,
    Rot,
}

fn parts(shape: &[Seg]) -> Vec<Part> {
    use Part::*;
    let mut out = Vec::new();
    for seg in shape {
        match seg {
            Seg::M(_) | Seg::L(_) => out.extend([X, Y]),
            Seg::C(..) => out.extend([X, Y, X, Y, X, Y]),
            Seg::A { .. } => out.extend([R, R, Rot, X, Y]),
            Seg::Z => {}
        }
    }
    out
}

fn nums(shape: &[Seg], dx: f32, dy: f32) -> Vec<f32> {
    let mut out = Vec::new();
    for seg in shape {
        match *seg {
            Seg::M([x, y]) | Seg::L([x, y]) => out.extend([x + dx, y + dy]),
            Seg::C(a, b, c) => out.extend([a[0] + dx, a[1] + dy, b[0] + dx, b[1] + dy, c[0] + dx, c[1] + dy]),
            Seg::A { r, rot, to, .. } => out.extend([r[0], r[1], rot, to[0] + dx, to[1] + dy]),
            Seg::Z => {}
        }
    }
    out
}

fn with_nums(shape: &[Seg], v: &[f32]) -> Vec<Seg> {
    let mut i = 0;
    let mut take = |n: usize| {
        i += n;
        &v[i - n..i]
    };
    shape
        .iter()
        .map(|seg| match *seg {
            Seg::M(_) => {
                let p = take(2);
                Seg::M([p[0], p[1]])
            }
            Seg::L(_) => {
                let p = take(2);
                Seg::L([p[0], p[1]])
            }
            Seg::C(..) => {
                let p = take(6);
                Seg::C([p[0], p[1]], [p[2], p[3]], [p[4], p[5]])
            }
            Seg::A { large, sweep, .. } => {
                let p = take(5);
                Seg::A { r: [p[0], p[1]], rot: p[2], large, sweep, to: [p[3], p[4]] }
            }
            Seg::Z => Seg::Z,
        })
        .collect()
}

/// Path data as short as it gets without moving a point: every point
/// rounded to a hundredth first, then written relative to the last one
/// (so the rounding never adds up), numbers without leading zeros or
/// needless spaces, repeated commands left out. `hv` allows `h`/`v` for
/// level and upright lines; a morph's frames must keep one structure, so
/// they go without.
fn tight(path: &[Seg], hv: bool) -> String {
    let q = |v: f32| (v * 100.).round() as i64;
    let mut out = String::new();
    let mut last_cmd = ' ';
    // Whether the last number written has a '.', so a following ".5"
    // needs a space and a following "5" does too.
    let mut last_dot = false;
    let mut first_num = true;
    let cmd = |out: &mut String, c: char, last_cmd: &mut char, first: &mut bool| {
        if c != *last_cmd || c == 'm' {
            out.push(c);
            *first = true;
        }
        *last_cmd = c;
    };
    let put = |out: &mut String, v: i64, first: &mut bool, dot: &mut bool| {
        let neg = v < 0;
        let a = v.unsigned_abs();
        let (int, frac) = (a / 100, a % 100);
        let mut t = String::new();
        if neg {
            t.push('-');
        }
        if int > 0 {
            t += &int.to_string();
        }
        if frac > 0 {
            let f = format!("{frac:02}");
            t.push('.');
            t += f.trim_end_matches('0');
        } else if int == 0 {
            t.push('0');
        }
        let has_dot = t.contains('.');
        if !*first && !(neg || (t.starts_with('.') && *dot)) {
            out.push(' ');
        }
        out.push_str(&t);
        *dot = has_dot;
        *first = false;
    };
    let flag = |out: &mut String, f: bool, first: &mut bool, dot: &mut bool| {
        if !*first {
            out.push(' ');
        }
        out.push(if f { '1' } else { '0' });
        *dot = false;
        *first = false;
    };
    let (mut cur, mut start) = ([0i64; 2], [0i64; 2]);
    for seg in path {
        match *seg {
            Seg::M([x, y]) => {
                let p = [q(x), q(y)];
                cmd(&mut out, 'm', &mut last_cmd, &mut first_num);
                put(&mut out, p[0] - cur[0], &mut first_num, &mut last_dot);
                put(&mut out, p[1] - cur[1], &mut first_num, &mut last_dot);
                cur = p;
                start = p;
            }
            Seg::L([x, y]) => {
                let p = [q(x), q(y)];
                let d = [p[0] - cur[0], p[1] - cur[1]];
                if hv && d[1] == 0 {
                    cmd(&mut out, 'h', &mut last_cmd, &mut first_num);
                    put(&mut out, d[0], &mut first_num, &mut last_dot);
                } else if hv && d[0] == 0 {
                    cmd(&mut out, 'v', &mut last_cmd, &mut first_num);
                    put(&mut out, d[1], &mut first_num, &mut last_dot);
                } else {
                    cmd(&mut out, 'l', &mut last_cmd, &mut first_num);
                    put(&mut out, d[0], &mut first_num, &mut last_dot);
                    put(&mut out, d[1], &mut first_num, &mut last_dot);
                }
                cur = p;
            }
            Seg::C(a, b, [x, y]) => {
                cmd(&mut out, 'c', &mut last_cmd, &mut first_num);
                for v in [a, b] {
                    put(&mut out, q(v[0]) - cur[0], &mut first_num, &mut last_dot);
                    put(&mut out, q(v[1]) - cur[1], &mut first_num, &mut last_dot);
                }
                let p = [q(x), q(y)];
                put(&mut out, p[0] - cur[0], &mut first_num, &mut last_dot);
                put(&mut out, p[1] - cur[1], &mut first_num, &mut last_dot);
                cur = p;
            }
            Seg::A { r, rot, large, sweep, to: [x, y] } => {
                cmd(&mut out, 'a', &mut last_cmd, &mut first_num);
                put(&mut out, q(r[0]), &mut first_num, &mut last_dot);
                put(&mut out, q(r[1]), &mut first_num, &mut last_dot);
                put(&mut out, q(rot), &mut first_num, &mut last_dot);
                flag(&mut out, large, &mut first_num, &mut last_dot);
                flag(&mut out, sweep, &mut first_num, &mut last_dot);
                let p = [q(x), q(y)];
                put(&mut out, p[0] - cur[0], &mut first_num, &mut last_dot);
                put(&mut out, p[1] - cur[1], &mut first_num, &mut last_dot);
                cur = p;
            }
            Seg::Z => {
                out.push('z');
                last_cmd = 'z';
                first_num = true;
                cur = start;
            }
        }
    }
    out
}

/// A world path back from the camera: `(p - off) / zoom`.
fn unfilm(path: &[Seg], (zoom, off): (f32, P2)) -> Vec<Seg> {
    let f = |p: P2| [(p[0] - off[0]) / zoom, (p[1] - off[1]) / zoom];
    path.iter()
        .map(|seg| match *seg {
            Seg::M(p) => Seg::M(f(p)),
            Seg::L(p) => Seg::L(f(p)),
            Seg::C(a, b, p) => Seg::C(f(a), f(b), f(p)),
            Seg::A { r, rot, large, sweep, to } => Seg::A { r: [r[0] / zoom, r[1] / zoom], rot, large, sweep, to: f(to) },
            Seg::Z => Seg::Z,
        })
        .collect()
}

/// The path's structure: what may tween into what.
fn signature(shape: &[Seg]) -> String {
    shape
        .iter()
        .map(|s| match s {
            Seg::M(_) => "M".to_string(),
            Seg::L(_) => "L".into(),
            Seg::C(..) => "C".into(),
            Seg::A { large, sweep, .. } => format!("A{}{}", *large as u8, *sweep as u8),
            Seg::Z => "Z".into(),
        })
        .collect()
}

fn geo_hash(v: &[f32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for x in v {
        ((x * 100.).round() as i64).hash(&mut h);
    }
    h.finish()
}

/// Lines `frame` up against the slots so far (a weighted LCS: the same
/// kind of path matches, the very same path matches better) and records it.
fn merge(slots: &mut Vec<Slot>, toks: Vec<Tok>, fi: usize, frames: usize, meta: &[(String, String, Option<u32>, bool)]) {
    let (m, n) = (slots.len(), toks.len());
    let w = n + 1;
    let mut dp = vec![0u32; (m + 1) * w];
    let score = |s: &Slot, t: &Tok| {
        if s.key != t.key {
            0
        } else if s.last == t.hash {
            3
        } else {
            1
        }
    };
    for i in 1..=m {
        for j in 1..=n {
            let mut best = dp[(i - 1) * w + j].max(dp[i * w + j - 1]);
            let s = score(&slots[i - 1], &toks[j - 1]);
            if s > 0 {
                best = best.max(dp[(i - 1) * w + j - 1] + s);
            }
            dp[i * w + j] = best;
        }
    }
    // Walk back: matched pairs, slots this frame lacks, and new paths.
    let mut order: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let s = score(&slots[i - 1], &toks[j - 1]);
            if s > 0 && dp[i * w + j] == dp[(i - 1) * w + j - 1] + s {
                order.push((Some(i - 1), Some(j - 1)));
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && (j == 0 || dp[i * w + j] == dp[(i - 1) * w + j]) {
            order.push((Some(i - 1), None));
            i -= 1;
        } else {
            order.push((None, Some(j - 1)));
            j -= 1;
        }
    }
    order.reverse();
    let mut toks: Vec<Option<Tok>> = toks.into_iter().map(Some).collect();
    let mut old: Vec<Option<Slot>> = std::mem::take(slots).into_iter().map(Some).collect();
    for (si, tj) in order {
        let tok = tj.and_then(|j| toks[j].take());
        let mut slot = match si {
            Some(i) => old[i].take().expect("each slot once"),
            None => {
                let t = tok.as_ref().expect("a new path");
                let (class, fill, group, world) = meta[t.key as usize].clone();
                Slot { key: t.key, last: 0, class, fill, group, world, shape: t.shape.clone(), vals: vec![None; frames] }
            }
        };
        if let Some(t) = tok {
            slot.last = t.hash;
            slot.vals[fi] = Some(t.val);
        }
        slots.push(slot);
    }
}

#[cfg(test)]
/// The samples to keep so straight tweens between them stay within `tol`
/// of every sample (Ramer–Douglas–Peucker over time).
fn keep(v: &[Vec<f32>], tol: f32) -> Vec<usize> {
    let n = v.len();
    let mut kept = vec![false; n];
    kept[0] = true;
    kept[n - 1] = true;
    let mut stack = vec![(0, n - 1)];
    while let Some((a, b)) = stack.pop() {
        let mut worst = (0f32, 0);
        for i in a + 1..b {
            let u = (i - a) as f32 / (b - a) as f32;
            let d = v[i]
                .iter()
                .zip(&v[a])
                .zip(&v[b])
                .map(|((x, p), q)| (x - (p + (q - p) * u)).abs())
                .fold(0., f32::max);
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tol {
            kept[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    (0..n).filter(|&i| kept[i]).collect()
}

/// A keyframe and the timing curve (CSS cubic-bezier, SMIL keySplines)
/// that carries it to the next one.
type Key = (usize, [f32; 4]);

const LINEAR: [f32; 4] = [0., 0., 1., 1.];

/// Keyframes for `v` with an eased tween between each pair: each stretch
/// is the longest one whose samples all lie on the straight line between
/// its ends (every channel moving in step) along one cubic-bezier timing,
/// to within `tol` at every sample. The scene's own easings are beziers,
/// so a whole eased move is two keyframes instead of dozens.
fn plan(v: &[Vec<f32>], tol: f32) -> Vec<Key> {
    let n = v.len();
    let mut keys = Vec::new();
    let mut a = 0;
    while a + 1 < n {
        let mut good = (a + 1, LINEAR);
        let mut bad = None;
        let mut step = 2;
        // Gallop out, then bisect back to the longest stretch that fits.
        loop {
            let b = (a + step).min(n - 1);
            if b <= good.0 {
                break;
            }
            match stretch(&v[a..=b], tol) {
                Some(c) => good = (b, c),
                None => {
                    bad = Some(b);
                    break;
                }
            }
            if b == n - 1 {
                break;
            }
            step *= 2;
        }
        if let Some(mut hi) = bad {
            let mut lo = good.0;
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                match stretch(&v[a..=mid], tol) {
                    Some(c) => {
                        good = (mid, c);
                        lo = mid;
                    }
                    None => hi = mid,
                }
            }
        }
        keys.push((a, good.1));
        a = good.0;
    }
    keys.push((n - 1, LINEAR));
    keys
}

/// The timing that takes `v[0]` to its last value through every sample,
/// if one does.
fn stretch(v: &[Vec<f32>], tol: f32) -> Option<[f32; 4]> {
    let (a, b) = (&v[0], &v[v.len() - 1]);
    let d: Vec<f32> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    let span = d.iter().fold(0f32, |m, x| m.max(x.abs()));
    if span <= tol {
        // Barely moving: the straight tween itself must stay within `tol`.
        let last = (v.len() - 1) as f32;
        let ok = v.iter().enumerate().all(|(i, w)| {
            let s = i as f32 / last;
            w.iter().zip(a).zip(&d).all(|((x, y), e)| (x - (y + s * e)).abs() <= tol)
        });
        return ok.then_some(LINEAR);
    }
    let dd: f32 = d.iter().map(|x| x * x).sum();
    let mut u = Vec::with_capacity(v.len());
    for w in v {
        let p = w.iter().zip(a).zip(&d).map(|((x, y), e)| (x - y) * e).sum::<f32>() / dd;
        // On the line between the ends? (Half the error here, half in the
        // timing: the two add up.)
        if w.iter().zip(a).zip(&d).any(|((x, y), e)| (x - (y + p * e)).abs() > tol / 2.) {
            return None;
        }
        u.push(p);
    }
    let last = (v.len() - 1) as f32;
    let s: Vec<f32> = (0..v.len()).map(|i| i as f32 / last).collect();
    timing(&s, &u, span, tol / 2.)
}

/// A cubic-bezier through `(s, u)` to within `tol / scale`: a known
/// easing when one fits, else fitted (Nelder–Mead on the worst miss).
fn timing(s: &[f32], u: &[f32], scale: f32, tol: f32) -> Option<[f32; 4]> {
    if u.iter().any(|x| !(-1e-3..=1. + 1e-3).contains(x)) {
        return None;
    }
    let miss = |c: &[f32; 4], s: &[f32], u: &[f32]| {
        s.iter().zip(u).map(|(s, u)| (super::cubic_bezier(c[0], c[1], c[2], c[3], *s) - u).abs()).fold(0f32, f32::max) * scale
    };
    let known = [LINEAR, [0.23, 1., 0.32, 1.], [0.77, 0., 0.175, 1.], [0.42, 0., 1., 1.], [0., 0., 0.58, 1.]];
    let mut best = known.iter().map(|c| (miss(c, s, u), *c)).min_by(|x, y| x.0.total_cmp(&y.0)).expect("candidates");
    if best.0 <= tol {
        return Some(best.1);
    }
    // A stretch cut out of one of the scene's easings (a key that started
    // before the part appeared, a corner on a rail): that piece exactly.
    for c in &known[1..3] {
        if let Some(sub) = piece(*c, s, u, &miss) {
            let m = miss(&sub, s, u);
            if m < best.0 {
                best = (m, sub);
            }
        }
    }
    if best.0 <= tol {
        return Some(best.1);
    }
    // Fit on at most 40 samples, check on all.
    let k = s.len().div_ceil(40);
    let (fs, fu): (Vec<f32>, Vec<f32>) = s.iter().zip(u).step_by(k).map(|(a, b)| (*a, *b)).unzip();
    let f = |c: &[f32; 4]| miss(&c.map(|x| x.clamp(0., 1.)), &fs, &fu);
    let mut simplex: Vec<([f32; 4], f32)> = (0..5)
        .map(|i| {
            let mut c = best.1;
            if i > 0 {
                c[i - 1] = (c[i - 1] + if c[i - 1] > 0.5 { -0.2 } else { 0.2 }).clamp(0., 1.);
            }
            (c, f(&c))
        })
        .collect();
    for _ in 0..300 {
        simplex.sort_by(|a, b| a.1.total_cmp(&b.1));
        if simplex[0].1 <= tol * 0.5 {
            break;
        }
        let mut mid = [0f32; 4];
        for (c, _) in &simplex[..4] {
            for j in 0..4 {
                mid[j] += c[j] / 4.;
            }
        }
        let worst = simplex[4].0;
        let at = |k: f32| -> [f32; 4] { std::array::from_fn(|j| (mid[j] + k * (worst[j] - mid[j])).clamp(0., 1.)) };
        let r = at(-1.);
        let fr = f(&r);
        if fr < simplex[0].1 {
            let e = at(-2.);
            let fe = f(&e);
            simplex[4] = if fe < fr { (e, fe) } else { (r, fr) };
        } else if fr < simplex[3].1 {
            simplex[4] = (r, fr);
        } else {
            let c = at(0.5);
            let fc = f(&c);
            if fc < simplex[4].1 {
                simplex[4] = (c, fc);
            } else {
                let b0 = simplex[0].0;
                for p in simplex.iter_mut().skip(1) {
                    p.0 = std::array::from_fn(|j| b0[j] + 0.5 * (p.0[j] - b0[j]));
                    p.1 = f(&p.0);
                }
            }
        }
    }
    simplex.sort_by(|a, b| a.1.total_cmp(&b.1));
    best = (miss(&simplex[0].0, s, u), simplex[0].0);
    (best.0 <= tol).then(|| best.1.map(|x| (x * 10000.).round() / 10000.)).filter(|c| miss(c, s, u) <= tol)
}

/// The part of easing `c` between inputs `q0..q1` that best explains the
/// samples, as a timing of its own (de Casteljau, rescaled to the unit
/// square); None when it can't be written as one (control points out of
/// 0..1).
fn piece(c: [f32; 4], s: &[f32], u: &[f32], miss: &impl Fn(&[f32; 4], &[f32], &[f32]) -> f32) -> Option<[f32; 4]> {
    let e = |q: f32| super::cubic_bezier(c[0], c[1], c[2], c[3], q);
    let k = s.len().div_ceil(24);
    let (fs, fu): (Vec<f32>, Vec<f32>) = s.iter().zip(u).step_by(k).map(|(a, b)| (*a, *b)).unzip();
    let err = |q0: f32, q1: f32| {
        let (e0, e1) = (e(q0), e(q1));
        if (e1 - e0).abs() < 1e-4 {
            return f32::MAX;
        }
        fs.iter().zip(&fu).map(|(s, u)| ((e(q0 + s * (q1 - q0)) - e0) / (e1 - e0) - u).abs()).fold(0f32, f32::max)
    };
    let mut best = (f32::MAX, 0f32, 1f32);
    for i in 0..20 {
        for j in (i + 1)..=20 {
            let (q0, q1) = (i as f32 / 20., j as f32 / 20.);
            let m = err(q0, q1);
            if m < best.0 {
                best = (m, q0, q1);
            }
        }
    }
    let mut step = 0.025;
    while step > 1e-5 {
        let mut moved = false;
        for (d0, d1) in [(step, 0.), (-step, 0.), (0., step), (0., -step)] {
            let (q0, q1) = ((best.1 + d0).clamp(0., 1.), (best.2 + d1).clamp(0., 1.));
            if q1 > q0 {
                let m = err(q0, q1);
                if m < best.0 {
                    best = (m, q0, q1);
                    moved = true;
                }
            }
        }
        if !moved {
            step /= 2.;
        }
    }
    let (q0, q1) = (best.1, best.2);
    // The curve's own parameter at those inputs (x is monotone).
    let bez = |a: f32, b: f32, t: f32| 3. * a * t * (1. - t).powi(2) + 3. * b * t * t * (1. - t) + t.powi(3);
    let tau = |x: f32| {
        let (mut lo, mut hi) = (0f32, 1f32);
        for _ in 0..40 {
            let m = (lo + hi) / 2.;
            if bez(c[0], c[2], m) < x { lo = m } else { hi = m }
        }
        (lo + hi) / 2.
    };
    let (t0, t1) = (tau(q0), tau(q1));
    let p = [[0., 0.], [c[0], c[1]], [c[2], c[3]], [1., 1.]];
    let split = |p: [[f32; 2]; 4], t: f32| -> ([[f32; 2]; 4], [[f32; 2]; 4]) {
        let l = |a: [f32; 2], b: [f32; 2]| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        let (a, b, c) = (l(p[0], p[1]), l(p[1], p[2]), l(p[2], p[3]));
        let (d, e) = (l(a, b), l(b, c));
        let f = l(d, e);
        ([p[0], a, d, f], [f, e, c, p[3]])
    };
    let (_, right) = split(p, t0);
    let (mid, _) = split(right, ((t1 - t0) / (1. - t0)).clamp(0., 1.));
    let (w, h) = (mid[3][0] - mid[0][0], mid[3][1] - mid[0][1]);
    if w.abs() < 1e-6 || h.abs() < 1e-6 {
        return None;
    }
    let n = |q: [f32; 2]| [(q[0] - mid[0][0]) / w, (q[1] - mid[0][1]) / h];
    let (a, b) = (n(mid[1]), n(mid[2]));
    let sub = [a[0], a[1], b[0], b[1]].map(|x| (x * 10000.).round() / 10000.);
    sub.iter().all(|x| (0. ..=1.).contains(x)).then_some(sub).filter(|c| miss(c, s, u).is_finite())
}

fn flat(v: &[Vec<f32>], tol: f32) -> bool {
    v.iter().all(|x| x.iter().zip(&v[0]).all(|(a, b)| (a - b).abs() <= tol))
}

/// A control value to four places, the way it was checked.
fn fine(x: f32) -> String {
    let s = format!("{x:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.into() }
}

/// A keyTime, fine enough that a fast part lands where it was sampled
/// (at 4 places a keyframe could sit 0.3 ms off, a visible slip mid-ease).
fn at(i: usize, n: usize) -> String {
    let s = format!("{:.6}", i as f64 / n as f64);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Animations already written, so parts whose motion matches within its
/// tolerance write the very same text (and can share a group).
#[derive(Default)]
struct Reg {
    seen: Vec<(String, Vec<Vec<f32>>, String)>,
    sets: usize,
}

impl Reg {
    /// The SMIL element(s) for `v` (one sample per frame plus the loop's
    /// end); empty when nothing changes. `write` turns the kept samples
    /// (their indices and values) into the text.
    fn smil(&mut self, kind: &str, v: &[Vec<f32>], tol: f32, write: impl Fn(&[Key], &[Vec<f32>]) -> String) -> String {
        if flat(v, tol) {
            return String::new();
        }
        let near = |w: &[Vec<f32>]| {
            w.len() == v.len()
                && w.iter().zip(v).all(|(a, b)| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol / 2.))
        };
        if let Some((_, _, text)) = self.seen.iter().find(|(k, w, _)| k == kind && near(w)) {
            return text.clone();
        }
        let keys = plan(v, tol);
        let text = write(&keys, v);
        self.sets += 1;
        self.seen.push((kind.to_string(), v.to_vec(), text.clone()));
        text
    }
}

fn smil_tag(tag: &str, extra: &str, seconds: f32, keys: &[Key], n: usize, values: impl Fn(usize) -> String) -> String {
    let times: Vec<String> = keys.iter().map(|&(i, _)| at(i, n)).collect();
    let vals: Vec<String> = keys.iter().map(|&(i, _)| values(i)).collect();
    let eased = keys[..keys.len() - 1].iter().any(|(_, c)| *c != LINEAR);
    let splines = if eased {
        let sp: Vec<String> =
            keys[..keys.len() - 1].iter().map(|(_, c)| c.map(fine).join(" ")).collect();
        format!(r#" calcMode="spline" keySplines="{}""#, sp.join(";"))
    } else {
        String::new()
    };
    format!(
        r#"<{tag} {extra}dur="{}s" repeatCount="indefinite" keyTimes="{}"{splines} values="{}"/>"#,
        num(seconds),
        times.join(";"),
        vals.join(";")
    )
}

/// The iso planes a part can spin in: screen = M · plane point, as
/// `[[m00, m01], [m10, m11]]` (columns are the plane's axes on screen).
const PLANES: [[[f32; 2]; 2]; 3] = {
    const C: f32 = 0.866_025_4;
    [
        [[C, -C], [0.5, 0.5]],  // top: x, y
        [[C, 0.], [0.5, -1.]],  // left wall: x, z
        [[-C, 0.], [0.5, -1.]], // right wall: y, z
    ]
};

fn inv2(m: [[f32; 2]; 2]) -> [[f32; 2]; 2] {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    [[m[1][1] / det, -m[0][1] / det], [-m[1][0] / det, m[0][0] / det]]
}

fn mul2(m: [[f32; 2]; 2], p: [f32; 2]) -> [f32; 2] {
    [m[0][0] * p[0] + m[0][1] * p[1], m[1][0] * p[0] + m[1][1] * p[1]]
}

/// A part turning about a fixed point of one iso plane (a fan blade, a
/// spinning disk's mark): which plane, the center in plane units, the
/// angle at every frame (degrees, unwrapped) and the base path's points
/// in plane units.
struct Spin {
    plane: usize,
    center: [f32; 2],
    deg: Vec<f32>,
    local: Vec<f32>,
}

fn spin(parts: &[Part], base: &[f32], vals: &[Val], tol: f32) -> Option<Spin> {
    // Lines and curves only: arcs would need their ellipses mapped too.
    if parts.iter().any(|p| matches!(p, Part::R | Part::Rot)) {
        return None;
    }
    let pts = |v: &[f32]| -> Vec<[f32; 2]> { v.chunks(2).map(|c| [c[0], c[1]]).collect() };
    'plane: for (k, m) in PLANES.iter().enumerate() {
        let mi = inv2(*m);
        let a: Vec<[f32; 2]> = pts(base).into_iter().map(|p| mul2(mi, p)).collect();
        let ca = centroid(&a);
        let mut turns = Vec::with_capacity(vals.len());
        for v in vals {
            let b: Vec<[f32; 2]> = pts(&v.nums).into_iter().map(|p| mul2(mi, p)).collect();
            let cb = centroid(&b);
            let (mut sxx, mut sxy) = (0., 0.);
            for (p, q) in a.iter().zip(&b) {
                let (px, py, qx, qy) = (p[0] - ca[0], p[1] - ca[1], q[0] - cb[0], q[1] - cb[1]);
                sxx += px * qx + py * qy;
                sxy += px * qy - py * qx;
            }
            let th = sxy.atan2(sxx);
            let (c, s) = (th.cos(), th.sin());
            let t = [cb[0] - (c * ca[0] - s * ca[1]), cb[1] - (s * ca[0] + c * ca[1])];
            // It must be that turn, back on screen to within `tol`.
            for (p, q) in a.iter().zip(&b) {
                let r = [c * p[0] - s * p[1] + t[0] - q[0], s * p[0] + c * p[1] + t[1] - q[1]];
                let e = mul2(*m, r);
                if e[0].abs() > tol || e[1].abs() > tol {
                    continue 'plane;
                }
            }
            turns.push((th, t));
        }
        // About one fixed point: solve (I - R)c = t where it turns most.
        let (th, t) = *turns.iter().max_by(|x, y| x.0.sin().abs().total_cmp(&y.0.sin().abs()))?;
        if th.sin().abs() < 0.05 {
            continue;
        }
        let (c, s) = (th.cos(), th.sin());
        let det = (1. - c) * (1. - c) + s * s;
        let center = [((1. - c) * t[0] - s * t[1]) / det, (s * t[0] + (1. - c) * t[1]) / det];
        for (th, t) in &turns {
            let (c, s) = (th.cos(), th.sin());
            let want = [(1. - c) * center[0] + s * center[1], -s * center[0] + (1. - c) * center[1]];
            let e = mul2(*m, [want[0] - t[0], want[1] - t[1]]);
            if e[0].abs() > tol || e[1].abs() > tol {
                continue 'plane;
            }
        }
        let mut deg = Vec::with_capacity(turns.len());
        let mut prev = 0f32;
        for (th, _) in &turns {
            let d = th.to_degrees();
            let step = (d - prev.rem_euclid(360.) + 540.).rem_euclid(360.) - 180.;
            prev += step;
            deg.push(prev);
        }
        let local = a.iter().flat_map(|p| [p[0], p[1]]).collect();
        return Some(Spin { plane: k, center, deg, local });
    }
    None
}

fn centroid(p: &[[f32; 2]]) -> [f32; 2] {
    let n = p.len().max(1) as f32;
    [p.iter().map(|q| q[0]).sum::<f32>() / n, p.iter().map(|q| q[1]).sum::<f32>() / n]
}

/// Frames that don't matter take the value of the nearest that does.
fn fill_gaps(v: &[Option<f32>]) -> Vec<f32> {
    let n = v.len();
    (0..n).map(|i| (0..=n / 2).find_map(|k| v[(i + k) % n].or(v[(i + n - k) % n])).unwrap_or(0.)).collect()
}

/// Each absent frame takes the value of the nearest frame that has one
/// (around the loop), so a part moves only while it is hidden.
fn filled(vals: &[Option<Val>]) -> Vec<Val> {
    let n = vals.len();
    (0..n)
        .map(|i| {
            (0..=n / 2)
                .find_map(|k| vals[(i + k) % n].as_ref().or(vals[(i + n - k) % n].as_ref()))
                .cloned()
                .expect("a slot is present somewhere")
        })
        .collect()
}

/// The carry at every frame: the fitted one where it fits, otherwise eased
/// straight across from the frames around (around the loop).
fn between(fits: &[Option<[f32; 3]>]) -> Vec<[f32; 3]> {
    let n = fits.len();
    (0..n)
        .map(|i| {
            if let Some(f) = fits[i] {
                return f;
            }
            let back = (1..n).find(|k| fits[(i + n - k) % n].is_some()).unwrap_or(0);
            let fwd = (1..n).find(|k| fits[(i + k) % n].is_some()).unwrap_or(0);
            let (a, b) = (fits[(i + n - back) % n].unwrap_or([1., 0., 0.]), fits[(i + fwd) % n].unwrap_or([1., 0., 0.]));
            let u = back as f32 / (back + fwd).max(1) as f32;
            [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * u)
        })
        .collect()
}

/// Fits `b ≈ s·a + t` over the points; Some((s, tx, ty)) when it holds
/// (arc radii scale too, angles stay).
fn fit(parts: &[Part], a: &[f32], b: &[f32], tol: f32) -> Option<[f32; 3]> {
    let pts = |v: &[f32]| -> Vec<[f32; 2]> {
        let mut out = Vec::new();
        for (k, p) in parts.iter().enumerate() {
            if *p == Part::X {
                out.push([v[k], v[k + 1]]);
            }
        }
        out
    };
    let (pa, pb) = (pts(a), pts(b));
    let c = |p: &[[f32; 2]]| {
        let n = p.len().max(1) as f32;
        [p.iter().map(|q| q[0]).sum::<f32>() / n, p.iter().map(|q| q[1]).sum::<f32>() / n]
    };
    let (ca, cb) = (c(&pa), c(&pb));
    let (mut num_, mut den) = (0., 0.);
    for (p, q) in pa.iter().zip(&pb) {
        let (ax, ay, bx, by) = (p[0] - ca[0], p[1] - ca[1], q[0] - cb[0], q[1] - cb[1]);
        num_ += ax * bx + ay * by;
        den += ax * ax + ay * ay;
    }
    let s = if den > 1e-6 { num_ / den } else { 1. };
    if (s - 1.).abs() > 1e-4 {
        // A scale only for a real shape, by a sane amount: any straight
        // line "scales" into any longer one on its axis, about an origin
        // far away (a leader growing 40x, swung by thousands of units).
        let (mut xx, mut xy, mut yy) = (0f32, 0f32, 0f32);
        for p in &pa {
            let (x, y) = (p[0] - ca[0], p[1] - ca[1]);
            xx += x * x;
            xy += x * y;
            yy += y * y;
        }
        let flat = xx * yy - xy * xy <= 1e-3 * (xx + yy).powi(2);
        if flat || !(0.25..=4.).contains(&s) {
            return None;
        }
    }
    let (tx, ty) = (cb[0] - s * ca[0], cb[1] - s * ca[1]);
    let ok = parts.iter().enumerate().all(|(k, p)| {
        let want = match p {
            Part::X => s * a[k] + tx,
            Part::Y => s * a[k] + ty,
            Part::R => s * a[k],
            Part::Rot => a[k],
        };
        (want - b[k]).abs() <= tol
    });
    ok.then_some([s, tx, ty])
}

/// The loop as one animated SVG. `frames` are evenly spaced samples of
/// one loop of `seconds`; the first is what shows before it plays.
pub fn loop_svg(title: &str, frames: &[Vec<Item>], cams: &[Option<(f32, P2)>], seconds: f32, palette: &Palette, pad: f32, px: f32) -> String {
    loop_in(title, frames, cams, seconds, palette, pad, view(frames), px)
}

/// The bounds of every frame together: the loop's view.
pub fn view(frames: &[Vec<Item>]) -> Rect {
    let all: Vec<Item> = frames.iter().flatten().cloned().collect();
    bounds(&all).unwrap_or([0., 0., 1., 1.])
}

/// `loop_svg` framed on `b` (a single frame in a loop's view, to compare).
/// `cams` is each frame's camera (`super::lens`), or empty: with one, the
/// parts of the world are drawn as the world has them, inside one group
/// that the camera moves, instead of each carrying the camera itself.
/// `px` is how far (in pixels, the drawing 1600 px across) any point may
/// stray from where the scene puts it.
#[allow(clippy::too_many_arguments)]
pub fn loop_in(
    title: &str,
    frames: &[Vec<Item>],
    cams: &[Option<(f32, P2)>],
    seconds: f32,
    palette: &Palette,
    pad: f32,
    b: Rect,
    px: f32,
) -> String {
    let (w, h) = (b[2] - b[0] + 2. * pad, b[3] - b[1] + 2. * pad);
    let (dx, dy) = (pad - b[0], pad - b[1]);
    let n = frames.len();
    // How far a point may stray, in the scene's units (scenes come in all
    // sizes; the error on screen is what shows).
    let geo = w.max(h) / 1600. * px;
    // The camera at every frame, as screen = off + zoom · world.
    let cam: Vec<(f32, P2)> = (0..n).map(|i| cams.get(i).copied().flatten().unwrap_or((1., [0., 0.]))).collect();
    let filmed = cam.iter().any(|(z, o)| (z - 1.).abs() > 1e-6 || o[0].abs() > 1e-4 || o[1].abs() > 1e-4);
    // An error in world units shows as large as the camera ever brings it.
    let zmax = cam.iter().fold(1f32, |m, c| m.max(c.0));

    let mut keys: HashMap<String, u32> = HashMap::new();
    let mut meta = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    for (fi, items) in frames.iter().enumerate() {
        let mut toks = Vec::new();
        for item in items {
            if item.alpha <= 0. {
                continue;
            }
            let Some((class, fill, flow)) = paint_of(item, palette) else { continue };
            let group = (item.layer >= NAMED).then_some(item.layer);
            let world = filmed && item.anchor == Anchor::World;
            let k = format!("{class}|{fill}|{group:?}|{world}|{}", signature(&item.path));
            let key = *keys.entry(k).or_insert_with(|| {
                meta.push((class, fill, group, world));
                meta.len() as u32 - 1
            });
            let path = if world { unfilm(&item.path, cam[fi]) } else { item.path.clone() };
            let v = nums(&path, dx, dy);
            toks.push(Tok {
                key,
                hash: geo_hash(&v),
                shape: path,
                val: Val { nums: v, alpha: item.alpha, dash: flow },
            });
        }
        merge(&mut slots, toks, fi, n, &meta);
    }

    let mut reg = Reg::default();
    let (mut carried, mut spun, mut morphed) = (0, 0, 0);
    // A named layer's opacity at every frame (0 where none of it is drawn):
    // one fade for the whole layer.
    let mut layers: HashMap<u32, Vec<f32>> = HashMap::new();
    for slot in &slots {
        if let Some(g) = slot.group {
            let a = layers.entry(g).or_insert_with(|| vec![0.; n]);
            for (i, v) in slot.vals.iter().enumerate() {
                if let Some(v) = v {
                    a[i] = v.alpha;
                }
            }
        }
    }
    let mut plans = Vec::with_capacity(slots.len());
    let screen_geo = geo;
    for slot in &slots {
        let geo = if slot.world { screen_geo / zmax } else { screen_geo };
        // What a frame shows of it: nothing where it's absent or faded out,
        // and then where it is or how it looks doesn't matter.
        let seen: Vec<Option<Val>> = slot.vals.iter().map(|v| v.clone().filter(|v| v.alpha > 0.004)).collect();
        if seen.iter().all(|v| v.is_none()) {
            continue;
        }
        let vals = filled(&seen);
        // One sample per frame, then the first again to close the loop.
        let series = |f: &dyn Fn(usize) -> Vec<f32>| -> Vec<Vec<f32>> { (0..=n).map(|i| f(i % n)).collect() };
        let fade = |reg: &mut Reg, v: &[Vec<f32>]| -> (String, String) {
            // Only there or not: switch visibility, which Chrome draws in
            // place. An opacity animation (above all one inside another)
            // gets a compositor layer, and everything painted over it
            // turns soft.
            if v.iter().all(|x| x[0] == 0. || x[0] == 1.) {
                if v.iter().all(|x| x[0] == v[0][0]) {
                    let attr = if v[0][0] == 0. { r#" visibility="hidden""#.to_string() } else { String::new() };
                    return (attr, String::new());
                }
                let mut times = vec!["0".to_string()];
                let mut vis = vec![v[0][0]];
                for i in 1..n {
                    if v[i][0] != v[i - 1][0] {
                        times.push(at(i, n));
                        vis.push(v[i][0]);
                    }
                }
                // Never nested: a visible child shows even inside a hidden
                // group (and Chrome won't animate to "inherit"), so only one
                // level of a chain switches visibility.
                let words: Vec<&str> = vis.iter().map(|x| if *x > 0.5 { "visible" } else { "hidden" }).collect();
                let anim = format!(
                    r#"<animate attributeName="visibility" dur="{}s" repeatCount="indefinite" calcMode="discrete" keyTimes="{}" values="{}"/>"#,
                    num(seconds),
                    times.join(";"),
                    words.join(";")
                );
                return (String::new(), anim);
            }
            let anim = reg.smil("opacity", v, 0.01, |keys, v| {
                smil_tag("animate", r#"attributeName="opacity" "#, seconds, keys, n, |i| num(v[i][0]))
            });
            let attr = if anim.is_empty() && v[0][0] < 0.995 { format!(r#" opacity="{}""#, num(v[0][0])) } else { String::new() };
            (attr, anim)
        };

        // A named layer fades as one picture, so a fading box doesn't show
        // its own back edges; its parts then only say when they're there,
        // and only where the layer shows at all.
        // A layer that only switches on and off needs no group: its switch
        // folds into each part's own.
        let group = slot.group.filter(|g| layers[g].iter().any(|x| *x > 0.004 && *x < 0.996));
        let layer = group.map(|g| {
            let a = &layers[&g];
            let (attr, anim) = fade(&mut reg, &series(&|i| vec![a[i]]));
            (g, attr, anim)
        });
        let snap = |x: f32| if x >= 0.996 { 1. } else if x <= 0.004 { 0. } else { x };
        let op = match group {
            Some(g) => {
                let a = &layers[&g];
                let shown: Vec<Option<f32>> =
                    (0..n).map(|i| (a[i] > 0.004).then(|| if seen[i].is_some() { 1. } else { 0. })).collect();
                let known: Vec<f32> = shown.iter().flatten().copied().collect();
                if known.iter().all(|x| *x == known[0]) {
                    series(&|_| vec![known[0]])
                } else {
                    let full = fill_gaps(&shown);
                    series(&|i| vec![full[i]])
                }
            }
            None => series(&|i| vec![seen[i].as_ref().map_or(0., |v| snap(v.alpha))]),
        };
        let (op_attr, op_anim) = fade(&mut reg, &op);

        // Dash flow, unwrapped so the dashes keep running the same way.
        let mut own = String::new();
        if vals.iter().any(|v| v.dash.is_some()) {
            let mut acc = Vec::with_capacity(n + 1);
            let mut prev = dash_offset(vals[0].dash.unwrap_or(0.));
            acc.push(vec![prev]);
            for i in 1..=n {
                let o = dash_offset(vals[i % n].dash.unwrap_or(0.));
                prev += (o - prev.rem_euclid(5.) + 7.5).rem_euclid(5.) - 2.5;
                acc.push(vec![prev]);
            }
            own += &reg.smil("dash", &acc, 0.05, |idx, v| {
                smil_tag("animate", r#"attributeName="stroke-dashoffset" "#, seconds, idx, n, |i| num(v[i][0]))
            });
        }

        // Geometry: still, carried, spun, or morphed.
        let parts = parts(&slot.shape);
        let first = slot.vals.iter().position(|v| v.is_some()).unwrap_or(0);
        let still = vals.iter().all(|v| v.nums.iter().zip(&vals[first].nums).all(|(a, b)| (a - b).abs() < 0.01));
        // Carry from the shape it has most of the time, not from a frame
        // where it is half clipped; from the loop's start when that works,
        // so the parts of one body move alike.
        let pick = if still {
            first
        } else {
            let fitting = |b: usize| vals.iter().filter(|v| fit(&parts, &vals[b].nums, &v.nums, geo / 4.).is_some()).count();
            let mut best = (fitting(0), 0);
            // A few dozen candidates are plenty: a shape held for a while
            // shows up among them.
            for b in (1..n).step_by(n.div_ceil(64)) {
                if best.0 == n {
                    break;
                }
                let f = fitting(b);
                if f > best.0 {
                    best = (f, b);
                }
            }
            best.1
        };
        let base = vals[pick].nums.clone();
        let fits: Vec<Option<[f32; 3]>> = vals.iter().map(|v| if still { None } else { fit(&parts, &base, &v.nums, geo / 4.) }).collect();
        let rigid = fits.iter().filter(|f| f.is_some()).count();
        // How far the part reaches from the origin, which a scale swings it
        // about: an error in the scale moves its points that much more.
        let reach = base.iter().zip(&parts).filter(|(_, p)| matches!(p, Part::X | Part::Y)).fold(1f32, |m, (v, _)| m.max(v.abs()));
        let carry_smil = |reg: &mut Reg, c: &[[f32; 3]], tol: f32| -> String {
            let scaled = c.iter().any(|f| (f[0] - 1.).abs() > 1e-5);
            if !scaled {
                return reg.smil("translate", &series(&|i| vec![c[i][1], c[i][2]]), tol, |keys, v| {
                    smil_tag("animateTransform", r#"attributeName="transform" type="translate" "#, seconds, keys, n, |i| {
                        format!("{} {}", num(v[i][0]), num(v[i][1]))
                    })
                });
            }
            // Translate and scale on the same keys, planned as one motion
            // (the scale in units of its reach), so their errors can't add up
            // past `tol` between keys.
            let joint = series(&|i| vec![c[i][1], c[i][2], c[i][0] * reach]);
            reg.smil(&format!("carry {reach}"), &joint, tol / 2., |keys, v| {
                let tr = smil_tag("animateTransform", r#"attributeName="transform" type="translate" "#, seconds, keys, n, |i| {
                    format!("{} {}", fine(v[i][0]), fine(v[i][1]))
                });
                let sc = smil_tag("animateTransform", r#"attributeName="transform" type="scale" additive="sum" "#, seconds, keys, n, |i| {
                    format!("{}", (v[i][2] as f64 / reach as f64 * 1e6).round() / 1e6)
                });
                format!("{tr}{sc}")
            })
        };
        let mut motion = String::new();
        let mut frame_attr = String::new();
        let mut start = base.clone();
        if still {
        } else if rigid == n {
            motion = carry_smil(&mut reg, &fits.iter().map(|f| f.expect("rigid")).collect::<Vec<_>>(), geo);
            carried += 1;
        } else if let Some(sp) = spin(&parts, &base, &vals, geo / 4.) {
            let m = PLANES[sp.plane];
            frame_attr = format!(r#" transform="matrix({} {} {} {} 0 0)""#, m[0][0], m[1][0], m[0][1], m[1][1]);
            let r = sp.local.chunks(2).map(|p| ((p[0] - sp.center[0]).powi(2) + (p[1] - sp.center[1]).powi(2)).sqrt()).fold(1f32, f32::max);
            let tol = (geo / r).to_degrees();
            let (cx, cy) = (num(sp.center[0]), num(sp.center[1]));
            // The loop closes a whole number of turns on, not back where it began.
            let mut turn: Vec<Vec<f32>> = sp.deg.iter().map(|d| vec![*d]).collect();
            let last = sp.deg[n - 1];
            turn.push(vec![last + ((sp.deg[0] - last).rem_euclid(360.) + 540.).rem_euclid(360.) - 180.]);
            motion = reg.smil(&format!("rotate {} {cx} {cy}", sp.plane), &turn, tol, |idx, v| {
                smil_tag("animateTransform", r#"attributeName="transform" type="rotate" additive="sum" "#, seconds, idx, n, |i| {
                    format!("{} {cx} {cy}", (v[i][0] * 100.).round() / 100.)
                })
            });
            start = sp.local;
            spun += 1;
        } else {
            // Mostly carried but reshaped for a while (clipped as it leaves
            // a port): carry it, and morph only what the carry can't explain.
            // Only when the fitting frames really travel: a part that just
            // grows (a gauge filling) is a morph, not a carry.
            let travels = {
                let f: Vec<&[f32; 3]> = fits.iter().flatten().collect();
                f.iter().any(|a| (a[0] - f[0][0]).abs() > 1e-3 || (a[1] - f[0][1]).abs() > geo || (a[2] - f[0][2]).abs() > geo)
            };
            let carry: Option<Vec<[f32; 3]>> = (rigid * 2 >= n && travels).then(|| between(&fits));
            let local = |i: usize| -> Vec<f32> {
                let v = &vals[i % n].nums;
                match &carry {
                    Some(c) => {
                        let [s_, tx, ty] = c[i % n];
                        parts
                            .iter()
                            .zip(v)
                            .map(|(p, x)| match p {
                                Part::X => (x - tx) / s_,
                                Part::Y => (x - ty) / s_,
                                Part::R => x / s_,
                                Part::Rot => *x,
                            })
                            .collect()
                    }
                    None => v.clone(),
                }
            };
            let v = series(&|i| local(i));
            start = v[0].clone();
            // Carried too: the carry and the morph each get half the error.
            let tol = if carry.is_some() { geo / 2. } else { geo };
            if !flat(&v, tol) {
                let keys = plan(&v, tol);
                own += &smil_tag("animate", r#"attributeName="d" "#, seconds, &keys, n, |i| tight(&with_nums(&slot.shape, &v[i]), false));
            }
            if let Some(c) = &carry {
                motion = carry_smil(&mut reg, c, geo / 2.);
            }
            morphed += 1;
        }

        let mut attrs = String::new();
        if !slot.class.is_empty() {
            let _ = write!(attrs, r#" class="{}""#, slot.class);
        }
        let mut st = slot.fill.clone();
        if let Some(f) = vals[0].dash {
            if !st.is_empty() {
                st.push(';');
            }
            let _ = write!(st, "stroke-dashoffset:{}", num(dash_offset(f)));
        }
        if !st.is_empty() {
            let _ = write!(attrs, r#" style="{st}""#);
        }
        let d = tight(&with_nums(&slot.shape, &start), !own.contains(r#"attributeName="d""#));
        plans.push(Plan {
            world: slot.world,
            layer,
            fade: (op_attr, op_anim),
            motion: (frame_attr, motion),
            path: format!(r#"<path{attrs} d="{d}""#),
            own,
        });
    }

    // Paint order as sampled. Runs that share a layer, then a fade, then a
    // motion each go in one <g> carrying it; a run of one puts it on the
    // path itself.
    let mut body = String::new();
    let runs = |from: usize, to: usize, key: &dyn Fn(&Plan) -> String| -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = from;
        while i < to {
            let k = key(&plans[i]);
            let len = plans[i..to].iter().take_while(|p| key(p) == k).count();
            out.push((i, i + len));
            i += len;
        }
        out
    };
    let pair = |p: &(String, String)| format!("{}|{}", p.0, p.1);
    // The camera: screen = off + zoom · world, around the view's margin.
    let lens = if filmed {
        let t: Vec<[f32; 3]> = cam.iter().map(|(z, o)| [*z, o[0] + dx * (1. - z), o[1] + dy * (1. - z)]).collect();
        let same = t.iter().all(|c| c.iter().zip(&t[0]).all(|(a, b)| (a - b).abs() < 1e-5));
        if same {
            (format!(r#" transform="translate({} {}) scale({})""#, fine(t[0][1]), fine(t[0][2]), (t[0][0] as f64 * 1e6).round() / 1e6), String::new())
        } else {
            let reach = w.max(h);
            let joint: Vec<Vec<f32>> = (0..=n).map(|i| vec![t[i % n][1], t[i % n][2], t[i % n][0] * reach]).collect();
            let anim = reg.smil("camera", &joint, screen_geo / 2., |keys, v| {
                let tr = smil_tag("animateTransform", r#"attributeName="transform" type="translate" "#, seconds, keys, n, |i| {
                    format!("{} {}", fine(v[i][0]), fine(v[i][1]))
                });
                let sc = smil_tag("animateTransform", r#"attributeName="transform" type="scale" additive="sum" "#, seconds, keys, n, |i| {
                    format!("{}", (v[i][2] as f64 / reach as f64 * 1e6).round() / 1e6)
                });
                format!("{tr}{sc}")
            });
            (String::new(), anim)
        }
    } else {
        (String::new(), String::new())
    };
    for (wa, wb) in runs(0, plans.len(), &|p: &Plan| p.world.to_string()) {
        let filmed_run = plans[wa].world;
        if filmed_run {
            let _ = writeln!(body, "<g{}>{}", lens.0, lens.1);
        }
        for (a, b) in runs(wa, wb, &|p: &Plan| format!("{:?}", p.layer)) {
            let lg = plans[a].layer.clone().filter(|(_, at, an)| !at.is_empty() || !an.is_empty());
            if let Some((_, attr, anim)) = &lg {
                let _ = writeln!(body, "<g{attr}>{anim}");
            }
            for (c, d) in runs(a, b, &|p: &Plan| pair(&p.fade)) {
                let fade = plans[c].fade.clone();
                let wrap_fade = (!fade.0.is_empty() || !fade.1.is_empty()) && d - c > 1;
                if wrap_fade {
                    let _ = writeln!(body, "<g{}>{}", fade.0, fade.1);
                }
                for (e, f) in runs(c, d, &|p: &Plan| pair(&p.motion)) {
                    let motion = plans[e].motion.clone();
                    let wrap_motion = (!motion.0.is_empty() || !motion.1.is_empty()) && f - e > 1;
                    if wrap_motion {
                        let _ = writeln!(body, "<g{}>{}", motion.0, motion.1);
                    }
                    for p in &plans[e..f] {
                        let mut attr = String::new();
                        let mut kids = String::new();
                        if !wrap_fade {
                            attr += &fade.0;
                            kids += &fade.1;
                        }
                        if !wrap_motion {
                            attr += &motion.0;
                            kids += &motion.1;
                        }
                        kids += &p.own;
                        if kids.is_empty() {
                            let _ = writeln!(body, "{}{attr}/>", p.path);
                        } else {
                            let _ = writeln!(body, "{}{attr}>{kids}</path>", p.path);
                        }
                    }
                    if wrap_motion {
                        body.push_str("</g>\n");
                    }
                }
                if wrap_fade {
                    body.push_str("</g>\n");
                }
            }
            if lg.is_some() {
                body.push_str("</g>\n");
            }
        }
        if filmed_run {
            body.push_str("</g>\n");
        }
    }

    let id = svg_id(title);
    let mut s = String::new();
    let _ = writeln!(
        s,
        r#"<svg class="iso-{id}" xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" fill="none" role="img"><title>{title}</title>"#,
        w = num(w),
        h = num(h),
    );
    let _ = writeln!(
        s,
        "<!-- {} paths: {carried} carried, {spun} spun, {morphed} morphed; {} motions -->",
        slots.len(),
        reg.sets
    );
    let _ = writeln!(s, "<style>\n{}\n</style>", style(&id, palette));
    s.push_str(&body);
    s.push_str("</svg>\n");
    s
}

/// How one slot is written: its layer's fade, the fade and the motion it
/// may share with its neighbours (attributes, SMIL children), and its own
/// path with what only it does (dash flow, morph).
struct Plan {
    world: bool,
    layer: Option<(u32, String, String)>,
    fade: (String, String),
    motion: (String, String),
    path: String,
    own: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tight_paths_keep_every_point() {
        let p = [Seg::M([10.5, 20.]), Seg::L([10.5, 25.25]), Seg::L([0.05, 25.25]), Seg::A { r: [2., 1.], rot: 0., large: false, sweep: true, to: [1., 1.] }, Seg::Z];
        assert_eq!(tight(&p, true), "m10.5 20v5.25h-10.45a2 1 0 0 1 .95-24.25z");
        assert_eq!(tight(&p, false), "m10.5 20l0 5.25-10.45 0a2 1 0 0 1 .95-24.25z");
    }

    #[test]
    fn eased_stretch_fits_every_sample() {
        let n = 1296;
        let io = |q: f32| crate::iso::cubic_bezier(0.77, 0., 0.175, 1., q.clamp(0., 1.));
        let v: Vec<Vec<f32>> = (0..=n).map(|i| vec![0., -40. + 40. * io((i as f32 / n as f32 - 0.84) / 0.11)]).collect();
        let keys = plan(&v, 0.02);
        for w in keys.windows(2) {
            let ((a, c), (b, _)) = (w[0], w[1]);
            for i in a..=b {
                let s = (i - a) as f32 / (b - a) as f32;
                let f = crate::iso::cubic_bezier(c[0], c[1], c[2], c[3], s);
                let want = v[i][1];
                let got = v[a][1] + (v[b][1] - v[a][1]) * f;
                assert!((got - want).abs() <= 0.021, "keys {a}..{b} {c:?}: at {i} {got} vs {want}");
            }
        }
    }

    #[test]
    fn keeps_only_the_turns() {
        let v: Vec<Vec<f32>> = (0..=10).map(|i| vec![if i <= 5 { i as f32 } else { 10. - i as f32 }]).collect();
        assert_eq!(keep(&v, 0.01), vec![0, 5, 10]);
    }

    #[test]
    fn a_moving_box_is_carried_not_morphed() {
        let pal = Palette { ink: [0.16; 3], ground: [1.; 3], accent: [0.11, 0.69, 0.61], dark: false, soft: false };
        let frames: Vec<Vec<Item>> = (0..8)
            .map(|i| {
                let x = (i as f32 / 8. * std::f32::consts::TAU).sin() * 20.;
                crate::iso::render(&[
                    crate::iso::Node::new(crate::iso::Shape::Cylinder { base: [0., 0., 0.], r: 30., h: 4. }),
                    crate::iso::Node::new(crate::iso::Shape::Cylinder { base: [x, 0., 4.], r: 4., h: 6. }),
                ])
            })
            .collect();
        let svg = loop_svg("Box", &frames, &[], 2., &pal, 16., 0.1);
        assert!(svg.contains(r#"type="translate""#), "{svg}");
        assert!(!svg.contains(r#"attributeName="d""#), "nothing changes shape");
    }
}
