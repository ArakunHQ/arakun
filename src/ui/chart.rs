//! Charts: a smooth area chart that explains a point when hovered, and a
//! sparkline for stat cells. Both draw Catmull-Rom curves through the values,
//! so lines flow instead of zig-zagging, and keep them inside the plot.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, BorderStyle, Bounds, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    PathBuilder, Pixels, Point, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas,
    div, point, px, quad, size,
};

use super::kit::{Type, Typography as _};
use super::RADIUS;
use crate::theme;

/// One line of a chart.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub name: SharedString,
    pub color: Hsla,
    pub values: Vec<f64>,
}

/// Turns a value into what the axis and tooltip print.
pub type Format = Rc<dyn Fn(f64) -> String>;

/// A smooth area chart, one series per line, with a value axis in its own
/// column, three date labels under it, and a card that follows the pointer.
pub struct AreaChart {
    series: Vec<Series>,
    /// What each point is called in the tooltip ("Tue, Oct 13").
    labels: Vec<SharedString>,
    /// Under the axis: first, middle and last.
    ticks: Vec<SharedString>,
    /// An extra line per point in the tooltip ("291M tokens").
    notes: Vec<Option<SharedString>>,
    format: Format,
    /// Short form for the axis ("$250", "2.5k").
    axis_format: Format,
    height: f32,
    hover: Option<usize>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl AreaChart {
    pub fn new(height: f32) -> Self {
        let plain: Format = Rc::new(|v| format!("{v:.0}"));
        Self {
            series: Vec::new(),
            labels: Vec::new(),
            ticks: Vec::new(),
            notes: Vec::new(),
            format: plain.clone(),
            axis_format: plain,
            height,
            hover: None,
            bounds: Rc::new(Cell::new(None)),
        }
    }

    /// New data. `labels` names each point; `ticks` go under the plot.
    #[allow(clippy::too_many_arguments)]
    pub fn set(
        &mut self,
        series: Vec<Series>,
        labels: Vec<SharedString>,
        ticks: Vec<SharedString>,
        notes: Vec<Option<SharedString>>,
        format: Format,
        axis_format: Format,
        cx: &mut Context<Self>,
    ) {
        // Views hand the data over on every render; only a change repaints.
        let same = self.series == series && self.labels == labels && self.ticks == ticks && self.notes == notes;
        self.format = format;
        self.axis_format = axis_format;
        if same {
            return;
        }
        self.series = series;
        self.labels = labels;
        self.ticks = ticks;
        self.notes = notes;
        if self.hover.is_some_and(|h| h >= self.len()) {
            self.hover = None;
        }
        cx.notify();
    }

    fn len(&self) -> usize {
        self.series.iter().map(|s| s.values.len()).max().unwrap_or(0)
    }

    fn pointer(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let (Some(b), n) = (self.bounds.get(), self.len()) else {
            return;
        };
        if n < 2 {
            return;
        }
        let f = ((x - b.origin.x).as_f32() / b.size.width.as_f32()).clamp(0., 1.);
        let i = (f * (n - 1) as f32).round() as usize;
        if self.hover != Some(i) {
            self.hover = Some(i);
            cx.notify();
        }
    }
}

/// A round step for the value axis (1, 2, 2.5 or 5 times a power of ten) and
/// how many of them reach `hi`: three to five gridlines above zero.
pub fn axis_steps(hi: f64) -> (f64, usize) {
    if hi <= 0. {
        return (1., 4);
    }
    let mag = 10f64.powi((hi / 5.).log10().floor() as i32);
    for m in [1., 2., 2.5, 5., 10., 20.] {
        let step = mag * m;
        let n = (hi / step).ceil() as usize;
        if n <= 5 {
            return (step, n.max(3));
        }
    }
    (mag * 50., 4)
}

/// Catmull-Rom spans through `pts`: (end, control 1, control 2), with the
/// controls kept between `top` and `base` so the curve never leaves the plot.
fn spans(pts: &[Point<Pixels>], top: f32, base: f32) -> Vec<(Point<Pixels>, Point<Pixels>, Point<Pixels>)> {
    let n = pts.len();
    let clamp = |y: f32| y.clamp(top, base);
    let f = |p: Point<Pixels>| (p.x.as_f32(), p.y.as_f32());
    (0..n.saturating_sub(1))
        .map(|i| {
            let (x0, y0) = f(pts[i.saturating_sub(1)]);
            let (x1, y1) = f(pts[i]);
            let (x2, y2) = f(pts[i + 1]);
            let (x3, y3) = f(pts[(i + 2).min(n - 1)]);
            (
                pts[i + 1],
                point(px(x1 + (x2 - x0) / 6.), px(clamp(y1 + (y2 - y0) / 6.))),
                point(px(x2 - (x3 - x1) / 6.), px(clamp(y2 - (y3 - y1) / 6.))),
            )
        })
        .collect()
}

/// Paints a series' fill and line.
fn paint_curve(pts: &[Point<Pixels>], top: f32, base: f32, color: Hsla, fill_alpha: f32, width: f32, window: &mut Window) {
    if pts.len() < 2 {
        return;
    }
    let spans = spans(pts, top, base);
    if fill_alpha > 0. {
        let mut fill = PathBuilder::fill();
        fill.move_to(point(pts[0].x, px(base)));
        fill.line_to(pts[0]);
        for (to, c1, c2) in &spans {
            fill.cubic_bezier_to(*to, *c1, *c2);
        }
        fill.line_to(point(pts[pts.len() - 1].x, px(base)));
        fill.close();
        if let Ok(path) = fill.build() {
            window.paint_path(path, color.opacity(fill_alpha));
        }
    }
    let mut line = PathBuilder::stroke(px(width));
    line.move_to(pts[0]);
    for (to, c1, c2) in &spans {
        line.cubic_bezier_to(*to, *c1, *c2);
    }
    if let Ok(path) = line.build() {
        window.paint_path(path, color);
    }
}

impl Render for AreaChart {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const AXIS_W: f32 = 56.;
        const INSET: f32 = 6.;
        let plot_h = self.height;
        let n = self.len();
        let hover = self.hover.filter(|h| *h < n);
        let hi = self.series.iter().flat_map(|s| s.values.iter().copied()).fold(0., f64::max);
        let (step, ticks) = axis_steps(hi);
        let top_value = step * ticks as f64;
        let series = self.series.clone();
        let bounds_cell = self.bounds.clone();
        let plot = canvas(
            move |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
                bounds_cell.set(Some(bounds));
                let o = bounds.origin;
                let (w, h) = (bounds.size.width.as_f32(), bounds.size.height.as_f32() - 2. * INSET);
                for k in 0..=ticks {
                    let y = o.y + px(INSET + h * k as f32 / ticks as f32);
                    let mut p = PathBuilder::stroke(px(1.));
                    p.move_to(point(o.x, y));
                    p.line_to(point(o.x + px(w), y));
                    if let Ok(path) = p.build() {
                        window.paint_path(path, if k == ticks { theme::border() } else { theme::border_subtle() });
                    }
                }
                if n < 2 {
                    return;
                }
                let y_of = |v: f64| o.y + px(INSET + h - (v / top_value).clamp(0., 1.) as f32 * h);
                let x_of = |i: usize| o.x + px(i as f32 * w / (n - 1) as f32);
                if let Some(d) = hover {
                    let mut p = PathBuilder::stroke(px(1.));
                    p.move_to(point(x_of(d), o.y + px(INSET)));
                    p.line_to(point(x_of(d), o.y + px(INSET + h)));
                    if let Ok(path) = p.build() {
                        window.paint_path(path, theme::border_strong());
                    }
                }
                let (top, base) = (o.y.as_f32() + INSET, y_of(0.).as_f32());
                for s in &series {
                    let pts: Vec<Point<Pixels>> =
                        (0..n).map(|i| point(x_of(i), y_of(s.values.get(i).copied().unwrap_or(0.)))).collect();
                    paint_curve(&pts, top, base, s.color, 0.13, 1.75, window);
                    if let Some(d) = hover {
                        let (q, r) = (pts[d], 4.);
                        window.paint_quad(quad(
                            Bounds::new(point(q.x - px(r), q.y - px(r)), size(px(r * 2.), px(r * 2.))),
                            px(r),
                            s.color,
                            px(2.),
                            theme::background(),
                            BorderStyle::Solid,
                        ));
                    }
                }
            },
        )
        .size_full();

        // The point's card, on the side of the guide line with more room.
        let card = hover.zip(self.bounds.get()).map(|(d, b)| {
            let w = b.size.width.as_f32();
            let x = if n > 1 { d as f32 * w / (n - 1) as f32 } else { 0. };
            let total: f64 = self.series.iter().map(|s| s.values.get(d).copied().unwrap_or(0.)).sum();
            let mut rows = div().flex().flex_col().gap(px(5.));
            if self.series.len() > 1 {
                for s in &self.series {
                    let v = s.values.get(d).copied().unwrap_or(0.);
                    rows = rows.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().size(px(7.)).rounded_full().bg(s.color))
                            .child(div().flex_1().text_color(theme::text_muted()).child(s.name.clone()))
                            .child(div().text_color(theme::text()).child((self.format)(v))),
                    );
                }
            }
            let note = self.notes.get(d).cloned().flatten();
            let card = div()
                .absolute()
                .top(px(0.))
                .w(px(210.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(10.))
                .rounded(px(RADIUS))
                .bg(theme::dialog())
                .border_1()
                .border_color(theme::border())
                .shadow_lg()
                .ty(Type::Label)
                .font_weight(FontWeight::NORMAL)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(div().flex_1().text_color(theme::text_subtle()).child(self.labels.get(d).cloned().unwrap_or_default()))
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child((self.format)(total))),
                )
                .when(self.series.len() > 1, |c| c.child(rows))
                .when_some(note, |c, note| c.child(div().ty(Type::Caption).text_color(theme::text_faint()).child(note)));
            if x > w * 0.6 { card.right(px(w - x + 12.)) } else { card.left(px(x + 12.)) }
        });

        let mut axis = div().relative().w(px(AXIS_W)).flex_none().h(px(plot_h));
        for k in 0..=ticks {
            let y = INSET + (plot_h - 2. * INSET) * k as f32 / ticks as f32;
            axis = axis.child(
                div()
                    .absolute()
                    .right(px(10.))
                    .top(px(y - 7.))
                    .ty(Type::Caption)
                    .text_color(theme::text_faint())
                    .child((self.axis_format)(step * (ticks - k) as f64)),
            );
        }
        let mut x_row = div().flex().justify_between().pl(px(AXIS_W)).ty(Type::Caption).text_color(theme::text_faint());
        for t in &self.ticks {
            x_row = x_row.child(t.clone());
        }
        let area = div()
            .id("area-chart-plot")
            .relative()
            .flex_1()
            .h(px(plot_h))
            .child(plot)
            .children(card)
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _, cx| this.pointer(ev.position.x, cx)))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.hover.take().is_some() {
                    cx.notify();
                }
            }));
        div().flex().flex_col().gap(px(8.)).child(div().flex().child(axis).child(area)).child(x_row)
    }
}

/// A tiny trend line for a stat: no axis, no hover, the last point marked.
pub fn sparkline(values: Vec<f64>, color: Hsla, width: f32, height: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
            let n = values.len();
            if n < 2 {
                return;
            }
            let hi = values.iter().copied().fold(0., f64::max).max(f64::MIN_POSITIVE);
            let o = bounds.origin;
            let (w, h) = (bounds.size.width.as_f32() - 3., bounds.size.height.as_f32() - 4.);
            let pts: Vec<Point<Pixels>> = values
                .iter()
                .enumerate()
                .map(|(i, v)| point(o.x + px(i as f32 * w / (n - 1) as f32), o.y + px(2. + h - (v / hi) as f32 * h)))
                .collect();
            let (top, base) = (o.y.as_f32() + 2., o.y.as_f32() + 2. + h);
            paint_curve(&pts, top, base, color, 0.12, 1.5, window);
            let (q, r) = (pts[n - 1], 2.5);
            window.paint_quad(quad(
                Bounds::new(point(q.x - px(r), q.y - px(r)), size(px(r * 2.), px(r * 2.))),
                px(r),
                color,
                px(0.),
                color,
                BorderStyle::Solid,
            ));
        },
    )
    .w(px(width))
    .h(px(height))
    .flex_none()
}

#[cfg(test)]
mod tests {
    use super::axis_steps;

    #[test]
    fn axis_steps_are_round_and_reach_the_top() {
        assert_eq!(axis_steps(250.), (50., 5));
        assert_eq!(axis_steps(11.8), (2.5, 5));
        assert_eq!(axis_steps(0.9), (0.2, 5));
        for hi in [0.3, 7., 42., 999., 3.0e9] {
            let (step, n) = axis_steps(hi);
            assert!(step * n as f64 >= hi && (3..=5).contains(&n), "{hi}: {step}×{n}");
        }
    }
}
