//! Thin bars in one hue for the Events and People tabs: a count per time
//! slot, the top of the scale at the right, the highest bar's value over
//! it, and a few time labels under them.

use gpui::prelude::FluentBuilder as _;
use gpui::{ParentElement as _, SharedString, Styled as _, div, px};

use crate::theme;
use crate::ui::{self, Tone, Type};

/// The top of the scale: the next round number at or above `max`, close
/// enough that the highest bar fills most of the height.
pub fn scale(max: u64) -> u64 {
    if max == 0 {
        return 1;
    }
    let step = 10f64.powf((max as f64).log10().floor());
    [1., 1.2, 1.5, 2., 2.5, 3., 4., 5., 6., 8., 10.]
        .into_iter()
        .map(|m| (step * m).round() as u64)
        .find(|top| *top >= max)
        .unwrap_or(max)
}

/// `values` as bars `height` tall, labeled under by `ticks` spread evenly.
pub fn bars(values: &[u64], height: f32, ticks: Vec<SharedString>) -> gpui::Div {
    let max = values.iter().copied().max().unwrap_or(0);
    let top = scale(max);
    let peak = values.iter().position(|v| *v == max).filter(|_| max > 0);
    let color = Tone::Info.color();
    let label = |text: String| {
        ui::text(Type::Caption, theme::text_faint(), text)
            .font_family(theme::FONT_MONO)
            .whitespace_nowrap()
    };
    let plot = div()
        .relative()
        .flex()
        .items_end()
        .gap(ui::sp(if values.len() > 20 { 4. } else { 8. }))
        .h(px(height))
        .px(ui::sp(4.))
        .border_b_1()
        .border_color(theme::border())
        .children(values.iter().enumerate().map(|(i, v)| {
            let h = (*v as f32 / top as f32 * (height - 16.)).max(if *v > 0 { 2. } else { 0. });
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_end()
                .when(peak == Some(i), |d| {
                    d.child(label(super::count(*v)).text_color(theme::text_subtle()))
                })
                .child(
                    div()
                        .w_full()
                        .h(px(h))
                        .rounded_t(ui::rad(ui::RADIUS_SM))
                        .bg(color),
                )
        }))
        .child(
            div()
                .absolute()
                .top(px(-16.))
                .right_0()
                .child(label(super::count(top))),
        );
    let n = ticks.len();
    div()
        .flex()
        .flex_col()
        .gap(ui::sp(4.))
        .min_w_0()
        .child(plot)
        .child(
            div()
                .flex()
                .justify_between()
                .px(ui::sp(4.))
                .when(n == 0, |d| d.invisible())
                .children(ticks.into_iter().map(|t| label(t.to_string()))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_rounds_up() {
        assert_eq!(scale(0), 1);
        assert_eq!(scale(7), 8);
        assert_eq!(scale(11), 12);
        assert_eq!(scale(503), 600);
        assert_eq!(scale(450), 500);
        assert_eq!(scale(3020), 4000);
        assert_eq!(scale(100), 100);
        assert_eq!(scale(1), 1);
    }
}
