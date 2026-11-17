//! Dragging sessions: reorder the session list (the card lifts and follows
//! the pointer, the rows slide out of its way, Esc puts it back), or drop a
//! session on the composer to hand its conversation to the session there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    Anchor, Animation, AnimationExt as _, AnyElement, AppContext as _, Context, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, anchored, deferred, div,
    ease_out_quint, point, px,
};

use super::{Shell, agent_icon};
use crate::agent::Entry;
use crate::model::Agent;
use crate::state;
use crate::theme;
use crate::ui::icon;

/// Roughly one session card and the gap under it.
const ROW_PITCH: f32 = 80.;
const SLIDE: Duration = Duration::from_millis(280);
/// How much of another session's conversation "Add to context" sends.
const CONTEXT_BUDGET: usize = 16 * 1024;

#[derive(Clone)]
pub(super) struct DraggedSession {
    pub env: PathBuf,
    pub id: SharedString,
    pub title: SharedString,
    pub agent: Agent,
    /// Where the pointer grabbed the card, so the preview stays under it.
    offset: Point<Pixels>,
}

impl DraggedSession {
    pub fn new(env: PathBuf, id: SharedString, title: SharedString, agent: Agent) -> Self {
        DraggedSession {
            env,
            id,
            title,
            agent,
            offset: Point::default(),
        }
    }
}

impl Render for DraggedSession {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let title = if self.title.is_empty() {
            SharedString::from("Untitled session")
        } else {
            self.title.clone()
        };
        div()
            .relative()
            .left(-self.offset.x)
            .top(-self.offset.y)
            .w(px(244.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(4.))
            .px(crate::ui::sp(10.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(12.))
            .border_1()
            .border_color(theme::active().opacity(0.6))
            .bg(theme::dialog())
            .shadow_lg()
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(agent_icon(self.agent, 14.))
                    .child(
                        div()
                            .text_size(px(crate::ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child(self.agent.label()),
                    ),
            )
            .child(
                div()
                    .text_size(px(crate::ui::TEXT_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(title),
            )
            .with_animation(
                "session-lift",
                Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                |el, t| el.opacity(0.55 + 0.45 * t),
            )
    }
}

/// A tab lifted from the tab bar, to drop on the left or right pane.
#[derive(Clone)]
pub(super) struct DraggedTab {
    pub id: u64,
    pub title: SharedString,
    offset: Point<Pixels>,
}

impl DraggedTab {
    pub fn new(id: u64, title: SharedString) -> Self {
        DraggedTab {
            id,
            title,
            offset: Point::default(),
        }
    }
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .left(-self.offset.x)
            .top(-self.offset.y)
            .w(px(200.))
            .h(px(30.))
            .flex()
            .items_center()
            .px(crate::ui::sp(10.))
            .rounded(crate::ui::rad(8.))
            .border_1()
            .border_color(theme::active().opacity(0.6))
            .bg(theme::dialog())
            .shadow_lg()
            .font_family(theme::FONT_SANS)
            .text_size(px(crate::ui::TEXT_MD))
            .text_color(theme::content())
            .child(div().min_w_0().truncate().child(self.title.clone()))
    }
}

/// The last reorder, for the slide animation.
pub(super) struct SessionSlide {
    pub generation: u64,
    pub moved: SharedString,
    pub from: usize,
    pub to: usize,
}

/// What dropping a session on the composer offers.
pub(super) struct DropMenu {
    pub dragged: DraggedSession,
    pub at: Point<Pixels>,
}

/// Another session's conversation waiting in the composer.
#[derive(Clone)]
pub(super) struct AttachedContext {
    pub title: SharedString,
    pub block: String,
}

impl Shell {
    /// The drag preview; remembers which session is lifted.
    pub(super) fn start_session_drag(
        &mut self,
        d: &DraggedSession,
        offset: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<DraggedSession> {
        self.lifted = Some(d.id.clone());
        self.lifted_tab = None;
        self.drop_menu = None;
        let mut d = d.clone();
        d.offset = offset;
        cx.new(|_| d)
    }

    pub(super) fn start_tab_drag(
        &mut self,
        d: &DraggedTab,
        offset: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<DraggedTab> {
        self.lifted_tab = Some(d.id);
        self.lifted = None;
        let mut d = d.clone();
        d.offset = offset;
        cx.notify();
        cx.new(|_| d)
    }

    /// The tab being dragged, while a drag is on.
    pub(super) fn lifted_tab(&self, cx: &gpui::App) -> Option<u64> {
        self.lifted_tab.filter(|_| cx.has_active_drag())
    }

    /// The session being dragged, while a drag is on.
    pub(super) fn lifted_session(&self, cx: &gpui::App) -> Option<&SharedString> {
        self.lifted.as_ref().filter(|_| cx.has_active_drag())
    }

    /// Esc during a drag: the card goes back where it was.
    pub(super) fn cancel_session_drag(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !cx.has_active_drag() {
            return false;
        }
        cx.stop_active_drag(window);
        if let Some(id) = self.lifted.take() {
            let from = self.env().session_index(&id).unwrap_or(0);
            self.slide(id, from, from, cx);
        }
        true
    }

    pub(super) fn drop_session_on_row(
        &mut self,
        d: &DraggedSession,
        to: usize,
        cx: &mut Context<Self>,
    ) {
        self.lifted = None;
        let Some(env) = self.find_env_mut(&d.env) else {
            return;
        };
        let Some(from) = env.session_index(&d.id) else {
            return;
        };
        let to = to.min(env.sessions.len() - 1);
        if from != to {
            let s = env.sessions.remove(from);
            env.sessions.insert(to, s);
            self.save(cx);
        }
        self.slide(d.id.clone(), from, to, cx);
    }

    fn slide(&mut self, moved: SharedString, from: usize, to: usize, cx: &mut Context<Self>) {
        let generation = self.slide.as_ref().map_or(0, |s| s.generation + 1);
        self.slide = Some(SessionSlide {
            generation,
            moved,
            from,
            to,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SLIDE + Duration::from_millis(60))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .slide
                    .as_ref()
                    .is_some_and(|s| s.generation == generation)
                {
                    this.slide = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Card `ix` (showing session `id`) sliding from where it was before the
    /// last reorder, or as is.
    pub(super) fn sliding_card(
        &self,
        ix: usize,
        id: &SharedString,
        card: gpui::Stateful<gpui::Div>,
    ) -> AnyElement {
        let Some(s) = &self.slide else {
            return card.into_any_element();
        };
        let rows: f32 = if *id == s.moved {
            s.from as f32 - s.to as f32
        } else if s.from < s.to && (s.from..s.to).contains(&ix) {
            1.
        } else if s.from > s.to && (s.to + 1..=s.from).contains(&ix) {
            -1.
        } else {
            0.
        };
        let landing = *id == s.moved;
        if rows == 0. && !landing {
            return card.into_any_element();
        }
        let dy = rows * ROW_PITCH;
        card.relative()
            .with_animation(
                SharedString::from(format!("slide-{}-{id}", s.generation)),
                Animation::new(SLIDE).with_easing(ease_out_quint()),
                move |el, t| {
                    let el = el.top(px(dy * (1. - t)));
                    if landing {
                        el.opacity(0.6 + 0.4 * t)
                    } else {
                        el
                    }
                },
            )
            .into_any_element()
    }

    /// A session dropped on the composer: ask what to do with it.
    pub(super) fn drop_session_on_composer(
        &mut self,
        d: &DraggedSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lifted = None;
        if self.active_session_id().as_ref() == Some(&d.id) {
            self.composer_flash =
                Some("That is this session. Drop it on another session's composer.".into());
            cx.notify();
            return;
        }
        self.drop_menu = Some(DropMenu {
            dragged: d.clone(),
            at: window.mouse_position(),
        });
        cx.notify();
    }

    pub(super) fn session_entries(&self, env: &Path, id: &str, cx: &gpui::App) -> Vec<Entry> {
        let live = self
            .find_env(env)
            .and_then(|e| e.sessions.iter().find(|s| s.id.as_ref() == id))
            .and_then(|s| s.live.as_ref())
            .map(|l| l.thread.read(cx).entries().to_vec());
        live.unwrap_or_else(|| {
            std::fs::read(state::transcript_path(id))
                .ok()
                .and_then(|b| serde_json::from_slice::<crate::agent::Transcript>(&b).ok())
                .map(|t| t.entries)
                .unwrap_or_default()
        })
    }

    fn add_session_context(&mut self, d: &DraggedSession, cx: &mut Context<Self>) {
        let entries = self.session_entries(&d.env, &d.id, cx);
        if entries.is_empty() {
            self.composer_flash = Some("That session has no messages yet.".into());
            return;
        }
        let block =
            crate::agent::notes::context_block(&d.title, d.agent.label(), &entries, CONTEXT_BUDGET);
        let env = self.env().path.clone();
        let list = self.attached.entry(env).or_default();
        list.retain(|c| c.title != d.title);
        list.push(AttachedContext {
            title: d.title.clone(),
            block,
        });
    }

    /// Writes the session out as markdown and mentions the file in the draft,
    /// so the agent can read as much of it as it needs.
    fn link_session(&mut self, d: &DraggedSession, cx: &mut Context<Self>) {
        let entries = self.session_entries(&d.env, &d.id, cx);
        let dir = state::data_dir().join("linked");
        let path = dir.join(format!("{}.md", d.id));
        let text = crate::agent::notes::transcript_markdown(&d.title, d.agent.label(), &entries);
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text)) {
            self.composer_flash = Some(format!("Could not link the session: {e}").into());
            return;
        }
        let title = if d.title.is_empty() {
            "Untitled session"
        } else {
            d.title.as_ref()
        };
        let line = format!(
            "Linked session \"{title}\" ({}), its full transcript: {} ",
            d.agent.label(),
            crate::agent::attach::mention(&path.to_string_lossy())
        );
        self.env_mut().draft.insert_word(&line);
        self.composer_changed(cx);
    }

    pub(super) fn render_session_drop_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.drop_menu.as_ref()?;
        let title = if menu.dragged.title.is_empty() {
            "this session".to_string()
        } else {
            format!("\"{}\"", menu.dragged.title)
        };
        let item =
            |id: &'static str, icon_name: &'static str, label: &'static str, hint: &'static str| {
                div()
                    .id(id)
                    .flex()
                    .items_start()
                    .gap(crate::ui::sp(8.))
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(6.))
                    .rounded(crate::ui::rad(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_selected()))
                    .child(
                        div()
                            .pt(crate::ui::sp(2.))
                            .child(icon(icon_name, 14.).text_color(theme::text_subtle())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(1.))
                            .child(div().text_size(px(crate::ui::TEXT_MD)).child(label))
                            .child(
                                div()
                                    .text_size(px(crate::ui::TEXT_XS))
                                    .text_color(theme::text_subtle())
                                    .child(hint),
                            ),
                    )
            };
        Some(
            deferred(
                anchored()
                    .position(point(menu.at.x, menu.at.y - px(8.)))
                    .anchor(Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("session-drop-menu")
                            .occlude()
                            .w(px(280.))
                            .flex()
                            .flex_col()
                            .p(crate::ui::sp(4.))
                            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::dialog())
                            .shadow_lg()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.drop_menu = None;
                                cx.notify();
                            }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                div()
                                    .px(crate::ui::sp(8.))
                                    .pt(crate::ui::sp(4.))
                                    .pb(crate::ui::sp(6.))
                                    .text_size(px(crate::ui::TEXT_XS))
                                    .text_color(theme::text_subtle())
                                    .truncate()
                                    .child(format!("Use {title} here")),
                            )
                            .child(
                                item(
                                    "drop-context",
                                    "attachment01",
                                    "Add to context",
                                    "Its recent conversation goes along with your next message.",
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if let Some(m) = this.drop_menu.take() {
                                            this.add_session_context(&m.dragged, cx);
                                        }
                                        window.focus(&this.composer_focus, cx);
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                item(
                                    "drop-link",
                                    "link01",
                                    "Link sessions",
                                    "Mentions its full transcript so the agent can read it.",
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if let Some(m) = this.drop_menu.take() {
                                            this.link_session(&m.dragged, cx);
                                        }
                                        window.focus(&this.composer_focus, cx);
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    pub(super) fn render_context_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let list = self
            .attached
            .get(&self.env().path)
            .filter(|v| !v.is_empty())?;
        Some(
            div()
                .flex()
                .flex_wrap()
                .gap(crate::ui::sp(6.))
                .px(crate::ui::sp(12.))
                .pt(crate::ui::sp(10.))
                .children(list.iter().enumerate().map(|(i, c)| {
                    div()
                        .id(("context-chip", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(26.))
                        .max_w(px(320.))
                        .pl(crate::ui::sp(8.))
                        .pr(crate::ui::sp(4.))
                        .rounded(crate::ui::rad(8.))
                        .border_1()
                        .border_color(theme::active().opacity(0.3))
                        .bg(theme::active().opacity(0.08))
                        .text_size(px(crate::ui::TEXT_SM))
                        .child(icon("chatting01", 12.).text_color(theme::active()))
                        .child(div().text_color(theme::text_subtle()).child("Context"))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme::text_muted())
                                .child(c.title.clone()),
                        )
                        .child(
                            div()
                                .text_color(theme::text_subtle())
                                .child(format!("{:.1}k", c.block.len() as f32 / 1000.)),
                        )
                        .child(
                            div()
                                .id(("context-remove", i))
                                .flex_none()
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(crate::ui::rad(4.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::fill_selected()))
                                .child(icon("cancel01", 11.).text_color(theme::text_subtle()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let env = this.env().path.clone();
                                    if let Some(v) = this.attached.get_mut(&env) {
                                        if i < v.len() {
                                            v.remove(i);
                                        }
                                        if v.is_empty() {
                                            this.attached.remove(&env);
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                }))
                .into_any_element(),
        )
    }
}
