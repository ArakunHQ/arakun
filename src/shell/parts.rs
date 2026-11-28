//! The rail, the title bar and the Workspace panel, each drawn as a view of
//! its own so the shell can draw it cached.
//!
//! While an agent works, the loops in its transcript and the mascot above
//! the composer redraw many times a second. Each time, gpui renders the shell
//! again, because it is the window's root. A cached view inside it reuses its
//! last drawing instead, as long as nothing it shows has changed.
//!
//! gpui redraws a cached view only when it, or a view drawn inside it,
//! notifies. A part draws from the shell's state and a few entities besides,
//! so it follows the shell and each of those. It asks which ones every time
//! it draws, because they change with the tabs and the session in view.

use super::*;

/// Subscribes a part to an entity it read, so a change there redraws it.
type Follow = Box<dyn FnOnce(&mut Context<Part>) -> Subscription>;

fn follow<W: 'static>(entity: &Entity<W>) -> Follow {
    let entity = entity.clone();
    Box::new(move |cx| cx.observe(&entity, |_, _, cx| cx.notify()))
}

/// How a part draws itself from the shell.
type Draw = fn(&Shell, &Window, &mut Context<Shell>) -> AnyElement;
/// The entities besides the shell that a part reads as it draws.
type Reads = fn(&Shell, &App) -> Vec<Follow>;

pub(super) struct Part {
    shell: gpui::WeakEntity<Shell>,
    draw: Draw,
    reads: Reads,
    follows: Vec<Subscription>,
}

impl Render for Part {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shell) = self.shell.upgrade() else {
            return div().into_any_element();
        };
        let draw = self.draw;
        let element = shell.update(cx, |shell, cx| draw(shell, window, cx));
        let reads = (self.reads)(shell.read(cx), cx);
        let mut follows = vec![cx.observe(&shell, |_, _, cx| cx.notify())];
        follows.extend(reads.into_iter().map(|follow| follow(cx)));
        self.follows = follows;
        element
    }
}

/// The shell's parts, each made the first time it shows.
#[derive(Default)]
pub(super) struct Parts {
    rail: Option<Entity<Part>>,
    titlebar: Option<Entity<Part>>,
    panel: Option<Entity<Part>>,
}

fn rail_reads(shell: &Shell, _: &App) -> Vec<Follow> {
    // The badges, the user row, the usage meter and the session filter.
    vec![
        follow(&shell.account),
        follow(&shell.teams),
        follow(&shell.ci),
        follow(&shell.github),
        follow(&shell.pilot),
        follow(&shell.views.extensions),
        follow(&shell.views.branches),
        follow(&shell.rail.tree_filter),
    ]
}

fn titlebar_reads(shell: &Shell, _: &App) -> Vec<Follow> {
    // A browser tab shows its page's icon.
    shell
        .strip
        .tabs
        .iter()
        .filter_map(|t| t.browser.as_ref())
        .map(|b| follow(&b.view))
        .collect()
}

fn panel_reads(shell: &Shell, cx: &App) -> Vec<Follow> {
    // The Session tab: the session's subagents, their trails and its pull
    // request.
    let mut reads = vec![follow(&shell.ci), follow(&shell.session_query)];
    if let Some(thread) = shell.active_thread(cx) {
        reads.extend(
            thread
                .read(cx)
                .subagents()
                .iter()
                .map(|s| follow(&s.thread)),
        );
        reads.push(follow(&thread));
    }
    reads
}

impl Shell {
    /// The rail, cached at its width, the strip included.
    pub(super) fn cached_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let style = gpui::StyleRefinement::default()
            .w(px(self.rail_width()))
            .h_full()
            .flex_none();
        self.cached_part(
            |p| &mut p.rail,
            |s, window, cx| s.render_rail(window, cx),
            rail_reads,
            style,
            cx,
        )
    }

    /// The title bar, cached across the main card's width.
    pub(super) fn cached_titlebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let style = gpui::StyleRefinement::default()
            .w_full()
            .h(px(titlebar::TITLEBAR_H))
            .flex_none();
        self.cached_part(
            |p| &mut p.titlebar,
            |s, window, cx| s.render_titlebar(window, cx).into_any_element(),
            titlebar_reads,
            style,
            cx,
        )
    }

    /// The Workspace panel, or Customize in its place, cached at its width.
    pub(super) fn cached_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let style = gpui::StyleRefinement::default()
            .w(px(self.panel_width()))
            .h_full()
            .flex_none();
        self.cached_part(
            |p| &mut p.panel,
            |s, _, cx| s.render_panel(cx),
            panel_reads,
            style,
            cx,
        )
    }

    fn cached_part(
        &mut self,
        slot: fn(&mut Parts) -> &mut Option<Entity<Part>>,
        draw: Draw,
        reads: Reads,
        style: gpui::StyleRefinement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shell = cx.entity().downgrade();
        let part = slot(&mut self.parts)
            .get_or_insert_with(|| {
                cx.new(|_| Part {
                    shell,
                    draw,
                    reads,
                    follows: Vec::new(),
                })
            })
            .clone();
        gpui::AnyView::from(part).cached(style).into_any_element()
    }
}
