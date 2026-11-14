//! `arakun iso`: renders and checks scene scripts without the window, so an
//! agent can look at what it wrote.

use std::path::{Path, PathBuf};
use std::time::Instant;

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

use super::raster::{self, Fit};
use super::script::ScriptScene;
use super::svg::{Palette, Rgb};
use super::{Rect, Seg};

const USAGE: &str = "\
arakun iso render <scene.rhai> [--at T] [--frames N] [--tau S] [--look paper|arakun] [--width PX] [--out FILE.png]
    one frame at T (0..1, default 0), or a sheet of N frames across the loop;
    with --tau and --frames, N frames of the ambient clock from S at a fixed T
    (what a guide stop does while it waits)
arakun iso svg <scene.rhai> [--at T] [--look paper|arakun] [--out FILE.svg]
    one frame as SVG, with arcs and themeable colors
arakun iso anim <scene.rhai> [--fps N] [--px P] [--look paper|arakun] [--out FILE.svg] [--still T]
    the whole loop as ONE animated SVG: every path once, only what changes
    animated (SMIL), tweened by the browser at the screen's own rate; no
    point strays more than P px (default 0.1) with it 1600 px across. --fps
    is how densely the loop is sampled to find that (default 240); --still
    T writes instead the exact frame at T in the loop's view, to compare
arakun iso check <scene.rhai>
    runs the whole loop: errors, empty frames, the loop seam, time per frame
";

struct Opts {
    file: PathBuf,
    at: f32,
    frames: usize,
    look: Option<String>,
    width: u32,
    out: Option<PathBuf>,
    tau: Option<f32>,
    fps: f32,
    still: Option<f32>,
    px: f32,
}

fn parse(args: &[String], cwd: &Path) -> Result<Opts, String> {
    let mut o = Opts { file: PathBuf::new(), at: 0., frames: 0, look: None, width: 1600, out: None, tau: None, fps: 240., still: None, px: 0.1 };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--at" => o.at = val("--at")?.parse().map_err(|_| "--at takes a number 0..1")?,
            "--frames" => o.frames = val("--frames")?.parse().map_err(|_| "--frames takes a count")?,
            "--look" => o.look = Some(val("--look")?),
            "--width" => o.width = val("--width")?.parse().map_err(|_| "--width takes pixels")?,
            "--out" => o.out = Some(cwd.join(val("--out")?)),
            "--px" => o.px = val("--px")?.parse().map_err(|_| "--px takes pixels")?,
            "--still" => o.still = Some(val("--still")?.parse().map_err(|_| "--still takes a number 0..1")?),
            "--fps" => o.fps = val("--fps")?.parse().map_err(|_| "--fps takes a number")?,
            "--tau" => o.tau = Some(val("--tau")?.parse().map_err(|_| "--tau takes seconds")?),
            f if !f.starts_with("--") && o.file.as_os_str().is_empty() => o.file = cwd.join(f),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if o.file.as_os_str().is_empty() {
        return Err("which scene? give a .rhai file".into());
    }
    Ok(o)
}

fn palette(look: &str) -> Palette {
    let rgb = |c: gpui::Hsla| -> Rgb {
        let c = c.to_rgb();
        [c.r, c.g, c.b]
    };
    match look {
        "arakun" | "night" | "dark" => Palette {
            ink: rgb(crate::theme::text()),
            ground: rgb(crate::theme::background()),
            accent: rgb(crate::theme::accent()),
            dark: true,
            soft: false,
        },
        _ => Palette { ink: [0.165; 3], ground: [1.; 3], accent: [0.114, 0.686, 0.612], dark: false, soft: false },
    }
}

fn out_path(o: &Opts, ext: &str, tag: &str) -> PathBuf {
    o.out.clone().unwrap_or_else(|| {
        let stem = o.file.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        std::env::temp_dir().join("arakun-iso").join(format!("{stem}-{tag}.{ext}"))
    })
}

fn extent(scene: &ScriptScene) -> Result<Rect, String> {
    let mut frames = Vec::new();
    for i in 0..=48 {
        frames.push(scene.frame(i as f32 / 48.)?);
    }
    super::rest_bounds(frames.into_iter()).ok_or_else(|| "the scene draws nothing".into())
}

/// One frame `w` px wide, as tall as the scene needs.
fn frame_pixmap(scene: &ScriptScene, ext: Rect, t: f32, tau: f32, pal: &Palette, w: u32) -> Result<Pixmap, String> {
    let pad = w as f32 * 0.04;
    let aspect = (ext[3] - ext[1]) / (ext[2] - ext[0]).max(1.);
    let h = ((w as f32 - 2. * pad) * aspect + 2. * pad).ceil() as u32;
    let fit = Fit::new(ext, w as f32, h as f32, pad).ok_or("the scene has no size")?;
    let items = super::shoot(&scene.frame_at(t, tau)?, ext);
    raster::frame(&items, pal, w, h, fit, (w as f32 / 1000.).max(1.) * 1.25).ok_or_else(|| "could not draw".into())
}

/// Writes `text` small in the corner of a sheet cell.
fn stamp(pm: &mut Pixmap, text: &str, x: f32, y: f32, color: Rgb) {
    let mut pb = PathBuilder::new();
    for seg in super::text::outline(text, [x, y], 13., 1., 0.) {
        match seg {
            Seg::M([a, b]) => pb.move_to(a, b),
            Seg::L([a, b]) => pb.line_to(a, b),
            Seg::C(c1, c2, to) => pb.cubic_to(c1[0], c1[1], c2[0], c2[1], to[0], to[1]),
            Seg::A { to, .. } => pb.line_to(to[0], to[1]),
            Seg::Z => pb.close(),
        }
    }
    if let Some(path) = pb.finish() {
        let mut paint = Paint { anti_alias: true, ..Paint::default() };
        paint.set_color(Color::from_rgba(color[0], color[1], color[2], 0.6).unwrap_or(Color::BLACK));
        pm.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
    }
}

fn render(o: &Opts) -> Result<String, String> {
    let scene = ScriptScene::load(&o.file)?;
    let mut pal = palette(o.look.as_deref().or(scene.meta.look.as_deref()).unwrap_or("paper"));
    pal.soft = scene.meta.soft;
    let ext = extent(&scene)?;
    let path = if o.frames > 1 {
        let cols = (o.frames as f32).sqrt().ceil() as u32;
        let rows = (o.frames as u32).div_ceil(cols);
        let cell_w = o.width / cols;
        // Across the loop, or (with --tau) across the ambient clock at a stop.
        let at = |i: usize| match o.tau {
            Some(s) => (o.at, s + i as f32 * 0.25),
            None => {
                let t = i as f32 / o.frames as f32;
                (t, t * scene.meta.seconds)
            }
        };
        let cells: Vec<Pixmap> = (0..o.frames)
            .map(|i| {
                let (t, tau) = at(i);
                frame_pixmap(&scene, ext, t, tau, &pal, cell_w)
            })
            .collect::<Result<_, _>>()?;
        let cell_h = cells[0].height();
        let gap = 2;
        let mut sheet = Pixmap::new(cols * (cell_w + gap), rows * (cell_h + gap)).ok_or("sheet too large")?;
        sheet.fill(Color::from_rgba8(128, 128, 128, 255));
        for (i, cell) in cells.iter().enumerate() {
            let (cx, cy) = ((i as u32 % cols) * (cell_w + gap), (i as u32 / cols) * (cell_h + gap));
            sheet.draw_pixmap(cx as i32, cy as i32, cell.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            let (t, tau) = at(i);
            stamp(&mut sheet, &format!("t {t:.3}  tau {tau:.2}s"), cx as f32 + 10., cy as f32 + 14., pal.ink);
        }
        let p = out_path(o, "png", &format!("sheet{}", o.frames));
        save(&sheet, &p)?;
        p
    } else {
        let pm = frame_pixmap(&scene, ext, o.at, o.tau.unwrap_or(o.at * scene.meta.seconds), &pal, o.width)?;
        let p = out_path(o, "png", &format!("{:03}", (o.at * 1000.).round() as u32));
        save(&pm, &p)?;
        p
    };
    Ok(path.display().to_string())
}

fn save(pm: &Pixmap, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    pm.save_png(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn svg(o: &Opts) -> Result<String, String> {
    let scene = ScriptScene::load(&o.file)?;
    let mut pal = palette(o.look.as_deref().or(scene.meta.look.as_deref()).unwrap_or("paper"));
    pal.soft = scene.meta.soft;
    let ext = extent(&scene)?;
    let items = super::shoot(&scene.frame(o.at)?, ext);
    let p = out_path(o, "svg", &format!("{:03}", (o.at * 1000.).round() as u32));
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&p, super::svg::frame(&scene.meta.name, &items, &pal, 16.)).map_err(|e| e.to_string())?;
    Ok(p.display().to_string())
}

fn anim(o: &Opts) -> Result<String, String> {
    let scene = ScriptScene::load(&o.file)?;
    let mut pal = palette(o.look.as_deref().or(scene.meta.look.as_deref()).unwrap_or("paper"));
    pal.soft = scene.meta.soft;
    let ext = extent(&scene)?;
    let secs = scene.meta.seconds;
    let n = ((secs * o.fps).round() as usize).max(2);
    let mut frames = Vec::with_capacity(n);
    let mut cams = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / n as f32;
        let nodes = scene.frame_at(t, t * secs).map_err(|e| format!("at t {t:.3}: {e}"))?;
        cams.push(super::lens(&nodes, ext));
        frames.push(super::shoot(&nodes, ext));
    }
    let p = out_path(o, "svg", "loop");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let svg = match o.still {
        // The exact frame at T in the loop's view: what the loop must match.
        Some(t) => {
            let nodes = scene.frame_at(t, t * secs)?;
            let one = [super::shoot(&nodes, ext)];
            super::anim::loop_in(&scene.meta.name, &one, &[super::lens(&nodes, ext)], secs, &pal, 16., super::anim::view(&frames), o.px)
        }
        None => super::anim::loop_svg(&scene.meta.name, &frames, &cams, secs, &pal, 16., o.px),
    };
    std::fs::write(&p, &svg).map_err(|e| e.to_string())?;
    Ok(format!("{} ({} KB, {n} samples)", p.display(), svg.len() / 1024))
}

fn check(o: &Opts) -> Result<String, String> {
    let scene = ScriptScene::load(&o.file)?;
    let n = 120;
    let mut report = Vec::new();
    let mut most = 0;
    let mut empty = Vec::new();
    let start = Instant::now();
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let items = super::render(&scene.frame(t).map_err(|e| format!("at t {t:.3}: {e}"))?);
        most = most.max(items.len());
        if items.is_empty() {
            empty.push(format!("{t:.3}"));
        }
    }
    let per = start.elapsed().as_secs_f64() * 1000. / (n + 1) as f64;
    let first = super::render(&scene.frame(0.)?);
    let last = super::render(&scene.frame(1.)?);
    let m = &scene.meta;
    report.push(format!("{}: {:.1}s loop, up to {most} paths a frame, {per:.2} ms a frame to build", m.name, m.seconds));
    if same_frame(&first, &last) {
        report.push("loop: seamless (the last frame is the first)".into());
    } else {
        report.push("loop: JUMPS — the frame at t 1 differs from t 0; end every motion where it began".into());
    }
    if !empty.is_empty() {
        report.push(format!("empty frames at t {}", empty.join(", ")));
    }
    if m.about.is_empty() {
        report.push("no `about` in scene(): say in one line what it shows".into());
    }
    Ok(report.join("\n"))
}

/// Whether two frames draw the same, to a thousandth of a unit: periodic
/// motion (sin, cos) comes back to its start only up to rounding.
fn same_frame(a: &[super::Item], b: &[super::Item]) -> bool {
    let close = |p: super::P2, q: super::P2| (p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3;
    let flow = |p: &super::Paint| match *p {
        super::Paint::Stroke { flow, .. } => flow,
        _ => 0.,
    };
    let same_paint = |p: &super::Paint, q: &super::Paint| match (*p, *q) {
        (super::Paint::Stroke { ink: i, dashed: d, .. }, super::Paint::Stroke { ink: j, dashed: e, .. }) => {
            // Dash phases are on a circle: 0.9999 and 0 are the same place.
            let df = (flow(p) - flow(q)).abs();
            i == j && d == e && (df < 1e-3 || df > 1. - 1e-3)
        }
        _ => p == q,
    };
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            same_paint(&x.paint, &y.paint)
                && (x.alpha - y.alpha).abs() < 1e-3
                && x.layer == y.layer
                && x.path.len() == y.path.len()
                && x.path.iter().zip(&y.path).all(|(s, t)| match (*s, *t) {
                    (Seg::M(p), Seg::M(q)) | (Seg::L(p), Seg::L(q)) => close(p, q),
                    (Seg::C(a1, b1, c1), Seg::C(a2, b2, c2)) => close(a1, a2) && close(b1, b2) && close(c1, c2),
                    (Seg::A { r: r1, to: t1, large: l1, sweep: s1, .. }, Seg::A { r: r2, to: t2, large: l2, sweep: s2, .. }) => {
                        close(r1, r2) && close(t1, t2) && l1 == l2 && s1 == s2
                    }
                    (Seg::Z, Seg::Z) => true,
                    _ => false,
                })
        })
}

/// Runs `arakun iso …`; the exit code.
pub fn run(args: &[String], cwd: &Path) -> i32 {
    let Some((cmd, rest)) = args.split_first() else {
        print!("{USAGE}");
        return 2;
    };
    let result = parse(rest, cwd).and_then(|o| match cmd.as_str() {
        "render" => render(&o),
        "svg" => svg(&o),
        "anim" => anim(&o),
        "check" => check(&o),
        _ => Err(format!("unknown command {cmd}\n{USAGE}")),
    });
    match result {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("arakun iso: {e}");
            1
        }
    }
}
