//! Draws a community widget's blocks (`crate::widget::blocks`) with the
//! app's own theme. A widget names tones, never colors, so it looks like the
//! rest of Arakun whatever it shows.
//!
//! This draws without actions: buttons and links show what they are and do
//! nothing. The Marketplace preview must never act, and a placed widget
//! wires its actions where it is placed, after asking.

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, Div, Hsla, IntoElement, ParentElement, Styled, div, px, relative};

use crate::theme;
use crate::ui::{self, Type, Typography as _};
use crate::widget::blocks::{Block, ListRow, Tone};

/// The color of a tone in the current theme.
pub fn color(tone: Tone) -> Hsla {
    match tone {
        Tone::Plain => theme::text(),
        Tone::Success => theme::success().into(),
        Tone::Error => theme::error().into(),
        Tone::Attention => theme::attention().into(),
        Tone::Link => theme::link().into(),
        Tone::Muted => theme::text_subtle(),
        Tone::Category(n) => theme::lane(usize::from(n.saturating_sub(1))),
    }
}

/// `block` as an element, `width` wide at most for its charts.
pub fn render(block: &Block, width: f32) -> AnyElement {
    draw(block, width).into_any_element()
}

fn draw(block: &Block, width: f32) -> Div {
    match block {
        Block::Stack(children) => children
            .iter()
            .fold(div().flex().flex_col().gap(ui::sp(8.)).min_w_0(), |d, c| {
                d.child(draw(c, width))
            }),
        Block::Row(children) => {
            let each = width / children.len().max(1) as f32;
            children.iter().fold(
                div().flex().items_center().gap(ui::sp(12.)).min_w_0(),
                |d, c| d.child(draw(c, each).flex_1().min_w_0()),
            )
        }
        Block::Text { text, tone } => div()
            .ty(Type::Body)
            .text_color(match tone {
                Tone::Plain => theme::text_muted(),
                t => color(*t),
            })
            .child(text.clone()),
        Block::Stat {
            label,
            value,
            change,
            tone,
        } => div()
            .flex()
            .items_end()
            .gap(ui::sp(8.))
            .min_w_0()
            .child(
                div()
                    .flex_none()
                    .ty(Type::Title)
                    .text_color(theme::text())
                    .child(value.clone()),
            )
            .child(
                div()
                    .truncate()
                    .pb(ui::sp(2.))
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child(label.clone())
                    .when(!change.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(match tone {
                                    Tone::Plain => theme::text_subtle(),
                                    t => color(*t),
                                })
                                .child(format!(" · {change}")),
                        )
                    }),
            ),
        Block::Dot { tone, label } => ui::legend(label.clone(), color(*tone)).ty(Type::Label),
        Block::Badge { text, tone } => div().child(ui::pill(text.clone(), color(*tone))),
        Block::List(rows) => rows
            .iter()
            .fold(div().flex().flex_col().gap(ui::sp(6.)).min_w_0(), |d, r| {
                d.child(list_row(r))
            }),
        Block::Table { columns, rows } => {
            let line = |cells: &[String], head: bool| {
                cells.iter().fold(div().flex().gap(ui::sp(8.)), |d, c| {
                    d.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ty(if head { Type::Caption } else { Type::Label })
                            .text_color(if head {
                                theme::text_faint()
                            } else {
                                theme::text_muted()
                            })
                            .child(c.clone()),
                    )
                })
            };
            rows.iter().fold(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(4.))
                    .min_w_0()
                    .when(!columns.is_empty(), |d| d.child(line(columns, true))),
                |d, r| d.child(line(r, false)),
            )
        }
        Block::Sparkline { values, tone } => div().child(ui::sparkline(
            values.iter().map(|v| f64::from(*v)).collect(),
            color(*tone),
            width.max(40.),
            36.,
        )),
        Block::Bars {
            values,
            labels,
            tone,
        } => bars(values, labels, color(*tone)),
        Block::Meter { value, label, tone } => div()
            .flex()
            .flex_col()
            .gap(ui::sp(4.))
            .child(
                div()
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child(label.clone()),
            )
            .child(
                div()
                    .h(px(4.))
                    .rounded_full()
                    .bg(theme::fill_selected())
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .w(relative(value.clamp(0., 1.)))
                            .bg(color(*tone)),
                    ),
            ),
        Block::Button { label, .. } => div().child(
            div()
                .px(ui::sp(10.))
                .py(ui::sp(4.))
                .rounded(ui::rad(ui::RADIUS))
                .border_1()
                .border_color(theme::border())
                .ty(Type::Label)
                .text_color(theme::text_muted())
                .child(label.clone()),
        ),
        Block::Link { label, .. } => div()
            .ty(Type::Label)
            .text_color(theme::link())
            .child(label.clone()),
        Block::Empty(text) => div()
            .ty(Type::Body)
            .text_color(theme::text_faint())
            .child(text.clone()),
        Block::Unknown(_) => div()
            .ty(Type::Caption)
            .text_color(theme::text_faint())
            .child(Block::NEWER),
    }
}

fn list_row(r: &ListRow) -> Div {
    div()
        .flex()
        .items_center()
        .gap(ui::sp(8.))
        .min_w_0()
        .ty(Type::Label)
        .children(
            r.dot
                .map(|t| div().flex_none().size(px(6.)).rounded_full().bg(color(t))),
        )
        .child(
            div()
                .flex_none()
                .ty(Type::Mono)
                .text_color(theme::text())
                .child(r.title.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme::text_subtle())
                .child(r.detail.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_color(theme::text_faint())
                .child(r.time.clone()),
        )
}

fn bars(values: &[f32], labels: &[String], color: Hsla) -> Div {
    let hi = values.iter().copied().fold(0f32, f32::max);
    values.iter().enumerate().fold(
        div().flex().items_end().gap(ui::sp(4.)).h(px(48.)),
        |d, (i, v)| {
            let share = if hi > 0. { (v / hi).clamp(0., 1.) } else { 0. };
            d.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_end()
                    .h_full()
                    .gap(ui::sp(2.))
                    .child(
                        div()
                            .w_full()
                            .h(relative(share))
                            .min_h(px(1.))
                            .rounded(ui::rad(2.))
                            .bg(color),
                    )
                    .children(labels.get(i).map(|l| {
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_faint())
                            .child(l.clone())
                    })),
            )
        },
    )
}
