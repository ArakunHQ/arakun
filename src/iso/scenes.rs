//! The scenes of the Isometric tab. A scene is a loop: a function from the
//! time in the loop (0..1) to the nodes to paint, back to front.

use super::{Clip, Ease, Face, Ink, Node, Shape, Tone, keyed};

pub struct Scene {
    pub name: &'static str,
    pub about: &'static str,
    /// How long one loop lasts.
    pub seconds: f32,
    pub build: fn(f32) -> Vec<Node>,
}

pub const ALL: &[Scene] = &[
    Scene {
        name: "Usage billing",
        about: "Usage drops into the meter, stacks up through the month, and the meter prints the invoice.",
        seconds: 3.6,
        build: billing,
    },
    Scene {
        name: "Primitives",
        about: "Every shape the engine draws: slabs sharp and round, cylinders, an open tube, plates and discs on each face.",
        seconds: 3.2,
        build: primitives,
    },
];

use Ease::{InOut, Linear, Out};

fn slab(min: [f32; 3], size: [f32; 3], r: f32) -> Node {
    Node::new(Shape::Slab { min, size, r })
}

fn cylinder(base: [f32; 3], r: f32, h: f32) -> Node {
    Node::new(Shape::Cylinder { base, r, h })
}

fn plate(face: Face, rect: [f32; 4], r: f32) -> Node {
    Node::new(Shape::Plate { face, rect, r })
}

fn disc(face: Face, c: [f32; 2], r: f32) -> Node {
    Node::new(Shape::Disc { face, c, r })
}

fn line(points: Vec<[f32; 3]>) -> Node {
    Node::new(Shape::Line { points })
}

// ---- Usage billing --------------------------------------------------------------
//
// One loop is a month: a coin of usage drops into the hopper each week, the
// gauge and the screen's weekly bars climb with them, then the month closes:
// the coins sink, the old invoice lifts away and a new one prints.

/// Top of the meter's body.
const TOP: f32 = 68.;
const COINS: usize = 4;
const COIN_H: f32 = 4.;
const PAPER_H: f32 = 52.;
/// The hopper: where it stands, outer and inner radius, height.
const HOPPER: [f32; 3] = [70., 80., TOP];
const HOPPER_R: (f32, f32, f32) = (30., 22., 10.);

/// When coin `i` starts to fall and when it lands, in loop time.
fn coin_times(i: usize) -> (f32, f32) {
    let start = 0.069 + 0.1 * i as f32;
    (start, start + 0.0945)
}

/// 0 before coin `i` lands, 1 once it has, back to 0 as the month closes.
fn landed(t: f32, i: usize) -> f32 {
    let (_, land) = coin_times(i);
    keyed(t, &[(land - 0.01, 0., Out), (land + 0.06, 1., Linear), (0.5417, 1., InOut), (0.6667, 0., Linear)])
}

fn billing(t: f32) -> Vec<Node> {
    let mut n = Vec::new();

    // Base and body.
    n.push(slab([0., 0., 0.], [220., 160., 8.], 16.));
    n.push(slab([20., 20., 8.], [180., 120., TOP - 8.], 10.));
    n.push(plate(Face::Top(TOP), [27., 27., 193., 133.], 6.));

    // Gauge on the left wall: fills a step per coin.
    let level = (0..COINS).map(|i| landed(t, i)).sum::<f32>() / COINS as f32;
    let gauge = Face::Left(140.);
    n.push(plate(gauge, [36., 20., 112., 56.], 6.).fill(Tone::Recess).ink(Ink::Edge));
    let fill_w = 64. * level;
    if fill_w >= 6. {
        n.push(plate(gauge, [42., 30., 42. + fill_w, 42.], 3.).accent().ink(Ink::Accent));
    }
    for i in 0..5 {
        let x = 44. + i as f32 * 15.;
        n.push(line(vec![[x, 140., 46.], [x, 140., 51.]]));
    }

    // Screen on the right wall: a bar per week, raised by that week's coin.
    let screen = Face::Right(200.);
    n.push(plate(screen, [34., 22., 104., 58.], 6.).fill(Tone::Recess).ink(Ink::Edge));
    n.push(line(vec![[200., 96., 27.], [200., 42., 27.]]));
    for (i, h) in [14., 22., 17., 26.].into_iter().enumerate() {
        let y = 94. - i as f32 * 14.;
        let h = h * landed(t, i);
        // Below twice its radius a bar would round into a dot.
        if h >= 4. {
            n.push(plate(screen, [y - 8., 28., y, 28. + h], 2.).accent().ink(Ink::Accent));
        }
    }

    // Print button: flashes when the invoice comes out.
    let flash = keyed(t, &[(0.661, 0., Out), (0.689, 1., Out), (0.819, 0., Linear)]);
    n.push(cylinder([178., 42., TOP], 8., 3.));
    n.push(disc(Face::Top(TOP + 3.), [178., 42.], 4.).ink(Ink::Mute));
    n.push(disc(Face::Top(TOP + 3.), [178., 42.], 4.).accent().no_ink().alpha(flash));

    // Paper slot.
    n.push(plate(Face::Top(TOP), [120., 96., 184., 106.], 5.).fill(Tone::Recess).ink(Ink::Edge));

    // The hopper and the coins that drop into it. Each coin rests on the one
    // below and bounces a little less, as the stack takes the blow; the
    // floor of the hopper swallows them when the month closes.
    let (r, inner, h) = HOPPER_R;
    n.push(Node::new(Shape::TubeBack { base: HOPPER, r, inner, h }).side_ink(Ink::Mute));
    for i in 0..COINS {
        let (start, land) = coin_times(i);
        let drop = 56. - 10. * i as f32;
        let bounce = 2.5 * (1. - 0.25 * i as f32);
        let dz = keyed(
            t,
            &[
                (start, drop, Out),
                (land, 0., Out),
                (land + 0.0194, bounce, InOut),
                (land + 0.036, 0., Linear),
                (0.5417, 0., InOut),
                (0.6667, -30., Linear),
            ],
        );
        let alpha = keyed(t, &[(start, 0., Out), (start + 0.056, 1., Linear), (0.6722, 1., Linear), (0.6778, 0., Linear)]);
        let z = TOP + i as f32 * COIN_H + dz;
        let coin = 10 + i as u32;
        n.push(
            cylinder([HOPPER[0], HOPPER[1], z], 16., COIN_H)
                .accent()
                .side_ink(Ink::AccentMute)
                .alpha(alpha)
                .layer(coin)
                .clip(Clip::above(TOP)),
        );
        n.push(
            disc(Face::Top(z + COIN_H), [HOPPER[0], HOPPER[1]], 11.)
                .ink(Ink::AccentMute)
                .alpha(alpha)
                .layer(coin)
                .clip(Clip::above(TOP)),
        );
    }
    n.push(Node::new(Shape::TubeFront { base: HOPPER, r, inner, h }));

    // Last month's invoice lifts away; this month's prints out of the slot,
    // its total marked once it is all out.
    let old_lift = keyed(t, &[(0.55, 0., Out), (0.65, 18., Linear)]);
    let old_alpha = keyed(t, &[(0.55, 1., Out), (0.6333, 0., Linear)]);
    invoice(&mut n, old_lift, old_alpha, 1.);
    if t > 0.6 {
        let rise = keyed(t, &[(0.6722, -PAPER_H, InOut), (0.8472, 0., Linear)]);
        let total = keyed(t, &[(0.84, 0., Out), (0.9, 1., Linear)]);
        invoice(&mut n, rise, 1., total);
    }
    n
}

/// A sheet standing in the slot, `dz` above where it rests, faded as one
/// to `alpha`; its accent total line at `total` on top of that.
fn invoice(n: &mut Vec<Node>, dz: f32, alpha: f32, total: f32) {
    if alpha <= 0. {
        return;
    }
    let clip = Clip::above(TOP);
    let (y, z0) = (101.2, TOP + dz);
    n.push(slab([126., 100., z0], [52., 1.2, PAPER_H], 0.).alpha(alpha).layer(1).clip(clip));
    for (i, len) in [30., 22., 26.].into_iter().enumerate() {
        let z = z0 + PAPER_H - 12. - i as f32 * 7.;
        n.push(line(vec![[134., y, z], [134. + len, y, z]]).alpha(alpha).layer(1).clip(clip));
    }
    let z = z0 + 12.;
    n.push(line(vec![[134., y, z + 6.], [150., y, z + 6.]]).alpha(alpha).layer(1).clip(clip));
    n.push(line(vec![[134., y, z], [170., y, z]]).ink(Ink::Accent).alpha(alpha * total).clip(clip));
}

// ---- Primitives ---------------------------------------------------------------------

fn primitives(t: f32) -> Vec<Node> {
    let bob = (t * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let mut n = Vec::new();
    n.push(slab([0., 0., 0.], [260., 180., 6.], 18.));
    for i in 1..6 {
        let x = i as f32 * 260. / 6.;
        n.push(line(vec![[x, 8., 6.], [x, 172., 6.]]).dashed());
    }

    // Back row: a sharp block and a rounded one.
    n.push(slab([20., 20., 6.], [50., 40., 36.], 0.));
    n.push(slab([100., 20., 6.], [56., 44., 26.], 12.));
    n.push(plate(Face::Top(32.), [108., 28., 148., 56.], 6.).fill(Tone::Recess));

    // A cylinder with a disc on top.
    n.push(cylinder([210., 46., 6.], 26., 30.));
    n.push(disc(Face::Top(36.), [210., 46.], 12.).accent());

    // Middle: plates on both visible walls of a block.
    n.push(slab([30., 90., 6.], [70., 60., 40.], 6.));
    n.push(plate(Face::Left(150.), [38., 14., 92., 38.], 5.).fill(Tone::Recess).ink(Ink::Edge));
    n.push(disc(Face::Left(150.), [50., 26.], 5.).accent());
    n.push(plate(Face::Right(100.), [98., 14., 142., 38.], 5.).fill(Tone::Recess).ink(Ink::Edge));
    n.push(disc(Face::Right(100.), [110., 26.], 5.).accent());

    // An open tube with a coin bobbing in it.
    let base = [190., 120., 6.];
    n.push(Node::new(Shape::TubeBack { base, r: 32., inner: 24., h: 16. }));
    n.push(cylinder([190., 120., 10. + 22. * bob], 18., 5.).accent().clip(Clip::above(6.)));
    n.push(Node::new(Shape::TubeFront { base, r: 32., inner: 24., h: 16. }));

    // A small accent cube floating.
    n.push(slab([120., 130., 24. + 10. * bob], [20., 20., 20.], 3.).accent());
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_paints_at_every_moment() {
        for s in ALL {
            for i in 0..=40 {
                let items = crate::iso::render(&(s.build)(i as f32 / 40.));
                assert!(!items.is_empty(), "{} at {i}", s.name);
                assert!(crate::iso::bounds(&items).is_some());
            }
        }
    }

    #[test]
    fn billing_loops_seamlessly() {
        let a = crate::iso::render(&billing(0.));
        let b = crate::iso::render(&billing(1.));
        assert_eq!(a, b, "the last frame must be the first");
    }
}
