//! The GitHub tab (the notifications inbox): opening it, its rail badge, what
//! its rows do, and the notices for new notifications that deserve one.

use std::path::PathBuf;

use gpui::{App, Context, Window};

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
            self.note_active_tab(cx);
        } else {
            // The queue doesn't need a project: it shows in the welcome's place.
            self.show_alone(Some(super::home::Alone::GitHub), cx);
        }
        self.github.update(cx, |g, cx| g.opened(cx));
        self.github_view.update(cx, |v, cx| {
            v.unfloat(cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    /// Ends the island's floating sheet once the GitHub tab is in front, by
    /// whatever path it got there.
    pub(super) fn unfloat_github(&mut self, cx: &mut Context<Self>) {
        if self.github_in_front() && self.github_view.read(cx).floating() {
            self.github_view.update(cx, |v, cx| v.unfloat(cx));
        }
    }

    /// The rail badge: how many things wait on the user (the agent's cards
    /// too), and whether any is one the alert rule cares about.
    pub(super) fn github_badge(&self, cx: &App) -> Option<(usize, bool)> {
        let g = self.github.read(cx);
        let p = self.pilot.read(cx);
        // With pilot's board, what it says needs the user, what waits on the
        // user where pilot doesn't look, and the cards that ask something
        // nothing else counts yet, once per pull request. Notices only show
        // in the tab. The cards still make it urgent as before the board.
        let focus = g.focus();
        if let Some(b) = p.board() {
            let cards = || github::board::visible_cards(b, p.shown(), focus);
            let n = github::board::waiting(b, g.queue(), cards());
            let urgent =
                b.verdict.te_necesita > 0 || g.wants_attention() || cards().any(pilot::Item::pops);
            return (n > 0).then_some((n, urgent));
        }
        let on_you = github::board::waiting_without_board(g.queue(), p.shown_in(focus));
        (on_you > 0).then(|| (on_you, g.wants_attention() || p.urgent(focus)))
    }

    /// Whether the GitHub tab is what the user looks at right now.
    fn watching_github(&self, window: &Window) -> bool {
        window.is_window_active() && self.github_in_front()
    }

    /// The pull request or issue at `url`, when it is on the host `gh` reads
    /// and `gh` is there to read it. A link someone typed on another host
    /// stays a web page.
    pub(super) fn github_item(&self, url: &str, cx: &App) -> Option<github::pr::ItemRef> {
        let item = github::pr::ItemRef::from_url_on(url, &github::pr::gh_host())?;
        // The inbox's problem is only known once the tab has polled; before
        // that, `gh_ready` looks for `gh` and a sign-in itself.
        let cant_read = matches!(
            self.github.read(cx).problem(),
            Some(github::Problem::NoGh | github::Problem::SignedOut)
        );
        (!cant_read && github::pr::gh_ready()).then_some(item)
    }

    /// Whether the window shows the GitHub tab, looked at or not.
    fn github_in_front(&self) -> bool {
        if self.has_projects() {
            matches!(self.strip.active().map(|t| &t.kind), Some(TabKind::GitHub))
        } else {
            self.alone == Some(super::home::Alone::GitHub)
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
        // What the tab's focus leaves out doesn't alert; with a board, the
        // cards in a followed pull request's sheet always show.
        let new: Vec<pilot::Item> = {
            let focus = self.github.read(cx).focus();
            match self.pilot.read(cx).board() {
                Some(b) => github::board::visible_cards(b, new.iter(), focus)
                    .cloned()
                    .collect(),
                None => new.iter().filter(|i| i.in_focus(focus)).cloned().collect(),
            }
        };
        if new.is_empty() {
            return;
        }
        if new.len() > NOTICES_AT_ONCE {
            self.notify_from(
                AlertKind::Info,
                "Your agent",
                &format!("{} things wait for you on GitHub", new.len()),
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

    /// A click on an agent notice: its pull request's sheet over the window
    /// in front, the rest dimmed; Esc gives the window back. In the GitHub
    /// tab, the same sheet there. A notice the board doesn't know opens the
    /// tab at its card.
    pub(super) fn open_pilot_notice(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The tab already draws the view: the sheet opens there, not over it.
        let over = !self.github_in_front();
        let shown = self
            .github_view
            .update(cx, |v, cx| v.float_pilot(id, over, cx));
        if !shown || !over {
            self.open_github_tab(window, cx);
            if !shown {
                self.github_view.update(cx, |v, cx| v.reveal_pilot(id, cx));
            }
            return;
        }
        self.github_view.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// The sheet the island opened, over the window: None unless it is up.
    pub(super) fn render_floating_sheet(&self, cx: &App) -> Option<gpui::AnyElement> {
        use gpui::{IntoElement as _, ParentElement as _, Styled as _};
        let floating = self.github_view.read(cx).floating() && !self.github_in_front();
        floating.then(|| {
            gpui::div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(self.github_view.clone())
                .into_any_element()
        })
    }

    pub(super) fn on_github_view_event(
        &mut self,
        ev: &GitHubViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            // "Open on GitHub" asks for the page, not the tab it came from.
            GitHubViewEvent::Open(url) => self.open_page(url.clone(), window, cx),
            // The sheet over this window closed: the window takes the keys back.
            GitHubViewEvent::Closed => {
                // The composer of a session, or what the tab in front takes.
                if self.has_projects() {
                    self.focus_active_content(window, cx);
                } else {
                    window.focus(&self.focus, cx);
                }
                cx.notify();
            }
            GitHubViewEvent::ConnectPilot => self.open_pilot_connect(window, cx),
            GitHubViewEvent::Ask {
                title,
                prompt,
                pull,
            } => {
                // An agent works in a project: without one, add one first.
                if !self.has_projects() {
                    self.open_picker(window, cx);
                    return;
                }
                match pull {
                    Some(on) => {
                        self.ask_on_pull(on.clone(), title.clone(), prompt.clone(), window, cx)
                    }
                    None => self.ask_here(title.clone(), prompt.clone(), window, cx),
                }
            }
        }
    }

    /// A session in the environment in front.
    fn ask_here(
        &mut self,
        title: String,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        self.ask_agent(title.into(), prompt, window, cx);
    }

    /// A session on a pull request, in the environment on its branch: the one
    /// there is, or a new one. Switching the environment in front instead
    /// would take its branch from the sessions working there, and git refuses
    /// a branch another worktree has. With no project of that repository
    /// here, the environment in front, whose agent then says so.
    fn ask_on_pull(
        &mut self,
        on: github::pr::PullTarget,
        title: String,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The project in front first: it is most likely the one.
        let mut roots: Vec<(PathBuf, String)> = Vec::new();
        let order = std::iter::once(self.active_project)
            .chain((0..self.projects.len()).filter(|&i| i != self.active_project));
        for pi in order {
            let p = &self.projects[pi];
            if !p.is_remote() {
                roots.push((p.path.clone(), p.name.to_string()));
            }
        }
        cx.spawn_in(window, async move |this, cx| {
            let target = on.clone();
            let found = cx
                .background_spawn(crate::guard::task(async move { pull_env(&roots, &target) }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| match found {
                Ok(Some((root, path))) => {
                    let Some(pi) = this.projects.iter().position(|p| p.path == root) else {
                        return;
                    };
                    if this.projects[pi].env_index(&path).is_none() {
                        this.projects[pi].environments.push(Environment::new(
                            path.clone(),
                            Some(on.branch.clone()),
                            false,
                        ));
                    }
                    this.refresh_project(pi, cx);
                    if let Some(ei) = this.projects[pi].env_index(&path) {
                        this.select_env(pi, ei, window, cx);
                    }
                    let agent = this.env().agent;
                    this.start_session_in(&path, agent, title.into(), prompt, true, window, cx);
                }
                Ok(None) => this.ask_here(title, prompt, window, cx),
                Err(e) => this.notify(
                    AlertKind::Failure,
                    &format!("Couldn't check out {}", on.branch),
                    &format!("{e}"),
                    None,
                    cx,
                ),
            });
        })
        .detach();
    }

    /// The form that connects arakun-pilot, or changes its settings. A save
    /// or a disconnect has pilot read its settings again at once, not at its
    /// next minute.
    pub(super) fn open_pilot_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::github_view::connect::{PilotDialog, PilotDialogEvent};
        self.menu = None;
        let dialog = cx.new(|cx| PilotDialog::new(window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            |this, _, ev: &PilotDialogEvent, window, cx| {
                if let PilotDialogEvent::Changed = ev {
                    this.pilot.update(cx, |p, cx| p.refresh(cx));
                }
                this.close_modal(window, cx);
            },
        );
        self.modal = Some((Modal::Pilot(dialog), sub));
        cx.notify();
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
            None => self.open_page(n.web_url(), window, cx),
        }
    }
}

/// A link clicked in a terminal: a pull request or issue `gh` can read goes
/// to the shell, which shows it in the GitHub tab. The rest is left to the
/// terminal, which opens it in the desktop browser as before.
pub fn open_terminal_link(url: &str, window: &mut Window, cx: &mut App) -> bool {
    let item = github::pr::ItemRef::from_url_on(url, &github::pr::gh_host())
        .filter(|_| github::pr::gh_ready());
    if item.is_some() {
        window.dispatch_action(
            Box::new(crate::agent::markdown::OpenLink {
                url: url.to_string(),
            }),
            cx,
        );
    }
    item.is_some()
}

/// The project of `on`'s repository among `roots` (folder and name, the
/// first one wins) and the worktree on its branch, made when there is none.
/// None when no project has a remote pointing at the repository.
fn pull_env(
    roots: &[(PathBuf, String)],
    on: &github::pr::PullTarget,
) -> anyhow::Result<Option<(PathBuf, PathBuf)>> {
    for (root, name) in roots {
        let Some(remote) = git::remote_for(root, &on.repo) else {
            continue;
        };
        let trees = git::worktrees(root)?;
        if let Some(tree) = trees
            .into_iter()
            .find(|t| t.branch.as_deref() == Some(on.branch.as_str()))
        {
            return Ok(Some((root.clone(), tree.path)));
        }
        let path = model::worktree_path(name, &on.branch);
        git::add_pull_worktree(root, &path, &remote, on.number, &on.branch, on.track)?;
        return Ok(Some((root.clone(), path)));
    }
    Ok(None)
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
