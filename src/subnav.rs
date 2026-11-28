//! A view's own pages (Marketplace, Cloudflare), drawn as a row of tabs
//! across the top of the view. The view says what its pages are as `Line`s;
//! the shell draws the row, so the sidebar keeps one row per view and never
//! holds a view's whole menu.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, App, Div, ElementId, Hsla, SharedString, Window, div, px};

use crate::theme;
use crate::ui::motion;
use crate::ui::{RADIUS, TOOLBAR_H, Tone, Type, Typography, icon};

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

/// How a view lays out the items that pick its pages: a row of tabs, or a
/// list down the side (Settings).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Run {
    Across,
    Down,
}

/// How far a new page travels as it comes in.
const PAGE_SHIFT: f32 = 5.;
/// How long a new page takes to come in.
const PAGE_IN: Duration = motion::FAST;
/// How long after a switch the page is still drawn as coming in. Longer
/// than `PAGE_IN`, so a late first frame still plays the whole way; after
/// it, a view shown again (another tab, then back) does not play it again.
const PAGE_HOLD: Duration = Duration::from_millis(PAGE_IN.as_millis() as u64 * 2);

/// The page a view showed last, and its last switch.
struct Shown {
    key: SharedString,
    index: usize,
    /// Counts switches, so each one starts its animation over.
    turn: usize,
    /// When the last switch was, and whether the page picked comes after the
    /// one left.
    switch: Option<(Instant, bool)>,
}

thread_local! {
    static SHOWN: RefCell<HashMap<SharedString, Shown>> = RefCell::new(HashMap::new());
}

/// Notes that `view` shows page `key`, at `index` among its pages. Returns
/// the switch to play when the page came in within `PAGE_HOLD`: its turn,
/// and whether it comes after the page left. The first page a view shows
/// does not play: the view arrives on its own.
fn note(
    shown: &mut HashMap<SharedString, Shown>,
    view: &SharedString,
    key: SharedString,
    index: usize,
    now: Instant,
) -> Option<(usize, bool)> {
    let Some(s) = shown.get_mut(view) else {
        shown.insert(
            view.clone(),
            Shown {
                key,
                index,
                turn: 0,
                switch: None,
            },
        );
        return None;
    };
    if s.key != key {
        // A page at the same place (one renamed) comes in as the next one.
        s.switch = Some((now, index >= s.index));
        s.turn += 1;
        s.key = key;
    }
    s.index = index;
    let (at, after) = s.switch?;
    (now.duration_since(at) < PAGE_HOLD).then_some((s.turn, after))
}

/// `el`, the page `view` shows, at `index` among its pages and named by
/// `key`. Right after a switch it fades in over `motion::FAST` while it
/// moves a few pixels in from the side of the item picked: from the right
/// for a tab to the right of the last one, from below for an item lower in a
/// list. The page left does not animate, and with Reduce motion on it cuts.
pub fn page<E: IntoElement + Styled + 'static>(
    el: E,
    view: impl Into<SharedString>,
    key: impl Into<SharedString>,
    index: usize,
    run: Run,
) -> AnyElement {
    let view = view.into();
    let played = SHOWN.with(|s| {
        note(
            &mut s.borrow_mut(),
            &view,
            key.into(),
            index,
            Instant::now(),
        )
    });
    let Some((turn, after)) = played else {
        return el.into_any_element();
    };
    let id = ElementId::from((SharedString::from(format!("{view}-page-in")), turn));
    match run {
        Run::Across => {
            let side = if after {
                motion::Side::Right
            } else {
                motion::Side::Left
            };
            motion::enter_from_side(el, id, side, PAGE_SHIFT, PAGE_IN).into_any_element()
        }
        Run::Down => {
            let from = if after {
                motion::From::Below
            } else {
                motion::From::Above
            };
            motion::enter(el, id, from, PAGE_SHIFT, PAGE_IN).into_any_element()
        }
    }
}

/// Forgets the page `view` showed, so the next one it shows arrives with
/// it, as the first does. For a view whose content changes under the same
/// pages (another pull request in the same sheet).
pub fn forget(view: &str) {
    SHOWN.with(|s| s.borrow_mut().remove(view));
}

/// The page `lines` mark as on: its key, and where it is among the pages.
/// The pages are the entries; headings and gaps are not.
pub fn on_page(lines: &[Line]) -> Option<(SharedString, usize)> {
    lines
        .iter()
        .filter_map(|l| match l {
            Line::Entry(e) => Some(e),
            _ => None,
        })
        .enumerate()
        .find(|(_, e)| e.on)
        .map(|(i, e)| (e.key.clone(), i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_plays_once_per_switch_from_the_side_picked() {
        let mut shown = HashMap::new();
        let view = SharedString::from("v");
        let t0 = Instant::now();
        // The first page shown arrives with its view.
        assert_eq!(note(&mut shown, &view, "a".into(), 0, t0), None);
        assert_eq!(note(&mut shown, &view, "a".into(), 0, t0), None);
        // A page to the right comes from the right, and keeps its turn while
        // it plays.
        assert_eq!(note(&mut shown, &view, "c".into(), 2, t0), Some((1, true)));
        let soon = t0 + Duration::from_millis(50);
        assert_eq!(
            note(&mut shown, &view, "c".into(), 2, soon),
            Some((1, true))
        );
        // After it, drawing the same page again plays nothing.
        let later = t0 + PAGE_HOLD;
        assert_eq!(note(&mut shown, &view, "c".into(), 2, later), None);
        // A page to the left comes from the left, with a new turn.
        assert_eq!(
            note(&mut shown, &view, "b".into(), 1, later),
            Some((2, false))
        );
    }

    #[test]
    fn views_keep_their_own_page() {
        let mut shown = HashMap::new();
        let t0 = Instant::now();
        let (a, b) = (SharedString::from("a"), SharedString::from("b"));
        note(&mut shown, &a, "x".into(), 0, t0);
        assert_eq!(note(&mut shown, &b, "y".into(), 3, t0), None);
        assert_eq!(note(&mut shown, &a, "x".into(), 0, t0), None);
    }

    #[test]
    fn the_page_on_counts_only_entries() {
        let lines = vec![
            Line::Heading("Group".into()),
            Line::Entry(Entry::new("one", Lead::Icon("cube"), "One")),
            Line::Gap,
            Line::Entry(Entry::new("two", Lead::Icon("cube"), "Two").on(true)),
        ];
        assert_eq!(on_page(&lines), Some(("two".into(), 1)));
        assert_eq!(on_page(&lines[..2]), None);
    }

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
