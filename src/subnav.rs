//! A view's own pages, drawn in the one sidebar. A view that has pages
//! (Marketplace, Cloudflare) says what they are as `Line`s and no longer
//! draws a column of its own: the rail shows them under the view's row while
//! its tab is open, and the shell draws them beside the view only when the
//! rail can't (hidden, narrowed, or without the view's row).

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, Div, Hsla, SharedString, Window, div, px};

use crate::theme;
use crate::ui::{RADIUS, Tone, Type, Typography, icon, segment, segmented};

/// What sits before a page's name.
#[derive(Clone)]
pub enum Lead {
    Icon(&'static str),
    /// A colored dot, as for a category.
    Dot(Hsla),
    Mark(crate::logos::Mark),
}

#[derive(Clone)]
pub struct Entry {
    /// What the view gets back when the page is picked.
    pub key: SharedString,
    pub lead: Lead,
    pub label: SharedString,
    pub count: Option<SharedString>,
    /// Colors the count, for one that needs a look.
    pub tone: Option<Tone>,
    pub on: bool,
    /// Can't be used now (a server that is not connected).
    pub dim: bool,
}

impl Entry {
    pub fn new(key: impl Into<SharedString>, lead: Lead, label: impl Into<SharedString>) -> Self {
        Entry {
            key: key.into(),
            lead,
            label: label.into(),
            count: None,
            tone: None,
            on: false,
            dim: false,
        }
    }

    pub fn count(mut self, n: Option<impl ToString>) -> Self {
        self.count = n.map(|n| n.to_string().into());
        self
    }

    pub fn tone(mut self, tone: Option<Tone>) -> Self {
        self.tone = tone;
        self
    }

    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    pub fn dim(mut self, dim: bool) -> Self {
        self.dim = dim;
        self
    }
}

#[derive(Clone)]
pub enum Line {
    /// Small capitals over a group.
    Heading(SharedString),
    /// Room between groups.
    Gap,
    Entry(Entry),
    /// One of a few, side by side: (key, label, on).
    Choice(Vec<(SharedString, SharedString, bool)>),
}

/// Called with the key of the page picked.
pub type Pick = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The lines as rows. `id` keeps the element ids of two lists apart;
/// `inset` moves the rows right, under the name of the rail row they belong to.
pub fn render(id: &str, lines: Vec<Line>, inset: f32, pick: Pick) -> Div {
    let mut out = div().flex().flex_col().gap(crate::ui::sp(1.));
    for (i, line) in lines.into_iter().enumerate() {
        let el_id = SharedString::from(format!("{id}-{i}"));
        out = out.child(match line {
            Line::Heading(text) => crate::ui::overline(&text)
                .flex_none()
                .pl(crate::ui::sp(8.) + px(inset))
                .pt(crate::ui::sp(10.))
                .pb(crate::ui::sp(4.))
                .into_any_element(),
            Line::Gap => div().flex_none().h(px(8.)).into_any_element(),
            Line::Choice(choices) => {
                let mut seg = segmented().w_full();
                for (j, (key, label, on)) in choices.into_iter().enumerate() {
                    let pick = pick.clone();
                    seg = seg.child(
                        segment((el_id.clone(), j), None, label, on)
                            .flex_1()
                            .justify_center()
                            .on_click(move |_, window, cx| pick(&key, window, cx)),
                    );
                }
                div()
                    .flex_none()
                    .ml(px(inset))
                    .py(crate::ui::sp(4.))
                    .child(seg)
                    .into_any_element()
            }
            Line::Entry(e) => {
                let pick = pick.clone();
                let key = e.key.clone();
                row(el_id, e, inset)
                    .on_click(move |_, window, cx| pick(&key, window, cx))
                    .into_any_element()
            }
        });
    }
    out
}

fn row(id: SharedString, e: Entry, inset: f32) -> gpui::Stateful<Div> {
    let on = e.on;
    let lead = match e.lead {
        Lead::Icon(name) => icon(name, 14.)
            .text_color(if on {
                theme::text()
            } else {
                theme::text_subtle()
            })
            .into_any_element(),
        Lead::Dot(color) => div()
            .flex_none()
            .size(px(8.))
            .mx(px(3.))
            .rounded_full()
            .bg(color)
            .into_any_element(),
        Lead::Mark(mark) => crate::logos::tile(mark, 16.).into_any_element(),
    };
    let count = e.count.map(|n| match e.tone {
        Some(t) => div()
            .flex_none()
            .flex()
            .items_center()
            .h(px(18.))
            .px(crate::ui::sp(6.))
            .rounded_full()
            .bg(t.color().opacity(0.18))
            .ty(Type::Caption)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(t.color())
            .child(n),
        None => div()
            .flex_none()
            .ty(Type::Caption)
            .text_color(theme::text_faint())
            .child(n),
    });
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(28.))
        .ml(px(inset))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(RADIUS))
        .cursor_pointer()
        .ty(Type::Label)
        .when(e.dim, |d| d.opacity(0.6))
        .when(on, |d| {
            d.bg(theme::fill_selected()).text_color(theme::text())
        })
        .when(!on, |d| {
            d.text_color(theme::text_muted())
                .hover(|d| d.bg(theme::fill_hover()).text_color(theme::text()))
        })
        .child(lead)
        .child(div().flex_1().min_w_0().truncate().child(e.label))
        .children(count)
}

/// The lines as a column of their own beside the view, for when the rail
/// can't hold them.
pub fn column(id: &str, lines: Vec<Line>, pick: Pick) -> gpui::Stateful<Div> {
    div()
        .id(SharedString::from(format!("{id}-column")))
        .flex()
        .flex_col()
        .flex_none()
        .w(px(COLUMN_W))
        .h_full()
        .overflow_y_scroll()
        .px(crate::ui::sp(12.))
        .py(crate::ui::sp(12.))
        .border_r_1()
        .border_color(theme::border_subtle())
        .child(render(id, lines, 0., pick))
}

/// The column's width.
const COLUMN_W: f32 = 212.;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_keeps_what_it_is_given() {
        let e = Entry::new("k", Lead::Icon("cube"), "Plugins")
            .count(Some(3))
            .on(true);
        assert_eq!(e.count.as_deref(), Some("3"));
        assert!(e.on && !e.dim && e.tone.is_none());
        let e = Entry::new("k", Lead::Icon("cube"), "Plugins").count(None::<usize>);
        assert!(e.count.is_none());
    }
}
