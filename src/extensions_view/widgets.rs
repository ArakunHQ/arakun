//! Community widgets in the Marketplace: the Widgets page of Discover, the
//! Community widgets shelf over Plugins, and a widget's details with its
//! preview, what it can use, Verified or not, and Report.

use std::collections::HashMap;
use std::time::SystemTime;

use gpui::{Hsla, Role};

use super::widget_index::{self, Card, REASONS, WIDGET_KEYS};
use super::*;
use crate::agent::marketplace;
use crate::widget::manifest::{CANT, Manifest};
use crate::widget::store::{self, Place, Source, Store};
use crate::widget_blocks;

/// How many cards the shelf over Plugins shows.
const SHELF: usize = 3;
/// The tallest a card's preview gets; the rest is cut.
const PREVIEW_H: f32 = 132.;
const CARD_MIN: f32 = 260.;
const DETAIL_W: f32 = 460.;

/// The widget index and what the user did with it on this page.
#[derive(Default)]
pub(super) struct WidgetState {
    pub(super) cards: Loadable<Vec<Card>, SharedString>,
    /// Installed widgets by id, with their version, as `widgets.toml` says.
    pub(super) installed: HashMap<String, String>,
    /// The widget being added, by id.
    pub(super) adding: Option<String>,
    /// Why the last add failed, by id.
    pub(super) add_error: Option<(String, SharedString)>,
    /// The Report menu is open for this id.
    pub(super) reporting: Option<String>,
    /// How the last report went, by id: Ok, or why it failed.
    pub(super) reported: Option<(String, Result<(), SharedString>)>,
}

impl ExtensionsView {
    /// Reads the index the first time it is needed, and what is installed.
    pub(super) fn load_widgets(&mut self, cx: &mut Context<Self>) {
        // A failed read is not asked again until the Marketplace opens
        // again, so the failure shows instead of a read at every redraw.
        if self.ws.cards.data.is_some() || self.ws.cards.loading() || self.ws.cards.error.is_some()
        {
            return;
        }
        self.ws.cards.error = None;
        self.read_installed_widgets();
        let project = self.cwd.clone();
        self.ws.cards.start(
            cx,
            |this| &mut this.ws.cards,
            async move {
                widget_index::fetch()
                    .map(|index| widget_index::cards(index, WIDGET_KEYS, project.as_deref()))
            },
            |this, found, _| {
                this.ws.cards.settle(found.map_err(SharedString::from));
            },
        );
    }

    fn read_installed_widgets(&mut self) {
        self.ws.installed = Store::open(&Store::dir())
            .map(|s| s.widgets.into_iter().map(|r| (r.id, r.version)).collect())
            .unwrap_or_default();
    }

    /// The widgets the search box matches, in the index's order.
    fn widget_matches(&self, cx: &App) -> Vec<Card> {
        let query = self.query.read(cx).text().trim().to_lowercase();
        self.ws
            .cards
            .data
            .iter()
            .flatten()
            .filter(|c| {
                query.is_empty()
                    || [&c.name, &c.about, &c.author, &c.id]
                        .iter()
                        .any(|t| t.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    pub(super) fn widget_count(&self) -> Option<usize> {
        self.ws.cards.data.as_ref().map(Vec::len)
    }

    fn project_name(&self) -> String {
        self.cwd
            .as_ref()
            .and_then(|d| d.file_name())
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "this project".into())
    }

    fn pick_widget(&mut self, id: &str, cx: &mut Context<Self>) {
        self.ms.selected = Some(id.to_string());
        self.ws.reporting = None;
        cx.notify();
    }

    /// Downloads the version at its pinned commit, checks it against what
    /// the user was shown, and installs it with exactly those needs.
    fn add_widget(&mut self, card: &Card, cx: &mut Context<Self>) {
        if self.ws.adding.is_some() {
            return;
        }
        let Some(project) = self.cwd.clone() else {
            self.ws.add_error = Some((card.id.clone(), "Open a project first.".into()));
            cx.notify();
            return;
        };
        let card = card.clone();
        self.ws.adding = Some(card.id.clone());
        self.ws.add_error = None;
        cx.notify();
        let task = cx.background_spawn(async move { install(&card, project) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let id = this.ws.adding.take().unwrap_or_default();
                match result {
                    Ok(()) => this.read_installed_widgets(),
                    Err(e) => this.ws.add_error = Some((id, e.into())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn report_widget(&mut self, card: &Card, reason: &'static str, cx: &mut Context<Self>) {
        let (id, version) = (card.id.clone(), card.version.version.clone());
        self.ws.reporting = None;
        let task = cx.background_spawn({
            let id = id.clone();
            async move { widget_index::report(&id, &version, reason) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.ws.reported = Some((id, result.map_err(SharedString::from)));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// What a card ends with: Installed, a spinner, or Add.
    fn widget_action(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        if let Some(v) = self.ws.installed.get(&card.id) {
            let word = if *v == card.version.version {
                "Installed"
            } else {
                "Update available"
            };
            return ui::status_dot(word, Tone::Success)
                .flex_none()
                .into_any_element();
        }
        if self.ws.adding.as_deref() == Some(card.id.as_str()) {
            return busy_line("Adding…").into_any_element();
        }
        // Add opens the details: a widget never installs before its needs
        // were shown.
        let id = card.id.clone();
        ui::button(
            SharedString::from(format!("widget-add-{}", card.id)),
            "Add",
            ButtonKind::Outline,
            self.ws.adding.is_none(),
        )
        .tooltip(ui::tip_text("See what it can use, then add it"))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.pick_widget(&id, cx);
        }))
        .into_any_element()
    }

    /// A widget's card: its preview on sample data, then who made it,
    /// Verified or not, installs, and Add.
    fn widget_card(&self, card: &Card, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.ms.selected.as_deref() == Some(card.id.as_str());
        let id = card.id.clone();
        ui::labelled(
            div().id(SharedString::from(format!("widget-card-{}", card.id))),
            Role::Button,
            format!("{}, widget", card.name),
        )
        .flex()
        .flex_col()
        .min_w_0()
        .overflow_hidden()
        .gap(ui::sp(10.))
        .p(ui::sp(10.))
        .rounded(ui::rad(ui::RADIUS_LG))
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
        .child(preview_box(card, CARD_MIN - 44.).h(px(PREVIEW_H)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(ui::sp(10.))
                .px(ui::sp(4.))
                .child(logos::tile(mark(card), 28.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .ty(Type::Label)
                                .text_color(theme::text())
                                .child(card.name.clone()),
                        )
                        .child(byline(card, true)),
                )
                .child(div().flex_none().child(self.widget_action(card, cx))),
        )
        .children(card.fits.clone().map(|why| {
            div()
                .px(ui::sp(4.))
                .truncate()
                .ty(Type::Caption)
                .text_color(theme::text_subtle())
                .child(why)
        }))
        .on_click(cx.listener(move |this, _, _, cx| this.pick_widget(&id, cx)))
    }

    /// Cards in lines of as many as fit.
    fn widget_grid(&self, cards: &[Card], per: usize, cx: &mut Context<Self>) -> Div {
        let per = per.max(1);
        let mut grid = div().flex().flex_col().gap(ui::sp(GRID_GAP));
        for line in cards.chunks(per) {
            let mut row = div().flex().w_full().gap(ui::sp(GRID_GAP));
            for card in line {
                row = row.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(self.widget_card(card, cx)),
                );
            }
            for _ in line.len()..per {
                row = row.child(div().flex_1());
            }
            grid = grid.child(row);
        }
        grid
    }

    /// The Community widgets shelf over Plugins: three cards and All.
    pub(super) fn render_widget_shelf(&self, per: usize, cx: &mut Context<Self>) -> Option<Div> {
        let cards = self.ws.cards.data.as_ref()?;
        if cards.is_empty() {
            return None;
        }
        let shown: Vec<Card> = cards.iter().take(SHELF.min(per.max(1))).cloned().collect();
        let all = cards.len();
        Some(
            div()
                .flex()
                .flex_col()
                .w_full()
                .gap(ui::sp(12.))
                .px(ui::sp(ui::PAGE_PAD))
                .pt(ui::sp(20.))
                .child(section_head(
                    "Community widgets",
                    "Small live views anyone can make. Arakun draws them, and they only reach what you approve.",
                    Some(
                        ui::button(
                            "widget-shelf-all",
                            format!("All {all}"),
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_market(Market::Widgets, cx)
                        }))
                        .into_any_element(),
                    ),
                ))
                .child(self.widget_grid(&shown, shown.len(), cx)),
        )
    }

    /// The Widgets page: what fits the project first, then the rest, and the
    /// picked widget's details on the right.
    pub(super) fn render_widgets(&self, cx: &mut Context<Self>) -> Div {
        let width = self.ms.last_width.get();
        let detail = self.render_widget_detail(cx);
        let room = if detail.is_some() {
            width - DETAIL_W
        } else {
            width
        } - 2. * ui::PAGE_PAD;
        let per = ((room + GRID_GAP) / (CARD_MIN + GRID_GAP)).floor().max(1.) as usize;
        let cards = self.widget_matches(cx);
        let (fit, rest): (Vec<Card>, Vec<Card>) =
            cards.iter().cloned().partition(|c| c.fits.is_some());
        let project = self.project_name();
        let body: AnyElement = match (&self.ws.cards.data, &self.ws.cards.error) {
            (None, Some(e)) => note(e.clone(), Tone::Danger).into_any_element(),
            (None, None) => ui::loading("Reading Arakun's widget index…").into_any_element(),
            (Some(all), _) if all.is_empty() => ui::empty_state(
                "dashboard-square01",
                "No widgets yet",
                "Nobody has published a widget yet. A widget is a Git repository with widget.toml, main.rhai and sample.json.",
                None,
            )
            .into_any_element(),
            (Some(_), _) if cards.is_empty() => ui::empty_state(
                "search01",
                "Nothing matches",
                "Try other words: a widget's name, what it shows or who made it.",
                None,
            )
            .into_any_element(),
            (Some(_), _) => div()
                .flex()
                .flex_col()
                .gap(ui::sp(20.))
                .when(!fit.is_empty(), |d| {
                    d.child(section_head(
                        &format!("Suggested for {project}"),
                        "It reads a file this project has.",
                        None,
                    ))
                    .child(self.widget_grid(&fit, per, cx))
                })
                .when(!rest.is_empty(), |d| {
                    d.when(!fit.is_empty(), |d| {
                        d.child(section_head("More widgets", "", None))
                    })
                    .child(self.widget_grid(&rest, per, cx))
                })
                .into_any_element(),
        };
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .id("widget-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(ui::sp(16.))
                    .px(ui::sp(ui::PAGE_PAD))
                    .pt(ui::sp(24.))
                    .pb(ui::sp(24.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .child(
                                div()
                                    .ty(Type::Heading)
                                    .text_color(theme::text())
                                    .child("Community widgets"),
                            )
                            .children(self.widget_count().map(|n| {
                                div()
                                    .ty(Type::Label)
                                    .text_color(theme::text_faint())
                                    .child(n.to_string())
                            }))
                            .when(self.ws.cards.loading(), |d| {
                                d.child(ui::spinner(13., theme::text_faint()))
                            }),
                    )
                    .child(
                        div()
                            .ty(Type::Body)
                            .text_color(theme::text_subtle())
                            .child(format!(
                                "Small live views anyone can make, the ones that fit {project} first. Arakun draws them, and they only reach what you approve."
                            )),
                    )
                    .child(body),
            )
            .children(detail)
    }

    /// The picked widget: its header, the preview, the notice when it is not
    /// verified, where it shows, what it can and can't use, and Allow and add.
    fn render_widget_detail(&self, cx: &mut Context<Self>) -> Option<Div> {
        let key = self.ms.selected.as_ref()?;
        let card = self
            .ws
            .cards
            .data
            .as_ref()?
            .iter()
            .find(|c| &c.id == key)?
            .clone();
        let installed = self.ws.installed.get(&card.id).cloned();
        let m = &card.manifest;
        let place = place_for(m);
        let adding = self.ws.adding.as_deref() == Some(card.id.as_str());

        let mut facts = vec![
            "Widget".to_string(),
            format!("by {}", card.author),
            match when(card.version.date) {
                Some(w) => format!("{}, {w}", card.version.version),
                None => card.version.version.clone(),
            },
            format!("{} installs", marketplace::short_count(card.installs)),
        ];
        if !card.license.is_empty() {
            facts.push(card.license.clone());
        }
        let source = card.source_url();
        let slug = card
            .repository
            .trim_start_matches("https://github.com/")
            .to_string();
        let header = div()
            .flex()
            .flex_col()
            .gap(ui::sp(8.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(ui::sp(12.))
                    .child(logos::tile(mark(&card), 44.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(ui::sp(4.))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(ui::sp(8.))
                                    .child(
                                        div()
                                            .ty(Type::Subtitle)
                                            .text_color(theme::text())
                                            .child(card.name.clone()),
                                    )
                                    .child(if card.verified {
                                        ui::badge("Verified", Tone::Success)
                                    } else {
                                        ui::badge("Not verified", Tone::Warning)
                                    })
                                    .child(match &installed {
                                        Some(_) => ui::status_dot("Installed", Tone::Success),
                                        None => ui::status_dot("Not installed", Tone::Neutral),
                                    }),
                            )
                            .child(
                                div()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(facts.join(" · ")),
                            ),
                    )
                    .child(
                        ui::icon_button("widget-detail-close", "cancel01", 14., "Close").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.ms.selected = None;
                                this.ws.reporting = None;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Mono)
                            .text_color(theme::text_subtle())
                            .child(format!("{slug} · {}", card.short_commit())),
                    )
                    .child(
                        ui::button_with(
                            "widget-source",
                            Some("link-square02"),
                            "Read the source",
                            ButtonKind::Ghost,
                            true,
                        )
                        .tooltip(ui::tip_text(
                            "The files at the commit this version is pinned to",
                        ))
                        .on_click(move |_, _, cx| cx.open_url(&source)),
                    )
                    .child({
                        let id = card.id.clone();
                        ui::button_with(
                            "widget-report",
                            Some("alert-circle"),
                            "Report",
                            ButtonKind::Ghost,
                            true,
                        )
                        .tooltip(ui::tip_text(
                            "Tell Arakun something is wrong with this widget",
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ws.reporting = if this.ws.reporting.as_deref() == Some(&id) {
                                None
                            } else {
                                Some(id.clone())
                            };
                            cx.notify();
                        }))
                    }),
            )
            .children(self.render_report(&card, cx));

        let can = m.needs.lines();
        let needs_count = match (can.len(), m.needs.secret.len()) {
            (0, _) => "Nothing beyond drawing".to_string(),
            (n, 0) => format!("{n} {}", plural(n, "thing", "things")),
            (n, t) => format!(
                "{n} {} · {t} {}",
                plural(n, "thing", "things"),
                plural(t, "token", "tokens")
            ),
        };
        let mut can_col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(ui::sp(10.))
            .child(ui::overline("It can"));
        if can.is_empty() {
            can_col = can_col.child(need_line(
                "checkmark-circle02",
                theme::text_faint(),
                "Only draw its sample and what it is given",
                "It asks for nothing else.",
            ));
        }
        for (what, why) in can {
            can_col = can_col.child(need_line(
                "checkmark-circle02",
                theme::success().into(),
                &what,
                &why,
            ));
        }
        let cant_col = CANT.iter().fold(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(ui::sp(10.))
                .child(ui::overline("It can't")),
            |d, c| {
                d.child(need_line(
                    "minus-sign",
                    theme::text_faint(),
                    c,
                    "No widget can, verified or not.",
                ))
            },
        );

        let mut panel = div()
            .id("widget-detail")
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .p(ui::sp(20.))
            .size_full()
            .overflow_y_scroll()
            .child(header)
            .child(
                div()
                    .ty(Type::Body)
                    .text_color(theme::text_muted())
                    .child(card.about.clone()),
            )
            .children(card.fits.clone().map(|why| {
                note(
                    format!("Suggested for {}: {why}.", self.project_name()),
                    Tone::Info,
                )
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(8.))
                    .child(ui::overline("Preview · sample data"))
                    .child(preview_box(&card, DETAIL_W - 64.)),
            );
        if !card.verified {
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(4.))
                    .p(ui::sp(12.))
                    .rounded(ui::rad(ui::RADIUS))
                    .border_1()
                    .border_color(theme::attention())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .child(icon("shield-alert", 14.).text_color(theme::attention()))
                            .child(
                                div()
                                    .ty(Type::Label)
                                    .text_color(theme::text())
                                    .child("Arakun has not reviewed this version"),
                            ),
                    )
                    .child(
                        div()
                            .ty(Type::Body)
                            .text_color(theme::text_subtle())
                            .child("It still can only do what is listed below: Arakun draws the widget and lends it each thing it uses. Verified means Arakun checked this exact version and its author."),
                    ),
            );
        }
        panel = panel
            .child(step(
                "Where",
                &match place {
                    Some(p) => format!("Just me · {} · this computer", p.label()),
                    None => "It draws in no place this Arakun has".into(),
                },
            ))
            .child(step("Needs", &needs_count))
            .child(div().flex().gap(ui::sp(16.)).child(can_col).child(cant_col));
        for s in &m.needs.secret {
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .p(ui::sp(12.))
                    .rounded(ui::rad(ui::RADIUS))
                    .border_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .ty(Type::Label)
                            .text_color(theme::text())
                            .child(format!("It needs a {}", s.label)),
                    )
                    .child(
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(format!(
                                "Kept in your keychain. Sent only to {}. You add it once the widget is placed.",
                                s.host
                            )),
                    ),
            );
        }
        let error = match &self.ws.add_error {
            Some((id, e)) if *id == card.id => Some(e.clone()),
            _ => None,
        };
        let footer: AnyElement = if let Some(v) = &installed {
            div()
                .ty(Type::Body)
                .text_color(theme::text_subtle())
                .child(if *v == card.version.version {
                    format!("Installed for you in {}.", self.project_name())
                } else {
                    format!(
                        "Version {v} is installed. An update that asks for more asks you again."
                    )
                })
                .into_any_element()
        } else if adding {
            busy_line("Downloading and checking the files…").into_any_element()
        } else {
            let target = card.clone();
            div()
                .flex()
                .items_center()
                .gap(ui::sp(12.))
                .child(
                    div()
                        .flex_1()
                        .ty(Type::Caption)
                        .text_color(theme::text_faint())
                        .child("An update that asks for more asks you again."),
                )
                .child(
                    ui::button(
                        "widget-allow-add",
                        "Allow and add",
                        ButtonKind::Primary,
                        place.is_some() && self.ws.adding.is_none(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.add_widget(&target, cx))),
                )
                .into_any_element()
        };
        panel = panel
            .children(error.map(|e| note(e, Tone::Danger)))
            .child(div().flex_1())
            .child(footer);
        Some(
            div()
                .flex()
                .flex_none()
                .w(px(DETAIL_W))
                .h_full()
                .border_l_1()
                .border_color(theme::border_subtle())
                .bg(theme::fill_raised())
                .child(panel),
        )
    }

    /// Report's reasons, or how the report went.
    fn render_report(&self, card: &Card, cx: &mut Context<Self>) -> Option<Div> {
        if let Some((id, result)) = &self.ws.reported
            && *id == card.id
            && self.ws.reporting.as_deref() != Some(card.id.as_str())
        {
            return Some(match result {
                Ok(()) => note(
                    "Thanks. A person at Arakun reads each report and can pull a widget from every computer.",
                    Tone::Success,
                ),
                Err(e) => note(e.clone(), Tone::Danger),
            });
        }
        if self.ws.reporting.as_deref() != Some(card.id.as_str()) {
            return None;
        }
        let mut menu = div()
            .flex()
            .flex_col()
            .gap(ui::sp(4.))
            .p(ui::sp(8.))
            .rounded(ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border())
            .bg(theme::background())
            .child(
                div()
                    .px(ui::sp(4.))
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child(format!(
                        "What is wrong with {} {}?",
                        card.name, card.version.version
                    )),
            );
        for (i, (reason, words)) in REASONS.into_iter().enumerate() {
            let target = card.clone();
            menu = menu.child(
                ui::button(("widget-report-reason", i), words, ButtonKind::Ghost, true).on_click(
                    cx.listener(move |this, _, _, cx| this.report_widget(&target, reason, cx)),
                ),
            );
        }
        Some(menu)
    }
}

/// The space between cards.
const GRID_GAP: f32 = 12.;

/// Off the main thread: the files at the pinned commit, checked against the
/// index's hash (by the store) and against the manifest the user was shown.
fn install(card: &Card, project: PathBuf) -> Result<(), String> {
    let place = place_for(&card.manifest)
        .ok_or_else(|| format!("{} draws in no place this Arakun has", card.name))?;
    let files = store::download(&card.repository, &card.version.commit)?;
    let shown = card.manifest_text.as_bytes();
    if files.get("widget.toml").map(Vec::as_slice) != Some(shown) {
        return Err(format!(
            "{}'s files don't match what the Marketplace showed. Nothing was installed; you can report it.",
            card.name
        ));
    }
    let mut s = Store::open(&Store::dir())?;
    s.install(
        Source {
            repo: card.repository.clone(),
            commit: card.version.commit.clone(),
            hash: card.version.hash.clone(),
            verified: card.verified,
        },
        &files,
        place,
        project,
    )?;
    widget_index::count_install(&card.id);
    Ok(())
}

/// Where a widget goes when added: the Workspace panel when it draws there,
/// else its own tab, else a bar.
fn place_for(m: &Manifest) -> Option<Place> {
    [Place::Workspace, Place::Tab, Place::Bar]
        .into_iter()
        .find(|p| p.size_for(m).is_some())
}

fn mark(card: &Card) -> logos::Mark {
    logos::mark(&card.author, "")
}

/// "mrvn · Verified · 2.1k installs".
fn byline(card: &Card, installs: bool) -> Div {
    let tone: Hsla = if card.verified {
        theme::text_subtle()
    } else {
        theme::attention().into()
    };
    div()
        .flex()
        .items_center()
        .gap(ui::sp(4.))
        .min_w_0()
        .ty(Type::Caption)
        .text_color(theme::text_subtle())
        .child(div().flex_none().child(card.author.clone()))
        .child("·")
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(ui::sp(3.))
                .text_color(tone)
                .child(
                    icon(
                        if card.verified {
                            "checkmark-circle02"
                        } else {
                            "shield-alert"
                        },
                        11.,
                    )
                    .text_color(tone),
                )
                .child(if card.verified {
                    "Verified"
                } else {
                    "Not verified"
                }),
        )
        .when(installs, |d| {
            d.child("·").child(div().truncate().child(format!(
                "{} installs",
                marketplace::short_count(card.installs)
            )))
        })
}

/// The preview on sample data, or why it can't draw.
fn preview_box(card: &Card, width: f32) -> Div {
    div()
        .flex()
        .flex_col()
        .justify_center()
        .overflow_hidden()
        .p(ui::sp(14.))
        .rounded(ui::rad(ui::RADIUS))
        .bg(theme::sunk())
        .border_1()
        .border_color(theme::border_subtle())
        .child(match &card.preview {
            Ok(block) => widget_blocks::render(block, width),
            Err(e) => div()
                .ty(Type::Caption)
                .text_color(theme::text_faint())
                .child(format!("The preview can't draw: {e}"))
                .into_any_element(),
        })
}

fn section_head(title: &str, sub: &str, end: Option<AnyElement>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(ui::sp(10.))
        .child(
            div()
                .flex_none()
                .ty(Type::Heading)
                .text_color(theme::text())
                .child(title.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .ty(Type::Caption)
                .text_color(theme::text_faint())
                .child(sub.to_string()),
        )
        .children(end)
}

/// A step of the add flow: its name and what it says.
fn step(name: &str, value: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(ui::sp(10.))
        .pt(ui::sp(12.))
        .border_t_1()
        .border_color(theme::border_subtle())
        .child(
            div()
                .flex_none()
                .ty(Type::Label)
                .text_color(theme::text())
                .child(name.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_end()
                .ty(Type::Caption)
                .text_color(theme::text_subtle())
                .child(value.to_string()),
        )
}

fn need_line(icon_name: &str, color: Hsla, what: &str, why: &str) -> Div {
    div()
        .flex()
        .items_start()
        .gap(ui::sp(8.))
        .child(icon(icon_name, 13.).flex_none().text_color(color))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .ty(Type::Label)
                        .text_color(theme::text())
                        .child(what.to_string()),
                )
                .when(!why.is_empty(), |d| {
                    d.child(
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(why.to_string()),
                    )
                }),
        )
}

fn busy_line(text: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(ui::sp(6.))
        .ty(Type::Caption)
        .text_color(theme::text_subtle())
        .child(ui::spinner(12., theme::text_faint()))
        .child(text.to_string())
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// "6 days ago", for a date in seconds since 1970.
fn when(date: u64) -> Option<String> {
    if date == 0 {
        return None;
    }
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(date);
    let days = SystemTime::now().duration_since(at).ok()?.as_secs() / 86_400;
    Some(match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=59 => format!("{days} days ago"),
        _ => format!("{} months ago", days / 30),
    })
}
