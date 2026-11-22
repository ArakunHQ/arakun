//! The PostHog tab's accounts: the saved sign-ins side by side, and adding
//! one with a personal API key.

use super::*;

/// What a personal API key needs to show the tab.
const KEY_SCOPES: &str = "user:read, organization:read, project:read, query:read, feature_flag:read, experiment:read, error_tracking:read, person:read, event_definition:read, property_definition:read, insight:read";
/// What it also needs to change flags and experiments from the tab.
const WRITE_SCOPES: &str = "feature_flag:write and experiment:write";

impl PostHogView {
    /// After a sign-in was saved: the new session's projects, and back to the project.
    fn signed_in(&mut self, s: &Session, cx: &mut Context<Self>) {
        self.sessions = posthog::sessions();
        self.connect_note = Some((
            format!("Signed in as {} on {}.", s.email, s.region_label()),
            false,
        ));
        self.key_input.update(cx, |i, cx| i.set_text("", cx));
        self.page = Page::Main;
        self.current = None;
        self.close_detail();
        self.overview = None;
        self.directory = None;
        self.sync_tabs(cx);
        self.directory_force(cx);
    }

    fn directory_force(&mut self, cx: &mut Context<Self>) {
        self.dir_loading = false;
        self._dir = None;
        self.refresh_directory(cx);
    }

    pub(super) fn connect_key(&mut self, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).text().trim().to_string();
        if key.is_empty() || self.connecting {
            return;
        }
        self.connecting = true;
        self.connect_note = None;
        cx.notify();
        let region = self.region;
        self._connect = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    let s = posthog::sign_in_with_key(&key, region).map_err(|e| e.message())?;
                    posthog::save_session(s.clone())?;
                    Ok::<Session, String>(s)
                }))
                .await;
            this.update(cx, |this, cx| {
                this.connecting = false;
                match got {
                    Ok(s) => this.signed_in(&s, cx),
                    Err(e) => this.connect_note = Some((e, true)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn remove(&mut self, id: String, cx: &mut Context<Self>) {
        if let Err(e) = posthog::remove_session(&id) {
            self.connect_note = Some((e, true));
            cx.notify();
            return;
        }
        self.sessions = posthog::sessions();
        // A read still running used the removed account: start it again
        // without it, or the picker waits on a read that was dropped.
        let reading = self.dir_loading;
        self._dir = None;
        self.dir_loading = false;
        if let Some(d) = &mut self.directory {
            d.projects.retain(|p| p.session != id);
            d.problems.clear();
        }
        if self.current.as_ref().is_some_and(|p| p.session == id) {
            self.current = None;
            self.overview = None;
            self.resolve(cx);
            self.sync_tabs(cx);
        }
        if reading {
            self.refresh_directory(cx);
        }
        cx.notify();
    }

    /// The form that adds an account. `first`: nothing is saved yet.
    pub(super) fn render_connect(&self, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.connecting;
        let regions: Vec<(Option<Region>, &'static str)> = vec![
            (None, "Auto"),
            (Some(Region::Us), "US"),
            (Some(Region::Eu), "EU"),
        ];
        let region = ui::toggle_group("ph-region", &regions, self.region, |seg, r| {
            seg.on_click(cx.listener(move |this, _, _, cx| {
                this.region = r;
                cx.notify();
            }))
        });
        let field = ui::field()
            .flex_1()
            .min_w_0()
            .child(icon("square-lock02", 14.).text_color(theme::text_subtle()))
            .child(self.key_input.clone());
        let mut card = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(14.))
            .w_full()
            .max_w(px(560.))
            .p(crate::ui::sp(20.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised());
        if first {
            card = card.child(ui::heading("Sign in to PostHog")).child(ui::text(
                Type::Body,
                theme::text_muted(),
                "Add every account you use; they stay signed in side by side. The tab shows the project that belongs to the Arakun project you have open.",
            ));
        } else {
            card = card.child(ui::heading("Add an account"));
        }
        card.child(
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .child(ui::text(Type::Label, theme::text_subtle(), "Personal API key"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .child(field)
                        .child(region)
                        .child(
                            ui::button("ph-key-save", if busy { "Checking…" } else { "Add" }, ButtonKind::Primary, !busy)
                                .when(!busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.connect_key(cx)))),
                        ),
                )
                .child(ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    format!("PostHog › Settings › Personal API keys. Read scopes: {KEY_SCOPES}. Changing flags and experiments also needs {WRITE_SCOPES}. Saved on this computer only."),
                )),
        )
        .when_some(self.connect_note.clone(), |d, (note, error)| {
            let note = crate::redact::text(&note).into_owned();
            d.child(ui::text(Type::Body, if error { theme::error().into() } else { theme::text_muted() }, note))
        })
        .into_any_element()
    }

    pub(super) fn render_accounts(&self, cx: &mut Context<Self>) -> AnyElement {
        let dir = self.directory.as_ref();
        let mut list = div().flex().flex_col().gap(crate::ui::sp(8.)).max_w(px(720.)).child(ui::heading("Accounts")).child(ui::text(
            Type::Body,
            theme::text_muted(),
            "Every sign-in stays saved. Each one sees all the organizations its user belongs to.",
        ));
        for (i, s) in self.sessions.iter().enumerate() {
            let projects: Vec<&Project> = dir
                .map(|d| d.projects.iter().filter(|p| p.session == s.id).collect())
                .unwrap_or_default();
            let mut orgs: Vec<String> = projects
                .iter()
                .map(|p| crate::redact::value(&p.org))
                .collect();
            orgs.dedup();
            let summary = if projects.is_empty() {
                "no projects read yet".to_string()
            } else {
                format!(
                    "{} · {} project{}",
                    orgs.join(", "),
                    projects.len(),
                    if projects.len() == 1 { "" } else { "s" }
                )
            };
            let id = s.id.clone();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .px(crate::ui::sp(12.))
                    .py(crate::ui::sp(10.))
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .child(icon("user", 16.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(8.))
                                    .child(ui::text(
                                        Type::Body,
                                        theme::text(),
                                        crate::redact::value(&s.email),
                                    ))
                                    .child(ui::badge(s.region_label(), Tone::Neutral)),
                            )
                            .child(
                                ui::text(Type::Caption, theme::text_faint(), summary).truncate(),
                            ),
                    )
                    .child(
                        ui::button(("ph-remove", i), "Remove", ButtonKind::Ghost, true).on_click(
                            cx.listener(move |this, _, _, cx| this.remove(id.clone(), cx)),
                        ),
                    ),
            );
        }
        for p in dir.map(|d| d.problems.clone()).unwrap_or_default() {
            list = list.child(notice("alert-circle", Tone::Warning, p));
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(28.))
            .child(list)
            .child(self.render_connect(false, cx))
            .into_any_element()
    }
}
