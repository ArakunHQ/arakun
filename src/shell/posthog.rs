//! The PostHog tab: opening it on the visible project and what its links do.

use gpui::{Context, Window};

use super::*;
use crate::posthog_view::PostHogEvent;

impl Shell {
    /// Opens (or focuses) the PostHog tab of the visible environment.
    pub(super) fn open_posthog_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("PostHog", TabKind::PostHog, |k| matches!(k, TabKind::PostHog));
        self.note_active_tab();
        let root = self.project().path.clone();
        self.posthog_view.update(cx, |v, cx| {
            v.set_project(&root, cx);
            v.refresh_if_stale(cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn on_posthog_event(&mut self, ev: &PostHogEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let env_path = self.env().path.clone();
        match ev {
            PostHogEvent::Open(url) => {
                self.push_browser_tab(&env_path, url.clone(), true, true, window, cx);
            }
        }
    }
}
