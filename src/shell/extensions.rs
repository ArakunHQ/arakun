//! The Marketplace tab: opening it on the visible environment and what its
//! buttons do in the window (terminals, files beside the chat). Installing on
//! a paired server goes through that server's connection (`shell::servers`).

use gpui::{Context, Window};

use super::*;
use crate::extensions_view::ExtensionsEvent;

impl Shell {
    /// Opens (or focuses) the Marketplace tab of the visible environment.
    pub(super) fn open_extensions_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.env_menu_open = false;
        if !self.has_projects() {
            self.pick_project_for("The Marketplace tab", Self::open_extensions_tab, window, cx);
            return;
        }
        self.show_view_tab("Marketplace", TabKind::Extensions, |k| {
            matches!(k, TabKind::Extensions)
        });
        self.note_active_tab(cx);
        let dir = self.env().path.clone();
        let targets = self.server_targets();
        self.extensions_view.update(cx, |v, cx| {
            v.set_servers(targets, cx);
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
        if let ExtensionsEvent::Toggled(view, on) = *ev {
            // Turned on, it shows where the user would look for it, even if
            // they took it out of their layout before.
            if on {
                self.change_sidebar(cx, |s| {
                    s.place(view);
                });
            }
            cx.notify();
            return;
        }
        match ev {
            ExtensionsEvent::ConnectServers => {
                self.servers_connect(window, cx);
                self.sync_servers_view(cx);
                return;
            }
            ExtensionsEvent::Server(id, msg) => {
                self.server_send_raw(id, msg);
                return;
            }
            _ => {}
        }
        if !self.has_projects() {
            return;
        }
        let env_path = self.env().path.clone();
        match ev {
            ExtensionsEvent::Run(command) => {
                self.run_in_terminal(&env_path, command, true, window, cx);
            }
            ExtensionsEvent::Open(path) => self.open_file_beside(&env_path, path, None, window, cx),
            ExtensionsEvent::Toggled(..)
            | ExtensionsEvent::ConnectServers
            | ExtensionsEvent::Server(..) => {}
        }
    }
}
