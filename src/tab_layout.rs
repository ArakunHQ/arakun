//! How sessions and tabs show: in the sidebar as a tree, in the title bar as
//! tabs, or both (Settings › Appearance › Tabs and sessions). Three presets
//! set every option at once; each option can then be changed alone. Kept in
//! `state.json` as `tabs`.

use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

/// Where every session of every project is listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionsIn {
    /// A tree in the sidebar, grouped by project.
    #[default]
    Sidebar,
    /// Only the tabs in the title bar, with the sidebar's projects and
    /// environments as they were before the tree.
    TitleBar,
    Both,
}

/// Which tabs the title bar holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarShows {
    /// Only the sessions pinned to it, from any project.
    #[default]
    Pinned,
    /// The open tabs of the project on screen.
    Project,
    /// Every open tab, of every project, in one row.
    AllOpen,
}

/// How a working, waiting or failed session shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateMark {
    /// A mark of its own shape for each state.
    #[default]
    Icon,
    /// A colored dot.
    Dot,
    /// The state in a word.
    Word,
}

/// What the sidebar tree does with a project that has nothing running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quiet {
    Open,
    #[default]
    Folded,
    Hidden,
}

/// The order of the sessions in the sidebar tree, within each project or,
/// without project headings, across all of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    /// The session used last on top.
    #[default]
    Latest,
    /// By title, A to Z.
    Title,
    /// Environment by environment, as the projects list shows them.
    Environment,
}

impl Sort {
    pub const ALL: [Sort; 3] = [Sort::Latest, Sort::Title, Sort::Environment];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Latest => "Latest",
            Sort::Title => "A–Z",
            Sort::Environment => "By environment",
        }
    }

    /// The order after this one, for the tree heading's button.
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// How a title too long for its row shows in the sidebar. Tabs always cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LongTitles {
    #[default]
    Cut,
    TwoLines,
}

/// A starting point that sets every option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// Sessions in the sidebar, pinned tabs in the bar.
    Focused,
    /// Every open tab in one row, as before the sidebar tree.
    Classic,
    /// Every project open, every tab in the bar.
    Everything,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Focused, Preset::Classic, Preset::Everything];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Focused => "Focused",
            Preset::Classic => "Classic",
            Preset::Everything => "Everything",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Preset::Focused => "Sessions in the sidebar, pinned tabs in the bar",
            Preset::Classic => "Every open tab in one row, as before",
            Preset::Everything => "Every project open, every tab in the bar",
        }
    }

    pub fn layout(self) -> TabLayout {
        match self {
            Preset::Focused => TabLayout {
                sessions_in: SessionsIn::Sidebar,
                bar_shows: BarShows::Pinned,
                state: StateMark::Icon,
                group: true,
                quiet: Quiet::Folded,
                titles: LongTitles::Cut,
                sort: Sort::Latest,
            },
            Preset::Classic => TabLayout {
                sessions_in: SessionsIn::TitleBar,
                bar_shows: BarShows::AllOpen,
                state: StateMark::Icon,
                group: true,
                quiet: Quiet::Open,
                titles: LongTitles::Cut,
                sort: Sort::Latest,
            },
            Preset::Everything => TabLayout {
                sessions_in: SessionsIn::Both,
                bar_shows: BarShows::AllOpen,
                state: StateMark::Icon,
                group: true,
                quiet: Quiet::Open,
                titles: LongTitles::Cut,
                sort: Sort::Latest,
            },
        }
    }

    /// The sidebar width a preset asks for at least, in points: the tree
    /// needs about 240 to show a title and its state. `None` leaves the
    /// width as it is.
    pub fn min_sidebar_w(self) -> Option<f32> {
        match self {
            Preset::Focused | Preset::Everything => Some(SIDEBAR_W),
            Preset::Classic => None,
        }
    }
}

/// The sidebar width the tree is drawn for.
pub const SIDEBAR_W: f32 = 248.;

/// An option of the Tabs and sessions settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opt {
    Group,
    Sort,
    Quiet,
    Titles,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabLayout {
    pub sessions_in: SessionsIn,
    pub bar_shows: BarShows,
    pub state: StateMark,
    /// Each project heads its own sessions in the tree.
    pub group: bool,
    pub quiet: Quiet,
    pub titles: LongTitles,
    pub sort: Sort,
}

impl Default for TabLayout {
    fn default() -> Self {
        Preset::Focused.layout()
    }
}

impl TabLayout {
    /// The layout for a state without this setting. A new install (no state
    /// file) starts on Focused. Someone who already used Arakun keeps the
    /// one row of tabs they know, and finds Focused in Settings.
    pub fn for_state_without_it(existing: bool) -> Self {
        if existing {
            Preset::Classic.layout()
        } else {
            Preset::Focused.layout()
        }
    }

    /// The preset this layout is, if it is one.
    pub fn preset(&self) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.layout() == *self)
    }

    /// Whether the sidebar shows the tree of sessions.
    pub fn tree(&self) -> bool {
        self.sessions_in != SessionsIn::TitleBar
    }

    /// Whether the title bar holds only pinned sessions.
    pub fn pinned_bar(&self) -> bool {
        self.bar_shows == BarShows::Pinned
    }

    /// Whether `opt` means something with the other options as they are.
    /// With sessions only in the title bar there is no tree to group, fold
    /// or wrap, and nothing to pin from; without groups nothing folds.
    pub fn applies(&self, opt: Opt) -> bool {
        match opt {
            Opt::Group | Opt::Sort | Opt::Titles => self.tree(),
            Opt::Quiet => self.tree() && self.group,
        }
    }

    /// Whether `bar` can be chosen with the other options as they are.
    pub fn allows_bar(&self, bar: BarShows) -> bool {
        bar != BarShows::Pinned || self.tree()
    }

    /// This layout with one option changed, kept sensible: moving sessions
    /// out of the sidebar can't leave a bar of pinned tabs nobody can pin to.
    pub fn with(mut self, change: impl FnOnce(&mut TabLayout)) -> Self {
        change(&mut self);
        if !self.allows_bar(self.bar_shows) {
            self.bar_shows = BarShows::AllOpen;
        }
        self
    }
}

static CURRENT: Mutex<Option<TabLayout>> = Mutex::new(None);

/// The layout in use.
pub fn current() -> TabLayout {
    CURRENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unwrap_or_default()
}

/// Uses `layout`, made sensible first: a file edited by hand, or written by
/// another version, may hold a combination the settings rule out.
pub fn set(layout: TabLayout) {
    *CURRENT.lock().unwrap_or_else(PoisonError::into_inner) = Some(layout.with(|_| {}));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_preset_is_found_again() {
        for p in Preset::ALL {
            assert_eq!(p.layout().preset(), Some(p));
        }
        let mine = Preset::Focused.layout().with(|l| l.state = StateMark::Word);
        assert_eq!(mine.preset(), None);
    }

    #[test]
    fn classic_is_the_strip_from_before() {
        let c = Preset::Classic.layout();
        assert!(!c.tree());
        assert_eq!(c.bar_shows, BarShows::AllOpen);
        assert!(Preset::Focused.layout().tree());
        assert!(Preset::Everything.layout().tree());
    }

    #[test]
    fn old_state_files_keep_the_strip_and_new_ones_get_focused() {
        assert_eq!(
            TabLayout::for_state_without_it(true).preset(),
            Some(Preset::Classic)
        );
        assert_eq!(
            TabLayout::for_state_without_it(false).preset(),
            Some(Preset::Focused)
        );
        // Set by hand to a pinned bar with nothing to pin from: `set` makes
        // it sensible the way `with` does, so the bar shows every open tab.
        let by_hand = TabLayout {
            sessions_in: SessionsIn::TitleBar,
            ..Preset::Focused.layout()
        };
        assert_eq!(by_hand.with(|_| {}).bar_shows, BarShows::AllOpen);
        // A file that has the setting reads it; a field it lacks takes Focused's.
        let read: TabLayout = serde_json::from_str(r#"{"bar_shows":"project"}"#).unwrap();
        assert_eq!(read.bar_shows, BarShows::Project);
        assert_eq!(read.sessions_in, SessionsIn::Sidebar);
        // A file from before sorting puts the latest session on top.
        assert_eq!(read.sort, Sort::Latest);
    }

    #[test]
    fn options_without_a_tree_are_off() {
        let bar_only = Preset::Focused
            .layout()
            .with(|l| l.sessions_in = SessionsIn::TitleBar);
        // Nothing to pin from: the bar shows every open tab instead.
        assert_eq!(bar_only.bar_shows, BarShows::AllOpen);
        assert!(!bar_only.allows_bar(BarShows::Pinned));
        assert!(!bar_only.applies(Opt::Group));
        assert!(!bar_only.applies(Opt::Quiet));
        assert!(!bar_only.applies(Opt::Titles));
        assert!(!bar_only.applies(Opt::Sort));

        let flat = Preset::Focused.layout().with(|l| l.group = false);
        assert!(flat.applies(Opt::Titles));
        assert!(!flat.applies(Opt::Quiet));
        assert!(Preset::Focused.layout().applies(Opt::Quiet));
    }

    #[test]
    fn the_sort_button_goes_round_every_order() {
        let mut s = Sort::default();
        assert_eq!(s, Sort::Latest);
        for _ in 0..Sort::ALL.len() {
            s = s.next();
        }
        assert_eq!(s, Sort::Latest);
        assert_eq!(Sort::Latest.next(), Sort::Title);
    }
}
