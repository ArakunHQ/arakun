//! The tab's frame and its newer pages: the side navigation, the Overview
//! (numbers, a week of traffic, security and certificates, what needs
//! attention grouped by kind, the domains), Security Center findings,
//! Containers and Email Routing.

use super::*;
use crate::cloudflare::fix::{self, Fix};
use crate::logos;
use crate::ui::{AreaChart, Format, Series, Tone, Type, Typography as _};

/// Issues of one kind, shown as one card when there are several.
struct Group<'a> {
    key: &'static str,
    title: String,
    issues: Vec<(usize, &'a Issue)>,
}

fn kind_of(subject: &Subject) -> (&'static str, &'static str) {
    match subject {
        Subject::Worker(_) => ("workers", "Workers logging errors"),
        Subject::Zone(_) => ("zones", "Domains answering 5xx"),
        Subject::Tunnel(_) => ("tunnels", "Tunnels down with live hostnames"),
        Subject::Dns(_) => ("dns", "DNS records that route nowhere"),
        Subject::Ssl(_) => ("ssl", "Weak SSL settings"),
        Subject::Registration(_) => ("registrations", "Domain registrations"),
        Subject::Pages(_) => ("pages", "Pages deployments failing"),
        Subject::Gateway(_) => ("gateways", "AI Gateway failures"),
        Subject::Workflow(_) => ("flows", "Workflows failing"),
        Subject::Queue(_) => ("queues", "Queues"),
        Subject::Cloudflared => ("cloudflared", "cloudflared on this machine"),
    }
}

impl CloudflareView {
    // -----------------------------------------------------------------------
    // Fixes: a button, a confirmation that says what changes, then the change.

    fn apply_fix(&mut self, fix: Fix, cx: &mut Context<Self>) {
        let key = fix.key();
        if self.fixing.is_some() {
            return;
        }
        self.fixing = Some(key.clone());
        self.fix_confirm = None;
        self.fix_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { fix::apply(&fix) }).await;
            let _ = this.update(cx, |this, cx| {
                this.fixing = None;
                match result {
                    Ok(()) => {
                        this.fixed.insert(key);
                        this.refresh(cx);
                    }
                    Err(e) => this.fix_error = Some((key, e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The button for `fix`, or its state (applying, done).
    fn fix_button(&self, fix: &Fix, id: String, cx: &mut Context<Self>) -> AnyElement {
        let key = fix.key();
        if self.fixed.contains(&key) {
            return ui::badge("Fixed ✓", Tone::Success).into_any_element();
        }
        if self.fixing.as_ref() == Some(&key) {
            return div()
                .flex()
                .items_center()
                .gap(px(6.))
                .ty(Type::Caption)
                .text_color(theme::text_subtle())
                .child(ui::spinner(
                    SharedString::from(format!("{id}-busy")),
                    12.,
                    theme::text_faint(),
                ))
                .child("Applying…")
                .into_any_element();
        }
        ui::button_with(
            SharedString::from(id),
            Some("wrench01"),
            fix.label(),
            ButtonKind::Outline,
            self.fixing.is_none(),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.fix_confirm = Some(key.clone());
            this.fix_error = None;
            cx.notify();
        }))
        .into_any_element()
    }

    /// Under the row whose button was clicked: what changes, what could go wrong, Apply.
    fn fix_confirmation(&self, fix: &Fix, id: String, cx: &mut Context<Self>) -> Option<Div> {
        let key = fix.key();
        let error = self
            .fix_error
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, e)| e.clone());
        if self.fix_confirm.as_ref() != Some(&key) && error.is_none() {
            return None;
        }
        let apply = fix.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(12.))
                .rounded(px(ui::RADIUS))
                .border_1()
                .border_color(theme::accent().opacity(0.5))
                .bg(theme::background())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon("wrench01", 13.).text_color(theme::accent()))
                        .child(
                            div()
                                .ty(Type::Body)
                                .text_color(theme::text())
                                .child(fix.describe()),
                        ),
                )
                .children(fix.warning().map(|w| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon("alert-circle", 13.).text_color(Tone::Warning.color()))
                        .child(
                            div()
                                .ty(Type::Caption)
                                .text_color(theme::text_muted())
                                .child(w),
                        )
                }))
                .children(error.map(|e| {
                    div()
                        .ty(Type::Caption)
                        .text_color(Tone::Danger.color())
                        .child(format!("Cloudflare said: {e}"))
                }))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(div().flex_1())
                        .child(
                            ui::button(
                                SharedString::from(format!("{id}-cancel")),
                                "Cancel",
                                ButtonKind::Ghost,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.fix_confirm = None;
                                this.fix_error = None;
                                cx.notify();
                            })),
                        )
                        .child(
                            ui::button_with(
                                SharedString::from(format!("{id}-apply")),
                                Some("checkmark-circle02"),
                                "Apply",
                                ButtonKind::Primary,
                                self.fixing.is_none(),
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.apply_fix(apply.clone(), cx)
                                }),
                            ),
                        ),
                ),
        )
    }

    // -----------------------------------------------------------------------
    // The frame.

    pub(super) fn render_header(&self, now: u64, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(px(60.))
            .px(px(20.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(logos::tile(logos::mark("cloudflare", ""), 32.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .ty(Type::Subtitle)
                            .text_color(theme::text())
                            .child("Cloudflare"),
                    )
                    .children(self.snap.as_ref().map(|s| {
                        div()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(format!("{} · {}", s.account_name, s.email))
                    })),
            )
            .child(div().flex_1())
            .when(self.analytics_pending, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(24.))
                        .px(px(8.))
                        .rounded(px(ui::RADIUS))
                        .bg(theme::fill_raised())
                        .ty(Type::Caption)
                        .text_color(theme::text_subtle())
                        .child(ui::spinner("cf-analytics", 11., theme::text_faint()))
                        .child("Updating analytics"),
                )
            })
            .child(ui::refresh_control(
                "cf-refresh",
                self.loading,
                self.snap.as_ref().map(|s| cloudflare::ago(s.fetched, now)),
                cx.listener(|this, _, _, cx| {
                    this.error = None;
                    this.refresh(cx)
                }),
            ))
            .when(self.snap.is_some(), |d| {
                d.child(
                    ui::button_with(
                        "cf-dash",
                        Some("link-square02"),
                        "Dashboard",
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.open("", cx))),
                )
            })
    }

    pub(super) fn render_nav(&self, s: &Snapshot, cx: &mut Context<Self>) -> Stateful<Div> {
        let issues = s.issues();
        let urgent = issues.iter().any(|i| i.high);
        let real = issues.iter().filter(|i| !i.bots).count();
        let down = s
            .tunnels
            .iter()
            .filter(|t| !t.up() && !t.hostnames.is_empty())
            .count();
        let critical = s.insights.iter().filter(|i| i.rank() <= 1).count();
        let failing_containers = s.containers.iter().filter(|c| c.failed > 0).count();
        let email_zones = s
            .zones
            .iter()
            .filter(|z| z.email_enabled == Some(true))
            .count();
        let mut nav = div()
            .id("cf-nav")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(216.))
            .h_full()
            .overflow_y_scroll()
            .gap(px(2.))
            .px(px(12.))
            .py(px(16.))
            .border_r_1()
            .border_color(theme::border_subtle());
        let entries: [(Page, &str, Option<String>, Option<Tone>); 11] = [
            (
                Page::Summary,
                "dashboard-square01",
                (real > 0).then(|| real.to_string()),
                (real > 0).then_some(if urgent { Tone::Danger } else { Tone::Warning }),
            ),
            (
                Page::Domains,
                "globe",
                Some(s.zones.len().to_string()),
                None,
            ),
            (
                Page::Workers,
                "flash",
                Some((s.workers.len() + s.pages.len()).to_string()),
                None,
            ),
            (
                Page::Logs,
                "activity01",
                self.tail.as_ref().map(|_| "live".to_string()),
                self.tail.as_ref().map(|_| Tone::Success),
            ),
            (
                Page::Containers,
                "cube",
                Some(s.containers.len().to_string()),
                (failing_containers > 0).then_some(Tone::Danger),
            ),
            (
                Page::Tunnels,
                "link01",
                Some(s.tunnels.len().to_string()),
                (down > 0).then_some(Tone::Danger),
            ),
            (
                Page::Storage,
                "archive02",
                Some(
                    (s.buckets.len() + s.databases.len() + s.kv.len() + s.queues.len()).to_string(),
                ),
                None,
            ),
            (
                Page::Security,
                "shield-alert",
                Some(s.insights.len().to_string()),
                (critical > 0).then_some(Tone::Danger),
            ),
            (Page::Email, "at", Some(email_zones.to_string()), None),
            (
                Page::Activity,
                "clock01",
                Some(s.alerts.len().to_string()),
                None,
            ),
            (Page::Tools, "wrench01", None, None),
        ];
        for (i, (page, icon_name, n, tone)) in entries.into_iter().enumerate() {
            if i == 1 || i == 7 || i == 9 {
                nav = nav.child(div().flex_none().h(px(10.)));
            }
            let on = self.page == page;
            nav = nav.child(
                div()
                    .id(("cf-nav-row", i))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .h(px(32.))
                    .px(px(10.))
                    .rounded(px(ui::RADIUS))
                    .cursor_pointer()
                    .ty(Type::Label)
                    .when(on, |d| {
                        d.bg(theme::fill_selected()).text_color(theme::text())
                    })
                    .when(!on, |d| {
                        d.text_color(theme::text_muted())
                            .hover(|d| d.bg(theme::fill_hover()).text_color(theme::text()))
                    })
                    .child(icon(icon_name, 14.).text_color(if on {
                        theme::text()
                    } else {
                        theme::text_subtle()
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(page.label()))
                    .children(n.map(|n| {
                        match tone {
                            Some(t) => div()
                                .flex_none()
                                .px(px(6.))
                                .h(px(18.))
                                .flex()
                                .items_center()
                                .rounded_full()
                                .bg(t.color().opacity(0.18))
                                .ty(Type::Caption)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.color())
                                .child(n),
                            None => div()
                                .flex_none()
                                .ty(Type::Caption)
                                .text_color(theme::text_faint())
                                .child(n),
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        this.scroll.set_offset(gpui::point(px(0.), px(0.)));
                        cx.notify();
                    })),
            );
        }
        nav
    }

    // -----------------------------------------------------------------------
    // Overview.

    pub(super) fn render_overview(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let now = crate::model::unix_now();
        let issues = s.issues();
        let real = issues.iter().filter(|i| !i.bots).count();
        let urgent = issues.iter().filter(|i| i.high).count();
        let (served, bad) = (s.served(), s.server_errors());
        let worker_requests: u64 = s.workers.iter().map(|w| w.requests).sum();
        let failing = s.workers.iter().filter(|w| w.errors > 0).count();
        let up = s.tunnels.iter().filter(|t| t.up()).count();
        let critical = s.insights.iter().filter(|i| i.rank() <= 1).count();
        // The week, summed over the domains.
        let days = DAYS as usize;
        let mut week = vec![Day::default(); days];
        for z in &s.zones {
            let off = days.saturating_sub(z.days.len());
            for (i, d) in z.days.iter().enumerate() {
                if let Some(w) = week.get_mut(off + i) {
                    w.requests += d.requests;
                    w.cached += d.cached;
                    w.threats += d.threats;
                    w.visitors += d.visitors;
                }
            }
        }
        let visitors = week.last().map_or(0, |d| d.visitors);
        let reqs: Vec<f64> = week.iter().map(|d| d.requests as f64).collect();

        let kpis = div()
            .flex()
            .gap(px(12.))
            .child(kpi(
                "Requests · 24h",
                known(s.traffic, count(served)),
                if served > 0 {
                    format!("{} server errors", pct(bad, served))
                } else {
                    format!("{} domains", s.zones.len())
                },
                if bad > 0 { Tone::Warning } else { Tone::Info },
                Some(reqs.clone()),
            ))
            .child(kpi(
                "Visitors · today",
                known(s.traffic, count(visitors)),
                "unique, all domains".into(),
                Tone::Info,
                Some(week.iter().map(|d| d.visitors as f64).collect()),
            ))
            .child(kpi(
                "Worker requests · 24h",
                known(s.usage, count(worker_requests)),
                format!(
                    "{} errors in {failing} of {}",
                    count(s.errors()),
                    s.workers.len()
                ),
                if failing > 0 {
                    Tone::Danger
                } else {
                    Tone::Success
                },
                None,
            ))
            .child(kpi(
                "Tunnels up",
                if s.tunnels_read {
                    format!("{up}/{}", s.tunnels.len())
                } else {
                    "—".into()
                },
                if s.tunnels.is_empty() {
                    "none".into()
                } else {
                    format!("{} down", s.tunnels.len() - up)
                },
                if up < s.tunnels.len() {
                    Tone::Danger
                } else {
                    Tone::Success
                },
                None,
            ))
            .child(kpi(
                "Security findings",
                s.insights.len().to_string(),
                if critical > 0 {
                    format!("{critical} critical or high")
                } else {
                    "none critical".into()
                },
                if critical > 0 {
                    Tone::Danger
                } else {
                    Tone::Success
                },
                None,
            ));

        // A week of traffic, all domains.
        let labels: Vec<SharedString> = (0..days)
            .map(|i| cloudflare::date(now - (days - 1 - i) as u64 * 86_400).into())
            .collect();
        let ticks = vec![
            labels.first().cloned().unwrap_or_default(),
            labels.get(days / 2).cloned().unwrap_or_default(),
            "Today".into(),
        ];
        let series = vec![
            Series {
                name: "Requests".into(),
                color: Tone::Info.color(),
                values: reqs,
            },
            Series {
                name: "From cache".into(),
                color: Tone::Success.color(),
                values: week.iter().map(|d| d.cached as f64).collect(),
            },
            Series {
                name: "Threats".into(),
                color: Tone::Danger.color(),
                values: week.iter().map(|d| d.threats as f64).collect(),
            },
        ];
        let fmt: Format = std::rc::Rc::new(|v| count(v as u64));
        self.chart.update(cx, |c, cx| {
            c.set(
                series,
                labels,
                ticks,
                vec![None; days],
                fmt.clone(),
                fmt,
                cx,
            )
        });
        let traffic = panel("Traffic · 7 days")
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .gap(px(16.))
                    .child(ui::legend("Requests", Tone::Info.color()))
                    .child(ui::legend("From cache", Tone::Success.color()))
                    .child(ui::legend("Threats", Tone::Danger.color())),
            )
            .child(if s.traffic {
                self.chart.clone().into_any_element()
            } else {
                quiet("Traffic analytics didn't come back this time.").into_any_element()
            });

        let side = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(360.))
            .gap(px(12.))
            .child(self.security_card(s, cx))
            .child(self.billing_card(s, now, cx))
            .child(self.certs_card(s, now));

        let mut groups: Vec<Group> = Vec::new();
        for (i, issue) in issues.iter().enumerate() {
            let (key, title) = kind_of(&issue.subject);
            match groups.iter_mut().find(|g| g.key == key) {
                Some(g) => g.issues.push((i, issue)),
                None => groups.push(Group {
                    key,
                    title: title.into(),
                    issues: vec![(i, issue)],
                }),
            }
        }
        let review_label: SharedString = format!("Review with {}", agent_name(self.agent)).into();
        let review = ui::button_with(
            SharedString::from("cf-review"),
            Some("sparkles"),
            review_label,
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(s) = &this.snap {
                cx.emit(CloudflareEvent::Ask(
                    s.review(this.mcp(), crate::model::unix_now()),
                ));
            }
        }));
        let mut attention = div().flex().flex_col().gap(px(10.)).child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .ty(Type::Heading)
                        .text_color(theme::text())
                        .child("Needs attention"),
                )
                .when(real > 0, |d| {
                    d.child(ui::badge(
                        real.to_string(),
                        if urgent > 0 {
                            Tone::Danger
                        } else {
                            Tone::Warning
                        },
                    ))
                })
                .child(div().flex_1())
                .child(review),
        );
        if issues.is_empty() {
            attention = attention.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .p(px(14.))
                    .rounded(px(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .child(icon("checkmark-circle02", 16.).text_color(Tone::Success.color()))
                    .child(div().ty(Type::Body).text_color(theme::text_muted()).child("No Worker errors, failing domains, broken Tunnels or DNS, or stuck jobs right now.")),
            );
        }
        for g in groups {
            attention = attention.child(self.render_group(s, g, cx));
        }

        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(kpis)
            .child(
                div()
                    .flex()
                    .gap(px(12.))
                    .items_start()
                    .child(traffic)
                    .child(side),
            )
            .child(attention)
            .child(self.domains_strip(s, cx))
    }

    fn render_group(&self, s: &Snapshot, g: Group, cx: &mut Context<Self>) -> AnyElement {
        if g.issues.len() == 1 {
            let (i, issue) = g.issues[0];
            return self.issue_row(s, i, issue, cx).into_any_element();
        }
        let open = self.groups_open.contains(g.key);
        let high = g.issues.iter().any(|(_, i)| i.high);
        let bots = g.issues.iter().all(|(_, i)| i.bots);
        let tone = if high {
            Tone::Danger
        } else if bots {
            Tone::Neutral
        } else {
            Tone::Warning
        };
        let key = g.key;
        let names: Vec<String> = g
            .issues
            .iter()
            .map(|(_, i)| subject_name(&i.subject))
            .collect();
        let mut card = div()
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .overflow_hidden()
            .child(
                div()
                    .id(SharedString::from(format!("cf-group-{key}")))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .py(px(12.))
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::fill_hover()))
                    .child(
                        div()
                            .flex_none()
                            .size(px(8.))
                            .rounded_full()
                            .bg(tone.color()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .ty(Type::Body)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme::text())
                                            .child(g.title),
                                    )
                                    .child(ui::badge(g.issues.len().to_string(), tone)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(names.join(" · ")),
                            ),
                    )
                    .child(
                        icon(if open { "arrow-up01" } else { "arrow-down01" }, 14.)
                            .text_color(theme::text_faint()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.groups_open.remove(key) {
                            this.groups_open.insert(key);
                        }
                        cx.notify();
                    })),
            );
        if open {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(10.))
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .bg(theme::background())
                    .children(
                        g.issues
                            .into_iter()
                            .map(|(i, issue)| self.issue_row(s, i, issue, cx)),
                    ),
            );
        }
        card.into_any_element()
    }

    fn issue_row(&self, s: &Snapshot, i: usize, issue: &Issue, cx: &mut Context<Self>) -> Div {
        let code = match &issue.subject {
            Subject::Worker(name) => {
                !issue.bots && s.worker(name).is_some_and(|w| !w.samples.is_empty())
            }
            _ => false,
        };
        let fixes = fix::for_issue(&issue.subject, s);
        if fixes.is_empty() {
            return self.render_issue(i, issue, code, cx);
        }
        let mut out = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(self.render_issue(i, issue, code, cx));
        for (n, f) in fixes.iter().enumerate() {
            let id = format!("cf-issue-fix-{i}-{n}");
            out = out
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .pl(px(40.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .child(f.describe()),
                        )
                        .child(self.fix_button(f, id.clone(), cx)),
                )
                .children(self.fix_confirmation(f, id, cx).map(|c| c.ml(px(40.))));
        }
        out
    }

    fn security_card(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let count_of = |sev: &str| s.insights.iter().filter(|i| i.severity == sev).count();
        let mut card = panel("Security Center").child(
            div().flex().gap(px(8.)).children(
                [
                    ("Critical", Tone::Danger),
                    ("High", Tone::Danger),
                    ("Moderate", Tone::Warning),
                    ("Low", Tone::Info),
                ]
                .into_iter()
                .map(|(sev, tone)| {
                    let n = count_of(sev);
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .gap(px(2.))
                        .p(px(8.))
                        .rounded(px(ui::RADIUS))
                        .bg(if n > 0 {
                            tone.color().opacity(0.1)
                        } else {
                            theme::fill_hover()
                        })
                        .child(
                            div()
                                .ty(Type::Heading)
                                .text_color(if n > 0 {
                                    tone.color()
                                } else {
                                    theme::text_faint()
                                })
                                .child(n.to_string()),
                        )
                        .child(
                            div()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .child(sev),
                        )
                }),
            ),
        );
        for i in s.insights.iter().take(3) {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_none()
                            .size(px(6.))
                            .rounded_full()
                            .bg(severity_tone(&i.severity).color()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_muted())
                            .child(format!("{} · {}", i.title(), i.subject)),
                    ),
            );
        }
        card.child(
            div().flex().child(
                ui::button(
                    "cf-see-security",
                    "See all findings",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.page = Page::Security;
                    cx.notify();
                })),
            ),
        )
    }

    /// What the account spends: the budget alert's figure, the invoices when readable.
    fn billing_card(&self, s: &Snapshot, now: u64, cx: &mut Context<Self>) -> Div {
        let budget = s
            .alerts
            .iter()
            .filter(|a| a.name.to_lowercase().contains("budget"))
            .find_map(|a| cloudflare::spend_in(&a.detail).map(|v| (v, a.sent)));
        let mut card = panel("Spend");
        card = card.child(match budget {
            Some((v, when)) => div()
                .flex()
                .items_end()
                .gap(px(8.))
                .child(
                    div()
                        .ty(Type::Title)
                        .text_color(theme::text())
                        .child(format!("${v:.2}")),
                )
                .child(
                    div()
                        .pb(px(3.))
                        .ty(Type::Caption)
                        .text_color(theme::text_subtle())
                        .child(format!(
                            "this period · budget alert {}",
                            cloudflare::ago(when, now)
                        )),
                ),
            None => div()
                .ty(Type::Caption)
                .text_color(theme::text_subtle())
                .child("No budget alert has reported a figure yet."),
        });
        match &s.billing {
            Some(charges) if !charges.is_empty() => {
                for c in charges.iter().take(3) {
                    card = card.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_muted())
                                    .child(c.description.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .ty(Type::Caption)
                                    .text_color(theme::text())
                                    .child(format!("{:.2} {}", c.amount, c.currency)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_faint())
                                    .child(cloudflare::ago(c.when, now)),
                            ),
                    );
                }
            }
            Some(_) => {
                card = card.child(
                    div()
                        .ty(Type::Caption)
                        .text_color(theme::text_subtle())
                        .child("No invoices yet."),
                )
            }
            None => {
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .child("The cf sign-in can't read invoices."),
                        )
                        .child(
                            ui::button_with(
                                "cf-billing-open",
                                Some("link-square02"),
                                "Billing",
                                ButtonKind::Outline,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.open("billing", cx))),
                        ),
                );
            }
        }
        card
    }

    fn certs_card(&self, s: &Snapshot, now: u64) -> Div {
        let mut certs: Vec<(&str, &Cert)> = s
            .zones
            .iter()
            .flat_map(|z| {
                z.certs
                    .iter()
                    .filter(|c| c.expires > 0)
                    .map(move |c| (z.name.as_str(), c))
            })
            .collect();
        certs.sort_by_key(|(_, c)| c.expires);
        let mut card = panel("Certificates");
        if certs.is_empty() {
            return card.child(quiet("No edge certificates read."));
        }
        for (zone, c) in certs.iter().take(4) {
            let days = c.days_left(now);
            let tone = if days < 14 {
                Tone::Danger
            } else if days < 30 {
                Tone::Warning
            } else {
                Tone::Success
            };
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon("square-lock02", 13.).text_color(tone.color()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_muted())
                            .child(c.hosts.first().cloned().unwrap_or_else(|| zone.to_string())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .ty(Type::Caption)
                            .text_color(tone.color())
                            .child(if days < 0 {
                                "expired".to_string()
                            } else {
                                format!("{days} days")
                            }),
                    ),
            );
        }
        card
    }

    fn domains_strip(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let now = crate::model::unix_now();
        let mut cards: Vec<AnyElement> = Vec::new();
        for z in &s.zones {
            let cert = z
                .certs
                .iter()
                .filter(|c| c.expires > 0)
                .map(|c| c.days_left(now))
                .min();
            let name = z.name.clone();
            cards.push(
                div()
                    .id(SharedString::from(format!("cf-dom-{name}")))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .p(px(14.))
                    .rounded(px(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .cursor_pointer()
                    .hover(|d| d.border_color(theme::border_strong()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(logos::tile(logos::mark(&z.name, ""), 28.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .truncate()
                                            .ty(Type::Body)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme::text())
                                            .child(z.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .ty(Type::Caption)
                                            .text_color(theme::text_subtle())
                                            .child(format!(
                                                "{} · {} req/24h",
                                                z.plan,
                                                count(z.served())
                                            )),
                                    ),
                            )
                            .child(spark(&z.days)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.))
                            .when(z.live_server_errors() > 0, |d| {
                                d.child(ui::badge(
                                    format!("{} × 5xx", count(z.live_server_errors())),
                                    Tone::Danger,
                                ))
                            })
                            .when_some(cert, |d, days| {
                                d.child(ui::badge(
                                    format!("SSL {days}d"),
                                    if days < 14 {
                                        Tone::Danger
                                    } else if days < 30 {
                                        Tone::Warning
                                    } else {
                                        Tone::Success
                                    },
                                ))
                            })
                            .when(z.email_enabled == Some(true), |d| {
                                d.child(ui::badge("Email", Tone::Info))
                            })
                            .when(z.settings.read, |d| {
                                d.child(ui::badge(
                                    format!("SSL {}", z.settings.ssl_label()),
                                    if z.settings.weak_ssl() {
                                        Tone::Warning
                                    } else {
                                        Tone::Neutral
                                    },
                                ))
                            }),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Domains;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .ty(Type::Heading)
                    .text_color(theme::text())
                    .child("Domains"),
            )
            .child(grid(cards, 3))
    }

    // -----------------------------------------------------------------------
    // Security, Containers, Email.

    pub(super) fn render_security(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let now = crate::model::unix_now();
        let fixable: HashSet<String> = s
            .insights
            .iter()
            .filter(|i| !fix::settled(i, s))
            .filter_map(|i| fix::for_insight(i, s))
            .map(|f| f.key())
            .collect();
        let mut out = div().flex().flex_col().gap(px(20.)).child(page_head(
            "Security Center",
            &format!(
                "{} findings Cloudflare's Security Center has for this account · {} changes here fix {} of them; it rescans within a day",
                s.insights_total,
                fixable.len(),
                s.insights.iter().filter(|i| !fix::settled(i, s) && fix::for_insight(i, s).is_some()).count()
            ),
        ));
        if s.insights.is_empty() {
            return out.child(ui::empty_state(
                "shield-alert",
                "No findings",
                "Security Center has nothing open for this account.",
                None,
            ));
        }
        for sev in ["Critical", "High", "Moderate", "Low"] {
            let list: Vec<&Insight> = s.insights.iter().filter(|i| i.severity == sev).collect();
            if list.is_empty() {
                continue;
            }
            let tone = severity_tone(sev);
            let mut rows = div()
                .flex()
                .flex_col()
                .rounded(px(ui::RADIUS_LG))
                .border_1()
                .border_color(theme::border())
                .overflow_hidden();
            for (n, i) in list.iter().enumerate() {
                let zone_name = s
                    .zones
                    .iter()
                    .find(|z| z.id == i.zone_id)
                    .map_or("", |z| z.name.as_str());
                let link = i
                    .link
                    .replace(":account_id", &s.account_id)
                    .replace(":zone_name", zone_name);
                let link = link.trim_start_matches('/').to_string();
                let settled = fix::settled(i, s);
                let fix = fix::for_insight(i, s).filter(|_| !settled);
                let id = format!("cf-ins-fix-{sev}-{n}");
                let confirm = fix
                    .as_ref()
                    .and_then(|f| self.fix_confirmation(f, id.clone(), cx));
                let button = if settled {
                    Some(ui::badge("Done · waiting for rescan", Tone::Success).into_any_element())
                } else {
                    fix.as_ref().map(|f| self.fix_button(f, id.clone(), cx))
                };
                rows = rows.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(14.))
                        .py(px(10.))
                        .when(n > 0, |d| {
                            d.border_t_1().border_color(theme::border_subtle())
                        })
                        .bg(theme::fill_raised())
                        .child(
                            div()
                                .flex_none()
                                .size(px(8.))
                                .rounded_full()
                                .bg(tone.color()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .ty(Type::Body)
                                        .text_color(theme::text())
                                        .child(i.title()),
                                )
                                .child(
                                    div()
                                        .ty(Type::Caption)
                                        .text_color(theme::text_subtle())
                                        .child(format!(
                                            "{} · since {}",
                                            i.subject,
                                            if i.since > 0 {
                                                cloudflare::ago(i.since, now)
                                            } else {
                                                "—".into()
                                            }
                                        )),
                                ),
                        )
                        .children(button)
                        .when(!link.is_empty() && fix.is_none() && !settled, |d| {
                            d.child(
                                ui::button_with(
                                    SharedString::from(format!("cf-ins-{sev}-{n}")),
                                    Some("link-square02"),
                                    "Fix in dashboard",
                                    ButtonKind::Ghost,
                                    true,
                                )
                                .on_click(cx.listener(
                                    move |_, _, _, cx| {
                                        cx.open_url(&format!("{}/{link}", cloudflare::DASH))
                                    },
                                )),
                            )
                        }),
                );
                if let Some(c) = confirm {
                    rows = rows.child(
                        div()
                            .px(px(14.))
                            .pb(px(12.))
                            .bg(theme::fill_raised())
                            .child(c),
                    );
                }
            }
            out = out.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::badge(sev, tone))
                            .child(
                                div()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(list.len().to_string()),
                            ),
                    )
                    .child(rows),
            );
        }
        out
    }

    pub(super) fn render_containers(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let now = crate::model::unix_now();
        let head = page_head(
            "Containers",
            "Applications on Cloudflare Containers and the health of their instances",
        );
        if s.containers.is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap(px(20.))
                .child(head)
                .child(ui::empty_state(
                    "cube",
                    "No containers",
                    "This account has no Containers applications.",
                    None,
                ));
        }
        let cards: Vec<AnyElement> = s
            .containers
            .iter()
            .map(|c| {
                let tone = if c.failed > 0 {
                    Tone::Danger
                } else if c.healthy < c.instances {
                    Tone::Warning
                } else {
                    Tone::Success
                };
                let pct = if c.max_instances > 0 {
                    c.instances as f32 * 100. / c.max_instances as f32
                } else {
                    0.
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(16.))
                    .rounded(px(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(icon("cube", 16.).text_color(theme::text_subtle()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text())
                                    .child(c.name.clone()),
                            )
                            .child(ui::status_dot(
                                if c.failed > 0 {
                                    format!("{} failed", c.failed)
                                } else {
                                    format!("{} healthy", c.healthy)
                                },
                                tone,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(format!(
                                        "{} of {} instances",
                                        c.instances, c.max_instances
                                    )),
                            )
                            .child(ui::meter(pct, tone, 4.)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(16.))
                            .child(stat("vCPU", format!("{}", c.vcpu)))
                            .child(stat("Memory", c.memory.clone()))
                            .child(stat("Version", c.version.to_string()))
                            .child(stat("Updated", cloudflare::ago(c.updated, now))),
                    )
                    .into_any_element()
            })
            .collect();
        let open = ui::button_with(
            "cf-containers-open",
            Some("link-square02"),
            "Open in dashboard",
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.open("workers/containers", cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(div().flex().items_end().child(head.flex_1()).child(open))
            .child(grid(cards, 2))
    }

    pub(super) fn render_email(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let mut out = div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(page_head("Email Routing", "Where mail to each domain goes"));
        for z in &s.zones {
            let name = z.name.clone();
            let state = match z.email_enabled {
                Some(true) => ui::badge("On", Tone::Success),
                Some(false) => ui::badge("Off", Tone::Neutral),
                None => ui::badge("Not readable", Tone::Warning),
            };
            let mut rows = div().flex().flex_col().gap(px(6.));
            for r in &z.email_rules {
                let to = match (r.targets.is_empty(), r.action.as_str()) {
                    (true, "drop") => "Dropped".to_string(),
                    (true, other) => other.to_string(),
                    _ => r.targets.join(", "),
                };
                rows = rows.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .when(!r.enabled, |d| d.opacity(0.5))
                        .child(
                            div()
                                .flex_none()
                                .w(px(240.))
                                .truncate()
                                .ty(Type::Mono)
                                .text_color(theme::text())
                                .child(r.to.clone()),
                        )
                        .child(icon("arrow-right01", 12.).text_color(theme::text_faint()))
                        .when(!r.targets.is_empty(), |d| {
                            d.child(ui::tag(r.action.clone(), Tone::Info.color()))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .ty(Type::Body)
                                .text_color(theme::text_muted())
                                .child(to),
                        ),
                );
            }
            let path = format!("{name}/email/routing/overview");
            out = out.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(16.))
                    .rounded(px(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(logos::tile(logos::mark(&z.name, ""), 24.))
                            .child(
                                div()
                                    .flex_1()
                                    .ty(Type::Body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text())
                                    .child(name.clone()),
                            )
                            .child(state)
                            .child(self.open_button(format!("cf-email-{name}"), path, cx)),
                    )
                    .when(z.email_rules.is_empty(), |d| d.child(quiet("No rules.")))
                    .child(rows),
            );
        }
        out
    }
}

/// A number with its label, a line under it, and a week's sparkline or a tint.
fn kpi(label: &str, value: String, sub: String, tone: Tone, spark: Option<Vec<f64>>) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(6.))
        .p(px(16.))
        .rounded(px(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().size(px(6.)).rounded_full().bg(tone.color()))
                .child(
                    div()
                        .ty(Type::Label)
                        .text_color(theme::text_subtle())
                        .child(label.to_string()),
                ),
        )
        .child(
            div()
                .flex()
                .items_end()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .ty(Type::Title)
                        .text_color(theme::text())
                        .child(value),
                )
                .children(
                    spark
                        .filter(|v| v.iter().any(|x| *x > 0.))
                        .map(|v| ui::sparkline(v, tone.color(), 72., 26.)),
                ),
        )
        .child(
            div()
                .truncate()
                .ty(Type::Caption)
                .text_color(theme::text_subtle())
                .child(sub),
        )
}

fn panel(title: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(16.))
        .rounded(px(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .child(
            div()
                .ty(Type::Heading)
                .text_color(theme::text())
                .child(title.to_string()),
        )
}

fn page_head(title: &str, about: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .ty(Type::Subtitle)
                .text_color(theme::text())
                .child(title.to_string()),
        )
        .child(
            div()
                .ty(Type::Caption)
                .text_color(theme::text_subtle())
                .child(about.to_string()),
        )
}

fn severity_tone(sev: &str) -> Tone {
    match sev {
        "Critical" | "High" => Tone::Danger,
        "Moderate" => Tone::Warning,
        _ => Tone::Info,
    }
}

fn subject_name(s: &Subject) -> String {
    match s {
        Subject::Worker(n)
        | Subject::Zone(n)
        | Subject::Gateway(n)
        | Subject::Workflow(n)
        | Subject::Queue(n)
        | Subject::Tunnel(n)
        | Subject::Dns(n)
        | Subject::Ssl(n)
        | Subject::Registration(n)
        | Subject::Pages(n) => n.clone(),
        Subject::Cloudflared => "cloudflared".into(),
    }
}

/// Cards `per` to a line, each as wide as the others.
fn grid(cards: Vec<AnyElement>, per: usize) -> Div {
    let cell = || div().flex().flex_col().flex_1().min_w_0();
    let mut out = div().flex().flex_col().gap(px(12.));
    let mut cards = cards.into_iter().peekable();
    while cards.peek().is_some() {
        let mut line = div().flex().gap(px(12.));
        let mut n = 0;
        for c in cards.by_ref().take(per) {
            line = line.child(cell().child(c));
            n += 1;
        }
        for _ in n..per {
            line = line.child(cell());
        }
        out = out.child(line);
    }
    out
}

pub(super) fn chart() -> AreaChart {
    AreaChart::new(200.)
}

// ---------------------------------------------------------------------------
// Live logs.

/// Events kept on screen.
const TAIL_KEEP: usize = 300;

impl CloudflareView {
    pub(super) fn start_tail(&mut self, worker: String, cx: &mut Context<Self>) {
        if self.tail.as_ref().is_some_and(|(w, _)| *w == worker) {
            cx.notify();
            return;
        }
        let account = self
            .snap
            .as_ref()
            .map(|s| s.account_id.clone())
            .unwrap_or_default();
        self.tail = None;
        self.tail_events.clear();
        self.tail_error = None;
        match cloudflare::tail::start(&worker, &account) {
            Ok(t) => self.tail = Some((worker, t)),
            Err(e) => self.tail_error = Some(e),
        }
        // Moves what arrived onto the screen a few times a second.
        self._tail_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let alive = this.update(cx, |this, cx| {
                    let Some((_, t)) = &this.tail else {
                        return false;
                    };
                    let mut got = false;
                    while let Ok(e) = t.events.try_recv() {
                        this.tail_events.push_front(e);
                        got = true;
                    }
                    this.tail_events.truncate(TAIL_KEEP);
                    if got {
                        cx.notify();
                    }
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn stop_tail(&mut self, cx: &mut Context<Self>) {
        self.tail = None;
        self._tail_task = None;
        cx.notify();
    }

    pub(super) fn render_logs(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let mut workers: Vec<&Worker> = s.workers.iter().collect();
        workers.sort_by(|a, b| {
            b.requests
                .cmp(&a.requests)
                .then_with(|| a.name.cmp(&b.name))
        });
        let current = self.tail.as_ref().map(|(w, _)| w.clone());
        let mut chips = div().flex().flex_wrap().gap(px(6.));
        for (i, w) in workers.iter().enumerate() {
            let name = w.name.clone();
            let on = current.as_ref() == Some(&w.name);
            chips = chips.child(
                ui::chip(
                    ("cf-tail-pick", i),
                    format!("{} · {}", w.name, count(w.requests)),
                    on,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.start_tail(name.clone(), cx))),
            );
        }
        let shown: Vec<&cloudflare::tail::TailEvent> = self
            .tail_events
            .iter()
            .filter(|e| !self.tail_errors_only || e.failed())
            .collect();
        let failed = self.tail_events.iter().filter(|e| e.failed()).count();
        let bar = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .p(px(12.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .child(match &current {
                Some(w) => div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().size(px(8.)).rounded_full().bg(Tone::Success.color()))
                    .child(
                        div()
                            .ty(Type::Body)
                            .text_color(theme::text())
                            .child(format!("Listening to {w}")),
                    )
                    .child(
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(format!(
                                "{} events · {failed} failed",
                                self.tail_events.len()
                            )),
                    ),
                None => div()
                    .ty(Type::Body)
                    .text_color(theme::text_subtle())
                    .child("Pick a Worker to see its requests as they happen."),
            })
            .child(div().flex_1())
            .child(
                div()
                    .id("cf-tail-errors")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .child(ui::switch(self.tail_errors_only))
                    .child(
                        div()
                            .ty(Type::Label)
                            .text_color(theme::text_muted())
                            .child("Errors only"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tail_errors_only = !this.tail_errors_only;
                        cx.notify();
                    })),
            )
            .child(
                ui::button(
                    "cf-tail-clear",
                    "Clear",
                    ButtonKind::Ghost,
                    !self.tail_events.is_empty(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.tail_events.clear();
                    cx.notify();
                })),
            )
            .when(current.is_some(), |d| {
                d.child(
                    ui::button_with(
                        "cf-tail-stop",
                        Some("pause"),
                        "Stop",
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.stop_tail(cx))),
                )
            });
        let mut list = div()
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .overflow_hidden();
        if shown.is_empty() {
            list = list.child(div().p(px(16.)).ty(Type::Body).text_color(theme::text_subtle()).child(match (&current, self.tail_errors_only) {
                (None, _) => "Nothing yet.",
                (Some(_), true) => "No failed requests since listening started.",
                (Some(_), false) => "Waiting for the Worker's next request… (connecting takes a few seconds)",
            }));
        }
        for (n, e) in shown.iter().enumerate() {
            let tone = if e.failed() {
                Tone::Danger
            } else {
                Tone::Success
            };
            let secs = e.when / 1000;
            let clock = format!(
                "{:02}:{:02}:{:02}",
                (secs / 3600 + 24 - local_offset_hours()) % 24,
                secs / 60 % 60,
                secs % 60
            );
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .px(px(14.))
                    .py(px(8.))
                    .when(n > 0, |d| {
                        d.border_t_1().border_color(theme::border_subtle())
                    })
                    .bg(if e.failed() {
                        Tone::Danger.color().opacity(0.06)
                    } else {
                        theme::fill_raised()
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex_none()
                                    .ty(Type::Mono)
                                    .text_color(theme::text_faint())
                                    .child(clock),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(7.))
                                    .rounded_full()
                                    .bg(tone.color()),
                            )
                            .children(e.status.map(|st| {
                                ui::badge(
                                    st.to_string(),
                                    if st >= 500 {
                                        Tone::Danger
                                    } else if st >= 400 {
                                        Tone::Warning
                                    } else {
                                        Tone::Neutral
                                    },
                                )
                            }))
                            .when(e.outcome != "ok", |d| {
                                d.child(ui::badge(e.outcome.clone(), Tone::Danger))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Mono)
                                    .text_color(theme::text())
                                    .child(e.what()),
                            )
                            .when(!e.country.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .ty(Type::Caption)
                                        .text_color(theme::text_subtle())
                                        .child(e.country.clone()),
                                )
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_faint())
                                    .child(format!("{} ms CPU", e.cpu_ms)),
                            ),
                    )
                    .children(e.exceptions.iter().map(|x| {
                        div()
                            .pl(px(76.))
                            .ty(Type::Mono)
                            .text_color(Tone::Danger.color())
                            .child(x.clone())
                    }))
                    .children(e.logs.iter().take(6).map(|(level, msg)| {
                        let color = match level.as_str() {
                            "error" => Tone::Danger.color(),
                            "warn" => Tone::Warning.color(),
                            _ => theme::text_muted(),
                        };
                        div()
                            .pl(px(76.))
                            .truncate()
                            .ty(Type::Mono)
                            .text_color(color)
                            .child(format!("{level}: {msg}"))
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(page_head("Live logs", "Each request, cron and queue batch of a Worker as it runs, with its logs and exceptions (wrangler tail)"))
            .children(self.tail_error.clone().map(|e| note(e, true)))
            .child(chips)
            .child(bar)
            .child(list)
    }
}

/// Hours to subtract from UTC for the local clock (`date +%z`), read once.
fn local_offset_hours() -> u64 {
    static OFFSET: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        let z = std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        // "-0600" → 6 hours behind UTC; "+0200" → 22 (i.e. −2 mod 24).
        let sign = if z.starts_with('-') { 1i64 } else { -1 };
        let hours: i64 = z.get(1..3).and_then(|h| h.parse().ok()).unwrap_or(0);
        (sign * hours).rem_euclid(24) as u64
    })
}

// ---------------------------------------------------------------------------
// A zone's traffic in detail.

impl CloudflareView {
    pub(super) fn toggle_traffic(&mut self, zone_id: String, cx: &mut Context<Self>) {
        if self.traffic_open.remove(&zone_id) {
            cx.notify();
            return;
        }
        self.traffic_open.insert(zone_id.clone());
        if !matches!(self.traffic.get(&zone_id), Some(Ok(_))) {
            self.traffic.remove(&zone_id);
            cx.spawn(async move |this, cx| {
                let id = zone_id.clone();
                let read = cx
                    .background_spawn(async move { cloudflare::zone_traffic(&id) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.traffic.insert(zone_id, read);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(super) fn render_traffic(&self, z: &Zone, _cx: &mut Context<Self>) -> Div {
        let shell = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(16.))
            .mt(px(-4.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::background());
        let t = match self.traffic.get(&z.id) {
            None => {
                return shell.child(ui::loading(
                    SharedString::from(format!("cf-traffic-load-{}", z.id)),
                    "Reading the last 24 hours…",
                ));
            }
            Some(Err(e)) => {
                return shell.child(note(format!("Couldn't read the traffic: {e}"), true));
            }
            Some(Ok(t)) => t,
        };
        let max = t.hours.iter().map(|h| h.1).max().unwrap_or(0).max(1) as f32;
        let total: u64 = t.hours.iter().map(|h| h.1).sum();
        let bars = div()
            .flex()
            .items_end()
            .gap(px(3.))
            .h(px(72.))
            .children(t.hours.iter().map(|(_, n)| {
                div()
                    .flex_1()
                    .h(px(3. + 69. * *n as f32 / max))
                    .rounded(px(2.))
                    .bg(Tone::Info.color().opacity(0.7))
            }));
        let top = |title: &'static str, rows: &[(String, u64)], mono: bool| {
            let max = rows.iter().map(|r| r.1).max().unwrap_or(0).max(1) as f32;
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(6.))
                .child(ui::overline(title))
                .children(rows.iter().take(6).map(|(k, n)| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .ty(if mono { Type::Mono } else { Type::Caption })
                                        .text_color(theme::text_muted())
                                        .child(if k.is_empty() {
                                            "—".to_string()
                                        } else {
                                            k.clone()
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .ty(Type::Caption)
                                        .text_color(theme::text())
                                        .child(count(*n)),
                                ),
                        )
                        .child(
                            div()
                                .h(px(3.))
                                .rounded_full()
                                .bg(theme::fill_selected())
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .w(relative(*n as f32 / max))
                                        .bg(Tone::Info.color().opacity(0.6)),
                                ),
                        )
                }))
        };
        shell
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::overline("Requests by hour · 24h"))
                            .child(
                                div()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(count(total)),
                            ),
                    )
                    .child(bars),
            )
            .child(
                div()
                    .flex()
                    .gap(px(24.))
                    .child(top("Top paths", &t.paths, true))
                    .child(top("Hostnames", &t.hosts, true))
                    .child(top("Countries", &t.countries, false))
                    .child(top("Browsers", &t.browsers, false)),
            )
    }
}
