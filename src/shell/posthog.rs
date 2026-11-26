//! The PostHog tab: opening it on the visible project and what its links do.

use gpui::{Context, Window};

use super::*;
use crate::posthog_view::PostHogEvent;

impl Shell {
    /// Opens (or focuses) the PostHog tab of the visible environment.
    pub(super) fn open_posthog_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.has_projects() {
            self.pick_project_for("The PostHog tab", Self::open_posthog_tab, window, cx);
            return;
        }
        self.show_view_tab("PostHog", TabKind::PostHog, |k| {
            matches!(k, TabKind::PostHog)
        });
        self.note_active_tab(cx);
        let root = self.project().path.clone();
        self.views.posthog.update(cx, |v, cx| {
            v.set_project(&root, cx);
            v.refresh_if_stale(cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn on_posthog_event(
        &mut self,
        ev: &PostHogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        let env_path = self.env().path.clone();
        match ev {
            PostHogEvent::Draft(text) => {
                // Stage text only. The usual Send action chooses the new session's worktree.
                self.show_new_session_tab(window, cx);
                self.edit_draft(&env_path, cx, |draft| {
                    let before = draft.trim_end().to_owned();
                    draft.set(if before.is_empty() {
                        text.clone()
                    } else {
                        format!("{before}\n\n{text}")
                    });
                });
                self.composer_changed(cx);
                cx.notify();
            }
            PostHogEvent::Open(url) => {
                self.push_browser_tab(&env_path, url.clone(), true, true, window, cx);
            }
        }
    }
}
