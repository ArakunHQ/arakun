//! The sidebar folded to icons (`Collapse::Icons`): one tile per project,
//! with a dot for its worst state, and beside the tile under the pointer a
//! flyout with that project's sessions and New session.
//!
//! The flyout is not a `ContextMenu`: a menu covers the window with a
//! backdrop that takes the pointer, and this one follows the pointer, from
//! tile to tile and onto itself, and closes once the pointer leaves both.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::Bounds;

use super::*;

/// How wide the flyout is.
const FLYOUT_W: f32 = 280.;

/// The tallest the flyout's list of sessions grows before it scrolls.
const FLYOUT_ROWS_MAX_H: f32 = 360.;

/// How long the flyout waits after the pointer left its tile or itself, so
/// the pointer can cross the gap between the two.
const FLYOUT_LINGER: Duration = Duration::from_millis(200);

/// Height of a project tile.
const TILE_H: f32 = 36.;

/// Which project's flyout shows, and where the pointer is.
#[derive(Default)]
pub(in crate::shell) struct Flyout {
    /// The project whose sessions show.
    open: Option<usize>,
    /// The tile under the pointer.
    over_tile: Option<usize>,
    over_card: bool,
    /// Where each project's tile was drawn last, to put the flyout beside it.
    tiles: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
}

impl Flyout {
    /// The pointer came onto tile `pi` (`over`) or left it. Coming, it opens
    /// that project's flyout. True when the flyout changed.
    fn tile(&mut self, pi: usize, over: bool) -> bool {
        if over {
            self.over_tile = Some(pi);
            return self.open.replace(pi) != Some(pi);
        }
        if self.over_tile == Some(pi) {
            self.over_tile = None;
        }
        false
    }

    fn card(&mut self, over: bool) {
        self.over_card = over;
    }

    /// Closes the flyout when the pointer is on neither a tile nor the
    /// flyout. True when it closed.
    fn settle(&mut self) -> bool {
        if self.over_tile.is_none() && !self.over_card {
            return self.open.take().is_some();
        }
        false
    }

    /// Closes the flyout now: a session was opened, or Esc. The flyout goes
    /// away under the pointer without saying it left, so it is let go here.
    pub(in crate::shell) fn close(&mut self) -> bool {
        self.over_card = false;
        self.open.take().is_some()
    }
}

/// A project's worst state, for its tile's dot: what needs the user, then
/// what failed, then what works. Nothing for a quiet project.
fn tile_state(states: impl IntoIterator<Item = RowState>) -> Option<RowState> {
    states
        .into_iter()
        .filter(|s| s.wants() || *s == RowState::Working)
        .min_by_key(|s| s.rank())
}

impl Shell {
    /// After the pointer left a tile or the flyout, closes the flyout unless
    /// it came back by then.
    fn settle_flyout_soon(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLYOUT_LINGER).await;
            let _ = this.update(cx, |this, cx| {
                if this.rail.flyout.settle() {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The folded sidebar's projects: a tile each, the open one marked, and
    /// Add project after them.
    pub(in crate::shell) fn render_project_tiles(&self, cx: &mut Context<Self>) -> AnyElement {
        let ground = self.rail_ground();
        let tiles = self.rail.flyout.tiles.clone();
        let mut list = div().flex().flex_col().gap(crate::ui::sp(2.));
        for (pi, project) in self.projects.iter().enumerate() {
            let selected = pi == self.active_project;
            let flyout = self.rail.flyout.open == Some(pi);
            let state = tile_state(self.tree_rows(pi, cx).iter().map(|r| r.state));
            let label = match state.and_then(|s| s.word()) {
                Some(word) => format!("{}, {word}", project.name),
                None => project.name.to_string(),
            };
            let tiles = tiles.clone();
            list = list.child(
                div()
                    .id(("rail-project-icon", pi))
                    .map(|d| ui::labelled(d, gpui::Role::Button, label))
                    .aria_selected(selected)
                    .aria_expanded(flyout)
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(TILE_H))
                    .rounded(crate::ui::rad(8.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(theme::selection_strong()))
                    .when(!selected && flyout, |d| d.bg(theme::hover()))
                    .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
                    // The open project's marker, on the strip's edge.
                    .when(selected, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(px(-8.))
                                .top(px(TILE_H / 2. - 8.))
                                .w(px(3.))
                                .h(px(16.))
                                .rounded_r(crate::ui::rad(2.))
                                .bg(theme::content()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .when(!selected && !flyout, |d| d.opacity(0.65))
                            .child(ui::project_icon(
                                project.icon,
                                1.5,
                                gpui::rgb(project.logo).into(),
                            )),
                    )
                    .children(state.map(|s| {
                        div()
                            .absolute()
                            .bottom(px(3.))
                            .right(px(6.))
                            .size(px(10.))
                            .rounded_full()
                            .border_2()
                            .border_color(ground)
                            .bg(s.color())
                    }))
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| {
                                tiles.borrow_mut().insert(pi, bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(super::super::hover::probe(
                        self.rail.flyout.over_tile == Some(pi),
                        cx.listener(move |this, over: &bool, _, cx| {
                            if this.rail.flyout.tile(pi, *over) {
                                cx.notify();
                            }
                            if !*over {
                                this.settle_flyout_soon(cx);
                            }
                        }),
                    ))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_project(pi, window, cx)),
                    ),
            );
        }
        list.child(
            div()
                .id("rail-project-add")
                .map(|d| ui::labelled(d, gpui::Role::Button, "Add project"))
                .group("rail-project-add")
                .flex()
                .items_center()
                .justify_center()
                .h(px(TILE_H))
                .rounded(crate::ui::rad(8.))
                .cursor_pointer()
                .tooltip(ui::tip_text("Add project"))
                .hover(|s| s.bg(theme::hover()))
                .child(hover_icon(
                    "add01",
                    15.,
                    theme::text_faint(),
                    theme::content(),
                    "rail-project-add",
                ))
                .on_click(cx.listener(|this, _, window, cx| this.open_picker(window, cx))),
        )
        .into_any_element()
    }

    /// The flyout beside the tile under the pointer: the project, its
    /// sessions as the tree lists them, and New session.
    pub(in crate::shell) fn render_project_flyout(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.rail_narrow() {
            return None;
        }
        let pi = self.rail.flyout.open?;
        let project = self.projects.get(pi)?;
        let tile = *self.rail.flyout.tiles.borrow().get(&pi)?;
        let layout = crate::tab_layout::current();
        let mut rows = self.tree_rows(pi, cx);
        sort_rows(&mut rows, project.sort.unwrap_or(layout.sort), None);
        let count = rows.len();
        let open_id = self.active_session_id();
        let branch = project
            .environments
            .iter()
            .find(|e| e.is_checkout)
            .map(|e| e.branch.clone())
            .filter(|b| !b.is_empty());
        let logo: Hsla = gpui::rgb(project.logo).into();
        let name = project.name.clone();

        let heading = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ROW_H))
            .px(crate::ui::sp(8.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(16.))
                    .child(ui::project_icon(project.icon, 1.5, logo)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ui::TEXT_MD))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(name.clone()),
            )
            .children(branch.map(|b| {
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(b)
            }))
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(count.to_string()),
            );

        let mut list = div()
            .id("project-flyout-sessions")
            .role(gpui::Role::Tree)
            .aria_label(format!("Sessions in {name}"))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(1.))
            .max_h(px(FLYOUT_ROWS_MAX_H))
            .overflow_y_scroll();
        if rows.is_empty() {
            list = list.child(
                div()
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(6.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_faint())
                    .child("No sessions yet"),
            );
        }
        for row in rows {
            let selected = open_id.as_ref() == Some(&row.id);
            list = list.child(self.render_tree_row(row, selected, false, &layout, cx));
        }

        // Ctrl+T starts one in the open project only.
        let shortcut = (pi == self.active_project).then(|| {
            div()
                .flex_none()
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_faint())
                .child(crate::platform::shortcut_label("Ctrl+T"))
        });
        let new = ui::menu_item(
            ("project-flyout-new", pi),
            "add01",
            "New session",
            false,
            true,
        )
        .aria_label(format!("New session in {name}"))
        .children(shortcut)
        .on_click(cx.listener(move |this, _, window, cx| {
            this.rail.flyout.close();
            this.new_session_in(pi, window, cx)
        }));

        let card = div()
            .id("project-flyout")
            .map(|d| ui::labelled(d, gpui::Role::Group, name.clone()))
            .occlude()
            .relative()
            .w(px(FLYOUT_W))
            .p(crate::ui::sp(4.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(1.))
            .bg(theme::dialog())
            .border_1()
            .border_color(theme::border())
            .rounded(crate::ui::rad(ui::MENU_RADIUS))
            .shadow_xl()
            .child(super::super::hover::probe(
                self.rail.flyout.over_card,
                cx.listener(|this, over: &bool, _, cx| {
                    this.rail.flyout.card(*over);
                    if !*over {
                        this.settle_flyout_soon(cx);
                    }
                }),
            ))
            .child(heading)
            .child(list)
            .child(
                div()
                    .my(crate::ui::sp(3.))
                    .mx(crate::ui::sp(4.))
                    .h(px(1.))
                    .bg(theme::stroke()),
            )
            .child(new)
            // A session opened from here closes it; the row opens it.
            .on_click(cx.listener(|this, _, _, cx| {
                if this.rail.flyout.close() {
                    cx.notify();
                }
            }));

        let right = self.sidebar.style.side == crate::sidebar::Side::Right;
        // As far from the tile as the tile is from the strip's edge.
        let at = if right {
            point(tile.left() - px(12.), tile.top() - px(5.))
        } else {
            point(tile.right() + px(12.), tile.top() - px(5.))
        };
        Some(
            gpui::deferred(
                gpui::anchored()
                    .when(right, |a| a.anchor(gpui::Anchor::TopRight))
                    .position(at)
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            )
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_shows_its_projects_worst_state() {
        use RowState::*;
        assert_eq!(tile_state([Idle, Passed]), None);
        assert_eq!(tile_state([Idle, Working]), Some(Working));
        assert_eq!(tile_state([Working, CiFailed]), Some(CiFailed));
        assert_eq!(tile_state([Failed, Working, NeedsYou]), Some(NeedsYou));
    }

    #[test]
    fn the_flyout_follows_the_pointer_from_tile_to_flyout_and_closes_after() {
        let mut f = Flyout::default();
        assert!(f.tile(0, true));
        assert_eq!(f.open, Some(0));
        // Onto the next tile: its flyout replaces the first.
        assert!(f.tile(1, true));
        assert!(!f.tile(0, false));
        assert_eq!(f.open, Some(1));
        // Off the tile and onto the flyout before it settles: it stays.
        f.tile(1, false);
        f.card(true);
        assert!(!f.settle());
        assert_eq!(f.open, Some(1));
        // Off the flyout too: it closes.
        f.card(false);
        assert!(f.settle());
        assert_eq!(f.open, None);
    }

    #[test]
    fn closing_lets_go_of_the_flyout_under_the_pointer() {
        let mut f = Flyout::default();
        f.tile(2, true);
        f.tile(2, false);
        f.card(true);
        assert!(f.close());
        assert!(!f.over_card);
        // Shown again from its tile, it settles as soon as the pointer goes.
        f.tile(2, true);
        f.tile(2, false);
        assert!(f.settle());
    }
}
