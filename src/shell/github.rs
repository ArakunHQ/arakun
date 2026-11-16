//! The GitHub tab (the notifications inbox): opening it, its rail badge, what
//! its rows do, and the notices for new notifications that deserve one.

use gpui::{App, Context, SharedString, Window};

use super::island::Open;
use super::*;
use crate::github::pilot::{self, PilotEvent};
use crate::github::{self, GitHubEvent, Notification};
use crate::github_view::GitHubViewEvent;
use crate::island::AlertKind;

/// More new notifications than this at once (the computer woke up, say) get one
/// notice for all of them.
const NOTICES_AT_ONCE: usize = 3;

impl Shell {
    /// Opens (or focuses) the GitHub tab of the visible environment.
    pub(super) fn open_github_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if self.has_projects() {
            self.show_view_tab("GitHub", TabKind::GitHub, |k| matches!(k, TabKind::GitHub));
            self.note_active_tab();
        } else {
            // The queue doesn't need a project: it shows in the welcome's place.
            self.github_alone = true;
            // The Servers tab was in that place: nobody is looking at it now.
            if std::mem::take(&mut self.servers_alone) {
                self.servers_tab_closed(cx);
            }
        }
        self.github.update(cx, |g, cx| g.opened(cx));
        self.github_view.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// The rail badge: how many things wait on the user (the agent's cards
    /// too), and whether any is one the alert rule cares about.
    pub(super) fn github_badge(&self, cx: &App) -> Option<(usize, bool)> {
        let g = self.github.read(cx);
        let p = self.pilot.read(cx);
        let on_you = g.on_you() + p.count();
        (on_you > 0).then(|| (on_you, g.wants_attention() || p.urgent()))
    }

    /// Whether the GitHub tab is what the user looks at right now.
    fn watching_github(&self, window: &Window) -> bool {
        window.is_window_active()
            && if self.has_projects() {
                matches!(
                    self.env().tabs.get(self.env().active_tab).map(|t| &t.kind),
                    Some(TabKind::GitHub)
                )
            } else {
                self.github_alone
            }
    }

    /// A notice for each new card that needs the user: the agent stopped for
    /// them, or an important pull request waits. None while the tab is in front.
    pub(super) fn on_pilot_event(
        &mut self,
        ev: &PilotEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PilotEvent::New(new) = ev;
        if self.watching_github(window) {
            return;
        }
        if new.len() > NOTICES_AT_ONCE {
            self.notify_from(
                AlertKind::Info,
                "Tu agente",
                &format!("{} cosas te esperan en GitHub", new.len()),
                Some(Open::GitHub(None)),
                None,
                "github",
                cx,
            );
            return;
        }
        for item in new {
            let (title, text) = item.notice();
            let image = self
                .github
                .read(cx)
                .avatar(&item.author)
                .map(std::path::Path::to_path_buf);
            self.notify_from(
                if item.kind == pilot::Kind::NeedsYou {
                    AlertKind::Warning
                } else {
                    AlertKind::Info
                },
                &title,
                &text,
                Some(Open::Pilot(item.id.clone())),
                image.as_deref(),
                "github",
                cx,
            );
        }
    }

    /// "Ver" on an agent notice: the GitHub tab, at the card.
    pub(super) fn open_pilot_notice(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_github_tab(window, cx);
        self.github_view.update(cx, |v, cx| v.reveal_pilot(id, cx));
    }

    pub(super) fn on_github_view_event(
        &mut self,
        ev: &GitHubViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            GitHubViewEvent::Open(url) => self.open_url(url.clone(), window, cx),
            GitHubViewEvent::Ask { title, prompt } => {
                // An agent works in a project: without one, add one first.
                if !self.has_projects() {
                    self.open_picker(window, cx);
                    return;
                }
                let (path, agent) = (self.env().path.clone(), self.env().agent);
                self.start_session_in(
                    &path,
                    agent,
                    SharedString::from(title.clone()),
                    prompt.clone(),
                    true,
                    window,
                    cx,
                );
            }
        }
    }

    /// A notice for each new notification that asks for the user by name or
    /// comes from a watched place. None while the inbox itself is in front.
    pub(super) fn on_github_event(
        &mut self,
        ev: &GitHubEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let GitHubEvent::New(new) = ev;
        if self.watching_github(window) {
            return;
        }
        let watch = self.github.read(cx).watch().to_vec();
        let focus = self.github.read(cx).focus().to_vec();
        let alerting: Vec<&Notification> = new
            .iter()
            .filter(|n| {
                github::alerts(n, &watch) && github::in_focus(&n.repository.full_name, &focus)
            })
            .collect();
        if alerting.len() > NOTICES_AT_ONCE {
            let repos = repo_names(&alerting);
            self.notify_from(
                AlertKind::Info,
                &format!("{} new on GitHub", alerting.len()),
                &repos,
                Some(Open::GitHub(None)),
                None,
                "github",
                cx,
            );
            return;
        }
        for n in alerting {
            let (title, line, image) = self.github.read(cx).notice(n);
            self.notify_from(
                AlertKind::Info,
                &title,
                &line,
                Some(Open::GitHub(Some(n.id.clone()))),
                image.as_deref(),
                "github",
                cx,
            );
        }
    }

    /// "Ver" on a GitHub notice: its pull request or issue in the GitHub tab,
    /// marked read. What the tab doesn't show (a CI run, a release) opens its
    /// page. The inbox instead when the notice was about several, or its
    /// thread is gone.
    pub(super) fn open_github_notice(
        &mut self,
        thread: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = thread.and_then(|id| self.github.read(cx).find(&id).cloned());
        let Some(n) = found else {
            self.open_github_tab(window, cx);
            return;
        };
        self.github.update(cx, |g, cx| g.mark_read(&n.id, cx));
        match crate::github::pr::ItemRef::from_notification(&n) {
            Some(item) => {
                self.open_github_tab(window, cx);
                self.github_view.update(cx, |v, cx| v.reveal(&item, cx));
            }
            None => self.open_url(n.web_url(), window, cx),
        }
    }
}

/// The repositories of `list`, each once: "ArakunHQ/arakun, k/app".
fn repo_names(list: &[&Notification]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for n in list {
        if !names.contains(&n.repository.full_name.as_str()) {
            names.push(&n.repository.full_name);
        }
    }
    names.join(", ")
}
