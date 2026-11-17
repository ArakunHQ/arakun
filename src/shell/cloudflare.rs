//! The Cloudflare tab: opening it and what its buttons do in the window
//! (browser tabs, agent sessions, terminals).

use std::path::PathBuf;

use gpui::{Context, SharedString, Window};

use super::island::Open;
use super::*;
use crate::cloudflare;
use crate::cloudflare_view::CloudflareEvent;
use crate::island::AlertKind;

impl Shell {
    /// Opens (or focuses) the Cloudflare tab of the visible environment.
    pub(super) fn open_cloudflare_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Cloudflare", TabKind::Cloudflare, |k| {
            matches!(k, TabKind::Cloudflare)
        });
        self.note_active_tab(cx);
        let roots: Vec<PathBuf> = self.projects.iter().map(|p| p.path.clone()).collect();
        let agent = self.env().agent;
        self.cloudflare_view.update(cx, |v, cx| {
            v.set_roots(roots);
            v.set_agent(agent, cx);
            v.refresh_if_stale(cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn on_cloudflare_event(
        &mut self,
        ev: &CloudflareEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        let env_path = self.env().path.clone();
        match ev {
            CloudflareEvent::Open(url) => {
                self.push_browser_tab(&env_path, url.clone(), true, true, window, cx);
            }
            CloudflareEvent::Ask(ask) => self.start_ask(ask, window, cx),
            CloudflareEvent::ConnectMcp(agent, state) => {
                // The button is off without Codex, but the state it showed can be old.
                if *agent == model::Agent::Codex && crate::agent::codex_cli().is_none() {
                    self.notify(
                        AlertKind::Warning,
                        "Connect Cloudflare to Codex",
                        "Codex isn't installed yet. Sign in to Codex in Settings to install it.",
                        Some(Open::Cloudflare),
                        cx,
                    );
                    return;
                }
                self.run_in_terminal(
                    &env_path,
                    &cloudflare::mcp_connect_command(*agent, state),
                    true,
                    window,
                    cx,
                );
            }
            CloudflareEvent::Share { port, emails } => {
                let note = match self.run_in_terminal(
                    &env_path,
                    &cloudflare::share_command(*port, emails),
                    true,
                    window,
                    cx,
                ) {
                    Some(_) => (
                        format!(
                            "Sharing localhost:{port}. The link shows in the terminal; close it to stop sharing."
                        ),
                        false,
                    ),
                    None => ("Couldn't open a terminal.".to_string(), true),
                };
                self.cloudflare_view
                    .update(cx, |v, cx| v.shared(note.0, note.1, cx));
            }
            CloudflareEvent::SignIn { install } => {
                let login = cloudflare::access_command("");
                let cmd = match (install, cfg!(windows)) {
                    (false, _) => login,
                    (true, false) => format!("npm install -g cf && {login}"),
                    (true, true) => format!("npm install -g cf; {login}"),
                };
                self.run_in_terminal(&env_path, &cmd, true, window, cx);
            }
            CloudflareEvent::Run(cmd) => {
                self.run_in_terminal(&env_path, cmd, true, window, cx);
            }
            CloudflareEvent::Notify {
                title,
                body,
                critical,
            } => {
                let kind = if *critical {
                    AlertKind::Failure
                } else {
                    AlertKind::Warning
                };
                self.notify(kind, title, body, Some(Open::Cloudflare), cx);
            }
            CloudflareEvent::OpenProject(root) => {
                if let Some(pi) = self.projects.iter().position(|p| &p.path == root) {
                    self.select_env(pi, 0, window, cx);
                }
            }
        }
    }

    /// Starts a session of the agent picked for new sessions for `ask`, in the project that deploys one of its
    /// Workers when one has the wrangler config, else in the visible environment.
    fn start_ask(&mut self, ask: &cloudflare::Ask, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.env().path.clone();
        let deploys = |p: &model::Project| {
            cloudflare::workers_in(&p.path)
                .iter()
                .any(|n| ask.workers.contains(n))
        };
        let current = self
            .projects
            .iter()
            .position(|p| p.env_index(&visible).is_some());
        let target: Option<(usize, PathBuf)> = if ask.workers.is_empty() {
            None
        } else {
            match current.filter(|&pi| deploys(&self.projects[pi])) {
                Some(pi) => Some((pi, visible.clone())),
                None => self
                    .projects
                    .iter()
                    .position(deploys)
                    .and_then(|pi| automations::main_env(&self.projects[pi]).map(|e| (pi, e))),
            }
        };
        let env_path = target.as_ref().map_or(visible.clone(), |(_, e)| e.clone());
        if env_path != visible
            && let Some((pi, _)) = target
            && let Some(ei) = self.projects[pi].env_index(&env_path)
        {
            self.select_env(pi, ei, window, cx);
        }
        let title: SharedString = ask.title.clone().into();
        let agent = self.env().agent;
        self.start_session_in(
            &env_path,
            agent,
            title,
            ask.prompt.clone(),
            true,
            window,
            cx,
        );
    }
}
