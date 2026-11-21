//! Writes scenes out as PNG, SVG and animated SVG. `arakun iso` and the
//! Isometric tab both call these, so the files match.

use tiny_skia::Pixmap;

use super::raster::{self, Fit};
use super::svg::Palette;
use super::{Item, Node, P2, Rect};

/// A scene's nodes at story time `t` (0..1) and ambient time `tau` (seconds).
pub type FrameFn<'a> = &'a dyn Fn(f32, f32) -> Result<Vec<Node>, String>;

/// The box the scene fills across its loop, at rest.
pub fn extent(frame: FrameFn, seconds: f32) -> Result<Rect, String> {
    let mut frames = Vec::new();
    for i in 0..=48 {
        let t = i as f32 / 48.;
        frames.push(frame(t, t * seconds)?);
    }
    super::rest_bounds(frames.into_iter()).ok_or_else(|| "the scene draws nothing".into())
}

/// One frame `width` px wide, as tall as the scene needs.
pub fn pixmap(
    frame: FrameFn,
    ext: Rect,
    t: f32,
    tau: f32,
    pal: &Palette,
    width: u32,
) -> Result<Pixmap, String> {
    let w = width as f32;
    let pad = w * 0.04;
    let aspect = (ext[3] - ext[1]) / (ext[2] - ext[0]).max(1.);
    let h = ((w - 2. * pad) * aspect + 2. * pad).ceil() as u32;
    let fit = Fit::new(ext, w, h as f32, pad).ok_or("the scene has no size")?;
    let items = super::shoot(&frame(t, tau)?, ext);
    raster::frame(&items, pal, width, h, fit, (w / 1000.).max(1.) * 1.25)
        .ok_or_else(|| "could not draw".into())
}

/// [`pixmap`], encoded as PNG.
// TEMP: the Isometric tab calls this in a later commit.
#[allow(dead_code)]
pub fn png(
    frame: FrameFn,
    ext: Rect,
    t: f32,
    tau: f32,
    pal: &Palette,
    width: u32,
) -> Result<Vec<u8>, String> {
    pixmap(frame, ext, t, tau, pal, width)?
        .encode_png()
        .map_err(|e| e.to_string())
}

/// One frame as SVG.
pub fn svg(
    name: &str,
    frame: FrameFn,
    ext: Rect,
    t: f32,
    tau: f32,
    pal: &Palette,
) -> Result<String, String> {
    let items = super::shoot(&frame(t, tau)?, ext);
    Ok(super::svg::frame(name, &items, pal, 16.))
}

/// The whole loop sampled `fps` times a second: the shots and their cameras.
#[allow(clippy::type_complexity)]
pub fn samples(
    frame: FrameFn,
    ext: Rect,
    seconds: f32,
    fps: f32,
) -> Result<(Vec<Vec<Item>>, Vec<Option<(f32, P2)>>), String> {
    let n = ((seconds * fps).round() as usize).max(2);
    let mut frames = Vec::with_capacity(n);
    let mut cams = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / n as f32;
        let nodes = frame(t, t * seconds).map_err(|e| format!("at t {t:.3}: {e}"))?;
        cams.push(super::lens(&nodes, ext));
        frames.push(super::shoot(&nodes, ext));
    }
    Ok((frames, cams))
}

/// The whole loop as one animated SVG, sampled `fps` times a second; no
/// point strays more than `px` px from the exact frame.
// TEMP: the Isometric tab calls this in a later commit.
#[allow(dead_code)]
pub fn loop_svg(
    name: &str,
    frame: FrameFn,
    ext: Rect,
    seconds: f32,
    pal: &Palette,
    fps: f32,
    px: f32,
) -> Result<String, String> {
    let (frames, cams) = samples(frame, ext, seconds, fps)?;
    Ok(super::anim::loop_svg(
        name, &frames, &cams, seconds, pal, 16., px,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso::starters;

    fn paper() -> Palette {
        Palette {
            ink: [0.165; 3],
            ground: [1.; 3],
            accent: [0.114, 0.686, 0.612],
            dark: false,
            soft: false,
        }
    }

    #[test]
    fn png_is_a_png() {
        let scene = starters::load(&starters::ALL[1]).unwrap();
        let f = |t, tau| scene.frame_at(t, tau);
        let ext = extent(&f, scene.meta.seconds).unwrap();
        let b = png(&f, ext, 0., 0., &paper(), 400).unwrap();
        assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn svg_frame_has_paths() {
        let scene = starters::load(&starters::ALL[1]).unwrap();
        let f = |t, tau| scene.frame_at(t, tau);
        let ext = extent(&f, scene.meta.seconds).unwrap();
        assert!(
            svg("CPU", &f, ext, 0.3, 1.5, &paper())
                .unwrap()
                .contains("<path")
        );
    }

    #[test]
    fn loop_svg_animates() {
        let scene = starters::load(&starters::ALL[1]).unwrap();
        let f = |t, tau| scene.frame_at(t, tau);
        let ext = extent(&f, scene.meta.seconds).unwrap();
        let out = loop_svg("CPU", &f, ext, scene.meta.seconds, &paper(), 24., 0.5).unwrap();
        assert!(out.contains("<animate"));
    }

    #[test]
    fn png_errors_on_empty_scene() {
        let empty = |_: f32, _: f32| Ok(Vec::new());
        assert!(png(&empty, [0., 0., 0., 0.], 0., 0., &paper(), 400).is_err());
        assert!(extent(&empty, 1.).is_err());
    }
}
