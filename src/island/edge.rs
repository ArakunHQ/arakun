//! The top of the screen where the island lives. A plain edge lets it grow out
//! of the bezel; a camera cutout takes the middle, so what the island shows
//! goes beside it and under it.

/// The widest and tallest cutout the island's window has room for.
const MAX_W: f32 = 280.;
const MAX_H: f32 = 60.;
const MAX_TOP: f32 = 20.;

/// A notch when only its kind is known (`ARAKUN_ISLAND_EDGE=notch`).
const NOTCH: (f32, f32) = (200., 32.);
/// A Dynamic Island when only its kind is known: its width, height and gap to the top.
const PILL: (f32, f32, f32) = (130., 30., 5.);

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Edge {
    /// Nothing in the way (Wayland, an external screen, a Mac without a
    /// cutout): the island hangs from the top edge.
    #[default]
    Flat,
    /// A notch `w` wide and `h` tall, part of the bezel: the island is the
    /// notch grown wider, and still hangs from the top edge.
    Notch { w: f32, h: f32 },
    /// A Dynamic Island: a pill `w` by `h` with screen all around it, `top`
    /// under the edge. The island is that pill grown, and floats like it.
    Pill { w: f32, h: f32, top: f32 },
}

impl Edge {
    /// The width in the island's middle that nothing can show in.
    pub fn cut_w(self) -> f32 {
        match self {
            Edge::Flat => 0.,
            Edge::Notch { w, .. } | Edge::Pill { w, .. } => w,
        }
    }

    /// How tall the cutout is, from the island's top down: an open island's
    /// content starts under it.
    pub fn cut_h(self) -> f32 {
        match self {
            Edge::Flat => 0.,
            Edge::Notch { h, .. } | Edge::Pill { h, .. } => h,
        }
    }

    /// How far under the screen's top edge the island's top is.
    pub fn top(self) -> f32 {
        match self {
            Edge::Pill { top, .. } => top,
            _ => 0.,
        }
    }

    /// Whether the island floats under the edge instead of hanging from it.
    pub fn floating(self) -> bool {
        matches!(self, Edge::Pill { .. })
    }

    /// Kept to sizes the island's window has room for; a cutout too small to
    /// be one is no cutout.
    pub fn clamped(self) -> Edge {
        let ok = |w: f32, h: f32| w.is_finite() && h.is_finite() && w >= 20. && h >= 10.;
        match self {
            Edge::Notch { w, h } if ok(w, h) => Edge::Notch {
                w: w.min(MAX_W),
                h: h.min(MAX_H),
            },
            Edge::Pill { w, h, top } if ok(w, h) && top.is_finite() => {
                let top = top.clamp(0., MAX_TOP);
                Edge::Pill {
                    w: w.min(MAX_W),
                    h: h.min(MAX_H - top),
                    top,
                }
            }
            _ => Edge::Flat,
        }
    }

    /// `ARAKUN_ISLAND_EDGE`: `flat`, `notch`, `notch:200x32`, `pill` or
    /// `pill:130x30+5` (width x height, then the gap to the top). It stands in
    /// for a cutout this screen doesn't have, or corrects one read wrong.
    pub fn parse(s: &str) -> Option<Edge> {
        let (kind, dims) = match s.trim().split_once(':') {
            Some((kind, dims)) => (kind, Some(dims)),
            None => (s.trim(), None),
        };
        let wh = |dims: &str| {
            let (w, h) = dims.split_once('x')?;
            Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
        };
        let edge = match (kind, dims) {
            ("flat", None) => Edge::Flat,
            ("notch", None) => Edge::Notch {
                w: NOTCH.0,
                h: NOTCH.1,
            },
            ("notch", Some(dims)) => {
                let (w, h) = wh(dims)?;
                Edge::Notch { w, h }
            }
            ("pill", None) => Edge::Pill {
                w: PILL.0,
                h: PILL.1,
                top: PILL.2,
            },
            ("pill", Some(dims)) => {
                let (dims, top) = match dims.split_once('+') {
                    Some((dims, top)) => (dims, top.parse::<f32>().ok()?),
                    None => (dims, PILL.2),
                };
                let (w, h) = wh(dims)?;
                Edge::Pill { w, h, top }
            }
            _ => return None,
        };
        // A size given and refused is a mistake to report, not a flat edge.
        (edge == Edge::Flat || edge.clamped() != Edge::Flat).then(|| edge.clamped())
    }

    pub fn from_env() -> Option<Edge> {
        let value = std::env::var("ARAKUN_ISLAND_EDGE").ok()?;
        let edge = Edge::parse(&value);
        if edge.is_none() {
            eprintln!(
                "arakun: ARAKUN_ISLAND_EDGE={value}: expected flat, notch[:WxH] or pill[:WxH[+TOP]]"
            );
        }
        edge
    }

    /// What a screen's safe area says about its top: `safe_top` is how much of
    /// it the menu bar's band takes, `left` and `right` how wide the free
    /// parts of that band are beside the cutout, on a screen `screen_w` wide.
    /// macOS has no word for the cutout's kind; `floating` is the caller's
    /// (see `mac::spot`). It doesn't give a pill's height or gap either: the
    /// pill is taken to sit in the middle of the band, `PILL.2` from its ends.
    #[cfg(any(target_os = "macos", test))]
    pub fn from_safe_area(
        safe_top: f32,
        screen_w: f32,
        left: f32,
        right: f32,
        floating: bool,
    ) -> Edge {
        if safe_top <= 0. {
            return Edge::Flat;
        }
        let between = screen_w - left - right;
        let w = if left > 0. && right > 0. && between > 0. {
            between
        } else if floating {
            PILL.0
        } else {
            NOTCH.0
        };
        if floating {
            let top = PILL.2.min(safe_top / 4.);
            Edge::Pill {
                w,
                h: safe_top - 2. * top,
                top,
            }
        } else {
            Edge::Notch { w, h: safe_top }
        }
        .clamped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_edge_takes_no_room() {
        assert_eq!(Edge::default(), Edge::Flat);
        assert_eq!(
            (Edge::Flat.cut_w(), Edge::Flat.cut_h(), Edge::Flat.top()),
            (0., 0., 0.)
        );
        assert!(!Edge::Flat.floating());
    }

    #[test]
    fn the_edge_reads_from_its_setting() {
        assert_eq!(Edge::parse("flat"), Some(Edge::Flat));
        assert_eq!(
            Edge::parse(" notch "),
            Some(Edge::Notch { w: 200., h: 32. })
        );
        assert_eq!(
            Edge::parse("notch:185x37"),
            Some(Edge::Notch { w: 185., h: 37. })
        );
        assert_eq!(
            Edge::parse("pill"),
            Some(Edge::Pill {
                w: 130.,
                h: 30.,
                top: 5.
            })
        );
        assert_eq!(
            Edge::parse("pill:120x28+6"),
            Some(Edge::Pill {
                w: 120.,
                h: 28.,
                top: 6.
            })
        );
        assert_eq!(
            Edge::parse("pill:120x28"),
            Some(Edge::Pill {
                w: 120.,
                h: 28.,
                top: 5.
            })
        );
        for bad in [
            "",
            "hole",
            "notch:200",
            "notch:x",
            "pill:120x28+",
            "notch:2x2",
            "flat:1x1",
        ] {
            assert_eq!(Edge::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_cutout_never_outgrows_the_window() {
        assert_eq!(
            Edge::Notch { w: 900., h: 200. }.clamped(),
            Edge::Notch { w: MAX_W, h: MAX_H }
        );
        let pill = Edge::Pill {
            w: 100.,
            h: 200.,
            top: 90.,
        }
        .clamped();
        assert_eq!(pill.top(), MAX_TOP);
        assert_eq!(pill.top() + pill.cut_h(), MAX_H);
        assert_eq!(
            Edge::Notch {
                w: f32::NAN,
                h: 30.
            }
            .clamped(),
            Edge::Flat
        );
        assert_eq!(Edge::Notch { w: 5., h: 30. }.clamped(), Edge::Flat);
    }

    #[test]
    fn a_safe_area_tells_the_cutout() {
        // An external screen, or a Mac without a cutout.
        assert_eq!(Edge::from_safe_area(0., 1920., 0., 0., false), Edge::Flat);
        // A 14-inch MacBook Pro's notch: 1512 wide, the band 32 tall.
        assert_eq!(
            Edge::from_safe_area(32., 1512., 663., 664., false),
            Edge::Notch { w: 185., h: 32. }
        );
        // The same numbers on a screen whose cutout floats.
        assert_eq!(
            Edge::from_safe_area(32., 1512., 663., 664., true),
            Edge::Pill {
                w: 185.,
                h: 22.,
                top: 5.
            }
        );
        // A band and no free parts beside it: the kind's usual width.
        assert_eq!(
            Edge::from_safe_area(38., 1728., 0., 0., false),
            Edge::Notch { w: 200., h: 38. }
        );
    }
}
