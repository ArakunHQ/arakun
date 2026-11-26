//! Discover, the store half of the Marketplace tab: Claude Code plugins its
//! marketplaces offer, and the official MCP Registry with an install form for
//! what a server needs.

use std::collections::HashMap;

use std::rc::Rc;

use gpui::{ListAlignment, ListOffset, ListState, list};

use super::*;
use serde_json::json;

use crate::agent::marketplace::{self, Input, Install, PluginEntry, RegistryServer};
use crate::agent::mirror::{self, RepoHas, RepoItem, Where};
use crate::agent::publishers;
use crate::agent::skills_sh::{self, SkillEntry, SkillSource};

/// Wait this long after the last keystroke before asking the registry.
const DEBOUNCE: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Market {
    Plugins,
    Registry,
    /// skills.sh.
    Skills,
}

/// What GitHub says of a skill picked on skills.sh.
#[derive(Clone)]
pub(super) enum Located {
    Looking,
    Found(SkillSource),
    Failed(SharedString),
}

/// A registry server being installed: the way picked and what the user typed.
pub(super) struct Installing {
    server: RegistryServer,
    method: usize,
    fields: Vec<(Input, Entity<TextInput>)>,
}

/// How long a failed load waits before opening the Marketplace asks again.
const RETRY: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct MarketState {
    pub(super) catalog: Loadable<Vec<PluginEntry>, SharedString>,
    /// When a load of the catalog or a server list last failed. Opening the
    /// Marketplace doesn't ask again until `RETRY` has passed, so a failure
    /// stays on screen instead of restarting at every redraw.
    pub(super) failed_at: Option<Instant>,
    /// The plugin category, or the company of official servers, the list is narrowed to.
    pub(super) category: Option<String>,
    pub(super) registry: Option<Vec<RegistryServer>>,
    pub(super) registry_next: Option<String>,
    /// The query the shown results answer.
    pub(super) registry_for: String,
    pub(super) registry_loading: bool,
    pub(super) registry_error: Option<SharedString>,
    pub(super) installing: Option<Installing>,
    pub(super) task: Option<Task<()>>,
    /// Show everything, not only what companies publish themselves.
    pub(super) everything: bool,
    /// Every server the known companies publish (`publishers`).
    /// Its errors show with the registry's, in `registry_error`.
    pub(super) official: Loadable<Vec<RegistryServer>, Infallible>,
    /// skills.sh's answer to the search box.
    pub(super) skills: Loadable<Vec<SkillEntry>, SharedString>,
    /// The query the shown skills answer.
    pub(super) skills_for: String,
    /// The repository the shown skills are all of, when the search named one.
    pub(super) skills_repo: Option<String>,
    /// The query skills.sh is being asked, so asking again for it does not
    /// drop the answer on its way.
    pub(super) skills_asking: Option<String>,
    /// The pause after typing before skills.sh is asked. One search or one
    /// pause at a time: either starting drops the other.
    pub(super) skills_debounce: Option<Task<()>>,
    /// Each picked skill's folder on GitHub, by skills.sh id.
    pub(super) located: HashMap<String, Located>,
    /// The plugin id or server name whose details show on the right.
    pub(super) selected: Option<String>,
    pub(super) sort: Sort,
    pub(super) rows: Rows,
    /// What the rows were laid out for, how many there were, and whether the
    /// top row (notes, the needs-you banner) was one of them.
    rows_for: std::cell::RefCell<Synced>,
    pub(super) last_width: std::cell::Cell<f32>,
    pub(super) check_pending: std::cell::Cell<bool>,
    /// Where Install puts things on this computer: for the user, or in the
    /// environment's repository.
    pub(super) at: Where,
    /// What that repository has already, read off the main thread.
    pub(super) repo_has: Option<RepoHas>,
}

impl ExtensionsView {
    /// Reads what it needs the first time the Marketplace shows.
    pub(super) fn open_market(&mut self, cx: &mut Context<Self>) {
        // A server says which plugins and MCP servers it has in one answer.
        self.ask_remote(false, cx);
        if self.ms.failed_at.is_some_and(|t| t.elapsed() < RETRY) {
            return;
        }
        match self.market {
            Market::Plugins if self.targets.target.is_some() => {}
            Market::Plugins if self.ms.catalog.data.is_none() && !self.ms.catalog.loading() => {
                self.load_catalog(cx)
            }
            Market::Registry if !self.ms.everything => self.load_official(cx),
            Market::Registry if self.ms.registry.is_none() && !self.ms.registry_loading => {
                self.search_registry(false, cx)
            }
            Market::Skills => self.search_skills(cx),
            _ => {}
        }
    }

    /// Asks skills.sh for the search box's words, unless it answered them already.
    fn search_skills(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().trim().to_string();
        if (self.ms.skills.data.is_some() || self.ms.skills.error.is_some())
            && query == self.ms.skills_for
            || self.ms.skills_asking.as_ref() == Some(&query)
        {
            return;
        }
        self.ms.skills.error = None;
        self.ms.skills_debounce = None;
        // Other words: the search on its way answers the old ones.
        self.ms.skills.cancel();
        if query.chars().count() < 2 {
            self.ms.skills.data = None;
            self.ms.skills_repo = None;
            self.ms.skills_for = query;
            self.ms.skills_asking = None;
            cx.notify();
            return;
        }
        self.ms.skills_asking = Some(query.clone());
        let q = query.clone();
        self.ms.skills.start(
            cx,
            |this| &mut this.ms.skills,
            // A repository named in the box lists every skill in it, as GitHub
            // has them now, whether skills.sh knows it or not.
            async move {
                let search = || {
                    skills_sh::search(&q).map(|l| {
                        l.into_iter()
                            .map(|e| (e, None::<skills_sh::SkillSource>))
                            .collect::<Vec<_>>()
                    })
                };
                let Some(repo) = skills_sh::repo_of(&q) else {
                    return search();
                };
                match skills_sh::package(&repo) {
                    Ok(p) => Ok(p.skills.into_iter().map(|(e, f)| (e, Some(f))).collect()),
                    // Words like `ci/cd` look like a repository too: what
                    // skills.sh finds for them beats GitHub's "not found".
                    Err(e) => match search() {
                        Ok(l) if !l.is_empty() => Ok(l),
                        _ => Err(e),
                    },
                }
            },
            |this, found, _| {
                this.ms.skills_asking = None;
                match found {
                    Ok(list) => {
                        // Only a package read from GitHub comes with its folders.
                        this.ms.skills_repo = list
                            .iter()
                            .find(|(_, f)| f.is_some())
                            .map(|(e, _)| e.source.clone());
                        let mut entries = Vec::with_capacity(list.len());
                        for (entry, found) in list {
                            if let Some(f) = found {
                                this.ms.located.insert(entry.id.clone(), Located::Found(f));
                            }
                            entries.push(entry);
                        }
                        this.ms.skills.data = Some(entries);
                    }
                    // A failed search keeps no old answer: it was for other words.
                    Err(e) => {
                        this.ms.skills.data = None;
                        this.ms.skills_repo = None;
                        this.ms.skills.error = Some(format!("{e:#}").into());
                    }
                }
                this.ms.skills_for = query;
            },
        );
    }

    /// Finds a picked skill's folder on GitHub, once, for its details and install.
    fn locate_skill(&mut self, entry: &SkillEntry, cx: &mut Context<Self>) {
        if matches!(
            self.ms.located.get(&entry.id),
            Some(Located::Looking | Located::Found(_))
        ) {
            return;
        }
        self.ms.located.insert(entry.id.clone(), Located::Looking);
        let e = entry.clone();
        cx.spawn(async move |this, cx| {
            let id = e.id.clone();
            let found = cx
                .background_spawn(crate::guard::task(async move { skills_sh::locate(&e) }))
                .await;
            let _ = this.update(cx, |this, cx| {
                let state = match found {
                    Ok(s) => Located::Found(s),
                    Err(e) => Located::Failed(format!("{e:#}").into()),
                };
                this.ms.located.insert(id, state);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Copies a skill from GitHub into `~/.claude/skills`, for every project here.
    fn install_skill(&mut self, entry: &SkillEntry, cx: &mut Context<Self>) {
        if let Some(s) = self.target_server() {
            self.error = Some(
                format!(
                    "Skills from skills.sh install on this computer only, not on {} yet.",
                    s.name
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let found = match self.ms.located.get(&entry.id) {
            Some(Located::Found(s)) => Some(s.clone()),
            _ => None,
        };
        let e = entry.clone();
        let at = self.ms.at;
        let Some(dir) = self.install_dir() else {
            return;
        };
        self.run_op(
            entry.name.clone(),
            format!("is installed {}", at.done()),
            move || {
                let found = match found {
                    Some(f) => f,
                    None => skills_sh::locate(&e)?,
                };
                mirror::install_skill(&e, &found, at, &dir)
            },
            None,
            cx,
        );
    }

    /// Installs every skill in the repository `source`, as Install does one.
    /// Skills installed already stay as they are.
    fn install_package(&mut self, source: &str, cx: &mut Context<Self>) {
        if let Some(s) = self.target_server() {
            self.error = Some(
                format!(
                    "Skills install on this computer only, not on {} yet.",
                    s.name
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let at = self.ms.at;
        let Some(dir) = self.install_dir() else {
            return;
        };
        let repo = source.to_string();
        let listed = self.listed_package(source);
        self.run_op(
            package_op(source),
            format!("are installed {}", at.done()),
            move || {
                let pkg = match listed {
                    Some(p) => p,
                    None => skills_sh::package(&repo)?,
                };
                mirror::install_package(&pkg, at, &dir).map(|_| ())
            },
            None,
            cx,
        );
    }

    /// The package a repository search listed, so Install all copies what it
    /// showed without reading it from GitHub again.
    fn listed_package(&self, source: &str) -> Option<skills_sh::Package> {
        if self.ms.skills_repo.as_deref() != Some(source) {
            return None;
        }
        let skills = self
            .ms
            .skills
            .data
            .iter()
            .flatten()
            .map(|e| match self.ms.located.get(&e.id) {
                Some(Located::Found(f)) => Some((e.clone(), f.clone())),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        Some(skills_sh::Package {
            source: source.to_string(),
            skills,
        })
    }

    /// The official servers: the last list at once, a new one when a day old.
    fn load_official(&mut self, cx: &mut Context<Self>) {
        if self.ms.official.loading() {
            return;
        }
        let mut stale = true;
        if self.ms.official.data.is_none() {
            if let Some((list, fresh)) = marketplace::cached_official() {
                self.ms.official.data = Some(list);
                stale = !fresh;
            }
        } else {
            stale = false;
        }
        if !stale {
            return;
        }
        self.ms.registry_error = None;
        self.ms.official.start(
            cx,
            |this| &mut this.ms.official,
            async move { marketplace::official_servers() },
            |this, found, _| match found {
                Ok(list) => this.ms.official.data = Some(list),
                Err(e) => {
                    this.ms.registry_error = Some(format!("{e:#}").into());
                    this.ms.failed_at = Some(Instant::now());
                }
            },
        );
    }

    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        if self.ms.catalog.data.is_none() {
            self.ms.catalog.data = marketplace::cached_plugins();
        }
        self.ms.catalog.error = None;
        self.ms.catalog.start(
            cx,
            |this| &mut this.ms.catalog,
            async move { marketplace::plugins(&cwd) },
            |this, found, _| {
                if found.is_err() {
                    this.ms.failed_at = Some(Instant::now());
                }
                this.ms
                    .catalog
                    .settle(found.map_err(|e| format!("{e:#}").into()));
            },
        );
    }

    /// Asks the registry for the search box's words, after a pause in typing.
    pub(super) fn query_changed(&mut self, cx: &mut Context<Self>) {
        if self.kind == Kind::Marketplace && self.market == Market::Skills {
            // This drops a search on its way: it is asked again, and nothing is
            // loading until it is (the words may be back to ones already answered).
            self.ms.skills_asking = None;
            self.ms.skills.cancel();
            self.ms.skills_debounce = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(DEBOUNCE).await;
                let _ = this.update(cx, |this, cx| this.search_skills(cx));
            }));
            return;
        }
        // The official list filters as you type; only "All" asks the registry.
        if self.kind != Kind::Marketplace || self.market != Market::Registry || !self.ms.everything
        {
            return;
        }
        self.ms.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.search_registry(false, cx));
        }));
    }

    fn search_registry(&mut self, more: bool, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().trim().to_string();
        if !more && self.ms.registry.is_some() && query == self.ms.registry_for {
            return;
        }
        let cursor = if more {
            self.ms.registry_next.clone()
        } else {
            None
        };
        self.ms.registry_loading = true;
        self.ms.registry_error = None;
        self.ms.task = Some(cx.spawn(async move |this, cx| {
            let q = query.clone();
            let found = cx
                .background_spawn(crate::guard::task(async move {
                    marketplace::search(&q, cursor.as_deref())
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ms.registry_loading = false;
                match found {
                    Ok((list, next)) => {
                        if more {
                            this.ms.registry.get_or_insert_with(Vec::new).extend(list);
                        } else {
                            this.ms.registry = Some(list);
                        }
                        this.ms.registry_next = next;
                        this.ms.registry_for = query;
                    }
                    Err(e) => {
                        this.ms.registry_error = Some(format!("{e:#}").into());
                        this.ms.failed_at = Some(Instant::now());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn install_plugin(&mut self, entry: &PluginEntry, cx: &mut Context<Self>) {
        let id = entry.id.clone();
        let name = entry.name.clone();
        if self.targets.target.is_some() {
            let done = format!(
                "is installed{} and on in every project there.",
                self.on_target()
            );
            let msg = json!({ "type": "install_plugin", "id": id });
            self.install_remote(name, id, msg, done, cx);
            return;
        }
        let Some(cwd) = self.install_dir() else {
            return;
        };
        let at = self.ms.at;
        self.run_op(
            name,
            format!("is installed and on {}", at.done()),
            move || {
                marketplace::install_plugin_as(&cwd, &id, at.claude())?;
                if at != Where::User {
                    mirror::remember_repo_install(
                        &cwd,
                        at == Where::Shared,
                        RepoItem::Plugin { id },
                    );
                }
                Ok(())
            },
            None,
            cx,
        );
    }

    fn start_install(
        &mut self,
        server: RegistryServer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut ins = Installing {
            server,
            method: 0,
            fields: Vec::new(),
        };
        self.fill_fields(&mut ins, cx);
        if let Some((_, first)) = ins.fields.first() {
            window.focus(&first.focus_handle(cx), cx);
        }
        self.ms.installing = Some(ins);
        cx.notify();
    }

    fn fill_fields(&self, ins: &mut Installing, cx: &mut Context<Self>) {
        let Some(method) = ins.server.installs.get(ins.method) else {
            return;
        };
        ins.fields = method
            .inputs()
            .into_iter()
            .map(|i| {
                let input = i.clone();
                let field = cx.new(|cx| {
                    let hint = if input.secret {
                        "Paste it here".to_string()
                    } else {
                        input.default.clone().unwrap_or_else(|| input.name.clone())
                    };
                    let t = TextInput::new(hint, cx);
                    let mut t = if input.secret { t.secret() } else { t };
                    if let Some(d) = &input.default {
                        t.set_text(d.clone(), cx);
                    }
                    t
                });
                (input, field)
            })
            .collect();
    }

    fn pick_method(&mut self, method: usize, cx: &mut Context<Self>) {
        if let Some(mut ins) = self.ms.installing.take() {
            ins.method = method;
            self.fill_fields(&mut ins, cx);
            self.ms.installing = Some(ins);
        }
        cx.notify();
    }

    fn finish_install(&mut self, cx: &mut Context<Self>) {
        let Some(ins) = &self.ms.installing else {
            return;
        };
        let Some(method) = ins.server.installs.get(ins.method).cloned() else {
            return;
        };
        let values: HashMap<String, String> = ins
            .fields
            .iter()
            .map(|(i, f)| (i.name.clone(), f.read(cx).text().to_string()))
            .collect();
        if let Err(e) = marketplace::config(&method, &values) {
            self.error = Some(format!("{e:#}").into());
            cx.notify();
            return;
        }
        let name = ins.server.short_name();
        let login = claude_ext::login_command(&name);
        let n = name.clone();
        let sign_in = matches!(&method, Install::Remote { headers, .. } if headers.is_empty());
        // What was filled in does not go to a server over plain http, where it
        // could be read on the way. Not only fields marked secret: registry
        // entries do not always mark an API key as one.
        let clear = self
            .target_server()
            .filter(|s| s.in_the_clear)
            .map(|s| s.name.clone());
        let filled = values.values().any(|v| !v.trim().is_empty());
        if let Some(server) = clear.filter(|_| filled) {
            self.error = Some(
                format!(
                    "{server} is reached over plain http, so what you filled in could be read on the way. Reach it over https or the relay to add this there."
                )
                .into(),
            );
            cx.notify();
            return;
        }
        // A shared install writes them into the repo's .mcp.json, which is committed.
        if self.targets.target.is_none() && self.ms.at == Where::Shared {
            let keys: Vec<&str> = ins
                .fields
                .iter()
                .filter(|(i, _)| looks_secret(i))
                .filter(|(i, _)| values.get(&i.name).is_some_and(|v| !v.trim().is_empty()))
                .map(|(i, _)| i.name.as_str())
                .collect();
            if !keys.is_empty() {
                self.error = Some(
                    format!(
                        "“This repo, shared” would commit {} to the repo. Pick “This repo, just me” or “Every repo” for a server that needs a key.",
                        keys.join(", ")
                    )
                    .into(),
                );
                cx.notify();
                return;
            }
        }
        if self.targets.target.is_some() {
            // Its sign-in opens a browser, so it is done on that machine.
            let mut done = format!("was added{} for every project there.", self.on_target());
            if sign_in {
                done.push_str(&format!(" Sign in to it there: {login}"));
            }
            let msg = json!({
                "type": "install_server", "name": n, "install": method, "values": values,
            });
            // A server it could not be sent to keeps the form as it was filled in.
            if self.install_remote(name, n, msg, done, cx) {
                self.ms.installing = None;
            }
            return;
        }
        let Some(cwd) = self.install_dir() else {
            return;
        };
        let at = self.ms.at;
        let n = self.server_name(&n, sign_in);
        let login = claude_ext::login_command(&n);
        self.ms.installing = None;
        self.run_op(
            n.clone(),
            format!("was added {}", at.done()),
            move || {
                marketplace::install_server_as(&cwd, &n, &method, &values, at.claude())?;
                if at != Where::User {
                    mirror::remember_repo_install(
                        &cwd,
                        at == Where::Shared,
                        RepoItem::Server { name: n },
                    );
                }
                Ok(())
            },
            sign_in.then_some(login),
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // The store: sources and filters on the left, a virtual list of cards in
    // the middle (only the visible lines are laid out, however many there are),
    // and the picked item's details and install on the right.

    /// What the list shows now, filtered and sorted.
    fn market_items(&self, cx: &App) -> Vec<Item> {
        let words: Vec<String> = self
            .query
            .read(cx)
            .text()
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let hit = |hay: String| {
            let hay = hay.to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        };
        let mut items: Vec<Item> = match self.market {
            Market::Plugins => self
                .plugin_catalog()
                .into_iter()
                .flatten()
                .filter(|p| self.ms.everything || p.official)
                .filter(|p| {
                    self.ms
                        .category
                        .as_ref()
                        .is_none_or(|c| p.category.as_ref() == Some(c))
                })
                .filter(|p| {
                    hit(format!(
                        "{} {} {} {}",
                        p.name,
                        p.description,
                        p.category.as_deref().unwrap_or_default(),
                        p.publisher.as_deref().unwrap_or_default()
                    ))
                })
                .cloned()
                .map(Item::Plugin)
                .collect(),
            Market::Registry => {
                let source = if self.ms.everything {
                    &self.ms.registry
                } else {
                    &self.ms.official.data
                };
                source
                    .iter()
                    .flatten()
                    .filter(|s| {
                        self.ms.everything
                            || self
                                .ms
                                .category
                                .as_ref()
                                .is_none_or(|c| s.official.as_ref() == Some(c))
                    })
                    .filter(|s| {
                        self.ms.everything
                            || hit(format!(
                                "{} {} {} {}",
                                s.name,
                                s.title,
                                s.description,
                                s.official.as_deref().unwrap_or_default()
                            ))
                    })
                    .cloned()
                    .map(Item::Server)
                    .collect()
            }
            // skills.sh searched the words already; the repository narrows it.
            Market::Skills => self
                .ms
                .skills
                .data
                .iter()
                .flatten()
                .filter(|s| self.ms.category.as_ref().is_none_or(|c| &s.source == c))
                .cloned()
                .map(Item::Skill)
                .collect(),
        };
        if self.ms.sort == Sort::Name {
            items.sort_by_key(|i| i.title().to_lowercase());
        }
        items
    }

    /// Cards to a line, from the list's width at the last layout: as many as fit
    /// at `MIN_CARD` each. Before the first layout it guesses, and looks again next frame.
    fn market_columns(&self, cx: &mut Context<Self>) -> usize {
        let read = |this: &Self| {
            this.ms.rows.0.viewport_bounds().size.width.as_f32()
                - 2. * f32::from(crate::ui::sp(ui::PAGE_PAD))
        };
        let width = read(self);
        self.ms.last_width.set(width);
        // The list learns its width when laid out, after this render: look once
        // more then, and draw again if it changed (details opened, window resized).
        if !self.ms.check_pending.replace(true) {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(32))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.ms.check_pending.set(false);
                    if (read(this) - this.ms.last_width.get()).abs() > 0.5 {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        if width <= 0. {
            return 3;
        }
        // The row's gap follows the Spacing setting, so count with it.
        let gap = f32::from(crate::ui::sp(GAP));
        (((width + gap) / (MIN_CARD + gap)).floor() as usize).clamp(1, 4)
    }

    fn installed(&self, item: &Item) -> bool {
        // In a repository, what it has: its files, and Claude's list for it.
        if self.targets.target.is_none() && self.ms.at != Where::User {
            let at = self.ms.at;
            let Some(has) = &self.ms.repo_has else {
                return false;
            };
            return match item {
                Item::Plugin(p) => has.plugin(at, &p.id),
                Item::Server(s) => {
                    let name = s.short_name();
                    has.server(at, &name) || has.server(at, &mirror::suffixed(&name, &has.slug))
                }
                Item::Skill(k) => has.skill(&k.skill_id),
            };
        }
        match item {
            // A server's list says what is installed there; Claude's files here
            // know of an install the list was read before.
            Item::Plugin(p) => {
                p.installed
                    || self.targets.target.is_none()
                        && self
                            .claude
                            .data
                            .as_ref()
                            .is_some_and(|x| x.plugins.iter().any(|i| i.id == p.id))
            }
            Item::Server(s) => self.remote_has_server(&s.short_name()).unwrap_or_else(|| {
                self.claude
                    .data
                    .as_ref()
                    .is_some_and(|x| x.servers.iter().any(|i| i.name == s.short_name()))
            }),
            Item::Skill(k) => self.skill_installed(&k.skill_id),
        }
    }

    /// Whether the skill `skill_id` is where Install puts it: the repository's
    /// skills folder, or for the user, its folder in ~/.claude/skills. A project's
    /// or a plugin's skill of that name leaves room for a copy of your own.
    fn skill_installed(&self, skill_id: &str) -> bool {
        if self.targets.target.is_some() {
            return false;
        }
        if self.ms.at != Where::User {
            return self
                .ms
                .repo_has
                .as_ref()
                .is_some_and(|has| has.skill(skill_id));
        }
        self.claude.data.as_ref().is_some_and(|x| {
            x.skills.iter().any(|i| {
                i.source == "personal"
                    && i.path
                        .file_name()
                        .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(skill_id))
            })
        })
    }

    /// The servers of an installed item that wait for a sign-in on this
    /// computer: a plugin's OAuth servers, or a registry server added without
    /// a token. A paired server's sign-in opens a browser there, so not those.
    fn awaiting_sign_in(&self, item: &Item) -> Vec<&Server> {
        if self.targets.target.is_some() {
            return Vec::new();
        }
        let Some(x) = self.claude.data.as_ref() else {
            return Vec::new();
        };
        let slug = self.ms.repo_has.as_ref().map(|h| h.slug.as_str());
        x.servers
            .iter()
            .filter(|s| waits_for_sign_in(item, s, slug))
            .collect()
    }

    /// Runs the server's sign-in in a terminal, and rereads until it is done.
    fn sign_in(&mut self, name: &str, cx: &mut Context<Self>) {
        self.run_and_watch(claude_ext::login_command(name), cx);
        cx.notify();
    }

    /// In the details of an installed item: what still waits for a sign-in,
    /// with a button for each.
    fn render_sign_ins(&self, item: &Item, cx: &mut Context<Self>) -> Option<Div> {
        let waiting = self.awaiting_sign_in(item);
        if waiting.is_empty() {
            return None;
        }
        let warn = Tone::Warning.color();
        let mut out = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .bg(warn.opacity(0.10))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .ty(Type::Body)
                    .text_color(theme::text())
                    .child(icon("alert-circle", 14.).text_color(warn))
                    .child(if waiting.len() == 1 {
                        "Its MCP server needs you to sign in before Claude can use it.".to_string()
                    } else {
                        format!(
                            "{} need you to sign in before Claude can use them.",
                            plural(waiting.len(), "MCP server")
                        )
                    }),
            )
            .child(
                div()
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child("Sign-in opens your browser once; Claude keeps it fresh after that."),
            );
        for (i, s) in waiting.iter().enumerate() {
            let name = s.name.clone();
            let label = if waiting.len() == 1 {
                "Sign in".to_string()
            } else {
                format!("Sign in to {}", display_name(&name))
            };
            out = out.child(
                div().flex().child(
                    ui::button_with(
                        ("ext-detail-sign-in", i),
                        Some("user"),
                        label,
                        ButtonKind::Primary,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.sign_in(&name, cx))),
                ),
            );
        }
        Some(out)
    }

    fn select(&mut self, item: &Item, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            Item::Plugin(p) => {
                self.ms.installing = None;
                self.ms.selected = Some(p.id.clone());
            }
            Item::Server(s) => {
                self.ms.selected = Some(s.name.clone());
                self.start_install(s.clone(), window, cx);
            }
            Item::Skill(k) => {
                self.ms.installing = None;
                self.ms.selected = Some(k.id.clone());
                self.locate_skill(k, cx);
            }
        }
        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.ms.selected = None;
        self.ms.installing = None;
        cx.notify();
    }

    /// How many plugins or servers Discover has for `m`, as the scope shows them.
    pub(super) fn discover_count(&self, m: Market) -> Option<usize> {
        match m {
            Market::Plugins => self.plugin_catalog().map(|c| {
                c.iter()
                    .filter(|p| self.ms.everything || p.official)
                    .count()
            }),
            Market::Registry if self.ms.everything => self.ms.registry.as_ref().map(Vec::len),
            Market::Registry => self.ms.official.data.as_ref().map(Vec::len),
            Market::Skills => self.ms.skills.data.as_ref().map(Vec::len),
        }
    }

    /// Opens Discover on plugins or on MCP servers.
    pub(super) fn show_market(&mut self, m: Market, cx: &mut Context<Self>) {
        self.set_kind(Kind::Marketplace, cx);
        self.market = m;
        self.ms.category = None;
        self.ms.selected = None;
        self.ms.installing = None;
        self.ms.sort = Sort::Popular;
        self.ms.rows.top();
        self.open_market(cx);
        if m == Market::Registry && self.ms.everything {
            self.search_registry(false, cx);
        }
        cx.notify();
    }

    /// Discover's categories for plugins, or the companies of official servers,
    /// for the sidebar.
    pub(super) fn category_lines(&self) -> Vec<Line> {
        let mut groups: Vec<(String, usize)> = Vec::new();
        let mut add = |g: &str| match groups.iter_mut().find(|(n, _)| n == g) {
            Some((_, n)) => *n += 1,
            None => groups.push((g.to_string(), 1)),
        };
        let heading = match self.market {
            Market::Plugins => {
                for p in self
                    .plugin_catalog()
                    .into_iter()
                    .flatten()
                    .filter(|p| self.ms.everything || p.official)
                {
                    if let Some(c) = &p.category {
                        add(c);
                    }
                }
                "Categories"
            }
            Market::Registry if !self.ms.everything => {
                for s in self.ms.official.data.iter().flatten() {
                    if let Some(c) = &s.official {
                        add(c);
                    }
                }
                "Companies"
            }
            Market::Registry => return Vec::new(),
            Market::Skills => {
                for s in self.ms.skills.data.iter().flatten() {
                    add(&s.source);
                }
                "Repositories"
            }
        };
        if groups.is_empty() {
            return Vec::new();
        }
        groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let total = self.discover_count(self.market).unwrap_or(0);
        let mut out = vec![
            Line::Heading(heading.into()),
            Line::Entry(
                Entry::new("cat:", Lead::Icon("dashboard-square01"), "All")
                    .count(Some(total))
                    .on(self.ms.category.is_none()),
            ),
        ];
        for (g, n) in groups {
            let on = self.ms.category.as_ref() == Some(&g);
            let (lead, label) = match self.market {
                Market::Plugins => (Lead::Dot(rgb(category_color(&g)).into()), capitalize(&g)),
                Market::Registry => (Lead::Mark(logos::mark(&g, "")), g.clone()),
                Market::Skills => {
                    let owner = g.split('/').next().unwrap_or_default();
                    (
                        Lead::Mark(mark_by(owner, "", publishers::by_org(owner))),
                        g.clone(),
                    )
                }
            };
            out.push(Line::Entry(
                Entry::new(format!("cat:{g}"), lead, label)
                    .count(Some(n))
                    .on(on),
            ));
        }
        out
    }

    /// Shows one category (or company) of Discover, or all of them.
    pub(super) fn pick_category(&mut self, category: Option<String>, cx: &mut Context<Self>) {
        self.ms.category = category;
        self.ms.rows.top();
        cx.notify();
    }

    /// Discover: what needs you, the featured strip, the heading with its
    /// filters, then a virtual list of card lines (only the visible ones are
    /// laid out, however many there are), and the picked item on the right.
    pub(super) fn render_market(&self, cx: &mut Context<Self>) -> Div {
        let items = Rc::new(self.market_items(cx));
        let detail = self.render_detail(cx);
        let per = self.market_columns(cx);
        let more = self.market == Market::Registry
            && self.ms.everything
            && self.ms.registry_next.is_some();
        let loading = match self.market {
            Market::Plugins => self.plugin_catalog().is_none(),
            Market::Registry if self.ms.everything => self.ms.registry.is_none(),
            Market::Registry => self.ms.official.data.is_none(),
            Market::Skills => self.ms.skills.data.is_none() && self.ms.skills.loading(),
        };
        let error = match self.market {
            Market::Plugins => self.catalog_error(),
            Market::Registry => self.ms.registry_error.clone(),
            Market::Skills => self.ms.skills.error.clone(),
        };
        let query = self.query.read(cx).text().trim().to_string();
        // The most installed, larger, when there is room for two side by side,
        // nothing narrows the list, and it is sorted by installs.
        let featured = if self.market == Market::Plugins
            && per >= 2
            && self.ms.sort == Sort::Popular
            && self.ms.category.is_none()
            && query.is_empty()
            && items.len() > per
        {
            per.min(3)
        } else {
            0
        };
        let top = self.top_signature();

        let main: AnyElement = if loading || items.is_empty() {
            // No list now: the next one starts over, even for the same search.
            forget_rows(&mut self.ms.rows_for.borrow_mut());
            let state: AnyElement = if loading {
                match error.clone() {
                    Some(e) => note(e, Tone::Danger).into_any_element(),
                    None => ui::loading(match self.market {
                        Market::Plugins => self.catalog_wait(),
                        Market::Registry if self.ms.everything => "Asking the MCP Registry…".into(),
                        Market::Registry => "Gathering what each company publishes…".into(),
                        Market::Skills => "Asking skills.sh…".into(),
                    })
                    .into_any_element(),
                }
            } else if self.market == Market::Skills && self.ms.skills.error.is_some() {
                // The heading says what went wrong.
                div().into_any_element()
            } else if self.market == Market::Skills && self.ms.skills.data.is_none() {
                ui::empty_state(
                    "sparkles",
                    "Search skills.sh",
                    "Type what you need in the search box, like react, pdf or testing, or a GitHub repository like anthropics/skills for all of its skills. A skill installs into ~/.claude/skills, where Claude Code and Workers AI find it.",
                    None,
                )
                .into_any_element()
            } else {
                ui::empty_state(
                    "search01",
                    "Nothing matches",
                    match self.market {
                        Market::Registry if self.ms.everything => {
                            "It searches server names: try a product name like github or postgres."
                        }
                        Market::Skills => "skills.sh found no skill for that. Try other words.",
                        _ => "Try other words, another category, or All instead of Official.",
                    },
                    None,
                )
                .into_any_element()
            };
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .children(top.is_some().then(|| self.render_top(cx)))
                .child(self.render_head(items.len(), loading, cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .px(crate::ui::sp(ui::PAGE_PAD))
                        .child(state),
                )
                .into_any_element()
        } else {
            let mut rows = Vec::new();
            if top.is_some() {
                rows.push(Row::Top);
            }
            if featured > 0 {
                rows.push(Row::Featured);
            }
            rows.push(Row::Head);
            let mut start = featured;
            while start < items.len() {
                rows.push(Row::Line(start));
                start += per;
            }
            if more {
                rows.push(Row::More);
            }
            self.sync_rows(
                RowsKey {
                    market: self.market,
                    category: self.ms.category.clone(),
                    everything: self.ms.everything,
                    by_name: self.ms.sort == Sort::Name,
                    query,
                    per,
                    featured,
                },
                rows.len(),
                top,
            );
            let rows = Rc::new(rows);
            list(
                self.ms.rows.0.clone(),
                cx.processor(move |this, ix: usize, _, cx| match rows.get(ix) {
                    Some(row) => this.render_market_row(*row, &items, per, featured, cx),
                    None => div().into_any_element(),
                }),
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };

        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(main),
            )
            .children(detail)
    }

    fn sync_rows(&self, key: RowsKey, total: usize, top: Option<u64>) {
        sync_rows(
            &self.ms.rows.0,
            &mut self.ms.rows_for.borrow_mut(),
            key,
            total,
            top,
        );
    }

    fn render_market_row(
        &mut self,
        row: Row,
        items: &[Item],
        per: usize,
        featured: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            Row::Top => self.render_top(cx).into_any_element(),
            Row::Featured => {
                let mut line = div().flex().w_full().gap(crate::ui::sp(GAP));
                for item in items.iter().take(featured) {
                    line = line.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(self.featured_card(item, cx)),
                    );
                }
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .gap(crate::ui::sp(12.))
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pt(crate::ui::sp(20.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .ty(Type::Heading)
                                    .text_color(theme::text())
                                    .child("Featured"),
                            )
                            .child(
                                div()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_faint())
                                    .child("Most installed"),
                            ),
                    )
                    .child(line)
                    .into_any_element()
            }
            Row::Head => self.render_head(items.len(), false, cx).into_any_element(),
            Row::Line(start) => {
                let mut line = div()
                    .flex()
                    .w_full()
                    .gap(crate::ui::sp(GAP))
                    .h(px(ROW_H))
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pb(crate::ui::sp(GAP));
                for item in items.iter().skip(start).take(per) {
                    line = line.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(self.item_card(item, cx)),
                    );
                }
                for _ in items.len().saturating_sub(start).min(per)..per {
                    line = line.child(div().flex_1());
                }
                line.into_any_element()
            }
            Row::More => div()
                .flex()
                .items_center()
                .justify_center()
                .h(px(ROW_H))
                .child(if self.ms.registry_loading {
                    ui::spinner(14., theme::text_faint()).into_any_element()
                } else {
                    ui::button("ext-registry-more", "Load more", ButtonKind::Outline, true)
                        .on_click(cx.listener(|this, _, _, cx| this.search_registry(true, cx)))
                        .into_any_element()
                })
                .into_any_element(),
        }
    }

    /// What the top row says, as a hash, or None when there is no top row:
    /// when it changes the row is measured again.
    fn top_signature(&self) -> Option<u64> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let first = self.needy().next();
        if self.error.is_none() && self.notice.is_none() && first.is_none() {
            return None;
        }
        let mut h = DefaultHasher::new();
        self.error.hash(&mut h);
        self.notice.hash(&mut h);
        first
            .map(|s| (&s.name, health_label(&s.health)))
            .hash(&mut h);
        self.needy().count().hash(&mut h);
        Some(h.finish())
    }

    /// Claude Code's MCP servers that need you, as last read.
    /// Only while Discover is on this computer: they are this computer's.
    fn needy(&self) -> impl Iterator<Item = &Server> {
        let here = self.targets.target.is_none();
        self.claude.data.iter().filter(move |_| here).flat_map(|x| {
            x.servers.iter().filter(|s| {
                matches!(s.health, Health::Failed(_))
                    || (s.health == Health::NeedsAuth && !s.unused_connector())
            })
        })
    }

    /// Above the store: the last error or notice, and a server that needs you.
    fn render_top(&self, cx: &mut Context<Self>) -> Div {
        let mut needy = self.needy();
        let first = needy.next();
        let others = needy.count();
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(ui::PAGE_PAD))
            .pt(crate::ui::sp(16.))
            .children(self.render_notes())
            .when_some(first, |d, s| {
                let warn = Tone::Warning.color();
                let name = display_name(&s.name);
                let mut what = match s.health {
                    Health::NeedsAuth => format!("{name} needs you to sign in"),
                    Health::Failed(_) => format!("{name} is failing"),
                    _ => format!("{name}: {}", health_label(&s.health)),
                };
                if others > 0 {
                    what.push_str(&format!(", and {} more", plural(others, "server")));
                }
                d.child(
                    div()
                        .id("ext-needs-you")
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(10.))
                        .px(crate::ui::sp(12.))
                        .py(crate::ui::sp(9.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .bg(warn.opacity(0.10))
                        .cursor_pointer()
                        .hover(|d| d.bg(warn.opacity(0.16)))
                        .ty(Type::Body)
                        .child(icon("alert-circle", 14.).text_color(warn))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme::text())
                                .child(what),
                        )
                        .when(s.health == Health::NeedsAuth, |d| {
                            let name = s.name.clone();
                            d.child(
                                ui::button_with(
                                    "ext-needs-you-sign-in",
                                    Some("user"),
                                    "Sign in",
                                    ButtonKind::Outline,
                                    true,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        // Not the banner's own click, which opens Installed.
                                        cx.stop_propagation();
                                        this.sign_in(&name, cx);
                                    },
                                )),
                            )
                        })
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme::text_subtle())
                                .child("Open in Installed"),
                        )
                        .child(icon("arrow-right01", 12.).text_color(theme::text_subtle()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            // A search from Discover would filter out the server it names.
                            this.query.update(cx, |q, cx| q.set_text("", cx));
                            this.set_kind(Kind::Servers, cx);
                        })),
                )
            })
    }

    /// The list's heading: what it shows and how many, Official or All, and the sort.
    fn render_head(&self, n: usize, loading: bool, cx: &mut Context<Self>) -> Div {
        let busy = !loading
            && match self.market {
                Market::Plugins => self.catalog_loading(),
                Market::Registry if self.ms.everything => self.ms.registry_loading,
                Market::Registry => self.ms.official.loading(),
                Market::Skills => self.ms.skills.loading(),
            };
        let error = match self.market {
            Market::Plugins => self.catalog_error(),
            Market::Skills => self.ms.skills.error.clone(),
            // The registry is read from here; installing from it needs the server.
            Market::Registry => self.server_refused().or(self.ms.registry_error.clone()),
        };
        let title = match (self.market, &self.ms.category) {
            (_, Some(c)) if self.market == Market::Plugins => capitalize(c),
            (_, Some(c)) => c.clone(),
            (Market::Plugins, None) if self.ms.everything => "All plugins".to_string(),
            (Market::Plugins, None) => "Official plugins".to_string(),
            (Market::Registry, None) if self.ms.everything => "MCP Registry".to_string(),
            (Market::Registry, None) => "Official MCP servers".to_string(),
            (Market::Skills, None) => self
                .ms
                .skills_repo
                .clone()
                .unwrap_or_else(|| "skills.sh".to_string()),
        };
        let mut scope = ui::segmented();
        for (i, (everything, label)) in [(false, "Official"), (true, "All")].into_iter().enumerate()
        {
            scope = scope.child(
                ui::segment(
                    ("ext-scope", i),
                    (!everything).then_some("checkmark-circle02"),
                    label,
                    self.ms.everything == everything,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.ms.everything = everything;
                    this.ms.category = None;
                    this.ms.rows.top();
                    this.open_market(cx);
                    // Back on All, the last results may answer an older query;
                    // an unchanged one is not asked again.
                    if everything && this.market == Market::Registry && this.ms.registry.is_some() {
                        this.search_registry(false, cx);
                    }
                    cx.notify();
                })),
            );
        }
        let mut sort = ui::segmented();
        for (i, (s, label)) in [
            (
                Sort::Popular,
                if self.market == Market::Registry {
                    "By company"
                } else {
                    "Popular"
                },
            ),
            (Sort::Name, "A–Z"),
        ]
        .into_iter()
        .enumerate()
        {
            sort = sort.child(
                ui::segment(("ext-sort", i), None, label, self.ms.sort == s).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.ms.sort = s;
                        this.ms.rows.top();
                        cx.notify();
                    },
                )),
            );
        }
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(ui::PAGE_PAD))
            .pt(crate::ui::sp(24.))
            .pb(crate::ui::sp(12.))
            // Narrow (details open), the filters drop below the title.
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(
                        div()
                            .flex_none()
                            .ty(Type::Heading)
                            .text_color(theme::text())
                            .child(title),
                    )
                    // Before a search, skills.sh has shown nothing to count.
                    .when(
                        !loading
                            && (self.market != Market::Skills || self.ms.skills.data.is_some()),
                        |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .ty(Type::Label)
                                    .text_color(theme::text_faint())
                                    .child(n.to_string()),
                            )
                        },
                    )
                    .when(busy, |d| d.child(ui::spinner(13., theme::text_faint())))
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap(crate::ui::sp(8.))
                            // skills.sh does not say which skills are official.
                            .when(self.market != Market::Skills, |d| d.child(scope))
                            .child(sort),
                    ),
            )
            .when(!loading, |d| {
                d.when_some(error, |d, e| d.child(note(e, Tone::Danger)))
            })
    }

    /// What a card offers: Installed, a spinner, or Install.
    fn card_action(&self, item: &Item, kind: ButtonKind, cx: &mut Context<Self>) -> AnyElement {
        // Installed but not usable yet: the card signs in, so nobody has to
        // find the server under Installed.
        if let Some(s) = self.awaiting_sign_in(item).first() {
            let name = s.name.clone();
            return ui::button_with(
                SharedString::from(format!("ext-card-sign-in-{}", item.key())),
                Some("user"),
                "Sign in",
                kind,
                true,
            )
            .tooltip(ui::tip_text(format!(
                "Installed. {} needs you to sign in",
                display_name(&name)
            )))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.sign_in(&name, cx);
            }))
            .into_any_element();
        }
        if self.installed(item) {
            return ui::state_pill("checkmark-circle02", "Installed", Tone::Success.color())
                .into_any_element();
        }
        let busy_name = match item {
            Item::Plugin(p) => Some(p.name.as_str()),
            Item::Skill(k) => Some(k.name.as_str()),
            Item::Server(_) => None,
        };
        if busy_name.is_some() && self.op.as_deref() == busy_name {
            return ui::spinner(14., theme::text_faint()).into_any_element();
        }
        let target = item.clone();
        ui::button(
            SharedString::from(format!("ext-card-install-{}", item.key())),
            "Install",
            kind,
            self.op.is_none(),
        )
        // A plugin goes on in every project, as the details say; a server
        // asks how in the details first.
        .when(matches!(item, Item::Plugin(_)), |b| {
            b.tooltip(ui::tip_text(match self.target_server() {
                Some(s) => format!("Install on {}, for every project", s.name),
                None => "Install for every project".to_string(),
            }))
        })
        .when(matches!(item, Item::Skill(_)), |b| {
            b.tooltip(ui::tip("Install into ~/.claude/skills, for every project"))
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            // The card under the button opens the details; Install alone installs.
            cx.stop_propagation();
            match &target {
                Item::Plugin(p) => this.install_plugin(p, cx),
                Item::Skill(k) => this.install_skill(k, cx),
                // A server asks what it needs first, in the details.
                Item::Server(_) => this.select(&target, window, cx),
            }
        }))
        .into_any_element()
    }

    fn item_card(&self, item: &Item, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.ms.selected.as_deref() == Some(item.key());
        let (by, verified) = item.by();
        let target = item.clone();
        div()
            .id(SharedString::from(format!("ext-card-{}", item.key())))
            .flex()
            .flex_col()
            // The row's bottom padding scales, so the card takes what is left.
            .h(px(ROW_H) - crate::ui::sp(GAP))
            .min_w_0()
            .overflow_hidden()
            .gap(crate::ui::sp(12.))
            .p(crate::ui::sp(16.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .cursor_pointer()
            .when(selected, |d| {
                d.border_color(theme::accent()).bg(theme::fill_raised())
            })
            .when(!selected, |d| {
                d.border_color(theme::border())
                    .bg(theme::background())
                    .hover(|d| {
                        d.bg(theme::fill_raised())
                            .border_color(theme::border_strong())
                    })
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .child(logos::tile(item.mark(), 40.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Heading)
                                    .text_color(theme::text())
                                    .child(item.title()),
                            )
                            .child(by_line(by, verified)),
                    )
                    .child(div().flex_none().child(self.card_action(
                        item,
                        ButtonKind::Outline,
                        cx,
                    ))),
            )
            .child(
                div()
                    .flex_1()
                    .max_h(px(two_lines()))
                    .overflow_hidden()
                    .ty(Type::Body)
                    .text_color(theme::text_subtle())
                    .child(item.blurb(110)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .children(
                        item.tags()
                            .into_iter()
                            .take(2)
                            .map(|(t, c)| ui::legend(t, rgb(c).into()).ty(Type::Caption)),
                    )
                    .child(div().flex_1())
                    .children(item.installs().map(installs)),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.select(&target, window, cx)))
    }

    /// One of the most installed, larger and on a flat wash of its brand's colour.
    fn featured_card(&self, item: &Item, cx: &mut Context<Self>) -> Stateful<Div> {
        let mark = item.mark();
        let hex = match mark {
            logos::Mark::Logo { hex, .. } | logos::Mark::Letter { hex, .. } => hex,
        };
        let tint = rgb(hex);
        let (by, verified) = item.by();
        let target = item.clone();
        div()
            .id(SharedString::from(format!("ext-feat-{}", item.key())))
            .flex()
            .flex_col()
            .h(px(FEATURED_H))
            .min_w_0()
            .overflow_hidden()
            .p(crate::ui::sp(18.))
            .rounded(crate::ui::rad(ui::RADIUS_XL))
            .border_1()
            .border_color(tint.opacity(0.28))
            .bg(tint.opacity(0.09))
            .cursor_pointer()
            .hover(|d| d.border_color(tint.opacity(0.5)))
            .child(
                div()
                    .flex()
                    .items_start()
                    .child(logos::tile(mark, 40.))
                    .child(div().flex_1())
                    .child(ui::pill("Popular", Tone::Accent.color())),
            )
            .child(
                div()
                    .mt(crate::ui::sp(14.))
                    .truncate()
                    .ty(Type::Title)
                    .text_color(theme::text())
                    .child(item.title()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .mt(crate::ui::sp(2.))
                    .min_w_0()
                    .child(by_line(by, verified))
                    .children(item.installs().map(|n| {
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .ty(Type::Caption)
                            .text_color(theme::text_faint())
                            .child("·")
                            .child(installs(n))
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .mt(crate::ui::sp(10.))
                    .max_h(px(two_lines()))
                    .overflow_hidden()
                    .ty(Type::Body)
                    .text_color(theme::text_muted())
                    .child(item.blurb(140)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .mt(crate::ui::sp(14.))
                    .child(self.card_action(item, ButtonKind::Primary, cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(4.))
                            .ty(Type::Label)
                            .text_color(theme::text_subtle())
                            .child("Details")
                            .child(icon("arrow-right01", 12.).text_color(theme::text_subtle())),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.select(&target, window, cx)))
    }

    /// The picked item: what it is, who makes it, and how to install it.
    fn render_detail(&self, cx: &mut Context<Self>) -> Option<Div> {
        let key = self.ms.selected.as_ref()?;
        let item = match self.market {
            Market::Plugins => Item::Plugin(
                self.plugin_catalog()?
                    .iter()
                    .find(|p| &p.id == key)?
                    .clone(),
            ),
            Market::Registry => Item::Server(self.ms.installing.as_ref()?.server.clone()),
            Market::Skills => Item::Skill(
                self.ms
                    .skills
                    .data
                    .as_ref()?
                    .iter()
                    .find(|k| &k.id == key)?
                    .clone(),
            ),
        };
        let installed = self.installed(&item);
        let (by, verified) = item.by();
        let mut panel = div()
            .id("ext-detail")
            .flex()
            .flex_col()
            .gap(crate::ui::sp(16.))
            .p(crate::ui::sp(20.))
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(crate::ui::sp(14.))
                    .child(logos::tile(item.mark(), 56.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(crate::ui::sp(4.))
                            .child(
                                div()
                                    .ty(Type::Subtitle)
                                    .text_color(theme::text())
                                    .child(item.title()),
                            )
                            .child(by_line(by, verified)),
                    )
                    .child(
                        ui::icon_button("ext-detail-close", "cancel01", 14., "Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx))),
                    ),
            );
        let mut facts = div().flex().flex_wrap().gap(crate::ui::sp(6.));
        for (t, c) in item.tags() {
            facts = facts.child(ui::tag(t, rgb(c)));
        }
        if let Some(n) = item.installs() {
            facts = facts.child(ui::badge(
                format!("{} installs", marketplace::short_count(n)),
                Tone::Neutral,
            ));
        }
        if installed {
            facts = facts.child(ui::badge("Installed", Tone::Success));
        }
        let description = match &item {
            Item::Skill(k) => match self.ms.located.get(&k.id) {
                Some(Located::Found(f)) if !f.description.is_empty() => f.description.clone(),
                _ => String::new(),
            },
            _ => item.description(),
        };
        panel = panel.child(facts).when(!description.is_empty(), |d| {
            d.child(
                div()
                    .ty(Type::Body)
                    .text_color(theme::text_muted())
                    .child(description),
            )
        });
        if let Some(picker) = self.render_where(cx) {
            panel = panel.child(picker);
        }
        panel = panel.children(self.render_sign_ins(&item, cx));
        match &item {
            Item::Plugin(p) => {
                let busy = self.op.as_ref() == Some(&p.name);
                let entry = p.clone();
                let mut links = div().flex().gap(crate::ui::sp(8.));
                if let Some(url) = p.homepage.clone() {
                    links = links.child(
                        ui::button_with(
                            "ext-detail-home",
                            Some("link-square02"),
                            "Homepage",
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                }
                panel = panel
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(ui::kv("Marketplace", p.marketplace.clone()))
                            .child(ui::kv("Id", p.id.clone()))
                            .when_some(p.category.clone(), |d, c| {
                                d.child(ui::kv("Category", capitalize(&c)))
                            }),
                    )
                    .child(links)
                    .child(div().flex_1())
                    .child(if installed {
                        div()
                            .ty(Type::Body)
                            .text_color(theme::text_subtle())
                            .child(match self.target_server() {
                                Some(s) => format!("Installed on {} for its user.", s.name),
                                None => "Installed for your user. Turn it on or off under Plugins."
                                    .to_string(),
                            })
                            .into_any_element()
                    } else if busy {
                        busy_row("Installing…").into_any_element()
                    } else {
                        ui::button_with(
                            "ext-detail-install",
                            Some("download04"),
                            match self.target_server() {
                                Some(s) => format!("Install on {}", s.name),
                                None => self.install_label().to_string(),
                            },
                            ButtonKind::Primary,
                            self.op.is_none(),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.install_plugin(&entry, cx)),
                        )
                        .into_any_element()
                    });
            }
            Item::Server(_) => {
                panel = panel.children(self.render_install_form(installed, cx));
            }
            Item::Skill(k) => panel = self.render_skill_detail(panel, k, installed, cx),
        }
        Some(
            div()
                .flex()
                .flex_none()
                .w(px(380.))
                .h_full()
                .border_l_1()
                .border_color(theme::border_subtle())
                .bg(theme::fill_raised())
                .child(panel),
        )
    }

    /// A skills.sh skill's folder on GitHub, its links, and Install.
    fn render_skill_detail(
        &self,
        panel: Stateful<Div>,
        k: &SkillEntry,
        installed: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let located = self.ms.located.get(&k.id).cloned();
        let found = match &located {
            Some(Located::Found(f)) => Some(f.clone()),
            _ => None,
        };
        let page = k.page();
        let mut links = div().flex().flex_wrap().gap(crate::ui::sp(8.));
        // A skill listed from its repository may not be on skills.sh at all.
        if self.ms.skills_repo.as_ref() != Some(&k.source) {
            links = links.child(
                ui::button_with(
                    "ext-skill-page",
                    Some("link-square02"),
                    "skills.sh",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(move |_, _, cx| cx.open_url(&page)),
            );
        }
        if let Some(url) = found.as_ref().map(SkillSource::url) {
            links = links.child(
                ui::button_with(
                    "ext-skill-github",
                    Some("github"),
                    "GitHub",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        let mut facts = div()
            .flex()
            .flex_col()
            .child(ui::kv("Repository", k.source.clone()))
            .child(ui::kv("Skill", k.skill_id.clone()));
        if let Some(f) = &found {
            facts = facts
                .child(ui::kv(
                    "Folder",
                    if f.dir.is_empty() {
                        "(the repository)".to_string()
                    } else {
                        f.dir.clone()
                    },
                ))
                .child(ui::kv(
                    "Files",
                    format!("{} ({})", f.files.len(), crate::ci::size_label(f.bytes())),
                ));
        }
        let state: Option<AnyElement> = match &located {
            Some(Located::Looking) => Some(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .ty(Type::Body)
                    .text_color(theme::text_subtle())
                    .child(ui::spinner(14., theme::text_faint()))
                    .child("Reading it on GitHub…")
                    .into_any_element(),
            ),
            Some(Located::Failed(e)) => Some(note(e.clone(), Tone::Danger).into_any_element()),
            _ => None,
        };
        let busy = self.op.as_deref() == Some(k.name.as_str());
        let entry = k.clone();
        let action = if installed {
            div()
                .ty(Type::Body)
                .text_color(theme::text_subtle())
                .child("Installed for your user. It shows under Skills.")
                .into_any_element()
        } else if busy {
            busy_row("Installing…").into_any_element()
        } else {
            ui::button_with(
                "ext-detail-install",
                Some("download04"),
                self.install_label(),
                ButtonKind::Primary,
                self.op.is_none() && !matches!(located, Some(Located::Failed(_))),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.install_skill(&entry, cx)))
            .into_any_element()
        };
        let package = found
            .as_ref()
            .filter(|f| f.package.len() > 1)
            .map(|f| self.render_package(&k.source, &f.package, cx));
        panel
            .children(state)
            .child(facts)
            .child(links)
            .children(package)
            .child(note(
                "A skill is instructions and sometimes scripts an agent follows. Read it on GitHub before you install one from someone you don't know.",
                Tone::Warning,
            ))
            .child(div().flex_1())
            .child(action)
    }

    /// The other skills in the picked skill's repository, and Install for them all.
    fn render_package(&self, source: &str, ids: &[String], cx: &mut Context<Self>) -> Div {
        let missing = ids.iter().filter(|id| !self.skill_installed(id)).count();
        let total = ids.len();
        let line = match missing {
            0 => format!("{source} has {total} skills. All of them are installed."),
            n if n == total => format!("{source} has {total} skills."),
            n => format!(
                "{source} has {total} skills. {} of them are installed.",
                total - n
            ),
        };
        let busy = self.op.as_deref() == Some(package_op(source).as_str());
        let action: Option<AnyElement> = if busy {
            Some(busy_row(format!("Installing {missing} skills…")).into_any_element())
        } else if missing > 0 {
            let label = if missing == total {
                format!("Install all {total}")
            } else {
                format!("Install {missing} more")
            };
            let source = source.to_string();
            Some(
                div()
                    .flex()
                    .child(
                        ui::button_with(
                            "ext-package-install",
                            Some("download04"),
                            label,
                            ButtonKind::Outline,
                            self.op.is_none(),
                        )
                        .tooltip(ui::tip_text(format!(
                            "Install every skill in {source}, {}",
                            self.ms.at.done()
                        )))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.install_package(&source, cx)),
                        ),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };
        let mut names = ids.to_vec();
        names.truncate(12);
        let more = total.saturating_sub(names.len());
        let mut list = names.join(", ");
        if more > 0 {
            list.push_str(&format!(" and {more} more"));
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(ui::overline("Package"))
            .child(div().ty(Type::Body).text_color(theme::text()).child(line))
            .child(
                div()
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child(list),
            )
            .children(action)
    }

    /// How to install the picked registry server: the way, what it needs, the command.
    /// The name an MCP server is added as. One that signs in and goes into a
    /// repository has the repository after its name (`github-acme-web`), so
    /// its sign-in is its own: Claude keeps one per name and address.
    fn server_name(&self, name: &str, sign_in: bool) -> String {
        if self.targets.target.is_some() || self.ms.at == Where::User || !sign_in {
            return name.to_string();
        }
        let slug = match &self.ms.repo_has {
            Some(has) => has.slug.clone(),
            // Not read yet: the folder's name, without asking git here.
            None => self
                .cwd
                .as_ref()
                .and_then(|d| d.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        mirror::suffixed(name, &slug)
    }

    /// Reads what the environment's repository has, when Install goes there.
    pub(super) fn load_repo_has(&mut self, cx: &mut Context<Self>) {
        if self.ms.at == Where::User {
            return;
        }
        let Some(dir) = self.cwd.clone() else { return };
        cx.spawn(async move |this, cx| {
            let d = dir.clone();
            let has = cx
                .background_spawn(crate::guard::task_or_default(async move {
                    mirror::repo_has(&d)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.cwd.as_ref() == Some(&dir) {
                    this.ms.repo_has = Some(has);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The folder a per-repository install goes into: the environment's.
    fn install_dir(&self) -> Option<PathBuf> {
        self.cwd.clone()
    }

    fn install_label(&self) -> &'static str {
        match self.ms.at {
            Where::User => "Install for every project",
            Where::Shared | Where::Mine => "Install in this repo",
        }
    }

    /// Picks where Install puts things on this computer.
    fn render_where(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.targets.target.is_some() || self.cwd.is_none() {
            return None;
        }
        let mut picker = ui::segmented();
        for (i, at) in Where::ALL.into_iter().enumerate() {
            picker = picker.child(
                ui::segment(("ext-where", i), None, at.label(), self.ms.at == at).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.ms.at = at;
                        this.load_repo_has(cx);
                        cx.notify();
                    }),
                ),
            );
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .child(ui::overline("Install in"))
                .child(div().flex().child(picker)),
        )
    }

    fn render_install_form(&self, installed: bool, cx: &mut Context<Self>) -> Option<Div> {
        let ins = self.ms.installing.as_ref()?;
        let s = &ins.server;
        let method = s.installs.get(ins.method)?;
        let mut out = div().flex().flex_col().gap(crate::ui::sp(14.));
        if let Some(url) = s.repository.clone().or_else(|| s.website.clone()) {
            out = out.child(
                div().flex().child(
                    ui::button_with(
                        "ext-ri-repo",
                        Some("link-square02"),
                        if s.repository.is_some() {
                            "Repository"
                        } else {
                            "Website"
                        },
                        ButtonKind::Ghost,
                        true,
                    )
                    .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
            );
        }
        if installed {
            return Some(
                out.child(
                    div().ty(Type::Body).text_color(theme::text_subtle()).child(
                        match self.target_server() {
                            Some(s) => {
                                format!("Claude Code on {} has a server of this name.", s.name)
                            }
                            None => {
                                "Already in Claude Code under this name: see it under MCP servers."
                                    .to_string()
                            }
                        },
                    ),
                ),
            );
        }
        out = out.child(ui::overline("Install"));
        if s.installs.len() > 1 {
            let mut methods = ui::segmented();
            for (i, m) in s.installs.iter().enumerate() {
                let label = match m {
                    Install::Remote { transport, .. } => {
                        format!("Remote · {}", transport.to_uppercase())
                    }
                    Install::Local { kind, .. } => format!("Local · {kind}"),
                };
                methods = methods.child(
                    ui::segment(("ext-method", i), None, label, i == ins.method)
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_method(i, cx))),
                );
            }
            out = out.child(div().flex().child(methods));
        }
        let sign_in = matches!(method, Install::Remote { headers, .. } if headers.is_empty());
        if ins.fields.is_empty() {
            out = out.child(div().ty(Type::Body).text_color(theme::text_subtle()).child(
                if sign_in && self.targets.target.is_some() {
                    "Nothing to fill in: you sign in on that machine once it's added."
                } else if sign_in {
                    "Nothing to fill in: you sign in with your account once it's added."
                } else {
                    "Nothing to fill in."
                },
            ));
        }
        for (input, field) in &ins.fields {
            out = out.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .child(
                                div()
                                    .ty(Type::Label)
                                    .text_color(theme::text())
                                    .child(input.name.clone()),
                            )
                            .when(input.required, |d| {
                                d.child(ui::badge("required", Tone::Warning))
                            })
                            .when(input.secret, |d| {
                                d.child(ui::badge("secret", Tone::Neutral))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .h(px(ui::INPUT_H))
                            .px(crate::ui::sp(10.))
                            .rounded(crate::ui::rad(ui::RADIUS))
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::background())
                            .ty(Type::Body)
                            .child(div().flex_1().min_w_0().child(field.clone())),
                    )
                    .when(!input.description.is_empty(), |d| {
                        d.child(
                            div()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .child(input.description.clone()),
                        )
                    }),
            );
        }
        let busy = self.op.is_some();
        Some(
            out.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(4.))
                    .child(
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(format!(
                                "Added as “{}”{}, {}. It runs{}:",
                                self.server_name(&s.short_name(), sign_in),
                                self.on_target(),
                                match self.targets.target {
                                    Some(_) => Where::User.done(),
                                    None => self.ms.at.done(),
                                },
                                if self.targets.target.is_some() {
                                    " there"
                                } else {
                                    ""
                                },
                            )),
                    )
                    .child(
                        div()
                            .px(crate::ui::sp(10.))
                            .py(crate::ui::sp(8.))
                            .rounded(crate::ui::rad(ui::RADIUS))
                            .bg(theme::background())
                            .ty(Type::Mono)
                            .text_color(theme::text_muted())
                            .child(marketplace::preview(
                                &self.server_name(&s.short_name(), sign_in),
                                method,
                                match self.targets.target {
                                    Some(_) => Where::User.claude(),
                                    None => self.ms.at.claude(),
                                },
                            )),
                    ),
            )
            .child(
                ui::button_with(
                    "ext-ri-go",
                    Some("download04"),
                    if sign_in && self.targets.target.is_none() {
                        "Install and sign in"
                    } else {
                        "Install"
                    },
                    ButtonKind::Primary,
                    !busy,
                )
                .when(!busy, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.finish_install(cx)))
                }),
            ),
        )
    }
}

/// Whether `s` is one of `item`'s servers and waits for a sign-in. `slug` is
/// the repository's, which a server added for one repository is named after.
fn waits_for_sign_in(item: &Item, s: &Server, slug: Option<&str>) -> bool {
    if s.health != Health::NeedsAuth || s.unused_connector() {
        return false;
    }
    match item {
        Item::Plugin(p) => s.scope == Scope::Plugin(p.name.clone()),
        Item::Server(r) => {
            let name = r.short_name();
            s.name == name || slug.is_some_and(|slug| s.name == mirror::suffixed(&name, slug))
        }
        Item::Skill(_) => false,
    }
}

/// A spinner and what is happening, where a button was.
fn busy_row(text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .ty(Type::Body)
        .text_color(theme::text_subtle())
        .child(ui::spinner(14., theme::text_faint()))
        .child(text.into())
}

/// The name a package's install runs under, so its button knows it is busy.
fn package_op(source: &str) -> String {
    format!("The skills of {source}")
}

/// Whether a field holds a key. Registry entries do not always mark one as
/// secret, so its name counts too.
fn looks_secret(input: &Input) -> bool {
    input.secret || crate::secrets::may_name_secret(&input.name)
}

/// Lines in the virtual list: the featured strip, the lines of cards, "Load more".
/// One row of Discover's list.
#[derive(Clone, Copy)]
enum Row {
    /// The last error or notice, and a server that needs you.
    Top,
    Featured,
    /// The list's title, Official or All, and the sort.
    Head,
    /// A line of cards, from this item on.
    Line(usize),
    /// "Load more" of the registry.
    More,
}

/// What the rows depend on: when it changes, the list starts over at the top.
#[derive(Clone, PartialEq)]
pub(super) struct RowsKey {
    market: Market,
    category: Option<String>,
    everything: bool,
    by_name: bool,
    query: String,
    per: usize,
    featured: usize,
}

/// What the rows were last laid out for: the key, how many there were, and
/// the top row's `top_signature` (None without one).
type Synced = Option<(RowsKey, usize, Option<u64>)>;

/// Keeps the list's rows in step: a new filter starts over at the top. The top
/// row coming or going (a notice after Install, the banner) and "Load more"
/// only splice, and a top row that says something else is measured again, so
/// the list keeps its place.
fn sync_rows(list: &ListState, last: &mut Synced, key: RowsKey, total: usize, top: Option<u64>) {
    match last.as_ref() {
        Some((k, n, had_top)) if *k == key => {
            let mut n = *n;
            match (had_top, top) {
                (None, Some(_)) => {
                    list.splice(0..0, 1);
                    n += 1;
                }
                (Some(_), None) => {
                    list.splice(0..1, 0);
                    n -= 1;
                }
                (Some(a), Some(b)) if *a != b => list.remeasure_items(0..1),
                _ => {}
            }
            if n != total {
                let keep = n.min(total).saturating_sub(1);
                list.splice(keep..n, total - keep);
            }
        }
        _ => list.reset(total),
    }
    *last = Some((key, total, top));
}

/// Drops what the rows were laid out for, so the next `sync_rows` resets.
fn forget_rows(last: &mut Synced) {
    *last = None;
}

/// Discover's list, its rows of different heights laid out only when in view.
pub(super) struct Rows(pub(super) ListState);

impl Default for Rows {
    fn default() -> Self {
        Rows(ListState::new(0, ListAlignment::Top, px(400.)))
    }
}

impl Rows {
    pub(super) fn top(&self) {
        self.0.scroll_to(ListOffset::default());
    }
}

/// Exactly two lines of body text, so a clipped description never shows half a third.
fn two_lines() -> f32 {
    2. * Type::Body.metrics().1
}

/// "↓ 12.4k": how many installed it.
fn installs(n: u64) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(4.))
        .ty(Type::Caption)
        .font_family(theme::FONT_MONO)
        .text_color(theme::text_subtle())
        .child(icon("download04", 12.).text_color(theme::text_faint()))
        .child(marketplace::short_count(n))
}

/// A line of cards, with the gap under it.
const ROW_H: f32 = 164.;
/// The narrowest a card gets before a line holds one fewer.
const MIN_CARD: f32 = 250.;
const FEATURED_H: f32 = 226.;
const GAP: f32 = 12.;

#[derive(Clone, Copy, PartialEq, Default)]
pub(super) enum Sort {
    #[default]
    Popular,
    Name,
}

/// A plugin or a registry server, as a card shows it.
#[derive(Clone)]
pub(super) enum Item {
    Plugin(PluginEntry),
    Server(RegistryServer),
    Skill(SkillEntry),
}

impl Item {
    fn key(&self) -> &str {
        match self {
            Item::Plugin(p) => &p.id,
            Item::Server(s) => &s.name,
            Item::Skill(k) => &k.id,
        }
    }

    fn title(&self) -> String {
        match self {
            Item::Plugin(p) => p.name.clone(),
            Item::Server(s) => {
                // "mcp" says nothing; the company does.
                let generic = matches!(
                    s.title.to_lowercase().as_str(),
                    "mcp" | "mcp server" | "mcp-server" | "server"
                );
                match (&s.official, generic) {
                    (Some(company), true) => company.clone(),
                    _ => s.title.clone(),
                }
            }
            Item::Skill(k) => k.name.clone(),
        }
    }

    fn by(&self) -> (Option<String>, bool) {
        match self {
            Item::Plugin(p) => (p.publisher.clone(), p.official),
            Item::Server(s) => match &s.official {
                Some(c) => (Some(c.clone()), true),
                None => (Some(s.publisher()), false),
            },
            Item::Skill(k) => match publishers::by_org(k.owner()) {
                Some(company) => (Some(company.to_string()), true),
                None => (Some(k.owner().to_string()), false),
            },
        }
    }

    fn mark(&self) -> logos::Mark {
        match self {
            Item::Plugin(p) => mark_by(&p.name, "", p.publisher.as_deref().filter(|_| p.official)),
            Item::Server(s) => mark_by(
                &s.title,
                s.website.as_deref().unwrap_or_default(),
                s.official.as_deref(),
            ),
            Item::Skill(k) => mark_by(
                &k.name,
                "",
                Some(publishers::by_org(k.owner()).unwrap_or(k.owner())),
            ),
        }
    }

    fn description(&self) -> String {
        match self {
            Item::Plugin(p) => p.description.clone(),
            Item::Server(s) => s.description.clone(),
            // skills.sh lists no descriptions; the details read the SKILL.md.
            Item::Skill(k) if k.description.is_empty() => format!("A skill from {}.", k.source),
            Item::Skill(k) => k.description.clone(),
        }
    }

    /// The description cut to `n` characters, so a card lays out little text.
    fn blurb(&self, n: usize) -> String {
        let d = self.description();
        if d.chars().count() > n {
            d.chars()
                .take(n - 1)
                .collect::<String>()
                .trim_end()
                .to_string()
                + "…"
        } else {
            d
        }
    }

    fn installs(&self) -> Option<u64> {
        match self {
            Item::Plugin(p) if p.installs > 0 => Some(p.installs),
            Item::Skill(k) if k.installs > 0 => Some(k.installs),
            _ => None,
        }
    }

    fn tags(&self) -> Vec<(String, u32)> {
        match self {
            Item::Plugin(p) => p
                .category
                .iter()
                .map(|c| (capitalize(c), category_color(c)))
                .collect(),
            Item::Server(s) => {
                let mut kinds: Vec<&str> = s.installs.iter().map(Install::kind).collect();
                kinds.dedup();
                let mut tags: Vec<(String, u32)> = kinds
                    .into_iter()
                    .map(|k| {
                        (
                            capitalize(k),
                            if k == "remote" { CAT_GREEN } else { CAT_BLUE },
                        )
                    })
                    .collect();
                if s.installs
                    .first()
                    .is_some_and(|i| i.inputs().iter().any(|i| i.secret))
                {
                    tags.push(("Needs a key".into(), CAT_AMBER));
                }
                tags
            }
            Item::Skill(_) => Vec::new(),
        }
    }
}

/// "✓ by Stripe" under a name.
fn by_line(by: Option<String>, verified: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(4.))
        .min_w_0()
        .ty(Type::Caption)
        .text_color(theme::text_subtle())
        .when_some(by, |d, by| {
            d.when(verified, |d| {
                d.child(icon("checkmark-circle02", 12.).text_color(Tone::Info.color()))
            })
            .child(
                div()
                    .truncate()
                    .when(verified, |d| d.text_color(theme::text_muted()))
                    .child(format!("by {by}")),
            )
        })
}

/// A marketplace card: logo, name and line under it, what it does, tags and the action.
pub(super) fn card(
    mark: logos::Mark,
    title: String,
    by: Option<(String, bool)>,
    sub: String,
    description: String,
    tags: Div,
    action: AnyElement,
) -> Div {
    let description: String = if description.chars().count() > 150 {
        description.chars().take(147).collect::<String>() + "…"
    } else {
        description
    };
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(crate::ui::sp(12.))
        .p(crate::ui::sp(16.))
        .rounded(crate::ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .hover(|d| d.border_color(theme::border_strong()))
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(12.))
                .child(logos::tile(mark, 40.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .ty(Type::Heading)
                                .text_color(theme::text())
                                .child(title),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(crate::ui::sp(4.))
                                .min_w_0()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .when_some(by, |d, (by, verified)| {
                                    d.when(verified, |d| {
                                        d.child(
                                            icon("checkmark-circle02", 12.)
                                                .text_color(Tone::Info.color()),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_none()
                                            .when(verified, |d| d.text_color(theme::text_muted()))
                                            .child(format!("by {by}")),
                                    )
                                    .when(!sub.is_empty(), |d| d.child("·"))
                                })
                                .child(div().truncate().child(sub)),
                        ),
                ),
        )
        // Up to three lines; the line's cards stretch to the tallest one.
        .child(
            div()
                .flex_1()
                .max_h(px(54.))
                .overflow_hidden()
                .ty(Type::Body)
                .text_color(theme::text_muted())
                .child(description),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .child(tags.flex_1().min_w_0().overflow_hidden())
                .child(div().flex_none().child(action)),
        )
}

/// The item's own logo, else its publisher's mark (logo, or initial in its color), else its own initial.
fn mark_by(name: &str, hint: &str, publisher: Option<&str>) -> logos::Mark {
    let own = logos::mark(name, hint);
    match (own, publisher) {
        (logos::Mark::Letter { .. }, Some(p)) => logos::mark(p, ""),
        _ => own,
    }
}

/// Cards `per` to a line, the last line padded so every card is as wide.
/// How narrow a card in `grid` gets before its line holds one card fewer.
const CARD_MIN_W: f32 = 200.;

pub(super) fn grid(cards: Vec<AnyElement>, per: usize) -> Div {
    // `per` cards a line, or fewer when they would be narrower than
    // CARD_MIN_W. Every cell has the same basis and the same growth, and
    // `per` empty cells with no height follow the cards, so a short last line
    // keeps the others' widths. Cards fill their cell's height.
    let cell = || {
        div()
            .flex()
            .flex_col()
            .flex_grow(1.)
            .flex_basis(gpui::relative(1. / (per as f32 + 1.)))
            .min_w(px(CARD_MIN_W))
    };
    // The space between lines is each card's bottom margin, not a row gap,
    // so a line of empty cells adds none; the last line's is taken back.
    div()
        .flex()
        .flex_wrap()
        .gap_x(crate::ui::sp(12.))
        .mb(-crate::ui::sp(12.))
        .children(
            cards
                .into_iter()
                .map(|c| cell().mb(crate::ui::sp(12.)).child(c)),
        )
        .children((0..per).map(|_| cell()))
}

fn category_color(c: &str) -> u32 {
    match c {
        "development" => CAT_BLUE,
        "productivity" => CAT_GREEN,
        "database" => CAT_VIOLET,
        "monitoring" => CAT_AMBER,
        "security" => CAT_CORAL,
        "deployment" => CAT_CYAN,
        "design" => theme::CATEGORY[6],
        _ => CAT_SAND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(query: &str) -> RowsKey {
        RowsKey {
            market: Market::Plugins,
            category: None,
            everything: false,
            by_name: false,
            query: query.to_string(),
            per: 2,
            featured: 2,
        }
    }

    /// The list scrolled to row `ix`, its rows synced once.
    fn scrolled(total: usize, top: Option<u64>, ix: usize) -> (ListState, Synced) {
        let list = ListState::new(0, ListAlignment::Top, px(400.));
        let mut last = None;
        sync_rows(&list, &mut last, key(""), total, top);
        list.scroll_to(ListOffset {
            item_ix: ix,
            offset_in_item: px(30.),
        });
        (list, last)
    }

    #[test]
    fn a_notice_appearing_keeps_the_place() {
        // Install on a card further down clears the notice, then sets one.
        let (list, mut last) = scrolled(12, None, 7);
        sync_rows(&list, &mut last, key(""), 13, Some(1));
        assert_eq!(list.item_count(), 13);
        assert_eq!(
            list.logical_scroll_top().item_ix,
            8,
            "same card, one row lower"
        );
        assert_eq!(list.logical_scroll_top().offset_in_item, px(30.));
        sync_rows(&list, &mut last, key(""), 12, None);
        assert_eq!(list.item_count(), 12);
        assert_eq!(list.logical_scroll_top().item_ix, 7);
    }

    #[test]
    fn load_more_keeps_the_place() {
        let (list, mut last) = scrolled(10, Some(1), 6);
        sync_rows(&list, &mut last, key(""), 15, Some(1));
        assert_eq!(list.item_count(), 15);
        assert_eq!(list.logical_scroll_top().item_ix, 6);
    }

    #[test]
    fn a_search_back_from_no_results_starts_at_the_top() {
        // Scrolled down, a search with no matches draws no list, then the
        // first words come back: same key, but not the old place.
        let (list, mut last) = scrolled(10, None, 6);
        forget_rows(&mut last);
        sync_rows(&list, &mut last, key(""), 10, None);
        assert_eq!(list.logical_scroll_top().item_ix, 0);
    }

    #[test]
    fn a_new_notice_beside_the_banner_keeps_the_place() {
        // The banner stays; Install adds a notice above it, so only its height changes.
        let (list, mut last) = scrolled(10, Some(1), 6);
        sync_rows(&list, &mut last, key(""), 10, Some(2));
        assert_eq!(list.item_count(), 10);
        assert_eq!(list.logical_scroll_top().item_ix, 6);
        assert_eq!(last.as_ref().map(|l| l.2), Some(Some(2)));
    }

    #[test]
    fn a_new_search_starts_at_the_top() {
        let (list, mut last) = scrolled(10, None, 6);
        sync_rows(&list, &mut last, key("github"), 4, None);
        assert_eq!(list.item_count(), 4);
        assert_eq!(list.logical_scroll_top().item_ix, 0);
    }

    fn server(name: &str, scope: Scope, health: Health) -> Server {
        Server {
            name: name.into(),
            scope,
            transport: "http".into(),
            target: "https://example.com/mcp".into(),
            headers: Vec::new(),
            env: Vec::new(),
            health,
            rules: Vec::new(),
            diagnosis: None,
        }
    }

    fn plugin(name: &str) -> Item {
        Item::Plugin(
            serde_json::from_value(json!({
                "id": format!("{name}@claude-plugins-official"), "name": name,
                "description": "", "marketplace": "claude-plugins-official",
                "category": null, "installs": 0, "homepage": null, "installed": true,
            }))
            .unwrap(),
        )
    }

    fn registry(name: &str) -> Item {
        Item::Server(
            serde_json::from_value(json!({
                "name": name, "title": "", "description": "", "version": "1",
                "website": null, "repository": null, "installs": [],
            }))
            .unwrap(),
        )
    }

    #[test]
    fn a_plugins_server_waiting_for_a_sign_in_is_found() {
        let posthog = plugin("posthog");
        let s = server(
            "plugin:posthog:posthog",
            Scope::Plugin("posthog".into()),
            Health::NeedsAuth,
        );
        assert!(waits_for_sign_in(&posthog, &s, None));
        // Signed in already, or another plugin's server.
        let ok = server(
            "plugin:posthog:posthog",
            Scope::Plugin("posthog".into()),
            Health::Connected,
        );
        assert!(!waits_for_sign_in(&posthog, &ok, None));
        assert!(!waits_for_sign_in(&plugin("stripe"), &s, None));
    }

    #[test]
    fn a_registry_server_is_found_by_its_name_or_its_repos() {
        let linear = registry("app.linear/linear");
        let mine = server("linear", Scope::User, Health::NeedsAuth);
        assert!(waits_for_sign_in(&linear, &mine, None));
        let repo = server(
            &mirror::suffixed("linear", "arakun"),
            Scope::Local,
            Health::NeedsAuth,
        );
        assert!(waits_for_sign_in(&linear, &repo, Some("arakun")));
        assert!(!waits_for_sign_in(&linear, &repo, None));
    }

    #[test]
    fn an_unused_claude_ai_connector_is_not_asked_for() {
        // Offered by the account, not installed from here.
        let gmail = server("claude.ai Gmail", Scope::ClaudeAi, Health::NeedsAuth);
        assert!(!waits_for_sign_in(
            &registry("x/claude.ai Gmail"),
            &gmail,
            None
        ));
    }
}
