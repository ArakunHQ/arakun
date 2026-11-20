//! The Doop tab: opening it, and what its links and buttons do.

use gpui::{Context, Window};

use super::*;
use crate::doop_view::DoopEvent;

impl Shell {
    /// Opens (or focuses) the Doop tab of the visible environment.
    pub(super) fn open_doop_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.has_projects() {
            self.pick_project_for("The Doop tab", Self::open_doop_tab, window, cx);
            return;
        }
        self.show_view_tab("Doop", TabKind::Doop, |k| matches!(k, TabKind::Doop));
        self.note_active_tab(cx);
        self.doop_view.update(cx, |v, cx| {
            v.refresh_if_stale(cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn on_doop_event(
        &mut self,
        ev: &DoopEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        let env_path = self.env().path.clone();
        match ev {
            DoopEvent::Open(url) => {
                self.push_browser_tab(&env_path, url.clone(), true, true, window, cx);
            }
            DoopEvent::Run(command) => {
                self.run_in_terminal(&env_path, command, true, window, cx);
            }
        }
    }
}
