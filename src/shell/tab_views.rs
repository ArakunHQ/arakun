//! The views behind the tabs that show one page each (Runs, Memory, GitHub, Linear…),
//! with the subscriptions that tie them to the shell. One view per kind lives
//! for the whole window; the tab only decides when it is drawn.

use gpui::{AppContext, Context, Entity, Subscription, Window};

use super::Shell;
use crate::agent::AgentKind;
use crate::branches_view::BranchesView;
use crate::ci::Ci;
use crate::cloudflare_view::CloudflareView;
use crate::doop_view::DoopView;
use crate::extensions_view::ExtensionsView;
use crate::gallery_view::GalleryView;
use crate::github::GitHub;
use crate::github::pilot::Pilot;
use crate::github_view::GitHubView;
use crate::iso_view::{IsoEvent, IsoView};
use crate::linear_view::LinearView;
use crate::memory_view::{MemoryEvent, MemoryView};
use crate::model::{self, TabKind};
use crate::phone_view::PhoneView;
use crate::posthog_view::PostHogView;
use crate::runs_view::RunsView;
use crate::servers_view::ServersView;
use crate::sidebar::{OptionalTabs, View};
use crate::usage_view::{UsageEvent, UsageView};

pub(super) struct TabViews {
    pub runs: Entity<RunsView>,
    pub memory: Entity<MemoryView>,
    pub cloudflare: Entity<CloudflareView>,
    pub posthog: Entity<PostHogView>,
    pub linear: Entity<LinearView>,
    pub doop: Entity<DoopView>,
    pub branches: Entity<BranchesView>,
    pub github: Entity<GitHubView>,
    pub extensions: Entity<ExtensionsView>,
    pub phone: Entity<PhoneView>,
    pub servers: Entity<ServersView>,
    pub usage: Entity<UsageView>,
    pub gallery: Entity<GalleryView>,
    pub iso: Entity<IsoView>,
    _subs: Vec<Subscription>,
}

impl TabViews {
    pub fn new(
        ci: &Entity<Ci>,
        github: &Entity<GitHub>,
        pilot: &Entity<Pilot>,
        extensions: OptionalTabs,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Self {
        let runs = cx.new(|cx| RunsView::new(ci.clone(), cx));
        let memory = cx.new(MemoryView::new);
        let cloudflare = cx.new(CloudflareView::new);
        let posthog = cx.new(PostHogView::new);
        let linear = cx.new(|cx| LinearView::new(window, cx));
        let doop = cx.new(DoopView::new);
        let branches = cx.new(BranchesView::new);
        let github = cx.new(|cx| GitHubView::new(github.clone(), pilot.clone(), cx));
        let extensions = cx.new(|cx| ExtensionsView::new(extensions, cx));
        let phone = cx.new(PhoneView::new);
        let servers = cx.new(ServersView::new);
        let usage = cx.new(UsageView::new);
        let gallery = cx.new(GalleryView::new);
        let iso = cx.new(IsoView::new);
        let subs = vec![
            cx.subscribe_in(&runs, window, |this, _, ev, window, cx| {
                this.on_runs_event(ev, window, cx)
            }),
            cx.subscribe_in(&memory, window, |this, _, ev, window, cx| match ev {
                MemoryEvent::Edit(path) => this.edit_in_terminal(path, None, window, cx),
            }),
            cx.subscribe_in(&cloudflare, window, |this, _, ev, window, cx| {
                this.on_cloudflare_event(ev, window, cx)
            }),
            // Open, its pages are tabs the shell draws: their counts follow the view.
            cx.observe(&cloudflare, |this, _, cx| {
                if this.shows_subnav_view(View::Cloudflare) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&posthog, window, |this, _, ev, window, cx| {
                this.on_posthog_event(ev, window, cx)
            }),
            cx.subscribe_in(&linear, window, |this, _, ev, window, cx| {
                this.on_linear_event(ev, window, cx)
            }),
            cx.subscribe_in(&doop, window, |this, _, ev, window, cx| {
                this.on_doop_event(ev, window, cx)
            }),
            cx.subscribe_in(&branches, window, |this, _, ev, window, cx| {
                this.on_branches_event(ev, window, cx)
            }),
            cx.subscribe_in(&github, window, |this, _, ev, window, cx| {
                this.on_github_view_event(ev, window, cx)
            }),
            cx.subscribe_in(&extensions, window, |this, _, ev, window, cx| {
                this.on_extensions_event(ev, window, cx)
            }),
            cx.observe(&extensions, |this, _, cx| {
                if this.shows_subnav_view(View::Marketplace) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&phone, window, |this, _, ev, window, cx| {
                this.on_phone_event(ev, window, cx)
            }),
            cx.subscribe_in(&servers, window, |this, _, ev, window, cx| {
                this.on_servers_event(ev, window, cx)
            }),
            cx.subscribe_in(&usage, window, |this, _, ev, window, cx| match ev {
                UsageEvent::OpenAccount => this.open_account(window, cx),
                UsageEvent::ClaudeSignIn => {
                    if let Some(cmd) = crate::agent::login_command(AgentKind::Claude) {
                        this.spawn_terminal_command(Some(cmd), true, window, cx);
                    }
                }
                UsageEvent::PlanUpdated => {
                    this.check_limit_alerts(cx);
                    this.phone_usage_changed(cx);
                }
                UsageEvent::Open(place) => this.open_usage_place(place, window, cx),
            }),
            // New scene and Change in the Isometric tab start sessions.
            cx.subscribe_in(&iso, window, |this, _, ev, window, cx| match ev {
                IsoEvent::Ask { title, prompt } => {
                    let env = this.env().path.clone();
                    this.start_session_in(
                        &env,
                        model::default_agent(),
                        title.clone().into(),
                        prompt.clone(),
                        true,
                        window,
                        cx,
                    );
                }
            }),
        ];
        Self {
            runs,
            memory,
            cloudflare,
            posthog,
            linear,
            doop,
            branches,
            github,
            extensions,
            phone,
            servers,
            usage,
            gallery,
            iso,
            _subs: subs,
        }
    }

    /// Focuses the view that draws a `kind` tab, when it is one of these.
    pub fn focus(&self, kind: &TabKind, window: &mut Window, cx: &mut gpui::App) {
        match kind {
            TabKind::Runs => self.runs.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Memory => self.memory.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Cloudflare => self.cloudflare.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::PostHog => self.posthog.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Linear => self.linear.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Doop => self.doop.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Branches => self.branches.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::GitHub => self.github.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Marketplace => self.extensions.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Phone => self.phone.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Servers => self.servers.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Usage => self.usage.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Gallery => self.gallery.update(cx, |v, cx| v.focus(window, cx)),
            TabKind::Isometric => self.iso.update(cx, |v, cx| v.focus(window, cx)),
            _ => {}
        }
    }
}
