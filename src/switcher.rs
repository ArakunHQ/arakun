//! Ctrl+K: jump to any project, environment or session, or run a command.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, icon};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Project(usize),
    Env(usize, usize),
    Session(usize, usize, SharedString),
    NewSession,
    NewEnvironment,
    NewTerminal,
    NewBrowserTab,
    AddProject,
    ToggleDock,
    ToggleSidebar,
    TogglePanel,
    ToggleSubagents,
    Account,
    Settings,
    Extensions,
    #[cfg(feature = "coderlabs")]
    Tasks,
    Runs,
    Branches,
    #[cfg(feature = "coderlabs")]
    Canvas,
    /// A branch of the visible project, shown in the Branches tab.
    Branch(String),
    /// A CoderSpace task, by id.
    #[cfg(feature = "coderlabs")]
    Task(String),
    SignIn(crate::agent::AgentKind),
}

#[derive(Clone)]
pub struct Item {
    pub icon: &'static str,
    pub label: SharedString,
    pub detail: SharedString,
    pub target: Target,
}

pub enum SwitcherEvent {
    Picked(Target),
    Dismissed,
}

pub struct Switcher {
    input: Entity<TextInput>,
    items: Vec<Item>,
    shown: Vec<usize>,
    selected: usize,
    scroll: ScrollHandle,
    _sub: Subscription,
}

impl EventEmitter<SwitcherEvent> for Switcher {}

impl Switcher {
    pub fn new(items: Vec<Item>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Jump to a session, environment or command…", cx));
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev, _, cx| match ev {
            InputEvent::Changed => {
                this.filter(cx);
                cx.notify();
            }
            InputEvent::Up => this.step(-1, cx),
            InputEvent::Down => this.step(1, cx),
            InputEvent::Submit => this.pick(this.selected, cx),
            InputEvent::Cancel => cx.emit(SwitcherEvent::Dismissed),
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        let shown = (0..items.len()).collect();
        Self {
            input,
            items,
            shown,
            selected: 0,
            scroll: ScrollHandle::new(),
            _sub: sub,
        }
    }

    fn filter(&mut self, cx: &App) {
        let query = self.input.read(cx).text().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut scored: Vec<(usize, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let hay = format!("{} {}", item.label, item.detail).to_lowercase();
                words
                    .iter()
                    .all(|w| hay.contains(w))
                    // Matches at the start of the label first.
                    .then(|| {
                        (
                            usize::from(
                                !item
                                    .label
                                    .to_lowercase()
                                    .starts_with(words.first().copied().unwrap_or("")),
                            ),
                            i,
                        )
                    })
            })
            .collect();
        scored.sort();
        self.shown = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0);
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.shown.is_empty() {
            return;
        }
        let n = self.shown.len() as isize;
        self.selected = ((self.selected as isize + delta).rem_euclid(n)) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn pick(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some(&i) = self.shown.get(row) {
            cx.emit(SwitcherEvent::Picked(self.items[i].target.clone()));
        }
    }
}

impl Focusable for Switcher {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Switcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.shown.iter().enumerate().map(|(row, &i)| {
            let item = &self.items[i];
            let selected = row == self.selected;
            div()
                .id(("switch-row", row))
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(ui::MENU_ROW_H))
                .flex_none()
                .px(px(8.))
                .rounded(px(ui::RADIUS))
                .cursor_pointer()
                .when(selected, |d| d.bg(theme::selection()))
                .hover(|s| s.bg(theme::selection_hover()))
                .child(icon(item.icon, 14.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(ui::TEXT_MD))
                        .child(item.label.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .max_w(px(220.))
                        .truncate()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(item.detail.clone()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.pick(row, cx)))
        });
        ui::modal_panel("switcher", 560.)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(8.))
                    .h(px(40.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .text_size(px(ui::TEXT_LG))
                    .child(icon("search01", 16.).text_color(theme::text_subtle()))
                    .child(div().flex_1().min_w_0().child(self.input.clone())),
            )
            .child(
                div()
                    .id("switcher-list")
                    .flex()
                    .flex_col()
                    .max_h(px(4. + 12. * (ui::MENU_ROW_H + 1.)))
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p(px(4.))
                    .gap(px(1.))
                    .children(rows)
                    .when(self.shown.is_empty(), |d| {
                        d.child(
                            div()
                                .py(px(16.))
                                .text_center()
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::text_faint())
                                .child("Nothing matches"),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .text_size(px(ui::TEXT_XS))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_faint())
                    .child("↑↓ move")
                    .child("Enter open")
                    .child("Esc close"),
            )
    }
}
