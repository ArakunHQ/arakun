//! The GitHub tab (the notifications inbox): opening it, its rail badge, what
//! its rows do, and the notices for new notifications that deserve one.

use gpui::{App, Context, SharedString, Window};

use super::island::Open;
use super::*;
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
        }
        self.github.update(cx, |g, cx| g.opened(cx));
        self.github_view.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// The rail badge: how many things wait on the user, and whether any
    /// unread notification is one the alert rule cares about.
    pub(super) fn github_badge(&self, cx: &App) -> Option<(usize, bool)> {
        let g = self.github.read(cx);
        let on_you = g.on_you();
        (on_you > 0).then(|| (on_you, g.wants_attention()))
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
        let watching = window.is_window_active()
            && if self.has_projects() {
                matches!(
                    self.env().tabs.get(self.env().active_tab).map(|t| &t.kind),
                    Some(TabKind::GitHub)
                )
            } else {
                self.github_alone
            };
        if watching {
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

    /// "Ver" on a GitHub notice: the notification's page, marked read. The
    /// inbox instead when the notice was about several, or its thread is gone.
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
        self.open_url(n.web_url(), window, cx);
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
