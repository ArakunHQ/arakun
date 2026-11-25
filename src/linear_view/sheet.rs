//! The sheet over the Linear tab: one issue in full, and what to do with it.

use gpui::relative;

use super::list::group_icon;
use super::*;
use crate::agent::markdown::{self, MdStyle};
use crate::linear::issues::Issue;
use crate::linear::prompt;

/// The sheet's width, as wide as a comment reads well.
const SHEET_W: f32 = 640.;

/// Issue text in the sheet: the GitHub sheet's body style.
fn body_style() -> MdStyle {
    MdStyle {
        size: ui::TEXT_LG,
        line_height: 22.,
        color: theme::text_muted(),
        strong: theme::text(),
        heading: theme::text(),
        gap: 8.,
    }
}

fn render_markdown(id: impl Into<gpui::ElementId>, blocks: &[Block]) -> AnyElement {
    markdown::render(
        id,
        blocks,
        body_style(),
        &crate::agent::TextMap::default(),
        None,
        &[],
        None,
        None,
        None,
        None,
    )
}

impl LinearView {
    /// Opens the sheet on issue `id` and reads it in full.
    pub(super) fn open_issue(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.picked = Some(id.clone());
        self.menu = None;
        if self.open.as_deref() != Some(id.as_str()) {
            self.open = Some(id);
            self.read_detail(cx);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn close_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = None;
        self.detail.reset();
        self.detail_md.clear();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn read_detail(&mut self, cx: &mut Context<Self>) {
        // Another issue's read must not land in this sheet.
        self.detail.reset();
        self.detail_md.clear();
        let (Some(id), Some(s)) = (self.open.clone(), self.session().cloned()) else {
            return;
        };
        let until = self.until(&s.id);
        if crate::model::unix_now() < until {
            self.detail.error = Some(ApiError::Limited { until });
            return;
        }
        let session = s.id.clone();
        let issue = id.clone();
        self.detail.start(
            cx,
            |this| &mut this.detail,
            async move { linear::issues::detail(&s, &issue) },
            move |this, got, _| {
                if this.open.as_deref() != Some(id.as_str())
                    || this.current_id().as_deref() != Some(session.as_str())
                {
                    return;
                }
                this.note_limit(&session, &got);
                if let Ok(d) = &got {
                    this.detail_md = std::iter::once(d.description.as_str())
                        .chain(d.comments.iter().map(|c| c.body.as_str()))
                        .map(markdown::parse)
                        .collect();
                }
                this.detail.settle(got);
            },
        );
    }

    /// The detail read for the open issue, once it has arrived. Opening
    /// another issue drops it, and a read for another one is ignored.
    fn open_detail(&self) -> Option<&Detail> {
        self.detail.data.as_ref().filter(|_| self.open.is_some())
    }

    /// The open issue: from its detail, else from the list while it reads.
    fn open_summary(&self) -> Option<&Issue> {
        let id = self.open.as_deref()?;
        self.open_detail().map(|d| &d.issue).or_else(|| {
            self.assigned
                .data
                .as_ref()
                .and_then(|a| a.issues.iter().find(|i| i.id == id))
        })
    }

    fn render_detail(&self) -> AnyElement {
        let Some(d) = self.open_detail() else {
            return match &self.detail.error {
                Some(e) => ui::text(
                    Type::Body,
                    theme::error().into(),
                    crate::redact::text(&error_line(e, crate::model::unix_now())),
                )
                .into_any_element(),
                None => ui::loading("Reading the issue").into_any_element(),
            };
        };
        let now = crate::model::unix_now();
        let description = match self.detail_md.first() {
            Some(blocks) if !d.description.trim().is_empty() => {
                render_markdown("lin-sheet-description", blocks)
            }
            _ => ui::text(Type::Body, theme::text_subtle(), "No description.").into_any_element(),
        };
        let comments = d.comments.iter().enumerate().map(|(i, c)| {
            let body = self
                .detail_md
                .get(i + 1)
                .map(|blocks| render_markdown(("lin-sheet-comment", i), blocks));
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .pt(crate::ui::sp(12.))
                .border_t_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .child(ui::text(Type::Label, theme::text(), c.author.clone()))
                        .child(ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            crate::memory::ago(c.at, now),
                        )),
                )
                .children(body)
        });
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(16.))
            .child(description)
            .when(!d.comments.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(12.))
                        .child(ui::overline("COMMENTS"))
                        .children(comments),
                )
            })
            .into_any_element()
    }

    /// The dimmed tab and the sheet over it, while an issue is open.
    pub(super) fn render_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let issue = self.open_summary()?.clone();
        let ready = self.open_detail().is_some();
        let work = ui::button_with(
            "lin-work",
            Some("sparkles"),
            "Work on it with agent",
            ButtonKind::Primary,
            ready,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(d) = this.open_detail() {
                cx.emit(LinearEvent::Ask {
                    title: prompt::title(d),
                    prompt: prompt::work(d),
                });
            }
        }));
        let url = issue.url.clone();
        let open = ui::button_with(
            "lin-open",
            Some("link-square02"),
            "Open in Linear",
            ButtonKind::Outline,
            !url.is_empty(),
        )
        .on_click(cx.listener(move |_, _, _, cx| {
            if !url.is_empty() {
                cx.emit(LinearEvent::Open(url.clone()));
            }
        }));
        let mut props = div()
            .flex()
            .flex_col()
            .child(ui::kv("State", issue.state.clone()))
            .child(ui::kv("Team", issue.team.clone()));
        if let Some(p) = &issue.project {
            props = props.child(ui::kv("Project", p.clone()));
        }
        let bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(20.))
            .h(px(ui::TOOLBAR_H))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(icon(group_icon(issue.group), 14.).text_color(theme::text_muted()))
            .child(ui::text(
                Type::Mono,
                theme::text_subtle(),
                issue.identifier.clone(),
            ))
            .child(div().flex_1())
            .child(
                ui::icon_button("lin-sheet-close", "cancel01", 12., "Close (Esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_sheet(window, cx))),
            );
        let content = div()
            .id("lin-sheet-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(16.))
                    .p(crate::ui::sp(20.))
                    .child(ui::text(Type::Subtitle, theme::text(), issue.title.clone()))
                    .child(props)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(crate::ui::sp(8.))
                            .child(work)
                            .child(open),
                    )
                    .child(self.render_detail()),
            );
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(
                    // A click on the dimmed list closes the sheet.
                    div()
                        .id("lin-sheet-dim")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .bg(gpui::black().opacity(0.45))
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| this.close_sheet(window, cx)),
                        )
                        .map(|d| {
                            crate::ui::motion::fade_in(
                                d,
                                "lin-sheet-dim-in",
                                crate::ui::motion::BASE,
                            )
                        }),
                )
                .child(
                    ui::labelled(
                        div().id("lin-sheet"),
                        Role::Dialog,
                        issue.identifier.clone(),
                    )
                    .absolute()
                    .top(crate::ui::sp(12.))
                    .bottom(crate::ui::sp(12.))
                    .right(crate::ui::sp(12.))
                    .w(px(SHEET_W))
                    .max_w(relative(0.92))
                    .flex()
                    .flex_col()
                    .rounded(crate::ui::rad(ui::RADIUS_XL))
                    .bg(theme::sheet())
                    .shadow_lg()
                    .overflow_hidden()
                    .occlude()
                    .child(bar)
                    .child(content)
                    .map(|d| {
                        crate::ui::motion::enter_from_right(
                            d,
                            "lin-sheet-in",
                            24.,
                            crate::ui::motion::BASE,
                        )
                    }),
                )
                .into_any_element(),
        )
    }
}
