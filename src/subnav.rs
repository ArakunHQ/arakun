//! A view's own pages (Marketplace, Cloudflare), drawn as a row of tabs
//! across the top of the view. The view says what its pages are as `Line`s;
//! the shell draws the row, so the sidebar keeps one row per view and never
//! holds a view's whole menu.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Div, Hsla, SharedString, Window, div, px};

use crate::theme;
use crate::ui::{RADIUS, TOOLBAR_H, Tone, Type, Typography, icon, segment, segmented};

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
    /// Small capitals before a group.
    Heading(SharedString),
    /// A line between groups.
    Gap,
    Entry(Entry),
    /// One of a few, side by side: (key, label, on).
    Choice(Vec<(SharedString, SharedString, bool)>),
}

/// Called with the key of the page picked.
pub type Pick = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The pages as a row of tabs over the view, and `filters`, what narrows
/// the page, in a quieter row under them. `id` keeps the element ids of two
/// bars apart. A row wider than the pane wraps, so every page stays one click
/// away without a sideways scroll.
pub fn bar(id: &str, lines: Vec<Line>, filters: Vec<Line>, pick: Pick) -> Div {
    let tabs = pieces(&format!("{id}-tab"), "Pages", lines, false, &pick).min_h(px(TOOLBAR_H));
    let filters = (!filters.is_empty()).then(|| {
        pieces(&format!("{id}-filter"), "Filters", filters, true, &pick).pb(crate::ui::sp(4.))
    });
    div()
        .flex()
        .flex_col()
        .flex_none()
        .px(crate::ui::sp(12.))
        .py(crate::ui::sp(4.))
        .gap(crate::ui::sp(2.))
        .border_b_1()
        .border_color(theme::border_subtle())
        .bg(theme::background())
        .child(tabs)
        .children(filters)
}

/// One row of `bar`. `small` draws the quieter filters. Each run of tabs
/// is a tab list of its own, named by the heading before it or else `name`;
/// headings, choices and lines stay outside the lists.
fn pieces(id: &str, name: &str, lines: Vec<Line>, small: bool, pick: &Pick) -> Div {
    let mut out = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(crate::ui::sp(2.));
    let mut run: Vec<AnyElement> = Vec::new();
    let mut label = SharedString::from(name.to_string());
    let mut lists = 0;
    let mut flush = |out: Div, run: &mut Vec<AnyElement>, label: &SharedString| {
        if run.is_empty() {
            return out;
        }
        lists += 1;
        out.child(tab_list(
            SharedString::from(format!("{id}-list-{lists}")),
            label.clone(),
            std::mem::take(run),
        ))
    };
    // A heading or a gap starts a new group; the line between groups goes in
    // before its first piece, so two in a row draw one line and none leads.
    let (mut started, mut split) = (false, false);
    for (i, line) in lines.into_iter().enumerate() {
        let el_id = SharedString::from(format!("{id}-{i}"));
        let (piece, is_tab) = match line {
            Line::Gap => {
                split = true;
                continue;
            }
            Line::Heading(text) => {
                split = true;
                let el = crate::ui::overline(&text)
                    .flex_none()
                    .px(crate::ui::sp(6.))
                    .into_any_element();
                // The heading names the tabs after it, so close the list before it.
                out = flush(out, &mut run, &label);
                label = text;
                (el, false)
            }
            Line::Choice(choices) => {
                let mut seg = segmented();
                for (j, (key, label, on)) in choices.into_iter().enumerate() {
                    let pick = pick.clone();
                    seg = seg.child(
                        segment((el_id.clone(), j), None, label, on)
                            .on_click(move |_, window, cx| pick(&key, window, cx)),
                    );
                }
                let el = div()
                    .flex_none()
                    .mx(crate::ui::sp(4.))
                    .child(seg)
                    .into_any_element();
                (el, false)
            }
            Line::Entry(e) => {
                let pick = pick.clone();
                let key = e.key.clone();
                let el = tab(el_id, e, small)
                    .on_click(move |_, window, cx| pick(&key, window, cx))
                    .into_any_element();
                (el, true)
            }
        };
        if started && split {
            out = flush(out, &mut run, &label);
            out = out.child(divider());
        }
        (started, split) = (true, false);
        if is_tab {
            run.push(piece);
        } else {
            out = flush(out, &mut run, &label);
            out = out.child(piece);
        }
    }
    flush(out, &mut run, &label)
}

/// Tabs that belong together, for screen readers. It wraps like the row
/// around it, so a long run still fits a narrow pane.
fn tab_list(id: SharedString, label: SharedString, tabs: Vec<AnyElement>) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .role(gpui::Role::TabList)
        .aria_label(label)
        .flex()
        .flex_wrap()
        .items_center()
        .min_w_0()
        .gap(crate::ui::sp(2.))
        .children(tabs)
}

/// The thin upright line between two groups of tabs.
fn divider() -> Div {
    div()
        .flex_none()
        .w(px(1.))
        .h(px(16.))
        .mx(crate::ui::sp(6.))
        .bg(theme::border())
}

fn tab(id: SharedString, e: Entry, small: bool) -> gpui::Stateful<Div> {
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
        .role(gpui::Role::Tab)
        .aria_label(e.label.clone())
        .aria_selected(on)
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .h(px(if small { 24. } else { 28. }))
        .px(crate::ui::sp(if small { 8. } else { 10. }))
        .rounded(crate::ui::rad(RADIUS))
        .cursor_pointer()
        .ty(if small { Type::Caption } else { Type::Label })
        .when(e.dim, |d| d.opacity(0.6))
        .when(on, |d| {
            d.bg(theme::fill_selected()).text_color(theme::text())
        })
        .when(!on, |d| {
            d.text_color(theme::text_muted())
                .hover(|d| d.bg(theme::fill_hover()).text_color(theme::text()))
        })
        .child(lead)
        // A long server or repository name is cut short, so one tab never
        // outgrows a narrow pane.
        .child(
            div()
                .min_w_0()
                .max_w(px(LABEL_MAX_W))
                .truncate()
                .child(e.label),
        )
        .children(count)
}

/// The widest a tab's label gets before it is cut short.
const LABEL_MAX_W: f32 = 180.;

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
