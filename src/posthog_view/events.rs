//! The Events tab: the project's events, newest first, filtered by event,
//! `key = value` properties, a person and a time range, with their volume
//! as bars. A row opens a drawer with every property, grouped.
//!
//! Each filter change waits `query::DEBOUNCE` for the next one, then reads
//! the list and the volume (two queries, or none when cached); "Load 50
//! more" reads one page more.

use super::*;
use crate::posthog::defs::{self, Defs};
use crate::posthog::events::{self as ev, Event, Filter, PropFilter, Range};
use crate::posthog::query::{self, Answer, Debounce, Paging};
use gpui::Focusable as _;

const PAGE: u32 = 50;
/// The person and key-property columns.
const KEY_COLUMNS: usize = 3;
/// Table column widths: the key-property columns give way first.
const TIME_W: f32 = 84.;
const EVENT_W: f32 = 200.;
const PERSON_MIN_W: f32 = 180.;
const KEY_W: f32 = 150.;

pub enum EventsEvent {
    /// Open a URL in a browser tab.
    Open(String),
    /// Show this person in the People tab.
    Person {
        person_id: String,
        distinct_id: String,
    },
}

pub struct EventsTab {
    target: Option<Target>,
    defs: Option<Defs>,
    defs_loading: bool,
    filter: Filter,
    events: Vec<Event>,
    page: Paging,
    more: bool,
    volume: Vec<u64>,
    keys: Vec<String>,
    loading: bool,
    loading_more: bool,
    error: Option<ApiError>,
    /// Queries the last read cost.
    spent: u32,
    /// The current filter has been read (or is being read).
    loaded: bool,
    debounce: Debounce,
    picker: bool,
    adding: bool,
    selected: Option<usize>,
    person_open: bool,
    copied: bool,
    /// Move the focus to the value field on the next frame.
    focus_value: bool,
    search: Entity<TextInput>,
    prop_key: Entity<TextInput>,
    prop_value: Entity<TextInput>,
    person: Entity<TextInput>,
    scroll: ScrollHandle,
    _load: Option<Task<()>>,
    _more: Option<Task<()>>,
    _defs: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<EventsEvent> for EventsTab {}

impl EventsTab {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search event definitions", cx));
        let prop_key = cx.new(|cx| TextInput::new("key", cx).mono());
        let prop_value = cx.new(|cx| TextInput::new("value", cx).mono());
        let person = cx.new(|cx| TextInput::new("Person: email or distinct_id", cx));
        let subs = vec![
            cx.subscribe(&search, |this: &mut Self, _, e: &InputEvent, cx| match e {
                InputEvent::Changed => cx.notify(),
                InputEvent::Cancel => {
                    this.picker = false;
                    cx.notify();
                }
                InputEvent::Submit => {
                    if let Some(name) = this.matching_events(cx).first().map(|d| d.name.clone()) {
                        this.pick_event(Some(name), cx);
                    }
                }
                _ => {}
            }),
            cx.subscribe(&prop_key, |this: &mut Self, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Submit | InputEvent::Complete) {
                    // Enter or Tab in the key moves on to the value.
                    this.focus_value = true;
                    cx.notify();
                } else {
                    this.on_prop_input(e, cx)
                }
            }),
            cx.subscribe(&prop_value, |this: &mut Self, _, e: &InputEvent, cx| {
                this.on_prop_input(e, cx)
            }),
            cx.subscribe(&person, |this: &mut Self, input, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Changed) {
                    let text = input.read(cx).text().trim().to_string();
                    if text != this.filter.person {
                        this.filter.person = text;
                        this.changed(cx);
                    }
                }
            }),
        ];
        Self {
            target: None,
            defs: None,
            defs_loading: false,
            filter: Filter::default(),
            events: Vec::new(),
            page: Paging::first(PAGE),
            more: false,
            volume: Vec::new(),
            keys: Vec::new(),
            loading: false,
            loading_more: false,
            error: None,
            spent: 0,
            loaded: false,
            debounce: Debounce::default(),
            picker: false,
            adding: false,
            selected: None,
            person_open: false,
            copied: false,
            focus_value: false,
            search,
            prop_key,
            prop_value,
            person,
            scroll: ScrollHandle::new(),
            _load: None,
            _more: None,
            _defs: None,
            _subs: subs,
        }
    }

    /// Follows the project the PostHog tab shows; a new one starts over.
    pub fn set_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        if self.target == target {
            return;
        }
        self.defs = target.as_ref().and_then(|t| defs::cached(&t.project));
        self.target = target;
        self.filter = Filter::default();
        self.events.clear();
        self.volume.clear();
        self.error = None;
        self.loaded = false;
        self.selected = None;
        self.picker = false;
        self.adding = false;
        self.debounce.bump();
        self.person.update(cx, |i, cx| i.set_text("", cx));
        cx.notify();
    }

    /// The tab became visible: read what it doesn't have yet.
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if self.target.is_none() {
            return;
        }
        if !self.loaded {
            self.load(query::FRESH, cx);
        }
        let now = crate::model::unix_now();
        if self
            .defs
            .as_ref()
            .is_none_or(|d| now.saturating_sub(d.fetched_at) >= defs::STALE)
        {
            self.load_defs(cx);
        }
    }

    /// The header's refresh: read again, past the cache.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.target.is_some() {
            self.load(0, cx);
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        // The rows and page on screen belong to the filter that made them:
        // leaving them under new controls would show old results as matches.
        self.events.clear();
        self.keys.clear();
        self.volume.clear();
        self.more = false;
        self.filter.until = None;
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
        // The read keeps this edge on every page, so arrivals in between
        // cannot shift what a later page returns. It is committed with the
        // rows: a failed refresh keeps the window its retained rows came from.
        let mut filter = self.filter.clone();
        filter.until = Some(crate::model::unix_now());
        self._load = Some(cx.spawn(async move |this, cx| {
            let f = filter.clone();
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    let (s, id) = (&t.session, t.project.id);
                    let (list, volume) = std::thread::scope(|scope| {
                        let l = scope.spawn(|| {
                            query::run(s, id, &ev::list_query(&f, Paging::first(PAGE), None), fresh)
                        });
                        let v = scope.spawn(|| query::run(s, id, &ev::volume_query(&f), fresh));
                        (posthog::joined(l), posthog::joined(v))
                    });
                    Ok::<(Answer, Answer), ApiError>((list?, volume?))
                }))
                .await;
            this.update(cx, |this, cx| {
                if !this.debounce.is_current(generation) {
                    return;
                }
                this.loading = false;
                match got {
                    Ok((list, volume)) => {
                        // The page resets with the rows, not at the request: a
                        // failed refresh keeps its rows and the page they fill.
                        this.page = Paging::first(PAGE);
                        this.filter.until = filter.until;
                        let (rows, more) = this.page.split(ev::parse_rows(&list.rows));
                        this.keys = ev::key_columns(&rows, KEY_COLUMNS);
                        this.events = rows;
                        this.more = more;
                        this.volume = ev::parse_volume(&volume.rows, filter.range);
                        this.spent = list.spent + volume.spent;
                        this.error = None;
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
        // here would bump the same generation and append new-filter rows to
        // the old table.
        if self.loading || self.loading_more || !self.more {
            return;
        }
        self.loading_more = true;
        cx.notify();
        let page = self.page.next();
        let generation = self.debounce.bump();
        let after = self
            .events
            .last()
            .map(|e| (e.stamp.clone(), e.uuid.clone()));
        let q = ev::list_query(
            &self.filter,
            page,
            after.as_ref().map(|(t, u)| (t.as_str(), u.as_str())),
        );
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
                        let (rows, more) = page.split(ev::parse_rows(&a.rows));
                        this.events.extend(rows);
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

    fn load_defs(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        if self.defs_loading {
            return;
        }
        self.defs_loading = true;
        self._defs = Some(cx.spawn(async move |this, cx| {
            let project = t.project.clone();
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    defs::load(&t.session, &t.project)
                }))
                .await;
            this.update(cx, |this, cx| {
                this.defs_loading = false;
                if this.target.as_ref().is_some_and(|t| t.project == project)
                    && let Ok(d) = got
                {
                    this.defs = Some(d);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn matching_events(&self, cx: &gpui::App) -> Vec<&defs::EventDef> {
        let needle = self.search.read(cx).text().trim().to_lowercase();
        self.defs
            .iter()
            .flat_map(|d| d.events.iter())
            .filter(|d| needle.is_empty() || d.name.to_lowercase().contains(&needle))
            .take(60)
            .collect()
    }

    fn pick_event(&mut self, event: Option<String>, cx: &mut Context<Self>) {
        self.picker = false;
        if self.filter.event != event {
            self.filter.event = event;
            self.changed(cx);
        }
        cx.notify();
    }

    fn set_range(&mut self, r: Range, cx: &mut Context<Self>) {
        if self.filter.range != r {
            self.filter.range = r;
            self.changed(cx);
        }
    }

    fn on_prop_input(&mut self, e: &InputEvent, cx: &mut Context<Self>) {
        match e {
            InputEvent::Submit => self.add_prop(cx),
            InputEvent::Cancel => {
                self.adding = false;
                cx.notify();
            }
            InputEvent::Changed => cx.notify(),
            _ => {}
        }
    }

    fn add_prop(&mut self, cx: &mut Context<Self>) {
        let key = self.prop_key.read(cx).text().trim().to_string();
        let value = self.prop_value.read(cx).text().trim().to_string();
        if key.is_empty() || value.is_empty() {
            return;
        }
        self.adding = false;
        self.prop_key.update(cx, |i, cx| i.set_text("", cx));
        self.prop_value.update(cx, |i, cx| i.set_text("", cx));
        let f = PropFilter { key, value };
        if !self.filter.props.contains(&f) {
            self.filter.props.push(f);
            self.changed(cx);
        }
        cx.notify();
    }

    fn remove_prop(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.filter.props.len() {
            self.filter.props.remove(i);
            self.changed(cx);
        }
    }

    fn open(&self, path: &str, cx: &mut Context<Self>) {
        if let Some(t) = &self.target {
            cx.emit(EventsEvent::Open(t.project.url(path)));
        }
    }
}

// ---------------------------------------------------------------------------
// Drawing.

/// A filled pill for a property filter, removed by its ×.
fn filter_chip(id: usize, f: &PropFilter, cx: &mut Context<EventsTab>) -> AnyElement {
    div()
        .id(("ph-ev-chip", id))
        .flex()
        .flex_none()
        .items_center()
        .gap(ui::sp(6.))
        .h(px(ui::BUTTON_H))
        .pl(ui::sp(10.))
        .pr(ui::sp(6.))
        .rounded_full()
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::fill_selected())
        .ty(Type::Label)
        .text_color(theme::text())
        .child(format!("{} = {}", f.key, f.value))
        .child(
            ui::icon_button(("ph-ev-chip-x", id), "cancel01", 12., "Remove")
                .on_click(cx.listener(move |this, _, _, cx| this.remove_prop(id, cx))),
        )
        .into_any_element()
}

fn pill_button(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(ui::sp(6.))
        .h(px(ui::BUTTON_H))
        .px(ui::sp(10.))
        .rounded(ui::rad(ui::RADIUS))
        .border_1()
        .border_color(theme::border_strong())
        .ty(Type::Label)
        .text_color(theme::text_muted())
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
}

/// Five labels under a chart of `range`, oldest to "Now".
fn ticks(range: Range, now: u64) -> Vec<SharedString> {
    let span = range.seconds();
    (0..5)
        .map(|i| {
            if i == 4 {
                return "Now".into();
            }
            let t = now.saturating_sub(span - span * i / 4);
            match range {
                Range::Hour | Range::Day => posthog::clock(t).into(),
                Range::Week | Range::Month => day(t).into(),
            }
        })
        .collect()
}

impl EventsTab {
    fn render_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        let event = pill_button("ph-ev-pick")
            .child(icon("filter", 13.).text_color(theme::text_subtle()))
            .child("Event")
            .child(
                div()
                    .ty(Type::Mono)
                    .text_color(theme::text())
                    .max_w(px(240.))
                    .truncate()
                    .child(self.filter.event.clone().unwrap_or("all events".into())),
            )
            .child(icon("unfold-more", 13.).text_color(theme::text_subtle()))
            .on_click(cx.listener(|this, _, window, cx| {
                this.picker = !this.picker;
                this.adding = false;
                if this.picker {
                    let total = this.defs.as_ref().map_or(0, |d| d.events.len());
                    this.search.update(cx, |i, cx| {
                        i.set_text("", cx);
                        i.set_placeholder(format!("Search {total} event definitions"), cx);
                    });
                    window.focus(&this.search.focus_handle(cx), cx);
                }
                cx.notify();
            }));
        let chips: Vec<AnyElement> = self
            .filter
            .props
            .iter()
            .enumerate()
            .map(|(i, f)| filter_chip(i, f, cx))
            .collect();
        let add: AnyElement = if self.adding {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(ui::sp(6.))
                .child(
                    ui::field()
                        .w(px(150.))
                        .h(px(ui::BUTTON_H))
                        .child(self.prop_key.clone()),
                )
                .child(ui::text(Type::Label, theme::text_subtle(), "="))
                .child(
                    ui::field()
                        .w(px(150.))
                        .h(px(ui::BUTTON_H))
                        .child(self.prop_value.clone()),
                )
                .child(
                    ui::button("ph-ev-prop-add", "Add", ButtonKind::Outline, true)
                        .on_click(cx.listener(|this, _, _, cx| this.add_prop(cx))),
                )
                .into_any_element()
        } else {
            pill_button("ph-ev-prop")
                .text_color(theme::text())
                .child("+ Property")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.adding = true;
                    this.picker = false;
                    window.focus(&this.prop_key.focus_handle(cx), cx);
                    cx.notify();
                }))
                .into_any_element()
        };
        let mut range = ui::segmented();
        for (i, r) in Range::ALL.into_iter().enumerate() {
            range = range.child(
                ui::segment(("ph-ev-range", i), None, r.label(), self.filter.range == r)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_range(r, cx))),
            );
        }
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui::sp(8.))
            .child(event)
            .children(chips)
            .child(add)
            .child(
                ui::field()
                    .flex_1()
                    .min_w(px(180.))
                    .max_w(px(300.))
                    .h(px(ui::BUTTON_H))
                    .child(icon("user", 13.).text_color(theme::text_subtle()))
                    .child(self.person.clone()),
            )
            .child(div().flex_1())
            .child(range)
            .into_any_element()
    }

    /// Property keys to pick from while adding a filter.
    fn render_key_hints(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.adding {
            return None;
        }
        let typed = self.prop_key.read(cx).text().trim().to_lowercase();
        let mut keys: Vec<String> = self.keys.clone();
        for k in self.defs.iter().flat_map(|d| d.props.iter()) {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        let keys: Vec<String> = keys
            .into_iter()
            .filter(|k| {
                typed.is_empty() || (k.to_lowercase().contains(&typed) && k.to_lowercase() != typed)
            })
            .take(12)
            .collect();
        if keys.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(ui::sp(6.))
                .child(ui::text(Type::Caption, theme::text_faint(), "Keys"))
                .children(keys.into_iter().enumerate().map(|(i, k)| {
                    let pick = k.clone();
                    ui::chip(("ph-ev-key", i), k, false)
                        .font_family(theme::FONT_MONO)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.prop_key
                                .update(cx, |input, cx| input.set_text(pick.clone(), cx));
                            window.focus(&this.prop_value.focus_handle(cx), cx);
                            cx.notify();
                        }))
                }))
                .into_any_element(),
        )
    }

    fn render_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let total = self.defs.as_ref().map_or(0, |d| d.events.len());
        let events: Vec<(String, Option<u64>, Option<u64>)> = self
            .matching_events(cx)
            .into_iter()
            .map(|d| (d.name.clone(), d.volume, d.last_seen))
            .collect();
        let row = |id: usize, on: bool| {
            div()
                .id(("ph-ev-def", id))
                .flex()
                .items_center()
                .gap(ui::sp(8.))
                .h(px(ui::MENU_ROW_H + 4.))
                .px(ui::sp(10.))
                .rounded(ui::menu_row_radius())
                .cursor_pointer()
                .when(on, |d| d.bg(theme::fill_selected()))
                .hover(|d| d.bg(theme::fill_hover()))
                .child(div().w(px(14.)).flex_none().when(on, |d| {
                    d.child(icon("tick02", 13.).text_color(theme::text()))
                }))
        };
        let current = self.filter.event.clone();
        let missing = self
            .defs
            .as_ref()
            .and_then(|d| d.missing.iter().find(|s| s.contains("definition")).cloned());
        let mut list = div()
            .id("ph-ev-defs")
            .flex()
            .flex_col()
            .max_h(px(380.))
            .overflow_y_scroll()
            .child(
                row(usize::MAX, current.is_none())
                    .child(ui::text(Type::Body, theme::text(), "All events"))
                    .on_click(cx.listener(|this, _, _, cx| this.pick_event(None, cx))),
            );
        for (i, (name, volume, seen)) in events.into_iter().enumerate() {
            let on = current.as_deref() == Some(name.as_str());
            let pick = name.clone();
            list = list.child(
                row(i, on)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Mono)
                            .text_color(theme::text())
                            .child(name),
                    )
                    .child(
                        ui::text(
                            Type::Mono,
                            theme::text_muted(),
                            volume.map(ev::group_digits).unwrap_or_default(),
                        )
                        .w(px(70.))
                        .text_right(),
                    )
                    .child(
                        ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            seen.map(|t| short_ago(t, now)).unwrap_or_default(),
                        )
                        .w(px(80.))
                        .text_right(),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.pick_event(Some(pick.clone()), cx)),
                    ),
            );
        }
        ui::menu_panel("ph-ev-picker", 520.)
            .p(ui::sp(6.))
            .child(
                ui::field()
                    .child(icon("search01", 14.).text_color(theme::text_subtle()))
                    .child(self.search.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .px(ui::sp(10.))
                    .pt(ui::sp(8.))
                    .pb(ui::sp(4.))
                    .child(div().w(px(14.)))
                    .child(ui::text(Type::Caption, theme::text_faint(), "Event").flex_1())
                    .child(
                        ui::text(Type::Caption, theme::text_faint(), "24 h")
                            .w(px(70.))
                            .text_right(),
                    )
                    .child(
                        ui::text(Type::Caption, theme::text_faint(), "Last seen")
                            .w(px(80.))
                            .text_right(),
                    ),
            )
            .child(list)
            .when(self.defs_loading && total == 0, |d| {
                d.child(quiet("Reading the event definitions…").px(ui::sp(10.)))
            })
            .when_some(missing, |d, scope| {
                d.child(
                    ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        format!("Only events from the last 24 hours: the key lacks {scope}."),
                    )
                    .px(ui::sp(10.))
                    .py(ui::sp(6.)),
                )
            })
            .into_any_element()
    }

    fn render_volume(&self) -> AnyElement {
        let total: u64 = self.volume.iter().sum();
        let now = crate::model::unix_now();
        div()
            .flex()
            .items_end()
            .gap(ui::sp(24.))
            .pt(ui::sp(8.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .w(px(160.))
                    .flex_none()
                    .pb(ui::sp(20.))
                    .child(ui::text(Type::Body, theme::text_muted(), "Matching events"))
                    .child(ui::text(
                        Type::Subtitle,
                        theme::text(),
                        if self.volume.is_empty() {
                            "–".to_string()
                        } else {
                            ev::group_digits(total)
                        },
                    ))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        self.filter.range.span(),
                    )),
            )
            .child(div().flex_1().min_w_0().child(bars::bars(
                if self.volume.is_empty() {
                    &[]
                } else {
                    &self.volume
                },
                72.,
                ticks(self.filter.range, now),
            )))
            .into_any_element()
    }

    fn render_table(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let drawer = self.selected.is_some();
        let keys: Vec<String> = if drawer {
            Vec::new()
        } else {
            self.keys.clone()
        };
        let head = |t: &str| {
            ui::text(Type::Label, theme::text_subtle(), t.to_string())
                .font_weight(gpui::FontWeight::NORMAL)
        };
        let header = div()
            .flex()
            .items_center()
            .gap(ui::sp(12.))
            .h(px(40.))
            .px(ui::sp(16.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(head("Time").w(px(TIME_W)).flex_none())
            .child(head("Event").w(px(EVENT_W)).flex_shrink_0())
            .child(head("Person").flex_1().min_w(px(PERSON_MIN_W)))
            .children(
                keys.iter()
                    .map(|k| head(k).flex_1().min_w_0().max_w(px(KEY_W)).truncate()),
            );
        let mut rows = div().flex().flex_col();
        for (i, e) in self.events.iter().enumerate() {
            let on = self.selected == Some(i);
            rows = rows.child(
                div()
                    .id(("ph-ev-row", i))
                    .flex()
                    .items_center()
                    .gap(ui::sp(12.))
                    .h(px(40.))
                    .px(ui::sp(16.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::fill_selected()))
                    .hover(|d| d.bg(theme::fill_hover()))
                    .child(
                        ui::text(Type::Mono, theme::text_faint(), time_of_day(e.time, now))
                            .w(px(TIME_W))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .w(px(EVENT_W))
                            .flex_shrink_0()
                            .truncate()
                            .ty(Type::Mono)
                            .text_color(theme::text())
                            .child(e.event.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(PERSON_MIN_W))
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .child(ui::avatar(e.who(), 20.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Body)
                                    .text_color(theme::text())
                                    .child(crate::redact::value(e.who())),
                            ),
                    )
                    .children(keys.iter().map(|k| {
                        ui::text(
                            Type::Mono,
                            theme::text_muted(),
                            e.props.get(k.as_str()).map(ev::show).unwrap_or_default(),
                        )
                        .flex_1()
                        .min_w_0()
                        .max_w(px(KEY_W))
                        .truncate()
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = if this.selected == Some(i) {
                            None
                        } else {
                            Some(i)
                        };
                        this.person_open = false;
                        this.copied = false;
                        cx.notify();
                    })),
            );
        }
        let total: u64 = self.volume.iter().sum();
        let left = self
            .target
            .as_ref()
            .map_or(query::HOURLY_QUERIES, |t| query::left(&t.session.id, now));
        let footer = div()
            .flex()
            .items_center()
            .gap(ui::sp(12.))
            .h(px(52.))
            .px(ui::sp(16.))
            .child(ui::text(
                Type::Body,
                theme::text_subtle(),
                if self.events.is_empty() {
                    "No events match.".to_string()
                } else {
                    format!(
                        "Showing {} of {}, newest first",
                        ev::group_digits(self.events.len() as u64),
                        ev::group_digits(total.max(self.events.len() as u64))
                    )
                },
            ))
            .child(div().flex_1())
            .child(ui::text(
                Type::Caption,
                theme::text_faint(),
                query::budget_line(self.spent, left),
            ))
            .when(self.more && !self.loading, |d| {
                d.child(
                    ui::button(
                        "ph-ev-more",
                        if self.loading_more {
                            "Loading…"
                        } else {
                            "Load 50 more"
                        },
                        ButtonKind::Outline,
                        !self.loading_more,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                )
            });
        div()
            .flex()
            .flex_col()
            .rounded(ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .overflow_hidden()
            .child(header)
            .child(rows)
            .child(footer)
            .into_any_element()
    }

    fn render_drawer(&self, e: &Event, cx: &mut Context<Self>) -> AnyElement {
        let (own, session) = ev::groups(e);
        let person = ev::person_rows(e);
        let group = |title: String| {
            div()
                .flex()
                .items_center()
                .h(px(40.))
                .px(ui::sp(24.))
                .border_b_1()
                .border_color(theme::border_subtle())
                .child(ui::text(Type::Overline, theme::text_muted(), title))
        };
        let line = |k: String, v: String| {
            div()
                .flex()
                .items_center()
                .gap(ui::sp(12.))
                .min_h(px(36.))
                .px(ui::sp(24.))
                .border_b_1()
                .border_color(theme::border_subtle())
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
        };
        let session_id = e.session().map(str::to_string);
        let json = ev::to_json(e);
        let target = (e.person_id.clone(), e.distinct_id.clone());
        let summary = {
            let names: Vec<&str> = person.iter().take(3).map(|r| r.0.as_str()).collect();
            match person.len() {
                0 => "none".to_string(),
                n if n <= 3 => names.join(", "),
                n => format!("{} and {} more", names.join(", "), n - 3),
            }
        };
        let mut body = div()
            .flex()
            .flex_col()
            .child(group(format!("EVENT · {}", own.len())))
            .children(own.into_iter().map(|(k, v)| line(k, v)))
            .child(group(format!("SESSION · {}", session.len())))
            .children(session.into_iter().map(|(k, v)| line(k, v)));
        body = body.child(
            div()
                .id("ph-ev-person-group")
                .flex()
                .items_center()
                .gap(ui::sp(8.))
                .h(px(44.))
                .px(ui::sp(24.))
                .cursor_pointer()
                .hover(|d| d.bg(theme::fill_hover()))
                .child(
                    icon(
                        if self.person_open {
                            "arrow-down01"
                        } else {
                            "arrow-right01"
                        },
                        12.,
                    )
                    .text_color(theme::text_subtle()),
                )
                .child(ui::text(
                    Type::Overline,
                    theme::text_muted(),
                    format!("PERSON · {}", person.len()),
                ))
                .when(!self.person_open, |d| {
                    d.child(ui::text(Type::Caption, theme::text_faint(), summary))
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.person_open = !this.person_open;
                    cx.notify();
                })),
        );
        if self.person_open {
            body = body.children(
                person
                    .into_iter()
                    .map(|(k, v)| line(k, crate::redact::value(&v))),
            );
        }
        let when = match crate::platform::local_time(e.time) {
            Some(l) => format!(
                "{}, {}",
                day(e.time),
                format_args!("{:02}:{:02}:{:02}", l.hour, l.minute, e.time % 60)
            ),
            None => posthog::clock(e.time),
        };
        div()
            .id("ph-ev-drawer")
            .occlude()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(560.))
            .flex()
            .flex_col()
            .bg(theme::dialog())
            .border_l_1()
            .border_color(theme::border())
            .shadow_xl()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(ui::sp(12.))
                    .px(ui::sp(24.))
                    .pt(ui::sp(20.))
                    .pb(ui::sp(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(ui::sp(4.))
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Mono)
                                    .text_size(px(ui::TEXT_LG))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(e.event.clone()),
                            )
                            .child(ui::text(
                                Type::Caption,
                                theme::text_subtle(),
                                format!("{when} · {}", crate::redact::value(e.who())),
                            )),
                    )
                    .child(ui::close_button("ph-ev-close").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.selected = None;
                            cx.notify();
                        },
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(ui::sp(8.))
                    .px(ui::sp(24.))
                    .pb(ui::sp(16.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        ui::button_with(
                            "ph-ev-copy",
                            Some(if self.copied { "tick02" } else { "copy01" }),
                            if self.copied {
                                "Copied"
                            } else {
                                "Copy as JSON"
                            },
                            ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(json.clone()));
                            this.copied = true;
                            cx.notify();
                        })),
                    )
                    .child(
                        ui::button_with(
                            "ph-ev-person",
                            Some("user"),
                            "Person",
                            ButtonKind::Outline,
                            !target.0.is_empty(),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(EventsEvent::Person {
                                person_id: target.0.clone(),
                                distinct_id: target.1.clone(),
                            })
                        })),
                    )
                    .child(
                        ui::button_with(
                            "ph-ev-replay",
                            Some("video01"),
                            "Replay",
                            ButtonKind::Outline,
                            session_id.is_some(),
                        )
                        .when_some(session_id, |b, sid| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.open(&ev::replay_path(&sid), cx)
                            }))
                        }),
                    )
                    .child(
                        ui::button_with(
                            "ph-ev-ask",
                            Some("sparkles"),
                            "Ask about this event",
                            ButtonKind::Outline,
                            false,
                        )
                        .tooltip(ui::tip("Comes with the Ask tab")),
                    ),
            )
            .child(
                div()
                    .id("ph-ev-drawer-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .into_any_element()
    }
}

impl Render for EventsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_value) && self.adding {
            window.focus(&self.prop_value.focus_handle(cx), cx);
        }
        if let (Some(ApiError::MissingScope(scope)), Some(t)) = (&self.error, &self.target) {
            let url = posthog::scope::keys_url(&t.session.host);
            return div()
                .size_full()
                .child(super::scope::missing_scope(
                    "Events",
                    scope,
                    &t.session.email,
                    &t.session.host,
                    &[],
                    "Overview, Flags and Errors",
                    cx.listener(move |_, _, _, cx| cx.emit(EventsEvent::Open(url.clone()))),
                    cx.listener(|this, _, _, cx| {
                        this.error = None;
                        this.load(0, cx);
                    }),
                ))
                .into_any_element();
        }
        let filters = self.render_filters(cx);
        let hints = self.render_key_hints(cx);
        let note: Option<AnyElement> = match &self.error {
            Some(ApiError::RateLimited(until)) => Some(notice(
                "clock01",
                Tone::Info,
                format!(
                    "This account used its queries for the hour. Showing what was read; the tab reads again after {}.",
                    posthog::clock(*until)
                ),
            )),
            Some(e) => Some(notice("alert-circle", Tone::Warning, e.message())),
            None => None,
        };
        let body: AnyElement = if self.loading && self.events.is_empty() {
            ui::loading("ph-ev-loading", "Reading events…").into_any_element()
        } else {
            self.render_table(cx)
        };
        let picker = self.picker.then(|| self.render_picker(cx));
        let drawer = self
            .selected
            .and_then(|i| self.events.get(i).cloned())
            .map(|e| self.render_drawer(&e, cx));
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("ph-ev-scroll")
                    .track_scroll(&self.scroll)
                    .size_full()
                    .overflow_y_scroll()
                    .px(ui::sp(ui::PAGE_PAD))
                    .pb(ui::sp(32.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(ui::sp(16.))
                            .child(div().relative().child(filters).when_some(picker, |d, p| {
                                d.child(
                                    gpui::deferred(
                                        div()
                                            .absolute()
                                            .top(px(ui::BUTTON_H + 6.))
                                            .left_0()
                                            .child(p),
                                    )
                                    .with_priority(1),
                                )
                            }))
                            .children(hints)
                            .children(note)
                            .child(self.render_volume())
                            .child(body),
                    ),
            )
            .when(self.picker, |d| {
                d.child(
                    div()
                        .id("ph-ev-backdrop")
                        .occlude()
                        .absolute()
                        .top(px(ui::BUTTON_H + 8.))
                        .left_0()
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.picker = false;
                                cx.notify();
                            }),
                        ),
                )
            })
            .children(drawer)
            .into_any_element()
    }
}
