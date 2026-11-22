//! The People tab: who sent events in the last 30 days, searched by email or
//! distinct_id, and one person's profile next to the list: identity,
//! properties, cohorts, the flags they get, 14 days of events, their
//! sessions (Watch opens the replay) and a timeline.
//!
//! The list needs query:read; the profile also needs person:read. When the
//! key lacks one, the tab names it and the other tabs keep working.

use super::*;
use crate::posthog::events as ev;
use crate::posthog::people::{self as pp, Kind, Person, Profile};
use crate::posthog::query::{self, Debounce, Paging};

const PAGE: u32 = 50;
/// Properties shown before "All".
const TOP_PROPS: usize = 8;

pub enum PeopleEvent {
    /// Open a URL in a browser tab.
    Open(String),
}

pub struct PeopleTab {
    target: Option<Target>,
    search: Entity<TextInput>,
    people: Vec<Person>,
    page: Paging,
    until: u64,
    more: bool,
    total: Option<u64>,
    loading: bool,
    loading_more: bool,
    loaded: bool,
    error: Option<ApiError>,
    spent: u32,
    debounce: Debounce,
    /// (person_id, distinct_id) of the open profile.
    selected: Option<(String, String)>,
    profile: Option<Profile>,
    profile_loading: bool,
    profile_error: Option<ApiError>,
    all_props: bool,
    timeline: Option<Kind>,
    list_scroll: ScrollHandle,
    _load: Option<Task<()>>,
    _more: Option<Task<()>>,
    _profile: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PeopleEvent> for PeopleTab {}

impl PeopleTab {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search by email or distinct_id", cx));
        let subs = vec![
            cx.subscribe(&search, |this: &mut Self, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Changed | InputEvent::Submit) {
                    this.changed(cx);
                }
            }),
        ];
        Self {
            target: None,
            search,
            people: Vec::new(),
            page: Paging::first(PAGE),
            until: 0,
            more: false,
            total: None,
            loading: false,
            loading_more: false,
            loaded: false,
            error: None,
            spent: 0,
            debounce: Debounce::default(),
            selected: None,
            profile: None,
            profile_loading: false,
            profile_error: None,
            all_props: false,
            timeline: None,
            list_scroll: ScrollHandle::new(),
            _load: None,
            _more: None,
            _profile: None,
            _subs: subs,
        }
    }

    pub fn set_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        if self.target == target {
            return;
        }
        self._profile = None;
        self._more = None;
        self.loading_more = false;
        self.profile_loading = false;
        self.more = false;
        // Another project's cutoff would hide this one's newest people.
        self.until = 0;
        self.target = target;
        self.people.clear();
        self.total = None;
        self.error = None;
        self.loaded = false;
        self.selected = None;
        self.profile = None;
        self.profile_error = None;
        self.debounce.bump();
        // Not set_text: its Changed event would schedule a read for a tab
        // that is not open; show() starts the first one when People opens.
        self.search.update(cx, |i, cx| i.clear(cx));
        cx.notify();
    }

    pub fn show(&mut self, cx: &mut Context<Self>) {
        if self.target.is_some() && !self.loaded {
            self.load(query::FRESH, cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.target.is_none() {
            return;
        }
        self.load(0, cx);
        if let Some((pid, did)) = self.selected.clone() {
            self.load_profile(pid, did, 0, cx);
        }
    }

    /// Opens a person picked elsewhere (an event's Person button).
    pub fn open_person(&mut self, person_id: String, distinct_id: String, cx: &mut Context<Self>) {
        self.show(cx);
        self.load_profile(person_id, distinct_id, query::FRESH, cx);
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        // The page on screen belongs to the search that made it: a Load more
        // under the new text would page a different result set.
        self.more = false;
        self.people.clear();
        self.total = None;
        self.selected = None;
        self.profile = None;
        self.profile_error = None;
        self._profile = None;
        self.profile_loading = false;
        let generation = self.debounce.bump();
        self.loading = true;
        cx.notify();
        self._load = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(query::DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if this.debounce.is_current(generation) {
                    this.load(query::FRESH, cx);
                }
            })
            .ok();
        }));
    }

    fn load(&mut self, fresh: u64, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let generation = self.debounce.bump();
        self.loaded = true;
        self.loading = true;
        cx.notify();
        // The cutoff is part of the query text, so a load inside the cache's
        // window keeps the last one and finds its answer on disk; a refresh
        // or an older one moves it to now.
        let now = crate::model::unix_now();
        let until = if fresh > 0 && now.saturating_sub(self.until) < fresh {
            self.until
        } else {
            now
        };
        let text = self.search.read(cx).text().trim().to_string();
        self._load = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    let (s, id) = (&t.session, t.project.id);
                    let (list, total) = std::thread::scope(|scope| {
                        let l = scope.spawn(|| {
                            query::run(
                                s,
                                id,
                                &pp::list_query(&text, Paging::first(PAGE), until, None),
                                fresh,
                            )
                        });
                        let n = scope
                            .spawn(|| query::run(s, id, &pp::total_query(&text, until), fresh));
                        (posthog::joined(l), posthog::joined(n))
                    });
                    let (list, total) = (list?, total?);
                    Ok::<_, ApiError>((list, total))
                }))
                .await;
            this.update(cx, |this, cx| {
                if !this.debounce.is_current(generation) {
                    return;
                }
                this.loading = false;
                match got {
                    Ok((list, total)) => {
                        // As in the events tab: the page resets with the rows,
                        // so a failed refresh keeps what it had.
                        this.until = until;
                        this.page = Paging::first(PAGE);
                        let (people, more) = this.page.split(pp::parse_list(&list.rows));
                        this.people = people;
                        this.more = more;
                        this.total = total.rows.first().map(|r| posthog::num(&r[0]));
                        this.spent = list.spent + total.spent;
                        this.error = None;
                        if this.selected.is_none()
                            && let Some(p) = this.people.first()
                        {
                            let (pid, did) = (p.id.clone(), p.distinct_id.clone());
                            this.load_profile(pid, did, query::FRESH, cx);
                        }
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        // `loading` covers the debounce and the first-page read: a page read
        // here would use the new search with the old page's offset, and bump
        // the generation the first page needs to apply.
        if self.loading || self.loading_more || !self.more {
            return;
        }
        self.loading_more = true;
        cx.notify();
        let page = self.page.next();
        let generation = self.debounce.bump();
        let after = self
            .people
            .last()
            .map(|p| (p.last_stamp.as_str(), p.id.as_str()));
        let q = pp::list_query(self.search.read(cx).text().trim(), page, self.until, after);
        self._more = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    query::run(&t.session, t.project.id, &q, query::FRESH)
                }))
                .await;
            this.update(cx, |this, cx| {
                this.loading_more = false;
                if !this.debounce.is_current(generation) {
                    return;
                }
                match got {
                    Ok(a) => {
                        let (rows, more) = page.split(pp::parse_list(&a.rows));
                        this.people.extend(rows);
                        this.more = more;
                        this.page = page;
                        this.spent = a.spent;
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_profile(&mut self, pid: String, did: String, fresh: u64, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let key = (pid.clone(), did.clone());
        if self.selected.as_ref() != Some(&key) {
            self.profile = None;
            self.all_props = false;
        }
        self.selected = Some(key.clone());
        self.profile_loading = true;
        self.profile_error = None;
        cx.notify();
        self._profile = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    pp::load_profile(&t.session, &t.project, &pid, &did, fresh)
                }))
                .await;
            this.update(cx, |this, cx| {
                if this.selected.as_ref() != Some(&key) {
                    return;
                }
                this.profile_loading = false;
                match got {
                    Ok(p) => {
                        this.spent = p.spent;
                        this.profile = Some(p);
                    }
                    Err(e) => this.profile_error = Some(e),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn open(&self, path: &str, cx: &mut Context<Self>) {
        if let Some(t) = &self.target {
            cx.emit(PeopleEvent::Open(t.project.url(path)));
        }
    }

    /// The scope a part of the tab was refused for.
    fn missing(&self) -> Option<&str> {
        [&self.error, &self.profile_error]
            .into_iter()
            .find_map(|e| match e {
                Some(ApiError::MissingScope(s)) => Some(s.as_str()),
                _ => None,
            })
    }
}

// ---------------------------------------------------------------------------
// Drawing.

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .rounded(ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::fill_raised())
        .overflow_hidden()
}

fn label(text: impl Into<SharedString>) -> gpui::Div {
    ui::text(Type::Overline, theme::text_muted(), text)
}

fn kv_row(k: String, v: String, first: bool) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(ui::sp(12.))
        .min_h(px(40.))
        .px(ui::sp(16.))
        .when(!first, |d| {
            d.border_t_1().border_color(theme::border_subtle())
        })
        .child(
            ui::text(Type::Mono, theme::text_subtle(), k)
                .w(px(180.))
                .flex_none()
                .truncate(),
        )
        .child(
            ui::text(Type::Mono, theme::text(), v)
                .flex_1()
                .min_w_0()
                .truncate(),
        )
}

fn kv_card(rows: Vec<(String, String)>) -> gpui::Div {
    card().children(
        rows.into_iter()
            .enumerate()
            .map(|(i, (k, v))| kv_row(k, v, i == 0)),
    )
}

/// "18 min", "1 h 12 min".
fn duration(secs: u64) -> String {
    let m = secs.div_ceil(60).max(1);
    if m < 60 {
        format!("{m} min")
    } else {
        format!("{} h {} min", m / 60, m % 60)
    }
}

impl PeopleTab {
    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let left = self
            .target
            .as_ref()
            .map_or(query::HOURLY_QUERIES, |t| query::left(&t.session.id, now));
        let mut list = card().p(ui::sp(6.)).gap(ui::sp(2.));
        if self.people.is_empty() {
            list = list.child(
                quiet(if self.loading {
                    "Reading people…"
                } else {
                    "Nobody with events in the last 30 days matches."
                })
                .px(ui::sp(12.)),
            );
        }
        for (i, p) in self.people.iter().enumerate() {
            let on = self.selected.as_ref().is_some_and(|s| s.0 == p.id);
            let sub = if p.email.is_some() && p.first_seen > 0 {
                format!(
                    "first seen {} · last seen {}",
                    day(p.first_seen),
                    short_ago(p.last_seen, now)
                )
            } else {
                format!("anonymous · last seen {}", short_ago(p.last_seen, now))
            };
            let (pid, did) = (p.id.clone(), p.distinct_id.clone());
            list = list.child(
                div()
                    .id(("ph-pp-row", i))
                    .flex()
                    .items_center()
                    .gap(ui::sp(12.))
                    .px(ui::sp(12.))
                    .py(ui::sp(8.))
                    .rounded(ui::rad(ui::RADIUS))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::fill_selected()))
                    .hover(|d| d.bg(theme::fill_hover()))
                    .child(ui::avatar(p.name(), 28.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Body)
                                    .text_color(theme::text())
                                    .child(crate::redact::value(p.name())),
                            )
                            .child(ui::text(Type::Caption, theme::text_subtle(), sub).truncate()),
                    )
                    .child(ui::text(
                        Type::Mono,
                        theme::text_muted(),
                        ev::group_digits(p.events),
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.load_profile(pid.clone(), did.clone(), query::FRESH, cx)
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(10.))
            .w(px(400.))
            .flex_none()
            .child(
                ui::field()
                    .child(icon("search01", 14.).text_color(theme::text_subtle()))
                    .child(self.search.clone()),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .px(ui::sp(4.))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        match self.total {
                            Some(n) => format!(
                                "{} {} seen in 30 days",
                                ev::group_digits(n),
                                if n == 1 { "person" } else { "people" }
                            ),
                            None => "People seen in 30 days".into(),
                        },
                    ))
                    .child(ui::text(Type::Caption, theme::text_subtle(), "Events")),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(ui::sp(4.))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        query::budget_line(self.spent, left),
                    ))
                    .when(self.more && !self.loading, |d| {
                        d.child(
                            ui::button(
                                "ph-pp-more",
                                if self.loading_more {
                                    "Loading…"
                                } else {
                                    "Load more"
                                },
                                ButtonKind::Ghost,
                                !self.loading_more,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_profile(&self, p: &Profile, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let person = self.people.iter().find(|x| x.id == p.person);
        let identity = p.identity.as_ref();
        let email = identity
            .and_then(|i| i.properties.get("email").and_then(|v| v.as_str()))
            .map(str::to_string)
            .or_else(|| person.and_then(|x| x.email.clone()));
        let distinct_id = self
            .selected
            .as_ref()
            .map(|s| s.1.clone())
            .unwrap_or_default();
        let name = email.clone().unwrap_or_else(|| distinct_id.clone());
        let last_seen = person
            .map(|x| x.last_seen)
            .or(p
                .timeline
                .as_ref()
                .ok()
                .and_then(|m| m.first())
                .map(|m| m.time))
            .unwrap_or(0);
        let mut facts: Vec<String> = Vec::new();
        if let Some(i) = identity {
            facts.push(if i.identified {
                "Identified".into()
            } else {
                "Anonymous".into()
            });
            if i.distinct_ids.len() > 1 {
                facts.push(format!("{} distinct_ids merged", i.distinct_ids.len()));
            }
            if i.created_at > 0 {
                facts.push(format!("first seen {}", day(i.created_at)));
            }
        }
        if last_seen > 0 {
            facts.push(format!("last seen {}", short_ago(last_seen, now)));
        }
        let first_name = name
            .split(['@', '.', ' '])
            .next()
            .map(|n| {
                let mut c = n.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect::<String>())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let person_path = format!("/person/{}", posthog::encode(&distinct_id));
        let header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui::sp(16.))
            .child(ui::avatar(&name, 48.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(240.))
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .child(
                        ui::text(Type::Title, theme::text(), crate::redact::value(&name))
                            .truncate(),
                    )
                    .child(
                        ui::text(Type::Body, theme::text_subtle(), facts.join(" · ")).truncate(),
                    ),
            )
            .child(
                ui::button_with(
                    "ph-pp-ask",
                    Some("sparkles"),
                    format!("Ask about {first_name}"),
                    ButtonKind::Outline,
                    false,
                )
                .tooltip(ui::tip("Comes with the Ask tab")),
            )
            .child(
                ui::button_with(
                    "ph-pp-open",
                    Some("link-square02"),
                    "Open in PostHog",
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open(&person_path, cx))),
            );

        // Left column: who they are.
        let mut idrows = Vec::new();
        if let Some(e) = &email {
            idrows.push(("email".to_string(), crate::redact::value(e)));
        }
        // An email is a valid distinct_id: it is masked like the email row.
        idrows.push((
            "distinct_id".into(),
            crate::redact::text(&distinct_id).into_owned(),
        ));
        if let Some(i) = identity {
            let merged: Vec<String> = i
                .distinct_ids
                .iter()
                .filter(|d| **d != distinct_id)
                .map(|d| crate::redact::text(d).into_owned())
                .collect();
            if !merged.is_empty() {
                idrows.push(("merged".into(), merged.join(" · ")));
            }
        }
        let mut props: Vec<(String, String)> = identity
            .map(|i| {
                i.properties
                    .iter()
                    .filter(|(k, _)| {
                        *k != "email" && !k.starts_with("$initial_") && !k.starts_with("$geoip_")
                    })
                    .map(|(k, v)| (k.clone(), ev::show(v)))
                    .collect()
            })
            .unwrap_or_default();
        props.sort_by(|a, b| {
            a.0.starts_with('$')
                .cmp(&b.0.starts_with('$'))
                .then(a.0.cmp(&b.0))
        });
        let n_props = props.len();
        if !self.all_props {
            props.truncate(TOP_PROPS);
        }
        let mut left = div()
            .flex()
            .flex_col()
            .gap(ui::sp(10.))
            .flex_1()
            .min_w(px(320.))
            .child(label("IDENTITY"))
            .child(kv_card(idrows))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pt(ui::sp(14.))
                    .child(label(format!("PROPERTIES · {n_props}")))
                    .when(n_props > TOP_PROPS, |d| {
                        d.child(
                            div()
                                .id("ph-pp-all")
                                .cursor_pointer()
                                .ty(Type::Label)
                                .text_color(theme::text_muted())
                                .hover(|s| s.text_color(theme::text()))
                                .child(if self.all_props { "Top" } else { "All" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.all_props = !this.all_props;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(if props.is_empty() {
                quiet("No properties.").into_any_element()
            } else {
                kv_card(props).into_any_element()
            });
        if let Some(cohorts) = &p.cohorts {
            left = left
                .child(label("COHORTS").pt(ui::sp(14.)))
                .child(if cohorts.is_empty() {
                    quiet("In no cohort.").into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(ui::sp(6.))
                        .children(cohorts.iter().map(|c| ui::badge(c.clone(), Tone::Neutral)))
                        .into_any_element()
                });
        }
        left = left
            .child(label("FLAGS THEY GET").pt(ui::sp(14.)))
            .child(match &p.flags {
                Ok(f) if !f.is_empty() => kv_card(f.clone()).into_any_element(),
                Ok(_) => quiet("Their recent events carry no flags.").into_any_element(),
                Err(e) => quiet(e.message()).into_any_element(),
            });

        // Right column: what they did.
        let peak_ticks: Vec<SharedString> =
            vec![day(now.saturating_sub(13 * 86_400)).into(), "Today".into()];
        let mut sessions = card().p(ui::sp(6.));
        match &p.sessions {
            Err(e) => {
                sessions = sessions.child(quiet(e.message()).px(ui::sp(12.)));
            }
            Ok(list) => {
                if list.is_empty() {
                    sessions = sessions.child(quiet("No sessions in 14 days.").px(ui::sp(12.)));
                }
                for (i, s) in list.iter().enumerate() {
                    let live = now.saturating_sub(s.end) < 5 * 60;
                    let title = if live {
                        format!("Now · {} so far", duration(s.end.saturating_sub(s.start)))
                    } else {
                        format!(
                            "{} {} · {}",
                            day_or_today(s.start, now),
                            posthog::clock(s.start),
                            duration(s.end.saturating_sub(s.start))
                        )
                    };
                    let detail = [s.detail.clone(), format!("{} events", s.events)]
                        .into_iter()
                        .filter(|x| !x.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let path = ev::replay_path(&s.id);
                    sessions = sessions.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(12.))
                            .px(ui::sp(10.))
                            .py(ui::sp(8.))
                            .child(ui::icon_tile("video01", 14., theme::text_muted()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(ui::text(Type::Body, theme::text(), title).truncate())
                                    .child(
                                        ui::text(Type::Caption, theme::text_subtle(), detail)
                                            .truncate(),
                                    ),
                            )
                            .child(
                                ui::button(("ph-pp-watch", i), "Watch", ButtonKind::Outline, true)
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.open(&path, cx)),
                                    ),
                            ),
                    );
                }
            }
        }
        let mut seg = ui::segmented();
        for (i, (k, l)) in [
            (None, "All"),
            (Some(Kind::AgentTurn), "Agent turns"),
            (Some(Kind::Error), "Errors"),
        ]
        .into_iter()
        .enumerate()
        {
            seg = seg.child(
                ui::segment(("ph-pp-tl", i), None, l, self.timeline == k).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.timeline = k;
                        cx.notify();
                    },
                )),
            );
        }
        let mut timeline = card();
        let moments: Vec<&pp::Moment> = match &p.timeline {
            Err(e) => {
                timeline = timeline.child(quiet(e.message()).px(ui::sp(16.)));
                Vec::new()
            }
            Ok(t) => t
                .iter()
                .filter(|m| self.timeline.is_none_or(|k| m.kind == k))
                .take(30)
                .collect(),
        };
        if p.timeline.is_ok() && moments.is_empty() {
            timeline = timeline.child(quiet("Nothing in 14 days.").px(ui::sp(16.)));
        }
        for (i, m) in moments.into_iter().enumerate() {
            timeline = timeline.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(12.))
                    .h(px(36.))
                    .px(ui::sp(16.))
                    .when(i > 0, |d| {
                        d.border_t_1().border_color(theme::border_subtle())
                    })
                    .child(
                        ui::text(Type::Mono, theme::text_faint(), posthog::clock(m.time))
                            .w(px(48.))
                            .flex_none(),
                    )
                    .child(
                        ui::text(
                            Type::Mono,
                            if m.kind == Kind::Error {
                                theme::error().into()
                            } else {
                                theme::text()
                            },
                            m.event.clone(),
                        )
                        .w(px(200.))
                        .flex_none()
                        .truncate(),
                    )
                    .child(
                        ui::text(Type::Caption, theme::text_subtle(), m.detail.clone())
                            .flex_1()
                            .min_w_0()
                            .truncate(),
                    ),
            );
        }
        let right = div()
            .flex()
            .flex_col()
            .gap(ui::sp(10.))
            .flex_1()
            .min_w(px(320.))
            .child(label(format!("EVENTS · {} DAYS", pp::DAYS)))
            .child(div().pt(ui::sp(16.)).child(match &p.daily {
                Ok(daily) => bars::bars(daily, 56., peak_ticks),
                Err(e) => div().pt(ui::sp(16.)).child(quiet(e.message())),
            }))
            .child(label("SESSIONS").pt(ui::sp(14.)))
            .child(sessions)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pt(ui::sp(14.))
                    .child(label("TIMELINE"))
                    .child(seg),
            )
            .child(timeline);
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(24.))
            .flex_1()
            .min_w_0()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap(ui::sp(32.))
                    .child(left)
                    .child(right),
            )
            .into_any_element()
    }
}

impl Render for PeopleTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let (Some(scope), Some(t)) = (self.missing(), &self.target) {
            let url = posthog::scope::keys_url(&t.session.host);
            let works: &[&str] = if self.error.is_none() && !self.people.is_empty() {
                &["query:read"]
            } else {
                &[]
            };
            return div()
                .size_full()
                .child(super::scope::missing_scope(
                    "People",
                    scope,
                    &t.session.email,
                    &t.session.host,
                    works,
                    "Overview, Events, Flags and Errors",
                    cx.listener(move |_, _, _, cx| cx.emit(PeopleEvent::Open(url.clone()))),
                    cx.listener(|this, _, _, cx| {
                        this.error = None;
                        this.profile_error = None;
                        this.selected = None;
                        this.load(0, cx);
                    }),
                ))
                .into_any_element();
        }
        let note: Option<AnyElement> = [&self.error, &self.profile_error]
            .into_iter()
            .flatten()
            .next()
            .map(|e| match e {
                ApiError::RateLimited(until) => notice(
                    "clock01",
                    Tone::Info,
                    format!(
                        "This account used its queries for the hour. The tab reads again after {}.",
                        posthog::clock(*until)
                    ),
                ),
                e => notice("alert-circle", Tone::Warning, e.message()),
            });
        let profile: AnyElement = match &self.profile {
            Some(p) => self.render_profile(&p.clone(), cx),
            None if self.profile_loading => ui::loading("Reading the profile…")
                .flex_1()
                .into_any_element(),
            None => div().flex_1().into_any_element(),
        };
        div()
            .id("ph-pp-scroll")
            .track_scroll(&self.list_scroll)
            .size_full()
            .overflow_y_scroll()
            .px(ui::sp(ui::PAGE_PAD))
            .pb(ui::sp(32.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(12.))
                    .children(note)
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(ui::sp(32.))
                            .child(self.render_list(cx))
                            .child(profile),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_short() {
        assert_eq!(duration(0), "1 min");
        assert_eq!(duration(18 * 60), "18 min");
        assert_eq!(duration(52 * 60 + 5), "53 min");
        assert_eq!(duration(72 * 60), "1 h 12 min");
    }
}
