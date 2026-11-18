//! Ctrl+K from the shell's side: what the palette lists (sessions of every
//! project, GitHub, runs, branches, views, commands, settings), and doing
//! what it picks. The palette itself is `crate::palette`.

use std::path::{Path, PathBuf};

use gpui::{App, ClipboardItem, Context, SharedString, Window};

use super::island::Open;
use super::*;
use crate::github::pilot;
use crate::github::pr::{self, ItemRef};
use crate::github::queue::{Group, Lane};
use crate::island::AlertKind;
use crate::model::Status;
use crate::palette::item::{Item, Kind, Lead, Preview, Setting, Target};
use crate::palette::{self, Failure, Palette, PaletteEvent, Seed};
use crate::sidebar::View;
use crate::ui::Tint;

fn head(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

/// `owner/name` of the GitHub repository at `dir`, from `arakun.github` or the origin remote.
pub(super) fn repo_slug(dir: &Path) -> Option<String> {
    if let Some(r) = crate::branches::github_repo(dir) {
        return Some(r);
    }
    let out = crate::platform::command("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.split("github.com/").nth(1))?;
    let slug = rest.trim_end_matches(".git").trim_matches('/');
    (slug.split('/').count() == 2).then(|| slug.to_string())
}

fn local_hour() -> usize {
    crate::schedule::local(crate::memory::now()).map_or(12, |t| t.hour.clamp(0, 23) as usize)
}

fn checks_tint(s: pr::CheckState) -> Tint {
    match s {
        pr::CheckState::Passed => Tint::Green,
        pr::CheckState::Failed => Tint::Red,
        pr::CheckState::Running => Tint::Amber,
        pr::CheckState::Skipped => Tint::Neutral,
    }
}

/// Checks as at most `n` bars, each the worst of the checks it stands for.
fn check_buckets(tints: &[Tint], n: usize) -> Vec<Tint> {
    if tints.len() <= n {
        return tints.to_vec();
    }
    let rank = |t: &Tint| match t {
        Tint::Red => 3,
        Tint::Amber => 2,
        Tint::Green => 1,
        _ => 0,
    };
    (0..n)
        .map(|i| {
            let (a, b) = (
                i * tints.len() / n,
                ((i + 1) * tints.len() / n).max(i * tints.len() / n + 1),
            );
            *tints[a..b]
                .iter()
                .max_by_key(|t| rank(t))
                .unwrap_or(&Tint::Neutral)
        })
        .collect()
}

impl Shell {
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.env_menu_open = false;
        self.refresh_repo_pulls(cx);
        // A project may have come or changed its remote since the last read.
        self.read_repo_slugs(cx);
        let seed = self.palette_seed(cx);
        let palette = cx.new(|cx| Palette::new(seed, window, cx));
        let sub = cx.subscribe_in(&palette, window, Self::on_palette_event);
        // What GitHub brings in while it is open shows up in it.
        let weak = palette.downgrade();
        self.palette_watch = Some(cx.observe(&self.github, move |this, _, cx| {
            if let Some(p) = weak.upgrade() {
                let items = this.palette_items(cx);
                p.update(cx, |p, cx| p.set_items(items, cx));
            }
        }));
        self.modal = Some((Modal::Palette(palette), sub));
        cx.notify();
    }

    /// Every open pull request of the projects, not only the user's queue:
    /// `#86` must find a teammate's.
    fn refresh_repo_pulls(&self, cx: &mut Context<Self>) {
        let repos = self.project_repos().into_iter().map(|(_, r)| r).collect();
        self.github
            .update(cx, |g, cx| g.refresh_repo_pulls(repos, cx));
    }

    /// Reads each project's repository in the background into `repo_slugs`,
    /// then shows its pull requests in the open palette.
    fn read_repo_slugs(&mut self, cx: &mut Context<Self>) {
        let mut dirs: Vec<PathBuf> = self.projects.iter().map(|p| p.path.clone()).collect();
        if self.has_projects() {
            dirs.push(self.env().path.clone());
        }
        dirs.sort();
        dirs.dedup();
        self.repo_slugs_read = Some(cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(crate::guard::task_or_default(async move {
                    dirs.into_iter()
                        .map(|d| {
                            let slug = repo_slug(&d);
                            (d, slug)
                        })
                        .collect::<std::collections::HashMap<_, _>>()
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.repo_slugs == read {
                    return;
                }
                this.repo_slugs = read;
                this.refresh_repo_pulls(cx);
                if let Some((Modal::Palette(p), _)) = &this.modal {
                    let p = p.clone();
                    let repo = this.visible_repo();
                    let repos = this.project_repos();
                    let items = this.palette_items(cx);
                    p.update(cx, |p, cx| p.set_repos(repo, repos, items, cx));
                }
            });
        }));
    }

    /// (project name, `owner/name`) of every project that lives on GitHub.
    /// Only reads `repo_slugs`: this runs on every GitHub update.
    pub(super) fn project_repos(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .projects
            .iter()
            .filter_map(|p| {
                let slug = self.repo_slugs.get(&p.path).cloned().flatten()?;
                Some((p.name.to_string(), slug))
            })
            .collect();
        out.sort();
        out.dedup_by(|a, b| a.1 == b.1);
        out
    }

    /// `owner/name` of the visible environment, or else of its project.
    fn visible_repo(&self) -> Option<String> {
        if !self.has_projects() {
            return None;
        }
        let slug = |p: &Path| self.repo_slugs.get(p).cloned().flatten();
        slug(&self.env().path).or_else(|| slug(&self.project().path))
    }

    fn on_palette_event(
        &mut self,
        palette: &Entity<Palette>,
        ev: &PaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            PaletteEvent::FindPull { number, repos } => {
                for repo in repos {
                    let r = ItemRef {
                        repo: repo.clone(),
                        number: *number,
                        kind: pr::ItemKind::Pull,
                    };
                    self.github.update(cx, |g, cx| g.fetch_item(r, cx));
                }
            }
            PaletteEvent::Run { target, keep } => {
                if *keep {
                    self.run_palette_target(target.clone(), window, cx);
                    let items = self.palette_items(cx);
                    palette.update(cx, |p, cx| p.set_items(items, cx));
                } else {
                    self.close_modal(window, cx);
                    self.modal = None;
                    self.run_palette_target(target.clone(), window, cx);
                }
            }
            PaletteEvent::Bulk { op, targets } => {
                let targets = targets.clone();
                let op = *op;
                self.close_modal(window, cx);
                self.modal = None;
                match op {
                    palette::Bulk::Summarize => {
                        let titles: Vec<String> = targets
                            .iter()
                            .filter_map(|t| match t {
                                Target::Session(pi, ei, id) => self
                                    .projects
                                    .get(*pi)
                                    .and_then(|p| p.environments.get(*ei))
                                    .and_then(|e| e.sessions.iter().find(|s| s.id == *id))
                                    .map(|s| format!("- {} ({})", s.title, s.id)),
                                _ => None,
                            })
                            .collect();
                        if self.has_projects() {
                            let (path, agent) = (self.env().path.clone(), self.env().agent);
                            let prompt = format!(
                                "Summarize what these Arakun sessions did and what is left, one short paragraph each. Read them with `arakun sessions show <id>`.\n{}",
                                titles.join("\n")
                            );
                            self.start_session_in(
                                &path,
                                agent,
                                "Summary of sessions".into(),
                                prompt,
                                true,
                                window,
                                cx,
                            );
                        }
                    }
                    _ => {
                        for t in targets {
                            self.run_palette_target(t, window, cx);
                        }
                    }
                }
            }
            PaletteEvent::KeepChatting { question, answer } => {
                self.close_modal(window, cx);
                self.modal = None;
                if self.has_projects() {
                    let (path, agent) = (self.env().path.clone(), self.env().agent);
                    let prompt = format!(
                        "{question}\n\nA quick answer said:\n> {}\n\nGo deeper: check it against the code and the logs.",
                        answer.replace('\n', "\n> ")
                    );
                    self.start_session_in(
                        &path,
                        agent,
                        head(question, 60).into(),
                        prompt,
                        true,
                        window,
                        cx,
                    );
                }
            }
            PaletteEvent::AddRule(draft) => {
                let dir = self.has_projects().then(|| self.env().path.clone());
                // A CI rule watches the project it was made in, the same one
                // its commands run in.
                let mut draft = draft.clone();
                if let palette::rules::Trigger::CiFailed { project, .. } = &mut draft.trigger {
                    *project = self.has_projects().then(|| self.project().path.clone());
                }
                let draft = &draft;
                let now = crate::memory::now();
                // The same rule twice would act twice: keep the one already waiting.
                let twin = self
                    .rules
                    .waiting()
                    .any(|r| r.trigger == draft.trigger && r.steps == draft.steps);
                let id = if twin {
                    0
                } else {
                    self.rules.add(draft.clone(), dir, now)
                };
                // Unsaved, it still waits here, but it can't run until it is saved.
                let _ = self.save_rules(cx);
                self.close_modal(window, cx);
                self.modal = None;
                self.notify_from(
                    AlertKind::Info,
                    if matches!(draft.trigger, palette::rules::Trigger::At { .. }) {
                        "Reminder set"
                    } else {
                        "Automation on"
                    },
                    &match draft.trigger {
                        palette::rules::Trigger::At { at } => {
                            format!("{} · at {}", draft.title, palette::rules::clock(at))
                        }
                        _ => format!("{} · expires in {} h", draft.title, draft.hours),
                    },
                    None,
                    None,
                    "arakun",
                    cx,
                );
                let _ = id;
                self.check_rules(window, cx);
                self.refresh_automations(cx);
            }
            PaletteEvent::Merge { item, head } => {
                let item = item.clone();
                // The head the confirmation showed, not the one GitHub shows now:
                // a push since the person said yes is not merged unseen.
                let head = head.clone();
                let weak = palette.downgrade();
                cx.spawn_in(window, async move |this, cx| {
                    let it = item.clone();
                    let result = if head.is_empty() {
                        Err(crate::github::Problem::Other(
                            "its latest commit isn't loaded yet; open it in the GitHub tab".into(),
                        ))
                    } else {
                        cx.background_spawn(crate::guard::task(async move {
                            pr::merge(&it, pr::MergeMethod::Squash, &head)
                        }))
                        .await
                    };
                    let (text, ok) = match result {
                        Ok(()) => (format!("Merged #{}", item.number), true),
                        Err(p) if pr::refused_read_only(&p) => (
                            "Read-only in this Arakun: nothing was sent to GitHub.".to_string(),
                            false,
                        ),
                        Err(p) => (format!("Could not merge: {}", p.text()), false),
                    };
                    let _ = weak.update(cx, |p, cx| p.show_note(text.clone(), ok, cx));
                    let _ = this.update(cx, |this, cx| {
                        if ok {
                            this.github
                                .update(cx, |g, cx| g.fetch_item(item.clone(), cx));
                        }
                    });
                })
                .detach();
            }
            PaletteEvent::CloudTask(req) => {
                let req = req.clone();
                let weak = palette.downgrade();
                cx.spawn_in(window, async move |this, cx| {
                    // Read again: the admin token is only taken when a task goes.
                    let result = cx
                        .background_spawn(crate::guard::task(async move {
                            let cfg = pilot::load_config()
                                .ok_or("couldn't find arakun-pilot's client.json")?;
                            pilot::send_task(&cfg, &req)
                        }))
                        .await;
                    let shown = weak
                        .update_in(cx, |p, window, cx| p.cloud_sent(result.clone(), window, cx));
                    // The palette closed meanwhile: the island says it instead.
                    if shown.is_err() {
                        let _ = this.update(cx, |this, cx| {
                            let (kind, text) = match &result {
                                Ok(id) => (AlertKind::Info, palette::cloud::sent_note(id)),
                                Err(e) => (AlertKind::Warning, format!("Not sent: {e}")),
                            };
                            this.notify_from(kind, "Cloud agent", &text, None, None, "github", cx);
                        });
                    }
                })
                .detach();
            }
            PaletteEvent::PinsChanged => {
                self.pins = palette::pins::Pins::load();
                cx.notify();
            }
            PaletteEvent::Dismissed => {
                self.close_modal(window, cx);
                self.modal = None;
            }
        }
    }

    /// Opens pin `n` (0-based) from anywhere: the sidebar, Ctrl+Alt+1..9.
    pub(super) fn open_pinned(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pin) = self.pins.items.get(n).cloned() else {
            return;
        };
        if pin.kind == "file" {
            self.run_palette_target(
                Target::File {
                    path: PathBuf::from(&pin.id),
                    line: None,
                },
                window,
                cx,
            );
            return;
        }
        let key = pin.key();
        if let Some(item) = self.palette_items(cx).into_iter().find(|i| i.key == key) {
            self.run_palette_target(item.target, window, cx);
        }
    }

    pub(super) fn palette_seed(&self, cx: &App) -> Seed {
        let items = self.palette_items(cx);
        let has = self.has_projects();
        let root = has.then(|| self.env().path.clone());
        let repo = self.visible_repo();
        let repos = self.project_repos();
        let branches: Vec<String> = if has {
            self.branches_view
                .read(cx)
                .branch_list()
                .into_iter()
                .map(|(n, _)| n)
                .chain(
                    self.projects
                        .iter()
                        .flat_map(|p| p.environments.iter().map(|e| e.branch.to_string())),
                )
                .collect()
        } else {
            Vec::new()
        };
        let sessions = self
            .projects
            .iter()
            .flat_map(|p| p.environments.iter())
            .flat_map(|e| e.sessions.iter())
            .map(|s| (s.id.to_string(), s.title.to_string()))
            .collect();
        Seed {
            items,
            project: if has {
                self.projects[self.active_project].name.to_string()
            } else {
                String::new()
            },
            branch: if has {
                self.env().branch.to_string()
            } else {
                String::new()
            },
            root,
            repo,
            repos,
            branches,
            sessions,
            read_only: crate::github::read_only(),
            heads: self
                .github
                .read(cx)
                .all_details()
                .filter_map(|(r, d)| match d {
                    crate::github::queue::Details::Pull(p) if !p.head_ref_oid.is_empty() => {
                        Some((r, p.head_ref_oid.clone()))
                    }
                    _ => None,
                })
                .collect(),
            agent: if has {
                self.env().agent.label().to_string()
            } else {
                "an agent".into()
            },
            failures: self.palette_failures(cx),
            clipboard: cx.read_from_clipboard().and_then(|c| c.text()),
            hour: local_hour(),
            pilot: self
                .pilot
                .read(cx)
                .config()
                .map(|c| palette::cloud::PilotSeed {
                    pull: self.github_view.read(cx).own_open_pull(cx),
                    admin: c.admin_token.is_some(),
                }),
        }
    }

    fn palette_failures(&self, cx: &App) -> Vec<Failure> {
        if !self.has_projects() {
            return Vec::new();
        }
        let root = self.projects[self.active_project].path.clone();
        let ci = self.ci.read(cx);
        let Some(repo) = ci.repo(&root) else {
            return Vec::new();
        };
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for run in &repo.runs {
            if !seen.insert(run.head_branch.clone()) || run.state() != crate::ci::State::Failed {
                continue;
            }
            let log = match repo.detail(run.id) {
                Some(crate::ci::Load::Ready(d)) => d
                    .failure
                    .as_ref()
                    .map(|f| {
                        f.lines
                            .iter()
                            .rev()
                            .take(30)
                            .rev()
                            .map(|l| l.text.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            out.push(Failure {
                branch: run.head_branch.clone(),
                what: format!(
                    "{} #{} failed {}",
                    run.workflow_name,
                    run.number,
                    crate::ci::parse_time(&run.updated_at)
                        .map(|t| crate::memory::ago(t, crate::memory::now()))
                        .unwrap_or_default()
                ),
                log,
                root: root.clone(),
                id: run.id,
            });
        }
        out
    }

    /// Everything the palette lists, before any typing.
    pub(super) fn palette_items(&self, cx: &App) -> Vec<Item> {
        let mut items = Vec::new();
        self.palette_sessions(cx, &mut items);
        self.palette_github(cx, &mut items);
        self.palette_runs(cx, &mut items);
        self.palette_places(cx, &mut items);
        self.palette_views(&mut items);
        self.palette_commands(&mut items, cx);
        self.palette_settings(&mut items);
        // CoderSpace tasks waiting on the person, as the old switcher listed them.
        #[cfg(feature = "coderlabs")]
        if self.has_projects() {
            let cs = self.coderspace.read(cx);
            for t in cs.pending() {
                items.push(
                    Item::new(
                        format!("task:{}", t.id),
                        Kind::Command,
                        format!("{} {}", t.key, t.title).trim().to_string(),
                        Target::Task(t.id.clone()),
                    )
                    .sub(format!(
                        "Task · {} · {}",
                        cs.project_name(&t.project).unwrap_or("CoderSpace"),
                        t.status.label()
                    ))
                    .icon("checkmark-circle02", Tint::Blue)
                    .section("Tasks"),
                );
            }
        }
        items
    }

    fn palette_sessions(&self, cx: &App, items: &mut Vec<Item>) {
        let now = crate::memory::now();
        let many = self.projects.len() > 1;
        for (pi, p) in self.projects.iter().enumerate() {
            for (ei, e) in p.environments.iter().enumerate() {
                let visible = pi == self.active_project && ei == p.active_env;
                for s in &e.sessions {
                    let thread = s.live.as_ref().map(|l| l.thread.read(cx));
                    let doing = thread
                        .and_then(|t| t.status_detail())
                        .map(|d| d.to_string());
                    let cost = thread.and_then(|t| t.usage().cost.clone()).map(|(c, cur)| {
                        if cur == "USD" {
                            format!("${c:.2}")
                        } else {
                            format!("{c:.2} {cur}")
                        }
                    });
                    let (lead, section, boost) = match s.status {
                        Status::Working => (Lead::Working, "Running", 40),
                        Status::NeedsInput => (Lead::Dot(Tint::Amber, true), "Needs you", 80),
                        Status::Error => (Lead::Dot(Tint::Red, false), "Needs you", 60),
                        Status::Idle => (Lead::Idle, "Sessions", 0),
                    };
                    let place = if e.is_checkout || e.name == p.name {
                        p.name.to_string()
                    } else {
                        format!("{} · {}", p.name, e.name)
                    };
                    let sub = match (s.status, &doing) {
                        (Status::Working, Some(d)) => d.clone(),
                        (Status::NeedsInput, _) => "needs your decision".into(),
                        _ if many || !visible => place.clone(),
                        _ => p.name.to_string(),
                    };
                    let old = now.saturating_sub(s.updated) > 3 * 86_400;
                    let mut item = Item::new(
                        format!("session:{}", s.id),
                        Kind::Session,
                        s.title.clone(),
                        Target::Session(pi, ei, s.id.clone()),
                    )
                    .sub(sub)
                    .meta(crate::model::age(s.updated).to_string())
                    .lead(lead)
                    .section(section)
                    .extra(format!(
                        "{} {} {} {}",
                        p.name,
                        e.name,
                        e.branch,
                        s.agent.label()
                    ))
                    .boost(boost + if visible { 15 } else { 0 })
                    .preview(Preview::Session {
                        place: format!("{} · {} · {}", p.name, e.branch, s.agent.label()),
                        agent: s.agent.label().to_string(),
                        status: s.status,
                        age: crate::model::age(s.updated).to_string(),
                        doing: doing.clone(),
                        added: s.added,
                        removed: s.removed,
                        files: None,
                        cost,
                        quote: None,
                    });
                    item.dim = s.status == Status::Idle && old;
                    items.push(item);
                }
            }
        }
    }

    fn palette_github(&self, cx: &App, items: &mut Vec<Item>) {
        let gh = self.github.read(cx);
        let me = gh.me().unwrap_or("").to_string();
        let projects: std::collections::HashMap<String, String> = self
            .project_repos()
            .into_iter()
            .map(|(name, repo)| (repo.to_ascii_lowercase(), name))
            .collect();
        let project_of = |repo: &str| projects.get(&repo.to_ascii_lowercase()).map(String::as_str);
        let mut listed: std::collections::HashSet<String> = Default::default();
        let mut people: std::collections::BTreeMap<String, (usize, usize, usize)> =
            Default::default();
        for (entry, placed) in &gh.queue().rows {
            let Some(r) = entry.item.clone() else {
                continue;
            };
            if placed.group == Group::Done {
                continue;
            }
            let author = if placed.author.is_empty() {
                entry.author.clone()
            } else {
                placed.author.clone()
            };
            let on_you = placed.lane == Lane::OnYou;
            let pull = match gh.details(&r) {
                Some(crate::github::queue::Details::Pull(p)) => Some(p.as_ref().clone()),
                _ => None,
            };
            if !author.is_empty() && !author.eq_ignore_ascii_case(&me) {
                let e = people.entry(author.clone()).or_default();
                e.2 += 1;
                if on_you {
                    e.0 += 1;
                }
            }
            if let Some(p) = &pull
                && p.author.login.eq_ignore_ascii_case(&me)
                && let pr::WaitingOn::Reviewers(who) = pr::waiting_on(p, &me)
            {
                for w in who {
                    people.entry(w).or_default().1 += 1;
                }
            }
            let (pill, tint) = match (placed.group, placed.checks) {
                (Group::NeedsReview, _) => (Some("review"), Tint::Amber),
                (_, Some(pr::CheckState::Failed)) => (Some("checks failed"), Tint::Red),
                (Group::Ready, _) => (Some("ready"), Tint::Green),
                _ => (None, Tint::Neutral),
            };
            let preview = match &pull {
                Some(p) => {
                    let sum = pr::checks_summary(p);
                    let waiting = pr::waiting_on(p, &me);
                    let waiting_text = match &waiting {
                        pr::WaitingOn::You(_) => Some(("Waiting on you".to_string(), true)),
                        pr::WaitingOn::Nobody => None,
                        w => Some((crate::github::queue::waiting_text(w), false)),
                    };
                    Preview::Pull {
                        repo: r.repo.clone(),
                        number: r.number,
                        author: author.clone(),
                        avatar: gh.avatar(&author).map(Path::to_path_buf),
                        waiting: waiting_text,
                        checks: (sum.total() > 0).then(|| {
                            (
                                sum.passed,
                                sum.total(),
                                check_buckets(
                                    &sum.checks
                                        .iter()
                                        .map(|c| checks_tint(c.state))
                                        .collect::<Vec<_>>(),
                                    6,
                                ),
                            )
                        }),
                        merge: Some(match p.mergeable.as_str() {
                            "CONFLICTING" => {
                                (format!("Conflicts with {}", p.base_ref_name), Tint::Red)
                            }
                            "MERGEABLE" if p.review_decision == "REVIEW_REQUIRED" => {
                                ("Needs an approval".into(), Tint::Amber)
                            }
                            "MERGEABLE" => ("Can be merged".into(), Tint::Green),
                            _ => ("Checking…".into(), Tint::Neutral),
                        }),
                        changes: Some((p.additions, p.deletions, p.changed_files)),
                        opened: crate::ci::parse_time(&p.created_at)
                            .map(|t| crate::memory::ago(t, crate::memory::now()))
                            .unwrap_or_default(),
                        diff: p.files.iter().take(6).map(|f| f.path.clone()).collect(),
                    }
                }
                None => Preview::Pull {
                    repo: r.repo.clone(),
                    number: r.number,
                    author: author.clone(),
                    avatar: gh.avatar(&author).map(Path::to_path_buf),
                    waiting: on_you.then(|| ("Waiting on you".to_string(), true)),
                    checks: None,
                    merge: None,
                    changes: placed.changes.map(|(a, d)| (a, d, 0)),
                    opened: String::new(),
                    diff: Vec::new(),
                },
            };
            listed.insert(crate::github::queue::key_of(&r));
            let mut item = Item::new(
                format!("pull:{}", crate::github::queue::key_of(&r)),
                Kind::Pull,
                entry.title.clone(),
                Target::Pull(r.clone()),
            )
            .repo_tags(&r.repo, project_of(&r.repo))
            .meta(format!("#{}", r.number))
            .lead(if author.is_empty() {
                Lead::Icon("git-pull-request", Tint::Green)
            } else {
                Lead::Avatar(author.clone(), gh.avatar(&author).map(Path::to_path_buf))
            })
            .section(if on_you { "Needs you" } else { "Pull requests" })
            .extra(format!(
                "{} #{} {} {}",
                r.repo, r.number, author, placed.sub
            ))
            .boost(if on_you { 70 } else { 10 })
            .preview(preview);
            if let Some(p) = pill {
                item = item.pill(p, tint);
            }
            items.push(item);
        }
        // Everyone's open pull requests in the projects' repositories, and any
        // asked for by number, after the user's own queue.
        let mut others: Vec<(ItemRef, String, String, bool, String)> = gh
            .repo_pulls()
            .map(|(repo, p)| {
                let r = ItemRef {
                    repo: repo.to_string(),
                    number: p.number,
                    kind: pr::ItemKind::Pull,
                };
                (
                    r,
                    p.title.clone(),
                    p.author.login.clone(),
                    p.is_draft,
                    "open".to_string(),
                )
            })
            .collect();
        for (r, d) in gh.all_details() {
            if let crate::github::queue::Details::Pull(p) = d {
                let state = if p.merged() {
                    "merged"
                } else if p.open() {
                    "open"
                } else {
                    "closed"
                };
                others.push((
                    r,
                    p.title.clone(),
                    p.author.login.clone(),
                    p.is_draft,
                    state.into(),
                ));
            }
        }
        for (r, title, author, draft, state) in others {
            if !listed.insert(crate::github::queue::key_of(&r)) {
                continue;
            }
            let short = r.repo.rsplit('/').next().unwrap_or(&r.repo).to_string();
            let mut item = Item::new(
                format!("pull:{}", crate::github::queue::key_of(&r)),
                Kind::Pull,
                title,
                Target::Pull(r.clone()),
            )
            .repo_tags(&r.repo, project_of(&r.repo))
            .meta(format!("#{}", r.number))
            .sub(format!("{short} · {author}"))
            .lead(if author.is_empty() {
                Lead::Icon("git-pull-request", Tint::Green)
            } else {
                Lead::Avatar(author.clone(), gh.avatar(&author).map(Path::to_path_buf))
            })
            .section("Pull requests")
            .extra(format!("{} #{} {} {}", r.repo, r.number, author, short))
            .preview(Preview::Pull {
                repo: r.repo.clone(),
                number: r.number,
                author: author.clone(),
                avatar: gh.avatar(&author).map(Path::to_path_buf),
                waiting: None,
                checks: None,
                merge: None,
                changes: None,
                opened: String::new(),
                diff: Vec::new(),
            });
            item.dim = state != "open";
            if draft {
                item = item.pill("draft", Tint::Neutral);
            } else if state != "open" {
                let tint = if state == "merged" {
                    Tint::Violet
                } else {
                    Tint::Neutral
                };
                item = item.pill(state, tint);
            }
            items.push(item);
        }
        for (login, (for_you, you_owe, theirs)) in people {
            let avatar = gh.avatar(&login).map(Path::to_path_buf);
            let mut item = Item::new(
                format!("person:{login}"),
                Kind::Person,
                login.clone(),
                Target::Nothing,
            )
            .lead(Lead::Avatar(login.clone(), avatar.clone()))
            .section("People")
            .meta(format!("{theirs} open"))
            .preview(Preview::Person {
                login: login.clone(),
                avatar,
                for_you,
                you_owe,
                theirs,
            });
            if for_you > 0 {
                item = item.pill(format!("{for_you} for you"), Tint::Amber);
            }
            items.push(item);
        }
    }

    fn palette_runs(&self, cx: &App, items: &mut Vec<Item>) {
        if !self.has_projects() {
            return;
        }
        let root = self.projects[self.active_project].path.clone();
        let ci = self.ci.read(cx);
        let Some(repo) = ci.repo(&root) else { return };
        let now = crate::memory::now();
        for run in repo.runs.iter().take(20) {
            let state = run.state();
            let (tint, word) = match state {
                crate::ci::State::Failed => (Tint::Red, "failed"),
                crate::ci::State::Passed => (Tint::Green, "passed"),
                crate::ci::State::Running | crate::ci::State::Queued => (Tint::Amber, "running"),
                _ => (Tint::Neutral, state.label()),
            };
            let log: Vec<String> = match repo.detail(run.id) {
                Some(crate::ci::Load::Ready(d)) => d
                    .failure
                    .as_ref()
                    .map(|f| {
                        f.lines
                            .iter()
                            .rev()
                            .take(14)
                            .rev()
                            .map(|l| l.text.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            let ago = crate::ci::parse_time(&run.updated_at)
                .map(|t| crate::memory::ago(t, now))
                .unwrap_or_default();
            let failed = state == crate::ci::State::Failed;
            let mut item = Item::new(
                format!("run:{}", run.id),
                Kind::Run,
                format!("{} · #{}", run.workflow_name, run.number),
                Target::Run {
                    root: root.clone(),
                    id: run.id,
                },
            )
            .sub(run.head_branch.clone())
            .meta(ago.clone())
            .icon(if failed { "alert-circle" } else { "activity01" }, tint)
            .section("Runs")
            .extra(format!(
                "{} {} {}",
                run.display_title, run.head_branch, word
            ))
            .boost(if failed { 5 } else { -20 })
            .preview(Preview::Run {
                title: run.display_title.clone(),
                place: format!(
                    "{} · run #{} · {}",
                    run.workflow_name, run.number, run.head_branch
                ),
                state: {
                    let mut w = word.to_string();
                    if let Some(c) = w.get_mut(0..1) {
                        c.make_ascii_uppercase();
                    }
                    w
                },
                tint,
                ago,
                duration: run.duration().map(crate::ci::duration_label),
                commit: run.short_sha().to_string(),
                log,
            });
            if failed {
                item = item.pill("failed", Tint::Red);
            }
            items.push(item);
        }
    }

    fn palette_places(&self, cx: &App, items: &mut Vec<Item>) {
        let home = crate::platform::home();
        for (pi, p) in self.projects.iter().enumerate() {
            let path = p.path.display().to_string();
            let short = p
                .path
                .strip_prefix(&home)
                .map(|r| format!("~/{}", r.display()))
                .unwrap_or_else(|_| path.clone());
            let env = p.environments.get(p.active_env);
            items.push(
                Item::new(
                    format!("project:{}", p.path.display()),
                    Kind::Project,
                    p.name.clone(),
                    Target::Project(pi),
                )
                .sub(short.clone())
                .meta(env.map(|e| e.branch.to_string()).unwrap_or_default())
                .icon("folder01", Tint::Blue)
                .section("Projects")
                .boost(-10)
                .preview(Preview::Project {
                    path: short,
                    branch: env.map(|e| e.branch.to_string()).unwrap_or_default(),
                    envs: p.environments.len(),
                    sessions: p.environments.iter().map(|e| e.sessions.len()).sum(),
                }),
            );
            for (ei, e) in p.environments.iter().enumerate() {
                if e.is_checkout {
                    continue;
                }
                items.push(
                    Item::new(
                        format!("env:{}", e.path.display()),
                        Kind::Project,
                        format!("{} / {}", p.name, e.name),
                        Target::Env(pi, ei),
                    )
                    .sub(e.branch.to_string())
                    .icon("git-branch", Tint::Green)
                    .section("Projects")
                    .boost(-15)
                    .preview(Preview::Branch {
                        name: format!("{} / {}", p.name, e.name),
                        detail: format!(
                            "Environment on {} with {} sessions.",
                            e.branch,
                            e.sessions.len()
                        ),
                    }),
                );
            }
        }
        if !self.has_projects() {
            return;
        }
        let current = self.env().branch.to_string();
        let ahead: std::collections::HashMap<String, (u64, u64)> = self
            .branches_view
            .read(cx)
            .branch_ahead()
            .into_iter()
            .map(|(n, a, b)| (n, (a, b)))
            .collect();
        let ci = self.ci.read(cx);
        let ci_repo = ci.repo(&self.projects[self.active_project].path);
        for (name, pr) in self.branches_view.read(cx).branch_list() {
            let failed = ci_repo
                .and_then(|r| r.branch_state(&name))
                .is_some_and(|(s, _)| s == crate::ci::State::Failed);
            let is_current = name == current;
            let mut item = Item::new(
                format!("branch:{name}"),
                Kind::Branch,
                name.clone(),
                Target::Branch(name.clone()),
            )
            .icon(
                "git-branch",
                if is_current {
                    Tint::Green
                } else {
                    Tint::Violet
                },
            )
            .section("Branches")
            .boost(-25)
            .preview(Preview::Branch {
                name: name.clone(),
                detail: match &pr {
                    Some((n, t)) => format!("Pull request #{n}: {t}"),
                    None => "No pull request yet.".into(),
                },
            });
            let mut facts: Vec<String> = Vec::new();
            if is_current {
                facts.push("current".into());
            }
            match (&pr, ahead.get(&name)) {
                (Some((n, t)), _) => facts.push(format!("#{n} {}", head(t, 36))),
                (None, Some((a, _))) if *a > 0 => facts.push(format!("{a} ahead")),
                _ => {}
            }
            if failed {
                facts.push("failed run".into());
            }
            item.sub = facts.join(" · ").into();
            if is_current {
                item.boost = -20;
            }
            if let Some((n, t)) = &pr {
                item.extra = format!("#{n} {t}");
            }
            items.push(item);
        }
    }

    fn palette_views(&self, items: &mut Vec<Item>) {
        let has = self.has_projects();
        for &v in View::ALL {
            let target = match v {
                View::Inbox => continue,
                View::Runs => Target::Runs,
                View::Branches => Target::Branches,
                View::GitHub => Target::GitHub,
                #[cfg(feature = "coderlabs")]
                View::Tasks => Target::Tasks,
                #[cfg(feature = "coderlabs")]
                View::Canvas => Target::Canvas,
                v => Target::View(v),
            };
            // GitHub reads GitHub, not a project, so it opens without one.
            if !has
                && !matches!(
                    v,
                    View::Usage | View::Extensions | View::Memory | View::GitHub
                )
            {
                continue;
            }
            let tint = match v {
                View::GitHub => Tint::Amber,
                View::Runs => Tint::Green,
                View::Branches => Tint::Violet,
                View::Memory => Tint::Amber,
                View::Cloudflare => Tint::Amber,
                View::Phone => Tint::Teal,
                View::Servers => Tint::Teal,
                _ => Tint::Blue,
            };
            items.push(
                Item::new(
                    format!("view:{}", v.label().to_lowercase()),
                    Kind::View,
                    v.label(),
                    target,
                )
                .sub(v.blurb())
                .icon(v.icon(), tint)
                .section("Views")
                .boost(-5)
                .preview(Preview::View {
                    blurb: v.blurb().to_string(),
                    stats: Vec::new(),
                }),
            );
        }
    }

    fn palette_commands(&self, items: &mut Vec<Item>, cx: &App) {
        let mut add = |key: &str,
                       icon: &'static str,
                       label: &str,
                       keys: &[&'static str],
                       blurb: &str,
                       target: Target| {
            items.push(
                Item::new(
                    format!("command:{key}"),
                    Kind::Command,
                    label.to_string(),
                    target,
                )
                .icon(icon, Tint::Amber)
                .keys(keys)
                .section("Commands")
                .boost(-5)
                .preview(Preview::Command {
                    blurb: blurb.to_string(),
                    keys: keys.to_vec(),
                    cli: None,
                }),
            );
        };
        if self.has_projects() {
            add(
                "new-session",
                "comment-add01",
                "New session",
                &["Ctrl", "T"],
                "A new agent session in the visible environment.",
                Target::NewSession,
            );
            add(
                "new-env",
                "git-branch",
                "New environment",
                &["Ctrl", "⇧", "N"],
                "A worktree on a new branch, with its own sessions and terminals.",
                Target::NewEnvironment,
            );
            add(
                "new-terminal",
                "computer-terminal01",
                "New terminal",
                &["Ctrl", "⇧", "T"],
                "A terminal in the visible environment.",
                Target::NewTerminal,
            );
            add(
                "new-browser",
                "globe",
                "New browser tab",
                &["Ctrl", "⇧", "B"],
                "A browser tab beside your work.",
                Target::NewBrowserTab,
            );
            add(
                "toggle-dock",
                "layout-bottom",
                "Toggle terminal dock",
                &["Ctrl", "J"],
                "Shows or hides the terminals under the main area.",
                Target::ToggleDock,
            );
            add(
                "toggle-sidebar",
                "sidebar-left",
                "Toggle sidebar",
                &["Ctrl", "B"],
                "Hides the sidebar and the workspace panel, or brings them back.",
                Target::ToggleSidebar,
            );
            add(
                "toggle-panel",
                "layout-align-right",
                "Toggle workspace panel",
                &[],
                "Sessions, files and changes of the environment.",
                Target::TogglePanel,
            );
            add(
                "toggle-subagents",
                "bot",
                "Toggle subagents panel",
                &[],
                "What the agent's helpers are doing.",
                Target::ToggleSubagents,
            );
        }
        add(
            "add-project",
            "folder-add",
            "Add project",
            &["Ctrl", "O"],
            "Open a folder as a project.",
            Target::AddProject,
        );
        add(
            "onboarding",
            "sparkles",
            "Show onboarding",
            &[],
            "The setup from first launch: agents, look, connections and a project.",
            Target::Onboarding,
        );
        #[cfg(feature = "coderlabs")]
        add(
            "tasks",
            "checkmark-circle02",
            "CoderSpace tasks",
            &[],
            "The tasks assigned to you.",
            Target::Tasks,
        );
        add(
            "account",
            "gauge",
            "Factory account and usage",
            &[],
            "Plan, usage and limits of the signed-in agents.",
            Target::Account,
        );
        add(
            "settings",
            "settings01",
            "Settings",
            &["Ctrl", ","],
            "Appearance, agents, the island and more.",
            Target::Settings,
        );
        add(
            "extensions",
            "store01",
            "Marketplace: plugins, skills & MCP",
            &[],
            "Plugins and MCP servers to add, and what the agents load now.",
            Target::Extensions,
        );
        if self.has_projects() {
            add(
                "automations",
                "alarm-clock",
                "Automations",
                &[],
                "Scheduled runs and the rules you made from the palette.",
                Target::View(View::Automations),
            );
        }
        add(
            "signin-claude",
            "square-lock02",
            "Sign in to Claude Code",
            &[],
            "Opens a terminal with Claude Code's login.",
            Target::SignIn(crate::agent::AgentKind::Claude),
        );
        add(
            "signin-droid",
            "square-lock02",
            "Sign in to Droid",
            &[],
            "Opens a terminal with Droid's login.",
            Target::SignIn(crate::agent::AgentKind::Droid),
        );
        add(
            "signin-opencode",
            "square-lock02",
            "Sign in to OpenCode",
            &[],
            "Opens a terminal with OpenCode's login.",
            Target::SignIn(crate::agent::AgentKind::OpenCode),
        );
        add(
            "signin-codex",
            "square-lock02",
            "Sign in to Codex",
            &[],
            "Opens a terminal with Codex's ChatGPT login.",
            Target::SignIn(crate::agent::AgentKind::Codex),
        );
        // The settings pilot read last (every minute, off this thread's
        // drawing): the palette never waits on the disk.
        items.extend(cloud_command(self.pilot.read(cx).config()));
    }

    fn palette_settings(&self, items: &mut Vec<Item>) {
        let rows: [(Setting, &str, &str, &str, bool); 8] = [
            (
                Setting::Redacted,
                "Redacted mode",
                "Settings › Privacy",
                "Hides emails, keys, tokens, pairing codes and addresses, for streams, demos and screen sharing (Ctrl+Shift+H).",
                crate::redact::on(),
            ),
            (
                Setting::Island,
                "Dynamic Island",
                "Settings › Island",
                "A small overlay at the top of the screen with what every agent is doing, and their notices.",
                crate::island::enabled(),
            ),
            (
                Setting::Glass,
                "Glass window",
                "Settings › Appearance",
                "Lets the desktop show through the window's background.",
                theme::glass(),
            ),
            (
                Setting::Arcade,
                "Animated raccoon",
                "Settings › Appearance",
                "The mascot moves with what the agents do.",
                self.arcade,
            ),
            (
                Setting::Dock,
                "Terminal dock",
                "Layout",
                "The terminals under the main area.",
                self.dock_open,
            ),
            (
                Setting::Sidebar,
                "Sidebar",
                "Layout",
                "The rail at the left.",
                self.rail_open,
            ),
            (
                Setting::Panel,
                "Workspace panel",
                "Layout",
                "Sessions, files and changes at the right.",
                self.panel_open,
            ),
            (
                Setting::Subagents,
                "Subagents panel",
                "Layout",
                "What the agent's helpers are doing.",
                self.subagents_open,
            ),
        ];
        for (s, name, path, blurb, on) in rows {
            let mut item = Item::new(
                format!("setting:{s:?}").to_lowercase(),
                Kind::Setting,
                name,
                Target::Setting(s),
            )
            .icon(
                match s {
                    Setting::Redacted => "view-off",
                    Setting::Island => "sparkles",
                    Setting::Glass | Setting::Arcade => "paint-board",
                    Setting::Dock => "layout-bottom",
                    Setting::Sidebar => "sidebar-left",
                    Setting::Panel => "layout-align-right",
                    Setting::Subagents => "bot",
                },
                Tint::Neutral,
            )
            .section("Settings")
            .boost(-30)
            .extra(path)
            .preview(Preview::Setting {
                path: path.to_string(),
                blurb: blurb.to_string(),
                on,
            });
            item.toggle = Some(on);
            items.push(item);
        }
    }

    pub(super) fn run_palette_target(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            Target::Project(pi) if pi < self.projects.len() => self.select_project(pi, window, cx),
            Target::Project(_) | Target::Todo(_) => {}
            Target::Env(pi, ei) => self.select_env(pi, ei, window, cx),
            Target::Session(pi, ei, id) => self.open_session_at(pi, ei, id, window, cx),
            Target::NewSession => self.new_session(window, cx),
            Target::NewEnvironment => self.quick_create_env(self.active_project, window, cx),
            Target::NewTerminal => self.new_terminal(window, cx),
            Target::NewBrowserTab => self.open_browser_tab(String::new(), window, cx),
            Target::AddProject => self.open_picker(window, cx),
            Target::Onboarding => self.start_onboarding(window, cx),
            Target::ToggleDock => self.toggle_dock(window, cx),
            Target::ToggleSidebar => self.toggle_sidebar(cx),
            Target::TogglePanel => self.toggle_panel(cx),
            Target::ToggleSubagents => self.toggle_subagents(cx),
            Target::Account => self.open_account(window, cx),
            Target::Settings => self.open_settings(window, cx),
            Target::Extensions => self.open_extensions_tab(window, cx),
            #[cfg(feature = "coderlabs")]
            Target::Tasks => self.open_tasks_tab(window, cx),
            Target::Runs => self.open_runs_tab(None, window, cx),
            Target::Branches => self.open_branches_tab(window, cx),
            Target::GitHub => self.open_github_tab(window, cx),
            #[cfg(feature = "coderlabs")]
            Target::Canvas => self.open_canvas_tab(None, window, cx),
            Target::Branch(name) => self.reveal_branch(name, window, cx),
            #[cfg(feature = "coderlabs")]
            Target::Task(id) => {
                self.open_tasks_tab(window, cx);
                self.tasks_view.update(cx, |v, cx| v.select_task(&id, cx));
            }
            Target::SignIn(kind) => self.sign_in(kind, window, cx),
            Target::View(View::Automations) => self.open_automations(false, window, cx),
            Target::View(v) => {
                self.open_view(v, gpui::point(gpui::px(0.), gpui::px(0.)), window, cx)
            }
            Target::Pull(item) => {
                self.open_github_tab(window, cx);
                let item2 = item.clone();
                self.github_view.update(cx, |v, cx| v.reveal(&item2, cx));
            }
            Target::Run { id, .. } => self.open_runs_tab(Some(id), window, cx),
            Target::Rerun { root, id } => self.ci.update(cx, |ci, cx| ci.act(&root, id, false, cx)),
            Target::FixRun { root, id } => self.fix_run(&root, id, window, cx),
            Target::File { path, line } => {
                if !self.has_projects() {
                    return;
                }
                let env_path = self.env().path.clone();
                let id = self.open_doc_in(&env_path, TabKind::File { path }, true, cx);
                if let (Some(id), Some(line)) = (id, line) {
                    self.reveal_line(&env_path, id, line as usize, cx);
                }
                self.save(cx);
            }
            Target::Url(url) => self.open_url(url, window, cx),
            Target::Setting(s) => self.flip_setting(s, window, cx),
            Target::Memory => self.open_memory_tab(window, cx),
            Target::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Target::Ask {
                prompt,
                model,
                branch,
            } => self.palette_ask(prompt, model, branch, window, cx),
            Target::FixError(e) => {
                let mut prompt = format!(
                    "Fix this {}:\n\n```\n{}\n```",
                    e.language.trim_start_matches("a "),
                    e.text
                );
                if let Some((p, l)) = &e.location {
                    prompt.push_str(&format!("\n\nIt points at {p}:{l}."));
                }
                self.palette_ask(prompt, None, None, window, cx);
            }
            // A pin of "Send to the cloud agent…": the palette, at its form.
            Target::CloudTask => {
                self.open_palette(window, cx);
                if let Some((Modal::Palette(p), _)) = &self.modal {
                    p.clone().update(cx, |p, cx| p.start_cloud(cx));
                }
            }
            Target::Merge(_) | Target::Nothing => {}
        }
    }

    fn palette_ask(
        &mut self,
        prompt: String,
        model: Option<String>,
        branch: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        let env = branch
            .as_deref()
            .and_then(|b| {
                self.projects[self.active_project]
                    .environments
                    .iter()
                    .find(|e| e.branch.as_ref() == b)
            })
            .map(|e| (e.path.clone(), e.agent));
        let (path, mut agent) = env.unwrap_or_else(|| (self.env().path.clone(), self.env().agent));
        if prompt.trim().is_empty() {
            // A branch with nothing typed: open a fresh session there.
            if let Some(pi) = Some(self.active_project)
                && let Some(ei) = self.projects[pi].env_index(&path)
            {
                self.select_env(pi, ei, window, cx);
            }
            self.new_session(window, cx);
            return;
        }
        let mut extra = Vec::new();
        if let Some(m) = model {
            agent = model::Agent::Claude;
            extra.push((crate::agent::Setting::Model, m));
        }
        let title: SharedString = head(prompt.lines().next().unwrap_or(""), 60).into();
        self.start_session_with(&path, agent, title, prompt, true, extra, window, cx);
    }

    fn flip_setting(&mut self, s: Setting, window: &mut Window, cx: &mut Context<Self>) {
        match s {
            Setting::Redacted => self.toggle_redacted(cx),
            Setting::Island => {
                crate::island::set_enabled(!crate::island::enabled());
                self.sync_island(window, cx);
                self.save(cx);
            }
            Setting::Glass => {
                theme::set_glass(!theme::glass());
                window.set_background_appearance(theme::window_background());
                self.save(cx);
                cx.refresh_windows();
            }
            Setting::Arcade => {
                self.arcade = !self.arcade;
                crate::mascot::set_animated(self.arcade);
                self.save(cx);
                cx.refresh_windows();
            }
            Setting::Dock => self.toggle_dock(window, cx),
            Setting::Sidebar => self.toggle_rail(cx),
            Setting::Panel => self.toggle_panel(cx),
            Setting::Subagents => self.toggle_subagents(cx),
        }
        cx.notify();
    }

    // ---- rules ---------------------------------------------------------------------------

    /// What the rules check against, from GitHub's last poll, CI and the sessions.
    fn rule_facts(&self, cx: &App) -> palette::rules::Facts {
        let gh = self.github.read(cx);
        let me = gh.me().unwrap_or("").to_string();
        let mut facts = palette::rules::Facts::default();
        for rule in self.rules.waiting() {
            if let palette::rules::Trigger::PrChecksPass { repo, number }
            | palette::rules::Trigger::PrMerged { repo, number } = &rule.trigger
            {
                let r = ItemRef {
                    repo: repo.clone(),
                    number: *number,
                    kind: pr::ItemKind::Pull,
                };
                if let Some(crate::github::queue::Details::Pull(p)) = gh.details(&r) {
                    let checks = match pr::checks_summary(p).overall() {
                        Some(pr::CheckState::Passed) => "passed",
                        Some(pr::CheckState::Failed) => "failed",
                        Some(pr::CheckState::Running) => "running",
                        _ => "",
                    };
                    facts.pulls.push(palette::rules::PullFacts {
                        repo: repo.clone(),
                        number: *number,
                        checks: checks.into(),
                        merged: p.merged(),
                        open: p.open(),
                        mergeable: p.mergeable.clone(),
                        review: p.review_decision.clone(),
                        reviewers: match pr::waiting_on(p, &me) {
                            pr::WaitingOn::Reviewers(w) => w,
                            _ => Vec::new(),
                        },
                    });
                }
            }
        }
        let ci = self.ci.read(cx);
        for p in &self.projects {
            if let Some(repo) = ci.repo(&p.path) {
                for run in &repo.runs {
                    if run.state() == crate::ci::State::Failed {
                        facts.failed_runs.push(palette::rules::FailedRun {
                            project: p.path.clone(),
                            branch: run.head_branch.clone(),
                            id: run.id,
                            at: crate::ci::parse_time(&run.updated_at).unwrap_or(0),
                        });
                    }
                }
            }
        }
        for e in self.projects.iter().flat_map(|p| p.environments.iter()) {
            for s in &e.sessions {
                facts
                    .sessions
                    .push((s.id.to_string(), s.status == Status::Working));
            }
        }
        facts
    }

    /// Asks GitHub about the pull requests rules wait on, when it has not yet.
    fn fetch_rule_pulls(&mut self, cx: &mut Context<Self>) {
        let refs: Vec<ItemRef> = self
            .rules
            .waiting()
            .filter_map(|r| match &r.trigger {
                palette::rules::Trigger::PrChecksPass { repo, number }
                | palette::rules::Trigger::PrMerged { repo, number } => Some(ItemRef {
                    repo: repo.clone(),
                    number: *number,
                    kind: pr::ItemKind::Pull,
                }),
                _ => None,
            })
            .collect();
        for r in refs {
            self.github.update(cx, |g, cx| g.fetch_item(r, cx));
        }
    }

    /// Checks every waiting rule and runs those whose moment came.
    pub(super) fn check_rules(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A save that failed earlier (a cancel, a finish) is tried again here.
        if self.rules.unsaved && self.save_rules(cx).is_ok() {
            self.refresh_automations(cx);
        }
        if self.rules.waiting().next().is_none() {
            return;
        }
        let facts = self.rule_facts(cx);
        let now = crate::memory::now();
        let mut fire = Vec::new();
        let mut failed = Vec::new();
        let mut changed = false;
        for rule in self.rules.items.iter_mut() {
            if rule.state != palette::rules::State::Waiting {
                continue;
            }
            let before = rule.note.clone();
            match palette::rules::check(rule, &facts, now) {
                palette::rules::Outcome::Fire => {
                    changed = true;
                    match palette::rules::workdir(rule, |d| d.exists()) {
                        Ok(dir) => {
                            // Done or Failed once its steps ended (`run_rule`);
                            // running, it isn't checked again.
                            rule.state = palette::rules::State::Running;
                            fire.push((rule.clone(), dir));
                        }
                        Err(why) => {
                            rule.note = why.clone();
                            rule.state = palette::rules::State::Failed(why);
                            failed.push(rule.clone());
                        }
                    }
                }
                palette::rules::Outcome::Expired => {
                    rule.state = palette::rules::State::Expired;
                    changed = true;
                }
                palette::rules::Outcome::Wait(_) => changed |= rule.note != before,
            }
        }
        self.rules.tidy(now);
        if changed {
            let ids: Vec<u64> = fire.iter().map(|(r, _)| r.id).collect();
            let was_unsaved = self.rules.unsaved;
            if let Err(why) = self.rules.save_fired(&ids) {
                // Running is not on disk, so nothing runs: after a restart the
                // file would say Waiting and run the steps a second time. The
                // rules stay waiting and the next check tries again.
                fire.clear();
                if !was_unsaved {
                    self.rules_not_saved(&why, cx);
                }
            }
            self.refresh_automations(cx);
        }
        for rule in failed {
            self.notify_from(
                AlertKind::Failure,
                &rule.title,
                &format!("Did not run: {}", rule.note),
                None,
                None,
                "arakun",
                cx,
            );
        }
        for (rule, dir) in fire {
            self.run_rule(rule, dir, window, cx);
        }
    }

    /// Saves the rules, and says so the first time saving fails (the 15 s
    /// checks retry it and would repeat the alert).
    pub(super) fn save_rules(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let was_unsaved = self.rules.unsaved;
        let done = self.rules.save();
        if let Err(why) = &done
            && !was_unsaved
        {
            self.rules_not_saved(why, cx);
        }
        done
    }

    fn rules_not_saved(&self, why: &str, cx: &mut Context<Self>) {
        self.notify_from(
            AlertKind::Failure,
            "Automations not saved",
            &format!("No automation runs until they are saved; Arakun keeps trying. {why}"),
            None,
            None,
            "arakun",
            cx,
        );
    }

    /// The head commit GitHub last showed for a pull request, if it was read.
    fn pull_head(&self, item: &ItemRef, cx: &App) -> Option<String> {
        match self.github.read(cx).details(item) {
            Some(crate::github::queue::Details::Pull(p)) if !p.head_ref_oid.is_empty() => {
                Some(p.head_ref_oid.clone())
            }
            _ => None,
        }
    }

    /// Runs a fired rule's steps in order. A merge is awaited before the
    /// next step starts, and a step that fails stops the rest: a command
    /// after a merge must not run when the merge didn't happen. The rule is
    /// marked done or failed from how the steps went.
    fn run_rule(
        &mut self,
        rule: palette::rules::Rule,
        // From `rules::workdir`: the rule's own folder, checked to exist.
        dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pull = match &rule.trigger {
            palette::rules::Trigger::PrChecksPass { repo, number }
            | palette::rules::Trigger::PrMerged { repo, number } => Some(ItemRef {
                repo: repo.clone(),
                number: *number,
                kind: pr::ItemKind::Pull,
            }),
            _ => None,
        };
        // The head whose checks just passed, read in the same moment as the
        // check: a push after that is not merged.
        let head = pull.as_ref().and_then(|p| self.pull_head(p, cx));
        cx.spawn_in(window, async move |this, cx| {
            // Err holds why it stopped, and whether that was already told.
            let mut result: Result<(), (String, bool)> = Ok(());
            for step in &rule.steps {
                let done = match step {
                    palette::rules::Step::Merge { method } => {
                        match (pull.clone(), head.clone()) {
                            (None, _) => Err(("there is no pull request to merge".into(), false)),
                            (Some(_), None) => {
                                Err(("not merged: its latest commit isn't loaded".into(), false))
                            }
                            (Some(item), Some(head)) => {
                                let method = match method.as_str() {
                                    "merge" => pr::MergeMethod::Merge,
                                    "rebase" => pr::MergeMethod::Rebase,
                                    _ => pr::MergeMethod::Squash,
                                };
                                let it = item.clone();
                                let sent = cx
                                    .background_spawn(crate::guard::task(async move {
                                        pr::merge(&it, method, &head)
                                    }))
                                    .await;
                                // A merge that GitHub refused was already told.
                                this.update(cx, |this, cx| {
                                    this.merged_by_rule(&rule, &item, sent, cx)
                                })
                                .unwrap_or_else(|_| Err("Arakun closed".into()))
                                .map_err(|why| (why, true))
                            }
                        }
                    }
                    step => this
                        .update_in(cx, |this, window, cx| {
                            this.run_rule_step(
                                &rule,
                                step,
                                dir.as_deref(),
                                pull.is_some(),
                                window,
                                cx,
                            )
                        })
                        .unwrap_or_else(|_| Err("Arakun closed".into()))
                        .map_err(|why| (why, false)),
                };
                if let Err(stop) = done {
                    result = Err(stop);
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if let Err((why, false)) = &result {
                    this.notify_from(
                        AlertKind::Failure,
                        &rule.title,
                        &format!("Stopped: {why}"),
                        pull.as_ref().map(|_| Open::GitHub(None)),
                        None,
                        "arakun",
                        cx,
                    );
                }
                if this.rules.finish(rule.id, result.map_err(|(why, _)| why)) {
                    // Unsaved, the file still says Running: after a restart the
                    // rule shows as cut short and is not run again.
                    let _ = this.save_rules(cx);
                    this.refresh_automations(cx);
                }
            });
        })
        .detach();
    }

    /// Says how a rule's merge went and reads the pull request again.
    /// Ok only when GitHub merged it; Err says why it didn't.
    fn merged_by_rule(
        &mut self,
        rule: &palette::rules::Rule,
        item: &ItemRef,
        sent: Result<(), crate::github::Problem>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let notify = rule
            .steps
            .iter()
            .any(|s| matches!(s, palette::rules::Step::Notify { .. }));
        let (kind, text, done) = match sent {
            Ok(()) => (
                AlertKind::Success,
                format!("Merged #{}", item.number),
                Ok(()),
            ),
            Err(p) if pr::refused_read_only(&p) => (
                AlertKind::Warning,
                "Read-only here: did not merge".to_string(),
                Err("read-only here: did not merge".to_string()),
            ),
            Err(p) => {
                let text = format!("Could not merge #{}: {}", item.number, p.text());
                (AlertKind::Failure, text.clone(), Err(text))
            }
        };
        // A merge says how it went itself.
        if notify || kind != AlertKind::Success {
            self.notify_from(
                kind,
                &rule.title,
                &text,
                Some(Open::GitHub(None)),
                None,
                "github",
                cx,
            );
        }
        self.github
            .update(cx, |g, cx| g.fetch_item(item.clone(), cx));
        done
    }

    /// One step of a rule other than a merge. Err when it could not start.
    fn run_rule_step(
        &mut self,
        rule: &palette::rules::Rule,
        step: &palette::rules::Step,
        dir: Option<&Path>,
        has_pull: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match step {
            palette::rules::Step::Merge { .. } => Ok(()),
            palette::rules::Step::Notify { text } => {
                // A merge says how it went itself.
                if !rule
                    .steps
                    .iter()
                    .any(|s| matches!(s, palette::rules::Step::Merge { .. }))
                {
                    // A reminder's title already holds its text.
                    let title = match rule.trigger {
                        palette::rules::Trigger::At { at } => {
                            palette::rules::reminder_title(at, crate::memory::now())
                        }
                        _ => rule.title.clone(),
                    };
                    self.notify_from(
                        AlertKind::Info,
                        &title,
                        text,
                        has_pull.then_some(Open::GitHub(None)),
                        None,
                        "arakun",
                        cx,
                    );
                }
                Ok(())
            }
            palette::rules::Step::Run { command } => {
                let d = dir.ok_or("no folder to run in")?;
                self.run_in_terminal(d, command, false, window, cx)
                    .map(|_| ())
                    .ok_or_else(|| format!("could not open a terminal for `{command}`"))
            }
            palette::rules::Step::Agent { prompt } => {
                let d = dir.ok_or("no folder to start an agent in")?;
                let agent = self
                    .find_env(d)
                    .map(|e| e.agent)
                    .unwrap_or(model::Agent::Claude);
                self.start_session_in(
                    d,
                    agent,
                    head(prompt, 60).into(),
                    prompt.clone(),
                    false,
                    window,
                    cx,
                )
                .map(|_| ())
                .ok_or_else(|| "could not start the agent".to_string())
            }
        }
    }

    /// The rule ticker: rules wait on GitHub and CI, which poll on their own;
    /// this looks at what they found every few seconds.
    pub(super) fn start_rule_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let mut n = 0u32;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(15))
                    .await;
                n += 1;
                let ok = this
                    .update_in(cx, |this, window, cx| {
                        // Every two minutes, read the watched pull requests again.
                        if n % 8 == 1 && this.rules.waiting().next().is_some() {
                            this.fetch_rule_pulls(cx);
                        }
                        this.check_rules(window, cx);
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
        .detach();
    }

    /// The Pinned block at the top of the sidebar.
    pub(super) fn render_pinned_block(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use crate::ui::Typography as _;
        use gpui::prelude::FluentBuilder as _;
        use gpui::{
            InteractiveElement as _, IntoElement, ParentElement as _,
            StatefulInteractiveElement as _, Styled as _, div, px,
        };
        if self.pins.items.is_empty() || self.rail_narrow() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .px(crate::ui::sp(8.))
                .pb(crate::ui::sp(6.))
                .gap(crate::ui::sp(1.))
                .child(
                    div()
                        .px(crate::ui::sp(8.))
                        .pt(crate::ui::sp(4.))
                        .pb(crate::ui::sp(3.))
                        .ty(crate::ui::Type::Caption)
                        .text_color(theme::text_faint())
                        .child("Pinned"),
                )
                .children(self.pins.items.iter().enumerate().map(|(n, p)| {
                    let view = View::ALL
                        .iter()
                        .find(|v| v.label().eq_ignore_ascii_case(&p.id));
                    let icon = match p.kind.as_str() {
                        "view" if view.is_some() => view.map_or("pin", |v| v.icon()),
                        "session" => "message01",
                        "pull" => "git-pull-request",
                        "file" => "folder01",
                        "view" => "dashboard-square01",
                        "branch" => "git-branch",
                        "project" => "folder01",
                        "command" => "flash",
                        "run" => "activity01",
                        "memory" => "bot",
                        "person" => "user",
                        _ => "pin",
                    };
                    div()
                        .id(("rail-pin", n))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(9.))
                        .h(px(28.))
                        .px(crate::ui::sp(9.))
                        .rounded(crate::ui::rad(8.))
                        .cursor_pointer()
                        .ty(crate::ui::Type::Label)
                        .text_color(theme::text_muted())
                        .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
                        .child(crate::ui::icon(icon, 14.).text_color(theme::text_subtle()))
                        .child(div().flex_1().min_w_0().truncate().child(p.label.clone()))
                        .when(n < 9, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .ty(crate::ui::Type::Caption)
                                    .font_family(theme::FONT_MONO)
                                    .text_color(theme::text_faint())
                                    .child(format!("{}", n + 1)),
                            )
                        })
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.open_pinned(n, window, cx)),
                        )
                        .into_any_element()
                }))
                .into_any_element(),
        )
    }
}

/// "Send to the cloud agent…", offered only when arakun-pilot is set up.
fn cloud_command(config: Option<&pilot::Config>) -> Option<Item> {
    let config = config?;
    let blurb = if config.admin_token.is_some() {
        "Hands a task to arakun-pilot: it works on your open pull request or on a new branch, and the result comes back as a card. Also: “cloud: <task>”."
    } else {
        "Hands a task to arakun-pilot. It needs \"admin_token\" in its client.json."
    };
    Some(
        Item::new(
            "command:cloud-task",
            Kind::Command,
            "Send to the cloud agent…",
            Target::CloudTask,
        )
        .icon("cloud-upload", Tint::Violet)
        .section("Commands")
        .boost(-5)
        .preview(Preview::Command {
            blurb: blurb.to_string(),
            keys: Vec::new(),
            cli: None,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cloud_command_needs_pilot_set_up() {
        assert!(cloud_command(None).is_none());
        let cfg = pilot::parse_config(br#"{"url": "https://p.dev", "token": "t"}"#).unwrap();
        let item = cloud_command(Some(&cfg)).unwrap();
        assert_eq!(item.target, Target::CloudTask);
        assert!(
            matches!(&item.preview, Preview::Command { blurb, .. } if blurb.contains("admin_token"))
        );
        let admin =
            pilot::parse_config(br#"{"url": "https://p.dev", "token": "t", "admin_token": "a"}"#)
                .unwrap();
        assert!(matches!(
            &cloud_command(Some(&admin)).unwrap().preview,
            Preview::Command { blurb, .. } if !blurb.contains("admin_token")
        ));
    }

    #[test]
    fn checks_fold_into_bars_keeping_failures() {
        let mut t = vec![Tint::Green; 24];
        t[23] = Tint::Red;
        let b = check_buckets(&t, 6);
        assert_eq!(b.len(), 6);
        assert_eq!(b[5], Tint::Red);
        assert_eq!(check_buckets(&t[..3], 6).len(), 3);
    }
}
