//! Discover, the store half of the Marketplace tab: Claude Code plugins its
//! marketplaces offer, and the official MCP Registry with an install form for
//! what a server needs.

use std::collections::HashMap;

use std::rc::Rc;

use gpui::{Hsla, ListAlignment, ListOffset, ListState, list};

use super::*;
use serde_json::json;

use crate::agent::marketplace::{self, Input, Install, PluginEntry, RegistryServer};

/// Wait this long after the last keystroke before asking the registry.
const DEBOUNCE: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Market {
    Plugins,
    Registry,
}

/// A registry server being installed: the way picked and what the user typed.
pub(super) struct Installing {
    server: RegistryServer,
    method: usize,
    fields: Vec<(Input, Entity<TextInput>)>,
}

#[derive(Default)]
pub(super) struct MarketState {
    pub(super) catalog: Option<Vec<PluginEntry>>,
    pub(super) catalog_loading: bool,
    pub(super) catalog_error: Option<SharedString>,
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
    pub(super) official: Option<Vec<RegistryServer>>,
    pub(super) official_loading: bool,
    /// The plugin id or server name whose details show on the right.
    pub(super) selected: Option<String>,
    pub(super) sort: Sort,
    pub(super) rows: Rows,
    /// What the rows were laid out for, how many there were, and whether the
    /// top row (notes, the needs-you banner) was one of them.
    rows_for: std::cell::RefCell<Synced>,
    pub(super) last_width: std::cell::Cell<f32>,
    pub(super) check_pending: std::cell::Cell<bool>,
}

impl ExtensionsView {
    /// Reads what it needs the first time the Marketplace shows.
    pub(super) fn open_market(&mut self, cx: &mut Context<Self>) {
        // A server says which plugins and MCP servers it has in one answer.
        self.ask_remote(false, cx);
        match self.market {
            Market::Plugins if self.targets.target.is_some() => {}
            Market::Plugins if self.ms.catalog.is_none() && !self.ms.catalog_loading => {
                self.load_catalog(cx)
            }
            Market::Registry if !self.ms.everything => self.load_official(cx),
            Market::Registry if self.ms.registry.is_none() && !self.ms.registry_loading => {
                self.search_registry(false, cx)
            }
            _ => {}
        }
    }

    /// The official servers: the last list at once, a new one when a day old.
    fn load_official(&mut self, cx: &mut Context<Self>) {
        if self.ms.official_loading {
            return;
        }
        let mut stale = true;
        if self.ms.official.is_none() {
            if let Some((list, fresh)) = marketplace::cached_official() {
                self.ms.official = Some(list);
                stale = !fresh;
            }
        } else {
            stale = false;
        }
        if !stale {
            return;
        }
        self.ms.official_loading = true;
        self.ms.registry_error = None;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(crate::guard::task(async move {
                    marketplace::official_servers()
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ms.official_loading = false;
                match found {
                    Ok(list) => this.ms.official = Some(list),
                    Err(e) => this.ms.registry_error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        if self.ms.catalog.is_none() {
            self.ms.catalog = marketplace::cached_plugins();
        }
        self.ms.catalog_loading = true;
        self.ms.catalog_error = None;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(crate::guard::task(
                    async move { marketplace::plugins(&cwd) },
                ))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ms.catalog_loading = false;
                match found {
                    Ok(list) => this.ms.catalog = Some(list),
                    Err(e) => this.ms.catalog_error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Asks the registry for the search box's words, after a pause in typing.
    pub(super) fn query_changed(&mut self, cx: &mut Context<Self>) {
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
                    Err(e) => this.ms.registry_error = Some(format!("{e:#}").into()),
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
        let Some(cwd) = self.cwd.clone() else { return };
        self.run_op(
            name,
            "is installed and on in every project",
            move || marketplace::install_plugin(&cwd, &id),
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
        if self.targets.target.is_some() {
            // Its sign-in opens a browser, so it is done on that machine.
            let mut done = format!("was added{} for every project there.", self.on_target());
            if sign_in {
                done.push_str(&format!(" Sign in to it there: {login}"));
            }
            let msg = json!({
                "type": "install_server", "name": n, "install": method, "values": values,
            });
            self.ms.installing = None;
            self.install_remote(name, n, msg, done, cx);
            return;
        }
        let Some(cwd) = self.cwd.clone() else { return };
        self.ms.installing = None;
        self.run_op(
            name,
            "was added for every project",
            move || marketplace::install_server(&cwd, &n, &method, &values).map(|_| ()),
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
                    &self.ms.official
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
        match item {
            // A server's list says what is installed there; Claude's files here
            // know of an install the list was read before.
            Item::Plugin(p) => {
                p.installed
                    || self.targets.target.is_none()
                        && self
                            .claude
                            .as_ref()
                            .is_some_and(|x| x.plugins.iter().any(|i| i.id == p.id))
            }
            Item::Server(s) => self.remote_has_server(&s.short_name()).unwrap_or_else(|| {
                self.claude
                    .as_ref()
                    .is_some_and(|x| x.servers.iter().any(|i| i.name == s.short_name()))
            }),
        }
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
            Market::Registry => self.ms.official.as_ref().map(Vec::len),
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
    /// for the left column.
    pub(super) fn render_market_filters(&self, cx: &mut Context<Self>) -> Option<Div> {
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
                for s in self.ms.official.iter().flatten() {
                    if let Some(c) = &s.official {
                        add(c);
                    }
                }
                "Companies"
            }
            Market::Registry => return None,
        };
        if groups.is_empty() {
            return None;
        }
        groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let total = self.discover_count(self.market).unwrap_or(0);
        let mut out = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(crate::ui::sp(2.))
            .child(nav_heading(heading, true))
            .child(
                nav_row(
                    "ext-cat-all",
                    "dashboard-square01",
                    "All",
                    Some(total),
                    self.ms.category.is_none(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ms.category = None;
                    this.ms.rows.top();
                    cx.notify();
                })),
            );
        for (i, (g, n)) in groups.into_iter().enumerate() {
            let on = self.ms.category.as_ref() == Some(&g);
            let row = match self.market {
                Market::Plugins => nav_dot_row(
                    ("ext-cat", i),
                    rgb(category_color(&g)).into(),
                    capitalize(&g),
                    n,
                    on,
                ),
                Market::Registry => {
                    nav_mark_row(("ext-cat", i), logos::mark(&g, ""), g.clone(), n, on)
                }
            };
            out = out.child(row.on_click(cx.listener(move |this, _, _, cx| {
                this.ms.category = Some(g.clone());
                this.ms.rows.top();
                cx.notify();
            })));
        }
        Some(out)
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
            Market::Registry => self.ms.official.is_none(),
        };
        let error = match self.market {
            Market::Plugins => self.catalog_error(),
            Market::Registry => self.ms.registry_error.clone(),
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
                    None => ui::loading(
                        "ext-market-loading",
                        match self.market {
                            Market::Plugins => self.catalog_wait(),
                            Market::Registry if self.ms.everything => {
                                "Asking the MCP Registry…".into()
                            }
                            Market::Registry => "Gathering what each company publishes…".into(),
                        },
                    )
                    .into_any_element(),
                }
            } else {
                ui::empty_state(
                    "search01",
                    "Nothing matches",
                    match self.market {
                        Market::Registry if self.ms.everything => {
                            "It searches server names: try a product name like github or postgres."
                        }
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
                    plugins: self.market == Market::Plugins,
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
                    ui::spinner("ext-registry-more-busy", 14., theme::text_faint())
                        .into_any_element()
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
        self.claude.iter().filter(move |_| here).flat_map(|x| {
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
                Market::Registry => self.ms.official_loading,
            };
        let error = match self.market {
            Market::Plugins => self.catalog_error(),
            // The registry is read from here; installing from it needs the server.
            Market::Registry => self.reach_error().or(self.ms.registry_error.clone()),
        };
        let title = match (self.market, &self.ms.category) {
            (_, Some(c)) if self.market == Market::Plugins => capitalize(c),
            (_, Some(c)) => c.clone(),
            (Market::Plugins, None) if self.ms.everything => "All plugins".to_string(),
            (Market::Plugins, None) => "Official plugins".to_string(),
            (Market::Registry, None) if self.ms.everything => "MCP Registry".to_string(),
            (Market::Registry, None) => "Official MCP servers".to_string(),
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
                if self.market == Market::Plugins {
                    "Popular"
                } else {
                    "By company"
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
                    .when(!loading, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .ty(Type::Label)
                                .text_color(theme::text_faint())
                                .child(n.to_string()),
                        )
                    })
                    .when(busy, |d| {
                        d.child(ui::spinner("ext-market-busy", 13., theme::text_faint()))
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap(crate::ui::sp(8.))
                            .child(scope)
                            .child(sort),
                    ),
            )
            .when(!loading, |d| {
                d.when_some(error, |d, e| d.child(note(e, Tone::Danger)))
            })
    }

    /// What a card offers: Installed, a spinner, or Install.
    fn card_action(&self, item: &Item, kind: ButtonKind, cx: &mut Context<Self>) -> AnyElement {
        if self.installed(item) {
            return ui::state_pill("checkmark-circle02", "Installed", Tone::Success.color())
                .into_any_element();
        }
        if let Item::Plugin(p) = item
            && self.op.as_deref() == Some(p.name.as_str())
        {
            return ui::spinner(
                SharedString::from(format!("ext-card-busy-{}", item.key())),
                14.,
                theme::text_faint(),
            )
            .into_any_element();
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
        .on_click(cx.listener(move |this, _, window, cx| {
            // The card under the button opens the details; Install alone installs.
            cx.stop_propagation();
            match &target {
                Item::Plugin(p) => this.install_plugin(p, cx),
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
                        ui::icon_button("ext-detail-close", "cancel01", 14.)
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
        panel = panel.child(facts).child(
            div()
                .ty(Type::Body)
                .text_color(theme::text_muted())
                .child(item.description()),
        );
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
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(ui::spinner("ext-detail-busy", 14., theme::text_faint()))
                            .child("Installing…")
                            .into_any_element()
                    } else {
                        ui::button_with(
                            "ext-detail-install",
                            Some("download04"),
                            match self.target_server() {
                                Some(s) => format!("Install on {}", s.name),
                                None => "Install for every project".to_string(),
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

    /// How to install the picked registry server: the way, what it needs, the command.
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
                                "Added as “{}”{}, for every project. It runs{}:",
                                s.short_name(),
                                self.on_target(),
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
                            .child(marketplace::preview(&s.short_name(), method)),
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
    plugins: bool,
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
}

impl Item {
    fn key(&self) -> &str {
        match self {
            Item::Plugin(p) => &p.id,
            Item::Server(s) => &s.name,
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
        }
    }

    fn by(&self) -> (Option<String>, bool) {
        match self {
            Item::Plugin(p) => (p.publisher.clone(), p.official),
            Item::Server(s) => match &s.official {
                Some(c) => (Some(c.clone()), true),
                None => (Some(s.publisher()), false),
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
        }
    }

    fn description(&self) -> String {
        match self {
            Item::Plugin(p) => p.description.clone(),
            Item::Server(s) => s.description.clone(),
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

fn nav_base(id: impl Into<gpui::ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(30.))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .cursor_pointer()
        .ty(Type::Label)
        .when(on, |d| {
            d.bg(theme::fill_selected()).text_color(theme::text())
        })
        .when(!on, |d| {
            d.text_color(theme::text_muted())
                .hover(|d| d.bg(theme::fill_hover()).text_color(theme::text()))
        })
}

fn nav_count(n: Option<usize>) -> Div {
    div()
        .flex_none()
        .ty(Type::Caption)
        .text_color(theme::text_faint())
        .children(n.map(|n| n.to_string()))
}

pub(super) fn nav_row(
    id: impl Into<gpui::ElementId>,
    icon_name: &str,
    label: &str,
    n: Option<usize>,
    on: bool,
) -> Stateful<Div> {
    nav_base(id, on)
        .child(icon(icon_name, 14.).text_color(if on {
            theme::text()
        } else {
            theme::text_subtle()
        }))
        .child(div().flex_1().min_w_0().truncate().child(label.to_string()))
        .child(nav_count(n))
}

/// A row of the left column whose count says how many need you.
pub(super) fn nav_alert_row(
    id: impl Into<gpui::ElementId>,
    icon_name: &str,
    label: &str,
    n: usize,
    on: bool,
) -> Stateful<Div> {
    let warn = Tone::Warning.color();
    nav_base(id, on)
        .child(icon(icon_name, 14.).text_color(if on {
            theme::text()
        } else {
            theme::text_subtle()
        }))
        .child(div().flex_1().min_w_0().truncate().child(label.to_string()))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(5.))
                .ty(Type::Caption)
                .text_color(warn)
                .child(div().size(px(6.)).rounded_full().bg(warn))
                .child(n.to_string()),
        )
}

fn nav_dot_row(
    id: impl Into<gpui::ElementId>,
    color: Hsla,
    label: String,
    n: usize,
    on: bool,
) -> Stateful<Div> {
    nav_base(id, on)
        .child(div().flex_none().size(px(8.)).rounded_full().bg(color))
        .child(div().flex_1().min_w_0().truncate().child(label))
        .child(nav_count(Some(n)))
}

fn nav_mark_row(
    id: impl Into<gpui::ElementId>,
    mark: logos::Mark,
    label: String,
    n: usize,
    on: bool,
) -> Stateful<Div> {
    nav_base(id, on)
        .child(logos::tile(mark, 18.))
        .child(div().flex_1().min_w_0().truncate().child(label))
        .child(nav_count(Some(n)))
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
            plugins: true,
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
}
