//! One sidebar: the pages of the open view (Marketplace, Cloudflare) show in
//! the rail under the view's row, not in a second column inside the view.
//! When the rail can't hold them, the pane draws them beside the view
//! (`crate::subnav`).

use std::rc::Rc;

use gpui::{App, SharedString};

use super::*;
use crate::sidebar::{Piece, View};
use crate::subnav::{self, Line, Pick};

/// How far the pages sit right of the view's row: under its name, past
/// the icon.
pub(super) const RAIL_INSET: f32 = 16.;

impl Shell {
    /// The view of the focused tab, when it has pages of its own.
    pub(super) fn subnav_view(&self) -> Option<View> {
        let tab = self.strip.tabs.get(self.strip.active_tab)?;
        tab_subnav_view(&tab.kind)
    }

    pub(super) fn subnav_lines(&self, view: View, cx: &App) -> Vec<Line> {
        match view {
            View::Extensions => self.extensions_view.read(cx).nav(),
            View::Cloudflare => self.cloudflare_view.read(cx).nav(),
            _ => Vec::new(),
        }
    }

    /// What a click on one of `view`'s pages does.
    pub(super) fn subnav_pick(&self, view: View) -> Pick {
        let extensions = self.extensions_view.downgrade();
        let cloudflare = self.cloudflare_view.downgrade();
        Rc::new(move |key: &SharedString, _, cx: &mut App| {
            let key = key.to_string();
            match view {
                View::Extensions => {
                    let _ = extensions.update(cx, |v, cx| v.pick_nav(&key, cx));
                }
                View::Cloudflare => {
                    let _ = cloudflare.update(cx, |v, cx| v.pick_nav(&key, cx));
                }
                _ => {}
            }
        })
    }

    /// Whether the rail draws `view`'s pages: it shows as a list (not
    /// hidden, narrowed or being arranged) and holds the view where it can
    /// be seen (not in a folded segment). In a row of icons, the pages go
    /// under the row.
    pub(super) fn rail_hosts_subnav(&self, view: View, cx: &App) -> bool {
        if !self.rail_shown() || self.rail_narrow() || self.arranging() {
            return false;
        }
        let piece = Piece::View { view };
        let Some((s, _)) = self.sidebar.find(&piece) else {
            return false;
        };
        let section = &self.sidebar.sections[s];
        self.rail_shows(&piece, cx) && !(section.collapsible && section.collapsed)
    }

    /// The pages under `view`'s row in the rail, when that view is open.
    /// `inset` is 0 under a row of icons, which has no name to line up with.
    pub(super) fn rail_subnav(
        &self,
        view: View,
        inset: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.subnav_view() != Some(view) || !self.rail_hosts_subnav(view, cx) {
            return None;
        }
        let lines = self.subnav_lines(view, cx);
        if lines.is_empty() {
            return None;
        }
        let pick = self.subnav_pick(view);
        Some(
            subnav::render("rail-subnav", lines, inset, pick)
                .pb(crate::ui::sp(4.))
                .into_any_element(),
        )
    }

    /// The pane of tab `index` holding view content `body`: the pages go in a
    /// column beside it when the rail doesn't show them.
    pub(super) fn with_subnav(
        &self,
        index: usize,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(view) = self
            .strip
            .tabs
            .get(index)
            .and_then(|t| tab_subnav_view(&t.kind))
        else {
            return body;
        };
        if index == self.strip.active_tab && self.rail_hosts_subnav(view, cx) {
            return body;
        }
        let lines = self.subnav_lines(view, cx);
        if lines.is_empty() {
            return body;
        }
        let pick = self.subnav_pick(view);
        div()
            .flex()
            .size_full()
            .child(subnav::column("pane-subnav", lines, pick).bg(theme::background()))
            .child(div().flex_1().min_w_0().h_full().child(body))
            .into_any_element()
    }
}

/// The view a tab shows, when that view has pages of its own.
fn tab_subnav_view(kind: &TabKind) -> Option<View> {
    match kind {
        TabKind::Extensions => Some(View::Extensions),
        TabKind::Cloudflare => Some(View::Cloudflare),
        _ => None,
    }
}
