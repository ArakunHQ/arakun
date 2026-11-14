//! Scenes written as Rhai scripts, so new drawings need no rebuild.
//!
//! A scene is a `.rhai` file. Every frame it runs top to bottom with `t`
//! (0..1, the place in the loop) in scope, and its last expression is the
//! nodes to paint, back to front: an array, arrays inside it are flattened.
//! `scene(#{ name, about, seconds, look })` names it. The functions it can
//! call are below (`api`); `.claude/skills/arakun-iso` documents them.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rhai::{AST, Array, Dynamic, Engine, EvalAltResult, FLOAT, Map, Scope};

use super::{Clip, Ease, Face, Ink, Node, Shape, Tone, V3, keyed};

type Res<T> = Result<T, Box<EvalAltResult>>;

/// What `scene(#{...})` says about the drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub name: String,
    pub about: String,
    pub seconds: f32,
    /// `"paper"` or `"arakun"`: the look it is drawn for.
    pub look: Option<String>,
    /// How many equal steps the loop is, for the step-by-step guide (0:
    /// not a guided scene).
    pub steps: u32,
    /// Where in each step the guide stops to let it be read, 0..1.
    pub hold: f32,
    /// `style: "soft"`: gradients, contact shadows, lighter outlines.
    pub soft: bool,
}

pub struct ScriptScene {
    pub path: PathBuf,
    pub meta: Meta,
    engine: Engine,
    ast: AST,
}

/// Script steps a frame may take: enough for thousands of shapes, few
/// enough that a runaway loop stops instead of freezing the window.
const MAX_OPERATIONS: u64 = 2_000_000;

impl ScriptScene {
    pub fn load(path: &Path) -> Result<ScriptScene, String> {
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let meta = Rc::new(RefCell::new(Meta {
            name: stem,
            about: String::new(),
            seconds: 4.,
            look: None,
            steps: 0,
            hold: 0.85,
            soft: false,
        }));
        let engine = engine(meta.clone());
        let ast = engine
            .compile(&source)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut scene = ScriptScene {
            path: path.to_path_buf(),
            meta: meta.borrow().clone(),
            engine,
            ast,
        };
        // The first frame runs the `scene(...)` call and proves the script draws.
        scene.frame(0.)?;
        scene.meta = meta.borrow().clone();
        Ok(scene)
    }

    /// The nodes at `t`, with the ambient clock where the loop puts it.
    pub fn frame(&self, t: f32) -> Result<Vec<Node>, String> {
        self.frame_at(t, t * self.meta.seconds)
    }

    /// The nodes at story time `t` (0..1) and ambient time `tau` (seconds):
    /// `tau` keeps running while the guide holds `t` still, for what should
    /// stay alive at a stop (flows, lights, pulses).
    pub fn frame_at(&self, t: f32, tau: f32) -> Result<Vec<Node>, String> {
        let mut scope = Scope::new();
        scope.push("t", t as FLOAT);
        scope.push("tau", tau as FLOAT);
        let out: Dynamic = self
            .engine
            .eval_ast_with_scope(&mut scope, &self.ast)
            .map_err(|e| {
                format!(
                    "{}: {e}",
                    self.path.file_name().unwrap_or_default().to_string_lossy()
                )
            })?;
        let mut nodes = Vec::new();
        flatten(out, &mut nodes).map_err(|e| format!("{}: {e}", self.path.display()))?;
        Ok(nodes)
    }
}

/// The `.rhai` scenes of a folder, by file name; the ones that fail to load
/// come back as their path and the error.
pub fn load_dir(dir: &Path) -> Vec<Result<ScriptScene, (PathBuf, String)>> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rhai"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| ScriptScene::load(&p).map_err(|e| (p, e)))
        .collect()
}

fn flatten(value: Dynamic, out: &mut Vec<Node>) -> Result<(), String> {
    if value.is_unit() {
        return Ok(());
    }
    if value.is_array() {
        for v in value.cast::<Array>() {
            flatten(v, out)?;
        }
        return Ok(());
    }
    if value.is::<Node>() {
        out.push(value.cast::<Node>());
        return Ok(());
    }
    Err(format!("a scene returns shapes, not {}", value.type_name()))
}

// ---- converting script values -------------------------------------------------------

fn num(v: &Dynamic) -> Res<f32> {
    if let Ok(f) = v.as_float() {
        return Ok(f);
    }
    if let Ok(i) = v.as_int() {
        return Ok(i as f32);
    }
    Err(format!("expected a number, got {}", v.type_name()).into())
}

fn nums<const N: usize>(a: &Array, what: &str) -> Res<[f32; N]> {
    if a.len() != N {
        return Err(format!("{what} takes {N} numbers, got {}", a.len()).into());
    }
    let mut out = [0.; N];
    for (o, v) in out.iter_mut().zip(a) {
        *o = num(v)?;
    }
    Ok(out)
}

fn v3(a: &Array) -> Res<V3> {
    nums::<3>(a, "a point [x, y, z]")
}

fn points(a: &Array) -> Res<Vec<V3>> {
    a.iter()
        .map(|p| {
            p.clone()
                .try_cast::<Array>()
                .ok_or_else(|| "points are [x, y, z] arrays".into())
                .and_then(|p| v3(&p))
        })
        .collect()
}

fn tone(name: &str) -> Res<Tone> {
    Ok(match name {
        "top" => Tone::Top,
        "left" => Tone::Left,
        "right" => Tone::Right,
        "recess" => Tone::Recess,
        "accent_top" => Tone::AccentTop,
        "accent_left" => Tone::AccentLeft,
        "accent_right" => Tone::AccentRight,
        "ink" => Tone::Ink,
        "mute" => Tone::Mute,
        "accent" => Tone::Accent,
        "ground" => Tone::Ground,
        "dark" => Tone::DarkTop,
        "mid" => Tone::MidTop,
        _ => return Err(format!("no fill \"{name}\": top, left, right, recess, accent_top, accent_left, accent_right, ink, mute, accent, ground").into()),
    })
}

fn ink(name: &str) -> Res<Option<Ink>> {
    Ok(Some(match name {
        "edge" => Ink::Edge,
        "mute" => Ink::Mute,
        "accent" => Ink::Accent,
        "accent_mute" => Ink::AccentMute,
        "none" => return Ok(None),
        _ => {
            return Err(format!("no ink \"{name}\": edge, mute, accent, accent_mute, none").into());
        }
    }))
}

fn ease(name: &str) -> Res<Ease> {
    Ok(match name {
        "out" => Ease::Out,
        "in_out" => Ease::InOut,
        "linear" => Ease::Linear,
        _ => return Err(format!("no easing \"{name}\": out, in_out, linear").into()),
    })
}

/// The point `u` (0..1) of the way along a polyline, by length.
pub fn along(points: &[V3], u: f32) -> V3 {
    let seg_len = |a: &V3, b: &V3| {
        ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt()
    };
    let total: f32 = points.windows(2).map(|w| seg_len(&w[0], &w[1])).sum();
    let Some(first) = points.first() else {
        return [0.; 3];
    };
    if total <= 0. {
        return *first;
    }
    let mut left = u.clamp(0., 1.) * total;
    for w in points.windows(2) {
        let l = seg_len(&w[0], &w[1]);
        if left <= l {
            let q = if l > 0. { left / l } else { 0. };
            return [0, 1, 2].map(|i| w[0][i] + (w[1][i] - w[0][i]) * q);
        }
        left -= l;
    }
    *points.last().unwrap_or(first)
}

fn arr(p: V3) -> Array {
    p.iter().map(|&v| Dynamic::from_float(v)).collect()
}

// ---- the API ---------------------------------------------------------------------------

fn engine(meta: Rc<RefCell<Meta>>) -> Engine {
    let mut e = Engine::new();
    e.set_max_operations(MAX_OPERATIONS);
    e.set_max_expr_depths(128, 64);
    e.register_type_with_name::<Node>("Shape");
    e.register_type_with_name::<Face>("Face");

    e.register_fn("scene", move |m: Map| -> Res<()> {
        let mut meta = meta.borrow_mut();
        for (k, v) in m {
            match k.as_str() {
                "name" => meta.name = v.to_string(),
                "about" => meta.about = v.to_string(),
                "seconds" => meta.seconds = num(&v)?.max(0.2),
                "look" => meta.look = Some(v.to_string()),
                "steps" => meta.steps = num(&v)?.max(0.) as u32,
                "hold" => meta.hold = num(&v)?.clamp(0.05, 0.98),
                "style" => meta.soft = v.to_string() == "soft",
                other => {
                    return Err(format!(
                        "scene() has no \"{other}\": name, about, seconds, look, steps, hold, style"
                    )
                    .into());
                }
            }
        }
        Ok(())
    });

    // Faces for flat things.
    e.register_fn("top", |z: Dynamic| -> Res<Face> { Ok(Face::Top(num(&z)?)) });
    e.register_fn("left", |y: Dynamic| -> Res<Face> {
        Ok(Face::Left(num(&y)?))
    });
    e.register_fn("right", |x: Dynamic| -> Res<Face> {
        Ok(Face::Right(num(&x)?))
    });

    // Shapes.
    e.register_fn("slab", |min: Array, size: Array| -> Res<Node> {
        Ok(Node::new(Shape::Slab {
            min: v3(&min)?,
            size: v3(&size)?,
            r: 0.,
        }))
    });
    e.register_fn("slab", |min: Array, size: Array, r: Dynamic| -> Res<Node> {
        Ok(Node::new(Shape::Slab {
            min: v3(&min)?,
            size: v3(&size)?,
            r: num(&r)?,
        }))
    });
    e.register_fn(
        "cylinder",
        |base: Array, r: Dynamic, h: Dynamic| -> Res<Node> {
            Ok(Node::new(Shape::Cylinder {
                base: v3(&base)?,
                r: num(&r)?,
                h: num(&h)?,
            }))
        },
    );
    e.register_fn(
        "tube_back",
        |base: Array, r: Dynamic, inner: Dynamic, h: Dynamic| -> Res<Node> {
            Ok(Node::new(Shape::TubeBack {
                base: v3(&base)?,
                r: num(&r)?,
                inner: num(&inner)?,
                h: num(&h)?,
            })
            .side_ink(Ink::Mute))
        },
    );
    e.register_fn(
        "tube_front",
        |base: Array, r: Dynamic, inner: Dynamic, h: Dynamic| -> Res<Node> {
            Ok(Node::new(Shape::TubeFront {
                base: v3(&base)?,
                r: num(&r)?,
                inner: num(&inner)?,
                h: num(&h)?,
            }))
        },
    );
    e.register_fn(
        "plate",
        |face: Face, rect: Array, r: Dynamic| -> Res<Node> {
            Ok(Node::new(Shape::Plate {
                face,
                rect: nums::<4>(&rect, "a rect [a0, b0, a1, b1]")?,
                r: num(&r)?,
            }))
        },
    );
    e.register_fn("plate", |face: Face, rect: Array| -> Res<Node> {
        Ok(Node::new(Shape::Plate {
            face,
            rect: nums::<4>(&rect, "a rect [a0, b0, a1, b1]")?,
            r: 0.,
        }))
    });
    e.register_fn("disc", |face: Face, c: Array, r: Dynamic| -> Res<Node> {
        Ok(Node::new(Shape::Disc {
            face,
            c: nums::<2>(&c, "a center [a, b]")?,
            r: num(&r)?,
        }))
    });
    e.register_fn("card", |at: Array, size: Array, r: Dynamic| -> Res<Node> {
        Ok(Node::new(Shape::Card {
            at: v3(&at)?,
            size: nums::<2>(&size, "a size [w, h]")?,
            r: num(&r)?,
        }))
    });
    // Organic bodies.
    e.register_fn("ellipsoid", |c: Array, r: Array| -> Res<Node> {
        let r = v3(&r)?;
        Ok(Node::new(Shape::Ellipsoid {
            c: v3(&c)?,
            axes: [[r[0], 0., 0.], [0., r[1], 0.], [0., 0., r[2]]],
        }))
    });
    e.register_fn("sphere", |c: Array, r: Dynamic| -> Res<Node> {
        let r = num(&r)?;
        Ok(Node::new(Shape::Ellipsoid {
            c: v3(&c)?,
            axes: [[r, 0., 0.], [0., r, 0.], [0., 0., r]],
        }))
    });
    fn body(n: &Node) -> Res<(V3, [V3; 3])> {
        match n.shape {
            Shape::Ellipsoid { c, axes } | Shape::Decal { c, axes, .. } => Ok((c, axes)),
            _ => Err("this works on ellipsoids and their decals".into()),
        }
    }
    fn set_axes(n: &mut Node, f: impl Fn(V3) -> V3) {
        if let Shape::Ellipsoid { axes, .. } | Shape::Decal { axes, .. } = &mut n.shape {
            *axes = axes.map(&f);
        }
    }
    // Turns it about the world's up axis through its center.
    e.register_fn("yaw", |mut n: Node, deg: Dynamic| -> Res<Node> {
        body(&n)?;
        let d = num(&deg)?;
        set_axes(&mut n, |a| super::yaw(a, d));
        Ok(n)
    });
    // Tilts it so its own up axis points along `up`.
    e.register_fn("orient", |mut n: Node, up: Array| -> Res<Node> {
        body(&n)?;
        let up = v3(&up)?;
        set_axes(&mut n, |a| super::turn_onto(a, [0., 0., 1.], up));
        Ok(n)
    });
    // The world point at `local` in its own axes ([0, 1, 0] is the front of a
    // y-facing head, [0, 0, 1] its top); follows every yaw and orient.
    e.register_fn("at", |n: Node, local: Array| -> Res<Array> {
        let (c, [a, b, d]) = body(&n)?;
        let l = v3(&local)?;
        Ok(arr(
            [0, 1, 2].map(|i| c[i] + a[i] * l[0] + b[i] * l[1] + d[i] * l[2])
        ))
    });
    // A patch painted on it around the local direction `dir`.
    e.register_fn(
        "decal",
        |n: Node, dir: Array, rect: Array, r: Dynamic| -> Res<Node> {
            let (c, axes) = body(&n)?;
            let mut d = Node::new(Shape::Decal {
                c,
                axes,
                dir: v3(&dir)?,
                rect: nums::<4>(&rect, "[lon0, lat0, lon1, lat1]")?,
                r: num(&r)?,
            });
            d.clip = n.clip;
            Ok(d)
        },
    );
    // A soft contact shadow on the ground (soft style only).
    e.register_fn(
        "shadow",
        |c: Array, r: Array, alpha: Dynamic| -> Res<Node> {
            Ok(Node::new(Shape::Shadow {
                c: v3(&c)?,
                r: nums::<2>(&r, "[rx, ry]")?,
                alpha: num(&alpha)?,
            }))
        },
    );
    e.register_fn("merge", |mut n: Node, id: rhai::INT| {
        n.merge = Some(id.max(0) as u32);
        n
    });
    e.register_fn("camera", |focus: Array, zoom: Dynamic| -> Res<Node> {
        Ok(Node::new(Shape::Camera {
            focus: v3(&focus)?,
            zoom: num(&zoom)?,
        }))
    });
    e.register_fn("line", |pts: Array| -> Res<Node> {
        Ok(Node::new(Shape::Line {
            points: points(&pts)?,
        }))
    });
    e.register_fn("text", |at: Array, s: &str, size: Dynamic| -> Res<Node> {
        Ok(Node::new(Shape::Text {
            at: v3(&at)?,
            text: s.into(),
            size: num(&size)?,
            align: 0.,
        }))
    });
    e.register_fn(
        "text",
        |at: Array, s: &str, size: Dynamic, align: Dynamic| -> Res<Node> {
            Ok(Node::new(Shape::Text {
                at: v3(&at)?,
                text: s.into(),
                size: num(&size)?,
                align: num(&align)?,
            }))
        },
    );
    e.register_fn(
        "label",
        |from: Array, length: Dynamic, s: &str| -> Res<Node> {
            Ok(Node::new(Shape::Label {
                from: v3(&from)?,
                length: num(&length)?,
                text: s.into(),
                size: 9.,
                reveal: 1.,
            }))
        },
    );
    e.register_fn(
        "label",
        |from: Array, length: Dynamic, s: &str, size: Dynamic| -> Res<Node> {
            Ok(Node::new(Shape::Label {
                from: v3(&from)?,
                length: num(&length)?,
                text: s.into(),
                size: num(&size)?,
                reveal: 1.,
            }))
        },
    );

    // The look of a shape: each returns the changed shape, so they chain.
    e.register_fn("accent", |n: Node| n.accent());
    // A color of its own, "#rrggbb": solids, bodies, decals and plates.
    e.register_fn("color", |mut n: Node, hex: &str| -> Res<Node> {
        let h = hex.trim_start_matches('#');
        let v = u32::from_str_radix(h, 16)
            .map_err(|_| format!("not a color: {hex} (use \"#rrggbb\")"))?;
        if h.len() != 6 {
            return Err(format!("not a color: {hex} (use \"#rrggbb\")").into());
        }
        let c = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        n.color = Some(c);
        if matches!(n.shape, Shape::Plate { .. } | Shape::Disc { .. }) {
            n.fill = Some(Tone::Custom(c, 0));
        }
        Ok(n)
    });
    e.register_fn("mid", |mut n: Node| {
        n.mid = true;
        if n.fill.is_none() && matches!(n.shape, Shape::Plate { .. } | Shape::Disc { .. }) {
            n.fill = Some(Tone::MidTop);
        }
        n
    });
    e.register_fn("dark", |mut n: Node| {
        n.dark = true;
        if n.fill.is_none() && matches!(n.shape, Shape::Plate { .. } | Shape::Disc { .. }) {
            n.fill = Some(Tone::DarkTop);
        }
        n
    });
    e.register_fn("fill", |n: Node, name: &str| -> Res<Node> {
        Ok(n.fill(tone(name)?))
    });
    e.register_fn("ink", |mut n: Node, name: &str| -> Res<Node> {
        n.ink = ink(name)?;
        Ok(n)
    });
    e.register_fn("side_ink", |mut n: Node, name: &str| -> Res<Node> {
        n.side_ink = ink(name)?;
        Ok(n)
    });
    e.register_fn("dashed", |n: Node| n.dashed());
    // Dashes that travel along the line: `.flow(tau * speed)`.
    e.register_fn("flow", |mut n: Node, f: Dynamic| -> Res<Node> {
        n.flow = num(&f)?;
        n.dashed = true;
        Ok(n)
    });
    e.register_fn("fixed", |mut n: Node| {
        n.fixed = true;
        n
    });
    e.register_fn("reveal", |mut n: Node, r: Dynamic| -> Res<Node> {
        let r = num(&r)?;
        match &mut n.shape {
            Shape::Label { reveal, .. } => *reveal = r,
            _ => return Err("reveal() is for labels".into()),
        }
        Ok(n)
    });
    e.register_fn("alpha", |n: Node, a: Dynamic| -> Res<Node> {
        Ok(n.alpha(num(&a)?))
    });
    e.register_fn("layer", |n: Node, l: rhai::INT| n.layer(l.max(0) as u32));
    e.register_fn("clip_above", |n: Node, z: Dynamic| -> Res<Node> {
        Ok(n.clip(Clip::above(num(&z)?)))
    });
    e.register_fn("clip", |n: Node, min: Array, max: Array| -> Res<Node> {
        Ok(n.clip(Clip {
            min: v3(&min)?,
            max: v3(&max)?,
        }))
    });

    // Motion.
    e.register_fn("keyed", |t: Dynamic, keys: Array| -> Res<FLOAT> {
        let mut ks = Vec::with_capacity(keys.len());
        for k in keys {
            let k = k
                .try_cast::<Array>()
                .ok_or("keys are [time, value] or [time, value, \"easing\"]")?;
            if k.len() < 2 {
                return Err("a key is [time, value] or [time, value, \"easing\"]".into());
            }
            let e = match k.get(2) {
                Some(name) => ease(&name.to_string())?,
                None => Ease::Linear,
            };
            ks.push((num(&k[0])?, num(&k[1])?, e));
        }
        Ok(keyed(num(&t)?, &ks))
    });
    e.register_fn("ease", |name: &str, x: Dynamic| -> Res<FLOAT> {
        Ok(ease(name)?.at(num(&x)?))
    });
    e.register_fn(
        "progress",
        |t: Dynamic, a: Dynamic, b: Dynamic| -> Res<FLOAT> {
            let (t, a, b) = (num(&t)?, num(&a)?, num(&b)?);
            Ok(if b > a {
                ((t - a) / (b - a)).clamp(0., 1.)
            } else if t >= b {
                1.
            } else {
                0.
            })
        },
    );
    e.register_fn("lerp", |a: Dynamic, b: Dynamic, x: Dynamic| -> Res<FLOAT> {
        let (a, b, x) = (num(&a)?, num(&b)?, num(&x)?);
        Ok(a + (b - a) * x)
    });
    e.register_fn(
        "clamp",
        |x: Dynamic, lo: Dynamic, hi: Dynamic| -> Res<FLOAT> {
            Ok(num(&x)?.clamp(num(&lo)?, num(&hi)?))
        },
    );
    // The ground point that lands at screen (sx, sy), for captions and
    // anything placed by where it shows rather than where it stands.
    e.register_fn("at_screen", |sx: Dynamic, sy: Dynamic| -> Res<Array> {
        let (sx, sy) = (num(&sx)? / 0.866_025_4, num(&sy)? * 2.);
        Ok(arr([(sx + sy) / 2., (sy - sx) / 2., 0.]))
    });
    e.register_fn("along", |pts: Array, u: Dynamic| -> Res<Array> {
        Ok(arr(along(&points(&pts)?, num(&u)?)))
    });
    // Where a row of `count` things `size` wide, `step` apart, starts to sit
    // centered in `total`: lay out details by this, never by eye.
    e.register_fn(
        "centered",
        |count: Dynamic, size: Dynamic, step: Dynamic, total: Dynamic| -> Res<FLOAT> {
            let (n, s, st, t) = (num(&count)?, num(&size)?, num(&step)?, num(&total)?);
            Ok((t - ((n - 1.).max(0.) * st + s)) / 2.)
        },
    );
    e.register_fn("text_width", |s: &str, size: Dynamic| -> Res<FLOAT> {
        let size = num(&size)?;
        Ok(super::text::width(s, size, super::text::tracking(s, size)))
    });
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, src: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-iso-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, src).unwrap();
        p
    }

    #[test]
    fn runs_a_scene_per_frame() {
        let p = write(
            "box.rhai",
            r#"
            scene(#{ name: "Box", seconds: 2 });
            let z = keyed(t, [[0, 0, "out"], [1, 10]]);
            [slab([0, 0, z], [10, 10, 4], 2).accent(), [disc(top(z + 4), [5, 5], 2)]]
            "#,
        );
        let s = ScriptScene::load(&p).unwrap();
        assert_eq!(s.meta.name, "Box");
        assert_eq!(s.meta.seconds, 2.);
        assert_eq!(s.frame(0.5).unwrap().len(), 2, "nested arrays flatten");
        assert_ne!(s.frame(0.).unwrap(), s.frame(1.).unwrap());
    }

    #[test]
    fn errors_name_the_line() {
        let p = write("bad.rhai", "let a = 1;\nslab([0, 0], [1, 1, 1])\n");
        let err = ScriptScene::load(&p).err().unwrap();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn a_runaway_loop_stops() {
        let p = write("loop.rhai", "loop { }\n");
        assert!(ScriptScene::load(&p).is_err());
    }

    #[test]
    fn along_walks_by_length() {
        let p = along(&[[0., 0., 0.], [10., 0., 0.], [10., 10., 0.]], 0.75);
        assert!((p[0] - 10.).abs() < 1e-4 && (p[1] - 5.).abs() < 1e-4);
    }
}
