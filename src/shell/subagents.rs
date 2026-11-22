//! The Session panel: what the session on screen is connected to, beside the
//! chat. The pull request its thread opened or pushed to, with its review and
//! checks, and its subagents, with the transcript of the one opened. It sits
//! between the chat and the Workspace panel, with its own width and its own
//! open flag (stored as the subagents column's, so layouts keep working).

use super::*;
use crate::agent::{SubagentState, Thread, ThreadView, ThreadViewEvent};
use crate::ui::{Tone, Type, Typography as _};

/// The pull request a session is connected to, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
struct PrCard {
    number: u64,
    url: String,
    /// `owner/repo`, for when the title is not known yet.
    repo: String,
    title: Option<String>,
    /// Draft, Open, Merged or Closed; None when nothing read says.
    state: Option<(&'static str, Tone)>,
    review: Option<(&'static str, Tone)>,
    conflicts: bool,
    /// The checks on its branch, and the run that speaks for them.
    checks: Option<(ci::State, u64)>,
}

/// `owner/repo` and the number of a `https://github.com/<owner>/<repo>/pull/<n>`.
fn parse_pr_url(url: &str) -> Option<(String, u64)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if parts.next()? != "pull" {
        return None;
    }
    Some((format!("{owner}/{repo}"), parts.next()?.parse().ok()?))
}

/// What the panel says about a session's pull request: the one its thread
/// linked (`link`), else the open one of the environment's `branch`. The rest
/// comes from what was read of the project's pull requests and runs (`repo`)
/// and of the branch's own pull request (`env_pr`), each when it is the same
/// pull request.
fn pr_card(
    link: Option<&str>,
    branch: &str,
    repo: Option<&ci::Repo>,
    env_pr: Option<&git::PullRequest>,
) -> Option<PrCard> {
    let url = match (link, repo) {
        (Some(url), _) => url.to_string(),
        (None, Some(repo)) => repo.pr_of(branch)?.url.clone(),
        // The project's pull requests are not read (yet): the branch's own
        // read, while it is open, still names it.
        (None, None) => env_pr.filter(|p| p.state == "OPEN")?.url.clone(),
    };
    let (slug, number) = parse_pr_url(&url)?;
    // Open pull requests of this repository only: another one's #12 is not ours.
    let open = repo.and_then(|r| {
        r.prs
            .iter()
            .find(|p| p.number == number && parse_pr_url(&p.url).is_some_and(|(s, _)| s == slug))
    });
    let read = env_pr.filter(|p| p.number == number && p.url == url);
    let title = open.map(|p| p.title.clone());
    // Merged is final: an open entry left in an older list read is stale.
    let open = open.filter(|_| !read.is_some_and(|p| p.state == "MERGED"));
    let state = match (open, read) {
        (Some(p), _) if p.is_draft => Some(("Draft", Tone::Neutral)),
        (Some(_), _) => Some(("Open", Tone::Success)),
        (None, Some(p)) => match p.state.as_str() {
            "MERGED" => Some(("Merged", Tone::Accent)),
            "CLOSED" => Some(("Closed", Tone::Danger)),
            "OPEN" if p.is_draft => Some(("Draft", Tone::Neutral)),
            "OPEN" => Some(("Open", Tone::Success)),
            _ => None,
        },
        (None, None) => None,
    };
    let review = open.and_then(|p| match p.review() {
        ci::Review::Approved => Some(("Approved", Tone::Success)),
        ci::Review::ChangesRequested => Some(("Changes requested", Tone::Warning)),
        ci::Review::Required => Some(("Review required", Tone::Neutral)),
        ci::Review::Draft | ci::Review::None => None,
    });
    // Its own branch when it is open; the environment's when that is the one read.
    let head = open
        .map(|p| p.head_ref_name.as_str())
        .or(read.map(|_| branch));
    let checks = head
        .and_then(|h| repo?.branch_state(h))
        .map(|(s, run)| (s, run.id));
    Some(PrCard {
        number,
        url,
        repo: slug,
        title,
        state,
        review,
        conflicts: open.is_some_and(|p| p.conflicts()),
        checks,
    })
}

/// The checks' state as the panel words it; None for a state that says nothing.
fn checks_label(state: ci::State) -> Option<(&'static str, Tone)> {
    match state {
        ci::State::Passed => Some(("Checks passed", Tone::Success)),
        ci::State::Failed => Some(("Checks failed", Tone::Danger)),
        ci::State::Running | ci::State::Queued => Some(("Checks running", Tone::Info)),
        ci::State::Cancelled | ci::State::Skipped => None,
    }
}

/// A subagent's transcript open in the panel.
pub(super) struct SubagentView {
    /// The session tab it belongs to.
    session: SharedString,
    id: String,
    view: Entity<ThreadView>,
    _events: Subscription,
}

/// What a row of the list shows.
struct Row {
    id: String,
    name: String,
    task: String,
    state: SubagentState,
    secs: u64,
}

fn tone(state: SubagentState) -> Tone {
    match state {
        SubagentState::Running => Tone::Info,
        SubagentState::Completed => Tone::Success,
        SubagentState::Failed => Tone::Danger,
        SubagentState::Cancelled => Tone::Neutral,
    }
}

fn fmt_secs(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// A group's eyebrow in the panel, with how many it holds.
fn section_title(title: &'static str, count: Option<usize>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .px(crate::ui::sp(8.))
        .pt(crate::ui::sp(8.))
        .pb(crate::ui::sp(2.))
        .child(ui::text(
            Type::Overline,
            theme::text_muted(),
            title.to_uppercase(),
        ))
        .when_some(count, |d, n| {
            d.child(ui::text(Type::Caption, theme::text_faint(), n.to_string()))
        })
}

impl Shell {
    pub(super) fn toggle_subagents(&mut self, cx: &mut Context<Self>) {
        // Left out for room, a click brings it back in the panel's place.
        if self.subagents_open && self.side_crowded.get()[1] {
            self.side_prefer = SideCard::Subagents;
            cx.notify();
            return;
        }
        self.subagents_open = !self.subagents_open;
        self.subagents_dismissed = !self.subagents_open;
        if self.subagents_open {
            self.side_prefer = SideCard::Subagents;
        }
        self.save(cx);
        self.sync_subagent_tick(cx);
        cx.notify();
    }

    /// Opens the panel on the list, or on `subagent` of session `session`.
    pub(super) fn open_subagent(
        &mut self,
        session: SharedString,
        subagent: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.subagents_open {
            self.subagents_open = true;
            self.save(cx);
        }
        self.side_prefer = SideCard::Subagents;
        match subagent {
            Some(id) => self.show_subagent(session, &id, window, cx),
            None => self.subagent_view = None,
        }
        self.sync_subagent_tick(cx);
        cx.notify();
    }

    /// A new subagent in a session.
    pub(super) fn subagent_spawned(&mut self, session: &str, cx: &mut Context<Self>) {
        self.session_connected(session, cx);
        self.sync_subagent_tick(cx);
    }

    /// The session got something the panel shows (a subagent, a pull request):
    /// the panel opens for the one on screen, unless the user closed it.
    pub(super) fn session_connected(&mut self, session: &str, cx: &mut Context<Self>) {
        let active = self
            .active_session_id()
            .is_some_and(|a| a.as_ref() == session);
        if active && !self.subagents_open && !self.subagents_dismissed {
            self.subagents_open = true;
            self.save(cx);
        }
        cx.notify();
    }

    fn session_thread(&self, session: &str) -> Option<Entity<Thread>> {
        let env = self.env();
        env.sessions[env.session_index(session)?]
            .live
            .as_ref()
            .map(|l| l.thread.clone())
    }

    fn show_subagent(
        &mut self,
        session: SharedString,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.session_thread(&session) else {
            return;
        };
        let Some((id, child)) = thread
            .read(cx)
            .subagent(id)
            .map(|s| (s.id.clone(), s.thread.clone()))
        else {
            return;
        };
        if self
            .subagent_view
            .as_ref()
            .is_some_and(|v| v.session == session && v.id == id)
        {
            return;
        }
        let child_thread = child.clone();
        let view = cx.new(|cx| ThreadView::new(child, window, cx));
        let (env, sid) = (self.env().path.clone(), session.clone());
        let events = cx.subscribe_in(
            &view,
            window,
            move |this, _, ev: &ThreadViewEvent, window, cx| match ev {
                ThreadViewEvent::OpenFile(file, line) => {
                    this.open_file_beside(&env, file, *line, window, cx)
                }
                ThreadViewEvent::OpenSubagent(id) => {
                    this.open_subagent(sid.clone(), id.clone(), window, cx)
                }
                ThreadViewEvent::AddNote { .. } => {}
                ThreadViewEvent::TellUs { error_event, agent } => this.tell_us(
                    &child_thread,
                    error_event.clone(),
                    agent.clone(),
                    window,
                    cx,
                ),
            },
        );
        self.subagent_view = Some(SubagentView {
            session,
            id,
            view,
            _events: events,
        });
    }

    /// Keeps the elapsed times of running subagents counting while the panel shows them.
    pub(super) fn sync_subagent_tick(&mut self, cx: &mut Context<Self>) {
        if self.subagent_tick.is_some() || !self.subagents_running(cx) {
            return;
        }
        self.subagent_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let going = this.update(cx, |this, cx| {
                    let going = this.subagents_running(cx);
                    if going {
                        cx.notify();
                    } else {
                        this.subagent_tick = None;
                    }
                    going
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn subagents_running(&self, cx: &App) -> bool {
        self.subagents_open
            && self.has_projects()
            && self
                .active_thread(cx)
                .is_some_and(|t| t.read(cx).running_subagents() > 0)
    }

    pub(super) fn render_subagents(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.active_session_id();
        let thread = self.active_thread(cx);
        let opened = self
            .subagent_view
            .as_ref()
            .filter(|v| Some(&v.session) == session.as_ref())
            .and_then(|v| {
                let t = thread.as_ref()?.read(cx);
                let s = t.subagent(&v.id)?;
                Some((v.view.clone(), s.name.clone(), s.state, s.elapsed_secs()))
            });

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(40.))
            .pl(crate::ui::sp(if opened.is_some() { 6. } else { 12. }))
            .pr(crate::ui::sp(6.))
            .gap(crate::ui::sp(6.))
            .map(|d| match &opened {
                Some((_, name, state, _)) => d
                    .child(
                        icon_button("subagent-back", "arrow-left01", 14., "Back to the session")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.subagent_view = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(ui::TEXT_LG))
                            .font_weight(FontWeight::MEDIUM)
                            .child(name.clone()),
                    )
                    .child(ui::badge(state.label(), tone(*state))),
                None => d
                    .child(
                        div()
                            .text_size(px(ui::TEXT_LG))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Session"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .on_mouse_down(MouseButton::Left, |ev, window, _| {
                                crate::ui::drag_window(ev, window)
                            })
                            .when(cfg!(windows), |d| {
                                d.window_control_area(WindowControlArea::Drag)
                            }),
                    ),
            })
            .child(
                icon_button("subagents-close", "cancel01", 14., "Close session panel")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_subagents(cx))),
            );

        let body: AnyElement = match (&opened, &thread) {
            (Some((view, ..)), _) => gpui::AnyView::from(view.clone())
                .cached(gpui::StyleRefinement::default().size_full())
                .into_any_element(),
            (None, None) => ui::empty_state(
                "bot",
                "No session",
                "Open a session to see its pull request and the subagents it runs.",
                None,
            )
            .into_any_element(),
            (None, Some(thread)) => {
                let pr = self.session_pr(thread, cx);
                // Newest first: the one just started is the one to look at.
                let rows: Vec<Row> = thread
                    .read(cx)
                    .subagents()
                    .iter()
                    .rev()
                    .map(|s| Row {
                        id: s.id.clone(),
                        name: s.name.clone(),
                        task: s
                            .task
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                        state: s.state,
                        secs: s.elapsed_secs(),
                    })
                    .collect();
                if rows.is_empty() && pr.is_none() {
                    ui::empty_state(
                        "bot",
                        "Nothing connected yet",
                        "When the session opens or pushes to a pull request, or hands work to a subagent, it shows here while you go on.",
                        None,
                    )
                    .into_any_element()
                } else {
                    let sid = session.clone().unwrap_or_default();
                    let count = rows.len();
                    div()
                        .id("subagent-list")
                        // Its own height, or it grows with its rows and never scrolls.
                        .size_full()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(2.))
                        .p(crate::ui::sp(6.))
                        .overflow_y_scroll()
                        .when_some(pr, |d, pr| {
                            d.child(section_title("Pull request", None))
                                .child(self.pr_section(pr, cx))
                        })
                        .child(section_title("Subagents", (count > 0).then_some(count)))
                        .when(rows.is_empty(), |d| {
                            d.child(div().px(crate::ui::sp(8.)).py(crate::ui::sp(4.)).child(
                                ui::text(
                                    Type::Caption,
                                    theme::text_subtle(),
                                    "None yet. Work the agent hands off shows here.",
                                ),
                            ))
                        })
                        .children(
                            rows.into_iter()
                                .enumerate()
                                .map(|(i, row)| self.subagent_row(i, row, &sid, cx)),
                        )
                        .into_any_element()
                }
            }
        };

        self.side_card()
            .id("subagents-card")
            .map(ui::stop_window_move)
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.subagents_width()))
            .h_full()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }

    /// The pull request of the session `thread` belongs to, from its own link or
    /// its environment's branch.
    fn session_pr(&self, thread: &Entity<Thread>, cx: &App) -> Option<PrCard> {
        let link = thread.read(cx).meta().pr;
        let env = self.env();
        let ci = self.ci.read(cx);
        let repo = ci.repo(&self.project().path);
        pr_card(link.as_deref(), &env.branch, repo, env.pr.as_ref())
    }

    fn pr_section(&self, pr: PrCard, cx: &mut Context<Self>) -> AnyElement {
        let PrCard {
            number,
            url,
            repo,
            title,
            state,
            review,
            conflicts,
            checks,
        } = pr;
        let failed_run = checks.and_then(|(s, id)| (s == ci::State::Failed).then_some(id));
        let checks = checks.and_then(|(s, _)| checks_label(s));
        let open = url.clone();
        div()
            .id("session-pr")
            .flex()
            .flex_col()
            .gap(crate::ui::sp(6.))
            .px(crate::ui::sp(8.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .hover(|d| d.bg(theme::fill_hover()))
            .tooltip(ui::tip("Open the pull request"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(icon("git-pull-request", 14.).text_color(theme::text_subtle()))
                    .child(ui::text(
                        Type::Label,
                        theme::text_subtle(),
                        format!("#{number}"),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_faint())
                            .child(repo),
                    )
                    .when_some(state, |d, (label, tone)| d.child(ui::badge(label, tone))),
            )
            .when_some(title, |d, title| {
                d.child(
                    div()
                        .min_w_0()
                        .line_clamp(2)
                        .ty(Type::Label)
                        .text_color(theme::text())
                        .child(title),
                )
            })
            .when(checks.is_some() || review.is_some() || conflicts, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(crate::ui::sp(4.))
                        .when_some(checks, |d, (label, tone)| {
                            d.child(match failed_run {
                                // A red build opens its run, not the pull request.
                                Some(id) => div()
                                    .id("session-pr-checks")
                                    .tooltip(ui::tip("Open the failed run"))
                                    .child(ui::badge(label, tone))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_runs_tab(Some(id), window, cx);
                                    }))
                                    .into_any_element(),
                                None => ui::badge(label, tone).into_any_element(),
                            })
                        })
                        .when_some(review, |d, (label, tone)| d.child(ui::badge(label, tone)))
                        .when(conflicts, |d| d.child(ui::badge("Conflicts", Tone::Danger))),
                )
            })
            // Through the shell, as a chat link: the GitHub tab when it has the
            // pull request, else the page.
            .on_click(
                cx.listener(move |this, _, window, cx| this.open_url(open.clone(), window, cx)),
            )
            .into_any_element()
    }

    fn subagent_row(
        &self,
        i: usize,
        row: Row,
        session: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Row {
            id,
            name,
            task,
            state,
            secs,
        } = row;
        let session = session.clone();
        let running = state.is_running();
        div()
            .id(("subagent-row", i))
            .flex()
            .items_start()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(8.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .hover(|d| d.bg(theme::fill_hover()))
            .child(
                div().flex_none().pt(crate::ui::sp(6.)).child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .bg(tone(state).color())
                        .when(!running, |d| d.opacity(0.6)),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(2.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Label)
                            .text_color(theme::text())
                            .child(name.clone()),
                    )
                    .when(!task.is_empty() && task != name, |d| {
                        d.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .child(task),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .items_end()
                    .gap(crate::ui::sp(2.))
                    .child(ui::badge(state.label(), tone(state)))
                    .child(ui::text(Type::Caption, theme::text_faint(), fmt_secs(secs))),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_subagent(session.clone(), Some(id.clone()), window, cx)
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://github.com/acme/app/pull/12";

    fn open_pr(number: u64, branch: &str, draft: bool) -> ci::Pr {
        ci::Pr {
            number,
            title: format!("Pull request {number}"),
            head_ref_name: branch.into(),
            url: format!("https://github.com/acme/app/pull/{number}"),
            is_draft: draft,
            review_decision: Some("APPROVED".into()),
            mergeable: Some("CONFLICTING".into()),
        }
    }

    fn failed_run(id: u64, branch: &str) -> ci::Run {
        ci::Run {
            id,
            number: id,
            display_title: String::new(),
            workflow_name: "CI".into(),
            head_branch: branch.into(),
            head_sha: String::new(),
            event: "push".into(),
            status: "completed".into(),
            conclusion: Some("failure".into()),
            created_at: "2026-11-20T10:00:00Z".into(),
            started_at: None,
            updated_at: "2026-11-20T10:00:00Z".into(),
            url: String::new(),
            attempt: 1,
        }
    }

    #[test]
    fn reads_owner_repo_and_number_from_a_pull_request_link() {
        assert_eq!(parse_pr_url(URL), Some(("acme/app".into(), 12)));
        assert_eq!(parse_pr_url("https://github.com/acme/app/issues/12"), None);
        assert_eq!(parse_pr_url("https://example.com/acme/app/pull/12"), None);
    }

    #[test]
    fn a_linked_pull_request_shows_its_review_and_checks() {
        let mut repo = ci::Repo::default();
        repo.prs = vec![open_pr(12, "feat", false)];
        repo.runs = vec![failed_run(7, "feat")];
        // The environment is on another branch: the link decides, not the branch.
        let card = pr_card(Some(URL), "main", Some(&repo), None).unwrap();
        assert_eq!(card.number, 12);
        assert_eq!(card.title.as_deref(), Some("Pull request 12"));
        assert_eq!(card.state, Some(("Open", Tone::Success)));
        assert_eq!(card.review, Some(("Approved", Tone::Success)));
        assert!(card.conflicts);
        assert_eq!(card.checks, Some((ci::State::Failed, 7)));
    }

    #[test]
    fn without_a_link_the_branch_pull_request_shows() {
        let mut repo = ci::Repo::default();
        repo.prs = vec![open_pr(12, "feat", true)];
        let card = pr_card(None, "feat", Some(&repo), None).unwrap();
        assert_eq!(
            (card.number, card.state),
            (12, Some(("Draft", Tone::Neutral)))
        );
        assert_eq!(pr_card(None, "other", Some(&repo), None), None);
        assert_eq!(pr_card(None, "feat", None, None), None);
    }

    #[test]
    fn without_project_pull_requests_the_branch_read_names_an_open_one() {
        let mut read = git::PullRequest {
            number: 12,
            url: URL.into(),
            state: "OPEN".into(),
            is_draft: false,
        };
        let card = pr_card(None, "feat", None, Some(&read)).unwrap();
        assert_eq!(
            (card.number, card.state),
            (12, Some(("Open", Tone::Success)))
        );
        read.state = "MERGED".into();
        assert_eq!(pr_card(None, "feat", None, Some(&read)), None);
        read.state = "CLOSED".into();
        assert_eq!(pr_card(None, "feat", None, Some(&read)), None);
    }

    #[test]
    fn a_merged_read_wins_over_a_stale_open_entry() {
        let mut repo = ci::Repo::default();
        repo.prs = vec![open_pr(12, "feat", false)];
        let merged = git::PullRequest {
            number: 12,
            url: URL.into(),
            state: "MERGED".into(),
            is_draft: false,
        };
        let card = pr_card(Some(URL), "feat", Some(&repo), Some(&merged)).unwrap();
        assert_eq!(card.state, Some(("Merged", Tone::Accent)));
        assert_eq!(card.title.as_deref(), Some("Pull request 12"));
        assert_eq!((card.review, card.conflicts), (None, false));
    }

    #[test]
    fn a_merged_pull_request_says_so_from_the_branch_read() {
        let merged = git::PullRequest {
            number: 12,
            url: URL.into(),
            state: "MERGED".into(),
            is_draft: false,
        };
        let card = pr_card(Some(URL), "feat", None, Some(&merged)).unwrap();
        assert_eq!(card.state, Some(("Merged", Tone::Accent)));
        assert_eq!((card.title, card.review, card.checks), (None, None, None));
        assert_eq!(card.repo, "acme/app");
    }

    #[test]
    fn another_repository_with_the_same_number_is_not_ours() {
        let mut other = open_pr(12, "feat", false);
        other.url = "https://github.com/acme/other/pull/12".into();
        let mut repo = ci::Repo::default();
        repo.prs = vec![other];
        let card = pr_card(Some(URL), "feat", Some(&repo), None).unwrap();
        assert_eq!((card.title, card.state), (None, None));
    }
}
