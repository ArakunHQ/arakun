//! The Extensions tab: opening it on the visible environment and what its
//! buttons do in the window (terminals, files beside the chat).

use gpui::{Context, Window};

use super::*;
use crate::extensions_view::ExtensionsEvent;

impl Shell {
    /// Opens (or focuses) the Extensions tab of the visible environment.
    pub(super) fn open_extensions_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.env_menu_open = false;
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Extensions", TabKind::Extensions, |k| {
            matches!(k, TabKind::Extensions)
        });
        self.note_active_tab();
        let dir = self.env().path.clone();
        self.extensions_view.update(cx, |v, cx| {
            v.set_dir(dir, cx);
            v.refresh_if_stale(cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn on_extensions_event(
        &mut self,
        ev: &ExtensionsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        let env_path = self.env().path.clone();
        match ev {
            ExtensionsEvent::Run(command) => {
                self.run_in_terminal(&env_path, command, true, window, cx);
            }
            ExtensionsEvent::Open(path) => self.open_file_beside(&env_path, path, None, window, cx),
        }
    }
}
