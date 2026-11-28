//! The Linear tab: opening it and what its links and buttons do.

use gpui::{Context, Window};

use super::*;
use crate::island::AlertKind;
use crate::linear_view::LinearEvent;

impl Shell {
    /// Opens (or focuses) the Linear tab. It reads Linear, not a project:
    /// with none added, it shows in the welcome's place.
    pub(super) fn open_linear_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if self.has_projects() {
            self.show_view_tab("Linear", TabKind::Linear, |k| matches!(k, TabKind::Linear));
            self.note_active_tab(cx);
        } else {
            self.show_alone(Some(super::home::Alone::Linear), cx);
        }
        self.push_linear_host(cx);
        self.views.linear.update(cx, |v, cx| {
            v.refresh_if_stale(cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn on_linear_event(
        &mut self,
        ev: &LinearEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            LinearEvent::Open(url) => {
                if !may_open(url) {
                    return;
                }
                if self.has_projects() {
                    let env_path = self.env().path.clone();
                    self.push_browser_tab(&env_path, url.clone(), true, true, window, cx);
                } else {
                    cx.open_url(url);
                }
            }
            LinearEvent::Ask { title, prompt } => {
                if self.has_projects() {
                    self.ask_agent(title.clone().into(), prompt.clone(), window, cx);
                    return;
                }
                // An agent works in a project. The picker can only open a
                // view afterwards, not carry the prompt, so it is asked again.
                self.pick_project_for(
                    "Working on a Linear issue",
                    Self::open_linear_tab,
                    window,
                    cx,
                );
                self.notify(
                    AlertKind::Info,
                    "Add a project first",
                    "An agent works in a project. Add one, then ask again from the issue.",
                    None,
                    cx,
                );
            }
            LinearEvent::DelegationChanged | LinearEvent::WorkspacesChanged => {
                self.push_linear_host(cx);
                self.sync_linear(window, cx);
                self.linear_push_servers(cx);
                // More slots may have been chosen: runs that wait start now.
                self.linear_start_waiting(window, cx);
            }
            LinearEvent::OpenPhoneTab => self.open_phone_tab(window, cx),
            LinearEvent::SignInToArakun => self.open_account_sign_in(window, cx),
            LinearEvent::LinkGitHub => self.open_link_github(window, cx),
        }
    }
}

/// The link comes from Linear's answer; only a web page may open, never a
/// `file:` or `javascript:` link.
fn may_open(url: &str) -> bool {
    url.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::may_open;

    #[test]
    fn only_https_links_open() {
        assert!(may_open("https://linear.app/a/issue/ENG-1"));
        for url in [
            "http://linear.app",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "",
        ] {
            assert!(!may_open(url), "{url}");
        }
    }
}
