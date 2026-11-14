//! Browser tabs. A page (and CEF itself, on the first one) is only created when its
//! tab is shown; only the pages on screen are marked visible so hidden tabs stay cheap.

use std::path::Path;

use gpui::{AppContext as _, Context, Entity, EntityId, Focusable as _, SharedString, Window};

use super::Shell;
use super::commands::port_open;
use crate::browser::{BrowserEvent, BrowserView, HomeInfo, Server};
use crate::model::{BrowserTab, Tab, TabKind};

/// Terminal lines searched for a dev server's address: enough to reach past a
/// server's startup banner and the agent chatter after it.
const SERVER_SCAN_LINES: usize = 200;

/// The ports of the local addresses in terminal output (`http://localhost:5173/`,
/// `127.0.0.1:3000`, `[::1]:8080`, `0.0.0.0:4000`), each once.
fn local_ports(text: &str) -> Vec<u16> {
    const HOSTS: [&str; 5] = ["localhost:", "127.0.0.1:", "0.0.0.0:", "[::1]:", "[::]:"];
    let mut out = Vec::new();
    for host in HOSTS {
        let mut rest = text;
        while let Some(i) = rest.find(host) {
            rest = &rest[i + host.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).take(5).collect();
            if let Ok(port) = digits.parse::<u16>()
                && port > 0
                && !out.contains(&port)
            {
                out.push(port);
            }
        }
    }
    out
}

impl Shell {
    /// Opens a browser tab in the visible environment; an empty url focuses the address bar.
    pub(super) fn open_browser_tab(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let path = self.env().path.clone();
        self.push_browser_tab(&path, url, true, true, window, cx);
    }

    /// Adds a browser tab to an environment; `focus` gives the page (or its
    /// address bar) the keyboard when it is on screen. Returns the tab's id.
    pub(super) fn push_browser_tab(
        &mut self,
        env_path: &Path,
        url: String,
        activate: bool,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let title = if url.is_empty() { "New tab".to_string() } else { url.clone() };
        let visible = self.has_projects() && self.env().path == env_path;
        let env = self.find_env_mut(env_path)?;
        let tab = Tab::new(title, TabKind::Browser { url: url.into() });
        let id = tab.id;
        env.tabs.push(tab);
        if activate {
            env.active_tab = env.tabs.len() - 1;
        }
        if activate && visible {
            self.note_active_tab();
        }
        self.ensure_active_browser(focus, window, cx);
        self.save(cx);
        cx.notify();
        Some(id)
    }

    /// Indices of the tabs on screen: the active one and the other pane's, if split.
    pub(super) fn visible_tabs(&self) -> Vec<usize> {
        if !self.has_projects() {
            return Vec::new();
        }
        let env = self.env();
        std::iter::once(env.active_tab).chain(env.split_other()).collect()
    }

    /// The pages of the browser tabs on screen.
    pub(super) fn visible_browsers(&self) -> Vec<EntityId> {
        let tabs = self.visible_tabs();
        tabs.into_iter()
            .filter_map(|i| self.env().tabs.get(i)?.browser.as_ref().map(|b| b.view.entity_id()))
            .collect()
    }

    /// Whether a browser tab on screen still lacks its page, or CEF has not been
    /// told which pages are on screen.
    pub(super) fn browsers_out_of_date(&self) -> bool {
        let env = self.env();
        let wants_page = self.visible_tabs().into_iter().any(|i| {
            env.tabs
                .get(i)
                .is_some_and(|t| matches!(t.kind, TabKind::Browser { .. }) && t.browser.is_none())
        });
        wants_page || self.visible_browsers() != self.shown_browsers
    }

    /// Creates the pages of the browser tabs on screen that have none yet; `focus`
    /// focuses the active one.
    pub(super) fn ensure_active_browser(&mut self, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let active = self.env().active_tab;
        for index in self.visible_tabs() {
            let page = self.ensure_page(index, window, cx);
            if focus
                && index == active
                && let Some(view) = page
            {
                if view.read(cx).is_home() {
                    view.update(cx, |v, cx| v.focus_address(window, cx));
                } else {
                    window.focus(&view.focus_handle(cx), cx);
                }
            }
        }
        self.sync_browsers(cx);
        self.refresh_home(cx);
    }

    /// Tells the blank tabs on screen what runs in their environment: its branch and
    /// pull request, and the local servers its terminals mention that answer right
    /// now. The ports are probed off the main thread; the poll calls this every tick.
    pub(super) fn refresh_home(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let env = self.env();
        let views: Vec<Entity<BrowserView>> = self
            .visible_tabs()
            .into_iter()
            .filter_map(|i| env.tabs.get(i)?.browser.as_ref().map(|b| b.view.clone()))
            .filter(|v| v.read(cx).is_home())
            .collect();
        if views.is_empty() {
            return;
        }
        let branch = env.branch.clone();
        let pr = env.pr.as_ref().map(|pr| (pr.number, SharedString::from(pr.url.clone())));
        let mut candidates: Vec<Server> = Vec::new();
        for t in &env.terminals {
            let text = t.terminal.read(cx).recent_text(SERVER_SCAN_LINES);
            let by: SharedString = match &t.command {
                Some(c) => c.clone(),
                None => format!("terminal {}", t.number).into(),
            };
            for port in local_ports(&text) {
                if !candidates.iter().any(|s| s.port == port) {
                    candidates.push(Server { port, by: by.clone() });
                }
            }
        }
        cx.spawn(async move |_, cx| {
            let servers = cx
                .background_spawn(async move {
                    let mut up: Vec<Server> = candidates.into_iter().filter(|s| port_open(s.port)).collect();
                    up.sort_by_key(|s| s.port);
                    up
                })
                .await;
            let info = HomeInfo { branch, pr, servers, checked: true };
            for view in views {
                view.update(cx, |v, cx| v.set_home(info.clone(), cx));
            }
        })
        .detach();
    }

    fn ensure_page(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<BrowserView>> {
        let env_path = self.env().path.clone();
        self.ensure_page_in(&env_path, index, window, cx)
    }

    /// The page of browser tab `index` of an environment, created when it has none.
    pub(super) fn ensure_page_in(
        &mut self,
        env_path: &Path,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<BrowserView>> {
        let tab = self.find_env_mut(env_path)?.tabs.get_mut(index)?;
        let TabKind::Browser { url } = &tab.kind else {
            return None;
        };
        if tab.browser.is_none() {
            let env_path = env_path.to_path_buf();
            let url = url.to_string();
            let view = cx.new(|cx| BrowserView::new(url, window, cx));
            let events = cx.subscribe_in(&view, window, move |this, view, ev: &BrowserEvent, window, cx| {
                this.browser_event(&env_path, view, ev, window, cx)
            });
            tab.browser = Some(BrowserTab { view: view.clone(), _events: events });
            self.browsers_dirty = true;
            return Some(view);
        }
        tab.browser.as_ref().map(|b| b.view.clone())
    }

    /// Marks only the pages on screen as visible.
    pub(super) fn sync_browsers(&mut self, cx: &mut Context<Self>) {
        let shown = self.visible_browsers();
        if shown == self.shown_browsers && !self.browsers_dirty {
            return;
        }
        self.shown_browsers = shown.clone();
        self.browsers_dirty = false;
        let views: Vec<Entity<BrowserView>> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.tabs)
            .filter_map(|t| t.browser.as_ref().map(|b| b.view.clone()))
            .collect();
        for view in views {
            let visible = shown.contains(&view.entity_id());
            view.update(cx, |v, cx| v.set_visible(visible, cx));
        }
    }

    fn browser_tab_mut(&mut self, env_path: &Path, id: EntityId) -> Option<(usize, &mut Tab)> {
        let env = self.find_env_mut(env_path)?;
        env.tabs
            .iter_mut()
            .enumerate()
            .find(|(_, t)| t.browser.as_ref().is_some_and(|b| b.view.entity_id() == id))
    }

    fn browser_event(
        &mut self,
        env_path: &Path,
        view: &Entity<BrowserView>,
        ev: &BrowserEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = view.entity_id();
        // Typed navigations set the view's url before CEF reports it, so no UrlChanged
        // arrives for them; take the url from the view on every event instead.
        let url = view.read(cx).url();
        if !url.is_empty()
            && let Some((_, tab)) = self.browser_tab_mut(env_path, id)
            && !matches!(&tab.kind, TabKind::Browser { url: u } if *u == url)
        {
            tab.kind = TabKind::Browser { url };
            self.save(cx);
        }
        match ev {
            BrowserEvent::OpenInNewTab(url) => {
                let visible = self.has_projects() && self.env().path == env_path;
                self.push_browser_tab(env_path, url.clone(), visible, visible, window, cx);
            }
            BrowserEvent::CloseRequested => {
                let visible = self.has_projects() && self.env().path == env_path;
                if let Some((i, _)) = self.browser_tab_mut(env_path, id) {
                    if visible {
                        self.close_tab(i, window, cx);
                    } else if let Some(env) = self.find_env_mut(env_path) {
                        env.tabs.remove(i);
                        if env.tabs.is_empty() {
                            env.tabs.push(Tab::new("New session", TabKind::NewSession));
                        }
                        env.active_tab = env.active_tab.min(env.tabs.len() - 1);
                    }
                }
            }
            BrowserEvent::TitleChanged(title) => {
                if let Some((_, tab)) = self.browser_tab_mut(env_path, id)
                    && !title.is_empty()
                {
                    tab.title = title.clone();
                }
                cx.notify();
            }
            BrowserEvent::UrlChanged(_)
            | BrowserEvent::LoadingChanged(_)
            | BrowserEvent::FaviconChanged => cx.notify(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::local_ports;

    #[test]
    fn ports_come_from_local_addresses_only() {
        let out = "  ➜  Local:   http://localhost:5173/\n  ➜  Network: http://192.168.1.4:5173/\n\
                   Server listening on 0.0.0.0:3000\nAPI at [::1]:8080 and localhost:5173 again\n\
                   see https://example.com:443/";
        assert_eq!(local_ports(out), vec![5173, 3000, 8080]);
        assert!(local_ports("localhost: nothing, localhost:0").is_empty());
    }
}
