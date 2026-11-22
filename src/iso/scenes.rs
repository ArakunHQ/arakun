//! The scenes of the Isometric tab. A scene is a loop: a function from the
//! time in the loop (0..1) to the nodes to paint, back to front.

use super::{Clip, Face, Ink, Node, Shape, Tone};

pub struct Scene {
    pub name: &'static str,
    pub about: &'static str,
    /// How long one loop lasts.
    pub seconds: f32,
    pub build: fn(f32) -> Vec<Node>,
}

pub const ALL: &[Scene] = &[Scene {
    name: "Primitives",
    about: "Every shape the engine draws: slabs sharp and round, cylinders, an open tube, plates and discs on each face.",
    seconds: 3.2,
    build: primitives,
}];

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
    n.push(
        plate(Face::Left(150.), [38., 14., 92., 38.], 5.)
            .fill(Tone::Recess)
            .ink(Ink::Edge),
    );
    n.push(disc(Face::Left(150.), [50., 26.], 5.).accent());
    n.push(
        plate(Face::Right(100.), [98., 14., 142., 38.], 5.)
            .fill(Tone::Recess)
            .ink(Ink::Edge),
    );
    n.push(disc(Face::Right(100.), [110., 26.], 5.).accent());

    // An open tube with a coin bobbing in it.
    let base = [190., 120., 6.];
    n.push(Node::new(Shape::TubeBack {
        base,
        r: 32.,
        inner: 24.,
        h: 16.,
    }));
    n.push(
        cylinder([190., 120., 10. + 22. * bob], 18., 5.)
            .accent()
            .clip(Clip::above(6.)),
    );
    n.push(Node::new(Shape::TubeFront {
        base,
        r: 32.,
        inner: 24.,
        h: 16.,
    }));

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
}
