//! The Doop tab: opening it, and what its links and buttons do.

use gpui::{Context, Window};

use super::*;
use crate::doop_view::DoopEvent;
use crate::island::AlertKind;

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
        self.views.doop.update(cx, |v, cx| {
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
                // The tab reads what the agents on this computer have. In an
                // environment on a server the command would change that
                // server's agents instead, and the tab would never see it.
                if crate::model::Remote::is_path(&env_path) {
                    self.notify(
                        AlertKind::Warning,
                        "Connect Doop to an agent",
                        "This environment runs on a server. Open one on this computer, then connect again.",
                        None,
                        cx,
                    );
                    return;
                }
                self.run_in_terminal(&env_path, command, true, window, cx);
            }
        }
    }
}
