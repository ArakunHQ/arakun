//! A view's pages (Marketplace, Cloudflare) as tabs across the top of its
//! pane (`crate::subnav`). The rail shows the view's row alone: its pages
//! never open under it.

use std::rc::Rc;

use gpui::{App, SharedString};

use super::*;
use crate::sidebar::View;
use crate::subnav::{self, Line, Pick};

impl Shell {
    /// The view of the focused tab, when it has pages of its own.
    pub(super) fn subnav_view(&self) -> Option<View> {
        let tab = self.strip.tabs.get(self.strip.active_tab)?;
        tab_subnav_view(&tab.kind)
    }

    pub(super) fn subnav_lines(&self, view: View, cx: &App) -> Vec<Line> {
        match view {
            View::Marketplace => self.views.extensions.read(cx).nav(),
            View::Cloudflare => self.views.cloudflare.read(cx).nav(),
            _ => Vec::new(),
        }
    }

    /// What narrows the page of `view`, in a row under its tabs.
    pub(super) fn subnav_filters(&self, view: View, cx: &App) -> Vec<Line> {
        match view {
            View::Marketplace => self.views.extensions.read(cx).filters(),
            _ => Vec::new(),
        }
    }

    /// What a click on one of `view`'s pages does.
    pub(super) fn subnav_pick(&self, view: View) -> Pick {
        let extensions = self.views.extensions.downgrade();
        let cloudflare = self.views.cloudflare.downgrade();
        Rc::new(move |key: &SharedString, _, cx: &mut App| {
            let key = key.to_string();
            match view {
                View::Marketplace => {
                    let _ = extensions.update(cx, |v, cx| v.pick_nav(&key, cx));
                }
                View::Cloudflare => {
                    let _ = cloudflare.update(cx, |v, cx| v.pick_nav(&key, cx));
                }
                _ => {}
            }
        })
    }

    /// Whether a tab shows `view`, so its pages are on screen and their
    /// counts must follow it.
    pub(super) fn shows_subnav_view(&self, view: View) -> bool {
        self.strip
            .tabs
            .iter()
            .any(|t| tab_subnav_view(&t.kind) == Some(view))
    }

    /// The pane of tab `index` holding view content `body`, with the view's
    /// pages as tabs over it.
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
        let lines = self.subnav_lines(view, cx);
        if lines.is_empty() {
            return body;
        }
        let filters = self.subnav_filters(view, cx);
        let pick = self.subnav_pick(view);
        // Only the pages move; a new filter narrows the page in place.
        let page = div().flex_1().min_h_0().w_full().child(body);
        let page = match subnav::on_page(&lines) {
            Some((key, index)) => subnav::page(
                page,
                format!("pane-{view:?}"),
                key,
                index,
                subnav::Run::Across,
            ),
            None => page.into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(subnav::bar("pane-subnav", lines, filters, pick))
            .child(page)
            .into_any_element()
    }
}

/// The view a tab shows, when that view has pages of its own.
fn tab_subnav_view(kind: &TabKind) -> Option<View> {
    match kind {
        TabKind::Marketplace => Some(View::Marketplace),
        TabKind::Cloudflare => Some(View::Cloudflare),
        _ => None,
    }
}
