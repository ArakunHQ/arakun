//! The Design system tab: every token and component of `ui` and `theme` on
//! one page, live. It is the full preview of the Customize panel: a change
//! there shows here on every component at once.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::theme;
use crate::ui::{self, AreaChart, ButtonKind, Series, Tone, Type, Typography as _};

pub struct GalleryView {
    chart: Entity<AreaChart>,
    chips: [bool; 4],
    switches: [bool; 2],
    radio: usize,
    segment: usize,
    toggle: usize,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

/// Thirty days of made-up usage: two waves that drift apart.
fn demo_series() -> (Vec<Series>, Vec<SharedString>, Vec<Option<SharedString>>) {
    let n = 30;
    let wave = |i: usize, f: f64, phase: f64, base: f64, amp: f64| {
        (base + amp * ((i as f64 * f + phase).sin() * 0.5 + 0.5) * (0.6 + i as f64 / n as f64))
            .max(0.)
    };
    let a: Vec<f64> = (0..n)
        .map(|i| {
            if i < 6 {
                0.
            } else {
                wave(i, 0.55, 0.3, 20., 180.)
            }
        })
        .collect();
    let b: Vec<f64> = (0..n).map(|i| wave(i, 0.31, 2.1, 4., 60.)).collect();
    let labels = (0..n)
        .map(|i| SharedString::from(format!("Oct {}", i + 1)))
        .collect();
    let notes = (0..n)
        .map(|i| {
            Some(SharedString::from(format!(
                "{}M tokens",
                ((a[i] + b[i]) * 1.4).round()
            )))
        })
        .collect();
    (
        vec![
            Series {
                name: "Droid".into(),
                color: theme::mix(0.72),
                values: a,
            },
            Series {
                name: "Claude Code".into(),
                color: gpui::rgb(0xd97757).into(),
                values: b,
            },
        ],
        labels,
        notes,
    )
}

/// Hands the chart its made-up data. The Droid line's gray comes from the
/// mode and palette in use, so this runs on every render too; the chart
/// repaints only when something changed.
fn set_demo(c: &mut AreaChart, cx: &mut Context<AreaChart>) {
    let (series, labels, notes) = demo_series();
    let usd: ui::Format = Rc::new(|v| format!("${v:.2}"));
    let short: ui::Format = Rc::new(|v| format!("${v:.0}"));
    c.set(
        series,
        labels,
        vec!["Oct 1".into(), "Oct 15".into(), "Oct 30".into()],
        notes,
        usd,
        short,
        cx,
    );
}

impl GalleryView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let chart = cx.new(|cx| {
            let mut c = AreaChart::new(200.);
            set_demo(&mut c, cx);
            c
        });
        Self {
            chart,
            chips: [true, false, false, true],
            switches: [true, false],
            radio: 1,
            segment: 0,
            toggle: 0,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }
}

impl gpui::Focusable for GalleryView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---- layout helpers --------------------------------------------------------------

/// A titled block of the page with a line about what it is for.
fn section(title: &str, about: &str, body: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(16.))
        .pt(crate::ui::sp(28.))
        .border_t_1()
        .border_color(theme::border_subtle())
        .child(
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(4.))
                .child(ui::text(Type::Title, theme::text(), title.to_string()))
                .child(ui::text(
                    Type::Body,
                    theme::text_subtle(),
                    about.to_string(),
                )),
        )
        .child(body)
        .into_any_element()
}

/// A labeled specimen inside a section.
fn specimen(label: &str, body: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(10.))
        .child(ui::overline(label))
        .child(body)
}

fn row() -> gpui::Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(crate::ui::sp(10.))
}

/// A color chip with its role name and where it comes from.
fn swatch(name: &str, note: &str, color: Hsla) -> AnyElement {
    div()
        .w(px(132.))
        .flex()
        .flex_col()
        .gap(crate::ui::sp(6.))
        .child(
            div()
                .h(px(48.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .border_1()
                .border_color(theme::border())
                .bg(theme::background())
                .child(
                    div()
                        .size_full()
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .bg(color),
                ),
        )
        .child(ui::text(Type::Label, theme::text(), name.to_string()))
        .child(ui::text(
            Type::Caption,
            theme::text_faint(),
            note.to_string(),
        ))
        .into_any_element()
}

/// A floating surface like the menus use, for showing pieces in context.
fn panel(width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .p(crate::ui::sp(4.))
        .flex()
        .flex_col()
        .gap(crate::ui::sp(1.))
        .bg(theme::dialog())
        .border_1()
        .border_color(theme::border())
        .rounded(crate::ui::rad(ui::RADIUS_LG))
        .shadow_xl()
}

impl Render for GalleryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.chart.update(cx, set_demo);
        let r = theme::grays().ramp();

        let header = div()
            .flex_none()
            .px(crate::ui::sp(ui::PAGE_PAD))
            .pt(crate::ui::sp(ui::PAGE_PAD))
            .pb(crate::ui::sp(20.))
            .child(ui::page_title(
                "Design system",
                Some("The tokens and components every view builds from. Change them in Customize and they change here, live.".into_any_element()),
            ));

        // ---- color -------------------------------------------------------------
        let text_roles = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(ui::text(
                Type::Heading,
                theme::text(),
                "Primary — titles, values, what is selected",
            ))
            .child(ui::text(
                Type::Body,
                theme::text_muted(),
                format!(
                    "Muted — names and descriptions (content at {:.0}%)",
                    r.text_muted * 100.
                ),
            ))
            .child(ui::text(
                Type::Body,
                theme::text_subtle(),
                format!(
                    "Subtle — labels, units, column headers ({:.0}%)",
                    r.text_subtle * 100.
                ),
            ))
            .child(ui::text(
                Type::Body,
                theme::text_faint(),
                format!(
                    "Faint — placeholders, axis ticks, disabled ({:.0}%)",
                    r.text_faint * 100.
                ),
            ));
        let surfaces = row()
            .items_start()
            .child(swatch(
                "Background",
                "theme::background",
                theme::background(),
            ))
            .child(swatch("Raised", "fill_raised", theme::fill_raised()))
            .child(swatch("Hover", "fill_hover", theme::fill_hover()))
            .child(swatch("Selected", "fill_selected", theme::fill_selected()))
            .child(swatch("Dialog", "theme::dialog", theme::dialog()));
        let borders = row().gap(crate::ui::sp(16.)).children(
            [
                ("Subtle", "rules between rows", theme::border_subtle()),
                ("Default", "cards and controls", theme::border()),
                ("Strong", "inputs", theme::border_strong()),
            ]
            .map(|(name, note, c)| {
                div()
                    .w(px(180.))
                    .h(px(56.))
                    .p(crate::ui::sp(10.))
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(c)
                    .flex()
                    .flex_col()
                    .justify_between()
                    .child(ui::text(Type::Label, theme::text(), name))
                    .child(ui::text(Type::Caption, theme::text_faint(), note))
            }),
        );
        let tones = row()
            .items_start()
            .children(Tone::ALL.map(|t| swatch(t.name(), "ui::Tone", t.color())));
        let category = row()
            .gap(crate::ui::sp(6.))
            .children(theme::CATEGORY.map(|c| {
                div()
                    .size(px(28.))
                    .rounded(crate::ui::rad(ui::RADIUS_SM))
                    .bg(gpui::rgb(c))
            }));
        let color = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(24.))
            .child(specimen("Text", text_roles))
            .child(specimen("Surfaces", surfaces))
            .child(specimen("Borders", borders))
            .child(specimen("Status", tones))
            .child(specimen(
                "Categories — chart series, project logos",
                category,
            ));

        // ---- type ------------------------------------------------------------------
        let mut type_rows = div().flex().flex_col();
        for t in Type::ALL {
            let (size, line, weight) = t.metrics();
            let sample = match t {
                Type::Display => "$1,989.64".to_string(),
                Type::Overline => "RAW TOKEN COST".to_string(),
                Type::Mono => "src/usage_view.rs:812".to_string(),
                _ => "Droid finished the session in 4m 12s".to_string(),
            };
            let w = match weight {
                FontWeight::SEMIBOLD => "Semibold",
                FontWeight::MEDIUM => "Medium",
                _ => "Regular",
            };
            type_rows = type_rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(16.))
                    .py(crate::ui::sp(10.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .w(px(150.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .child(ui::text(Type::Label, theme::text(), t.name()))
                            .child(ui::text(
                                Type::Caption,
                                theme::text_faint(),
                                format!("{size:.0}/{line:.0} · {w}"),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ty(t)
                            .text_color(theme::text())
                            .child(sample),
                    ),
            );
        }

        // ---- space & shape -----------------------------------------------------------
        let spacing = row().items_end().gap(crate::ui::sp(14.)).children(
            [2., 4., 8., 12., 16., 24., 32.].map(|s| {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(
                        div()
                            .size(px(s))
                            .bg(theme::accent().opacity(0.7))
                            .rounded(crate::ui::rad(1.)),
                    )
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        format!("{s:.0}"),
                    ))
            }),
        );
        let radii = row().gap(crate::ui::sp(14.)).children(
            [
                (ui::RADIUS_SM, "4 · chips, badges"),
                (ui::RADIUS, "8 · controls"),
                (ui::RADIUS_LG, "12 · cards, menus"),
                (ui::RADIUS_XL, "16 · window cards"),
            ]
            .map(|(r, note)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(6.))
                    .child(
                        div()
                            .w(px(110.))
                            .h(px(64.))
                            .rounded(crate::ui::rad(r))
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::fill_raised()),
                    )
                    .child(ui::text(Type::Caption, theme::text_faint(), note))
            }),
        );
        let heights = row().items_end().gap(crate::ui::sp(14.)).children(
            [
                (20., "Badge, kbd"),
                (ui::ICON_BUTTON, "Icon button"),
                (ui::CONTROL_H, "Chip, segmented"),
                (ui::BUTTON_H, "Button"),
                (ui::MENU_ROW_H, "Menu row"),
                (ui::INPUT_H, "Input"),
            ]
            .map(|(h, note)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(6.))
                    .child(
                        div()
                            .w(px(84.))
                            .h(px(h))
                            .rounded(crate::ui::rad(ui::RADIUS))
                            .border_1()
                            .border_color(theme::border())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::text(
                                Type::Caption,
                                theme::text_subtle(),
                                format!("{h:.0}"),
                            )),
                    )
                    .child(ui::text(Type::Caption, theme::text_faint(), note))
            }),
        );

        // ---- controls ----------------------------------------------------------------------
        let buttons = row()
            .child(ui::button("ds-b1", "Primary", ButtonKind::Primary, true))
            .child(ui::button("ds-b2", "Outline", ButtonKind::Outline, true))
            .child(ui::button("ds-b3", "Ghost", ButtonKind::Ghost, true))
            .child(ui::button("ds-b4", "Danger", ButtonKind::Danger, true))
            .child(ui::button_with(
                "ds-b5",
                Some("add01"),
                "With icon",
                ButtonKind::Outline,
                true,
            ))
            .child(ui::button("ds-b6", "Disabled", ButtonKind::Primary, false));
        let icon_buttons = row().gap(crate::ui::sp(4.)).children(
            [
                "add01",
                "refresh01",
                "settings01",
                "search01",
                "more-horizontal",
            ]
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                ui::icon_button(("ds-ib", i), name, 14.).tooltip(ui::tip("Icon button"))
            }),
        );
        let mut seg = ui::segmented();
        for (i, label) in ["7 days", "30 days", "90 days"].into_iter().enumerate() {
            seg = seg.child(
                ui::segment(("ds-seg", i), None, label, self.segment == i).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.segment = i;
                        cx.notify();
                    },
                )),
            );
        }
        let toggle = ui::toggle_group(
            "ds-toggle",
            &[(0usize, "Cost"), (1, "Tokens")],
            self.toggle,
            |seg, v| {
                seg.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle = v;
                    cx.notify();
                }))
            },
        );
        let chips = row().gap(crate::ui::sp(6.)).children(
            ["Claude Code", "Droid", "This week", "Failed"]
                .into_iter()
                .enumerate()
                .map(|(i, label)| {
                    ui::chip(("ds-chip", i), label, self.chips[i]).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.chips[i] = !this.chips[i];
                            cx.notify();
                        },
                    ))
                }),
        );
        let switches = row().gap(crate::ui::sp(20.)).children(
            ["Glass", "Read aloud"]
                .into_iter()
                .enumerate()
                .map(|(i, label)| {
                    div()
                        .id(("ds-switch", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .cursor_pointer()
                        .child(ui::switch(self.switches[i]))
                        .child(ui::text(Type::Body, theme::text_muted(), label))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.switches[i] = !this.switches[i];
                            cx.notify();
                        }))
                }),
        );
        let badges = row()
            .gap(crate::ui::sp(6.))
            .children(Tone::ALL.map(|t| ui::badge(t.name(), t)))
            .child(ui::tag("DNS", gpui::rgb(theme::CATEGORY[3])))
            .child(div().w(px(12.)))
            .children(
                [
                    ("Running", Tone::Info),
                    ("Passed", Tone::Success),
                    ("Flaky", Tone::Warning),
                    ("Failed", Tone::Danger),
                ]
                .map(|(l, t)| ui::status_dot(l, t)),
            );
        let keys = row()
            .gap(crate::ui::sp(16.))
            .child(ui::shortcut(&["Ctrl", "K"]))
            .child(ui::shortcut(&["Shift", "Tab"]))
            .child(ui::shortcut(&["Esc"]));
        let input = ui::field()
            .w(px(280.))
            .child(ui::icon("search01", 14.).text_color(theme::text_faint()))
            .child(
                div()
                    .text_color(theme::text_faint())
                    .child("Search sessions…"),
            );

        // ---- menus ----------------------------------------------------------------------------
        let modes = [
            ("Manual", "Always ask before making changes"),
            ("Accept edits", "Automatically accept all file edits"),
            ("Plan", "Create a plan before making changes"),
            ("Bypass permissions", "Accepts all permissions"),
        ];
        let mut mode_menu = panel(300.).child(ui::menu_heading("Mode"));
        for (i, (name, desc)) in modes.into_iter().enumerate() {
            mode_menu = mode_menu.child(
                ui::choice_row(("ds-radio", i), self.radio == i)
                    .child(div().flex_1().flex().flex_col().child(name).child(ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        desc,
                    )))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.radio = i;
                        cx.notify();
                    })),
            );
        }
        let model_menu = panel(250.)
            .child(ui::submenu_row("ds-sub1", "Model", "Opus 5.5", true))
            .child(ui::submenu_row("ds-sub2", "Reasoning", "High", false))
            .child(ui::switch_row("ds-fast", "Fast mode", true))
            .child(ui::menu_divider())
            .child(ui::menu_item(
                "ds-reset",
                "undo",
                "Reset to default",
                false,
                true,
            ));
        let menus = row()
            .items_start()
            .gap(crate::ui::sp(24.))
            .child(model_menu)
            .child(mode_menu);

        // ---- data -------------------------------------------------------------------------------
        let spark = |seed: f64| -> Vec<f64> {
            (0..14)
                .map(|i| {
                    let x = i as f64;
                    ((x * (0.35 + seed * 0.18) + seed).sin() + 1.2) * (1. + x * seed / 25.)
                        + (x * 1.7 + seed * 3.).cos() * 0.35
                })
                .collect()
        };
        let stat_cell = |label: &str, value: &str, sub: &str, seed: f64, last: bool| {
            div()
                .flex_1()
                .min_w(px(160.))
                .flex()
                .items_end()
                .justify_between()
                .gap(crate::ui::sp(8.))
                .px(crate::ui::sp(16.))
                .py(crate::ui::sp(14.))
                .when(!last, |d| {
                    d.border_r_1().border_color(theme::border_subtle())
                })
                .child(ui::stat(label, value.to_string(), sub.to_string()))
                .child(ui::sparkline(spark(seed), theme::accent(), 56., 24.))
        };
        let stats = div()
            .flex()
            .flex_wrap()
            .border_t_1()
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(stat_cell(
                "Processed tokens",
                "3.1B",
                "148M/active day",
                0.2,
                false,
            ))
            .child(stat_cell(
                "Cached input",
                "2.91B",
                "94.7% of input",
                1.4,
                false,
            ))
            .child(stat_cell(
                "Output",
                "25.7M",
                "227 sessions · 21 days",
                2.6,
                false,
            ))
            .child(stat_cell(
                "Cache savings",
                "$11,893",
                "6.0× the raw cost",
                3.3,
                true,
            ));
        let limits = div()
            .w(px(340.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(ui::limit_line(
                "5-hour limit",
                12.,
                Some("resets in 2h 25m".into()),
            ))
            .child(ui::limit_line(
                "Core · Week",
                61.,
                Some("resets Oct 31".into()),
            ))
            .child(ui::limit_line(
                "Core · Month",
                78.,
                Some("resets Nov 8".into()),
            ))
            .child(ui::limit_line(
                "Standard · Month",
                100.,
                Some("resets Nov 1".into()),
            ));
        let kvs = div()
            .w(px(340.))
            .flex()
            .flex_col()
            .child(ui::kv("Model priced", "95.7%"))
            .child(ui::kv("Unpriced", "4.3%"))
            .child(ui::kv("Cache savings", "$11,893.75"));
        let chart = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .child(ui::heading("Daily cost").flex_1())
                    .child(ui::legend("Droid", theme::mix(0.72)))
                    .child(ui::legend("Claude Code", gpui::rgb(0xd97757).into())),
            )
            .child(self.chart.clone());

        // ---- feedback ---------------------------------------------------------------------------
        let tooltip = div()
            .px(crate::ui::sp(8.))
            .py(crate::ui::sp(4.))
            .rounded(crate::ui::rad(ui::RADIUS_SM))
            .bg(theme::dialog())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .ty(Type::Caption)
            .text_color(theme::text())
            .child("Read the logs again");
        let empty = div()
            .w(px(380.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .child(ui::empty_state(
                "dashboard-square01",
                "No usage in this range",
                "Every turn Claude Code or Droid finishes shows up here, with its cost at API prices.",
                Some(ui::button("ds-empty", "Show 90 days", ButtonKind::Outline, true).into_any_element()),
            ));

        // ---- work surfaces --------------------------------------------------------------------
        let amber = theme::accent();
        let green: Hsla = theme::success().into();
        let blue: Hsla = theme::link().into();
        let surfaces = ui::surface_card()
            .w(px(440.))
            .child(
                row()
                    .gap(crate::ui::sp(11.))
                    .px(crate::ui::sp(16.))
                    .pt(crate::ui::sp(14.))
                    .pb(crate::ui::sp(12.))
                    .child(ui::icon_chip("github", 34., None))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(ui::text(Type::Heading, theme::text(), "Review queue"))
                            .child(ui::text(
                                Type::Label,
                                theme::text_subtle(),
                                "ArakunHQ · 11 tracked",
                            )),
                    )
                    .child(ui::pill("4 waiting on you", amber)),
            )
            .child(div().px(crate::ui::sp(16.)).child(ui::segment_bar(
                &[
                    (4., amber),
                    (2., blue),
                    (1., green),
                    (4., theme::border_strong()),
                ],
                7.,
            )))
            .child(
                ui::inset_panel().m(px(10.)).child(
                    row()
                        .gap(crate::ui::sp(11.))
                        .px(crate::ui::sp(14.))
                        .py(crate::ui::sp(8.))
                        .child(ui::icon_chip("git-pull-request", 28., Some(amber)))
                        .child(div().flex_1().child(ui::text(
                            Type::Body,
                            theme::text(),
                            "ci: add a weekly report on runner sizing",
                        )))
                        .child(ui::mini_squares(&[green, green, green, amber], 9.)),
                ),
            );
        let pills = row()
            .gap(crate::ui::sp(8.))
            .child(ui::key_pill(
                "ds-kp-1",
                None,
                "Comment",
                Some("C"),
                ui::PillKind::Normal,
                true,
            ))
            .child(ui::key_pill(
                "ds-kp-2",
                None,
                "Discard",
                None,
                ui::PillKind::Quiet,
                true,
            ))
            .child(ui::key_pill(
                "ds-kp-3",
                Some(("tick02", None)),
                "Approve",
                Some("A"),
                ui::PillKind::Primary,
                true,
            ))
            .child(ui::outline_tab("ds-ot-1", "Timeline", None, true))
            .child(ui::outline_tab("ds-ot-2", "Files", Some("8".into()), false))
            .child(ui::key_hint("Next", "J"))
            .child(ui::icon_tile("git-pull-request", 26., green))
            .child(ui::avatar("rikvisser-dev", 24.))
            .child(ui::avatar("karlo", 24.))
            // Pictures come from GitHub; with none on disk they show initials.
            .child(ui::avatar_image("ana", None, 24.))
            .child(ui::state_pill("tick02", "Approved", green))
            .child(ui::avatar_badge(
                "rikvisser-dev",
                None,
                32.,
                "git-pull-request",
                amber,
                theme::background(),
            ))
            .child(ui::avatar_stack(
                &[
                    ("rikvisser-dev".into(), None),
                    ("karlo".into(), None),
                    ("ana".into(), None),
                ],
                24.,
                theme::fill_raised(),
            ));

        let body = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(36.))
            .child(section("Color", "Roles, not numbers: views pick what a gray is for. The ramp behind them is Customize › Colors › Grays.", color))
            .child(section("Type", "Each style is a size, a line height and a weight together. `.ty(Type::Body)` on any element.", type_rows))
            .child(section(
                "Space and shape",
                "Spacing in steps of 4 (2 for hairlines). Radii follow the rule of 4. Controls of a kind share a height.",
                div().flex().flex_col().gap(crate::ui::sp(24.)).child(specimen("Spacing", spacing)).child(specimen("Radius", radii)).child(specimen("Heights", heights)),
            ))
            .child(section(
                "Controls",
                "Buttons for actions, segmented for modes, toggle groups for what a chart shows, chips for filters.",
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(24.))
                    .child(specimen("Buttons", buttons))
                    .child(specimen("Icon buttons", icon_buttons))
                    .child(specimen("Segmented and toggle group", row().gap(crate::ui::sp(16.)).child(seg).child(toggle)))
                    .child(specimen("Chips", chips))
                    .child(specimen("Switches", switches))
                    .child(specimen("Badges and status", badges))
                    .child(specimen("Keys", keys))
                    .child(specimen("Input", input)),
            ))
            .child(section("Menus", "Hover a row with a chevron to open its choices beside it; radio rows say what each choice means.", menus))
            .child(section(
                "Data",
                "Numbers line up, health reads from color, and the chart explains any day you point at.",
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(28.))
                    .child(specimen("Stats", stats))
                    .child(row().items_start().gap(crate::ui::sp(40.)).child(specimen("Limits", limits)).child(specimen("Key and value", kvs)))
                    .child(specimen("Area chart", chart)),
            ))
            .child(section(
                "Work surfaces",
                "A card of its own inside a view, the darker panel nested in it, round status chips, pills with their key, outlined tabs.",
                div().flex().flex_col().gap(crate::ui::sp(20.)).child(specimen("Card and inset", surfaces)).child(specimen("Pills, tabs and people", pills)),
            ))
            .child(section(
                "Feedback",
                "Tooltips name icon-only buttons; empty states say what a place is for and the one thing to do.",
                row().items_start().gap(crate::ui::sp(40.)).child(specimen("Tooltip", tooltip)).child(specimen("Empty state", empty)),
            ));

        div()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::text())
            .child(header)
            .child(
                div()
                    .id("gallery-body")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pb(crate::ui::sp(48.))
                    .child(body),
            )
    }
}
