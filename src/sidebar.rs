//! What the rail shows and how: segments of pieces each person arranges for
//! themselves (in the sidebar builder, `crate::sidebar_builder`), plus the
//! rail's own style. `Shell::render_rail` draws it. Kept in `sidebar.json` next
//! to the state, so a layout can be copied between machines.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A view the rail can link to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Inbox,
    #[cfg(feature = "coderlabs")]
    Tasks,
    Runs,
    Branches,
    Notes,
    Memory,
    Usage,
    Extensions,
    Cloudflare,
    PostHog,
    #[cfg(feature = "coderlabs")]
    Site,
    Isometric,
    #[cfg(feature = "coderlabs")]
    Canvas,
    Phone,
    Automations,
    /// What the team hub holds (`team_view`).
    #[cfg(feature = "team")]
    Team,
}

impl View {
    /// In the order the rail used to list them.
    pub const ALL: &[View] = &[
        View::Inbox,
        #[cfg(feature = "coderlabs")]
        View::Tasks,
        View::Runs,
        View::Branches,
        View::Notes,
        View::Memory,
        View::Usage,
        View::Extensions,
        View::Cloudflare,
        View::PostHog,
        #[cfg(feature = "coderlabs")]
        View::Site,
        View::Isometric,
        #[cfg(feature = "coderlabs")]
        View::Canvas,
        View::Phone,
        View::Automations,
        #[cfg(feature = "team")]
        View::Team,
    ];

    pub fn label(self) -> &'static str {
        match self {
            View::Inbox => "Inbox",
            #[cfg(feature = "coderlabs")]
            View::Tasks => "Tasks",
            View::Runs => "Runs",
            View::Branches => "Branches",
            View::Notes => "Notes",
            View::Memory => "Memory",
            View::Usage => "Usage",
            View::Extensions => "Extensions",
            View::Cloudflare => "Cloudflare",
            View::PostHog => "PostHog",
            #[cfg(feature = "coderlabs")]
            View::Site => "arakun.dev",
            View::Isometric => "Isometric",
            #[cfg(feature = "coderlabs")]
            View::Canvas => "Canvas",
            View::Phone => "Phone",
            View::Automations => "Automations",
            #[cfg(feature = "team")]
            View::Team => "Team",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            View::Inbox => "inbox",
            #[cfg(feature = "coderlabs")]
            View::Tasks => "checkmark-circle02",
            View::Runs => "activity01",
            View::Branches => "git-branch",
            View::Notes => "file01",
            View::Memory => "ai-idea",
            View::Usage => "dashboard-square01",
            View::Extensions => "sparkles",
            View::Cloudflare => "internet",
            View::PostHog => "chart-breakout-square",
            #[cfg(feature = "coderlabs")]
            View::Site => "globe",
            View::Isometric => "cube",
            #[cfg(feature = "coderlabs")]
            View::Canvas => "artboard",
            View::Phone => "signal-full02",
            View::Automations => "flash",
            #[cfg(feature = "team")]
            View::Team => "ungroup-items",
        }
    }

    /// One line for the block library.
    #[allow(dead_code)] // the builder does not list views with a blurb yet
    pub fn blurb(self) -> &'static str {
        match self {
            View::Inbox => "Sessions waiting for you",
            #[cfg(feature = "coderlabs")]
            View::Tasks => "Your task list",
            View::Runs => "CI runs and their logs",
            View::Branches => "Every branch and its PR",
            View::Notes => "Project notes",
            View::Memory => "What agents remember here",
            View::Usage => "Plan limits and spend",
            View::Extensions => "MCP servers, skills and plugins",
            View::Cloudflare => "Workers, tunnels and DNS",
            View::PostHog => "Product analytics for each repo",
            #[cfg(feature = "coderlabs")]
            View::Site => "Your arakun.dev account",
            View::Isometric => "Animated isometric scenes",
            #[cfg(feature = "coderlabs")]
            View::Canvas => "Design frames, drawn natively",
            View::Phone => "Arakun on your phone",
            View::Automations => "Scheduled agent runs",
            #[cfg(feature = "team")]
            View::Team => "What the team finds, kept to read",
        }
    }
}

/// Something of the user's own, one click away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Pin {
    /// Opens in a browser tab.
    Url { title: String, url: String },
    /// Opens in a file tab.
    File { title: String, path: PathBuf },
    /// Runs in a new terminal of the active environment.
    Command { title: String, command: String },
}

impl Pin {
    pub fn title(&self) -> &str {
        match self {
            Pin::Url { title, .. } | Pin::File { title, .. } | Pin::Command { title, .. } => title,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Pin::Url { .. } => "globe",
            Pin::File { .. } => "file01",
            Pin::Command { .. } => "computer-terminal01",
        }
    }
}

/// How the rail sits in the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    /// A card beside the content, as the rail always was.
    #[default]
    Card,
    /// No card: the rail is part of the window's background.
    Flush,
    /// A card lifted off the window, with a shadow.
    Floating,
}

/// What Ctrl+B (and the rail's own button) does to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collapse {
    /// It hides completely.
    #[default]
    Hide,
    /// It narrows to a strip of icons.
    Icons,
    /// It always shows.
    Never,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Roomy,
}

impl Density {
    /// Height of a row.
    pub fn row_h(self) -> f32 {
        match self {
            Density::Compact => 28.,
            Density::Normal => 32.,
            Density::Roomy => 36.,
        }
    }
}

/// The rail as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub variant: Variant,
    pub collapse: Collapse,
    pub side: Side,
    pub density: Density,
    /// Counts on rows (Inbox, Runs…).
    pub badges: bool,
    /// Keyboard shortcuts on rows that have one.
    pub shortcuts: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            variant: Variant::Card,
            collapse: Collapse::Hide,
            side: Side::Left,
            density: Density::Normal,
            badges: true,
            shortcuts: true,
        }
    }
}

/// What a piece is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Piece {
    /// The search box (Ctrl+K).
    Search,
    View {
        view: View,
    },
    /// Projects, with the environments of the open one.
    Projects {
        #[serde(default = "yes")]
        envs: bool,
        #[serde(default = "yes")]
        changes: bool,
        /// Only the open project, the others a click away in a dropdown.
        #[serde(default)]
        only_open: bool,
    },
    Pin {
        pin: Pin,
    },
    /// Who is signed in, with the account menu.
    User,
    /// Plan usage of the signed-in agents.
    Usage,
    Settings,
    /// The raccoon, whose mood follows the agents.
    Mascot,
    Separator,
    /// A line of text, as a heading inside a segment.
    Title {
        text: String,
    },
    /// Empty room that pushes what follows to the segment's end.
    Spacer,
}

fn yes() -> bool {
    true
}

impl Piece {
    pub fn view(view: View) -> Self {
        Piece::View { view }
    }

    pub fn projects() -> Self {
        Piece::Projects {
            envs: true,
            changes: true,
            only_open: false,
        }
    }

    /// One of a kind: placing it again moves it.
    pub fn unique(&self) -> bool {
        !matches!(
            self,
            Piece::Pin { .. } | Piece::Separator | Piece::Title { .. } | Piece::Spacer
        )
    }

    /// The same piece, whatever its options.
    pub fn same(&self, other: &Piece) -> bool {
        match (self, other) {
            (Piece::Projects { .. }, Piece::Projects { .. }) => true,
            (a, b) => a == b,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Piece::Search => "Search".into(),
            Piece::View { view } => view.label().into(),
            Piece::Projects { .. } => "Projects".into(),
            Piece::Pin { pin } => pin.title().into(),
            Piece::User => "User".into(),
            Piece::Usage => "Usage card".into(),
            Piece::Settings => "Settings".into(),
            Piece::Mascot => "Mascot".into(),
            Piece::Separator => "Separator".into(),
            Piece::Title { text } => {
                if text.is_empty() {
                    "Title".into()
                } else {
                    text.clone()
                }
            }
            Piece::Spacer => "Spacer".into(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Piece::Search => "search01",
            Piece::View { view } => view.icon(),
            Piece::Projects { .. } => "folder01",
            Piece::Pin { pin } => pin.icon(),
            Piece::User => "user",
            Piece::Usage => "dashboard-square01",
            Piece::Settings => "settings01",
            Piece::Mascot => "smile-plus",
            Piece::Separator => "minus-sign",
            Piece::Title { .. } => "pencil-edit01",
            Piece::Spacer => "unfold-more",
        }
    }
}

/// How a row shows its count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeMode {
    /// The number, whenever there is one.
    #[default]
    Count,
    /// A dot instead of the number.
    Dot,
    /// Only when it is urgent (a failure, something waiting).
    Urgent,
    Off,
}

/// A piece where the user put it, with how they want it to look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    #[serde(flatten)]
    pub piece: Piece,
    /// Shown instead of the piece's own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// An emoji shown instead of the piece's icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub badge: BadgeMode,
    /// Shown only while it has something to say (a count).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub only_when_busy: bool,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

impl Item {
    pub fn new(piece: Piece) -> Self {
        Item {
            piece,
            label: None,
            emoji: None,
            badge: BadgeMode::Count,
            only_when_busy: false,
        }
    }

    pub fn name(&self) -> String {
        self.label
            .clone()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| self.piece.label())
    }
}

/// A button on a segment's title.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleAction {
    AddProject,
    NewSession,
    NewTerminal,
}

impl TitleAction {
    pub const ALL: [TitleAction; 3] = [
        TitleAction::AddProject,
        TitleAction::NewSession,
        TitleAction::NewTerminal,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TitleAction::AddProject => "Add project",
            TitleAction::NewSession => "New session",
            TitleAction::NewTerminal => "New terminal",
        }
    }
}

/// Whether a segment stays put or scrolls with its neighbours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Always on screen.
    Fixed,
    /// Scrolls; scrolling segments next to each other share one scroll.
    #[default]
    Scroll,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub show_title: bool,
    #[serde(default)]
    pub collapsible: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub line_above: bool,
    /// Not drawn while the rail is narrowed to icons.
    #[serde(default)]
    pub hide_in_icons: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<TitleAction>,
    #[serde(default)]
    pub items: Vec<Item>,
}

impl Section {
    pub fn new(title: &str, mode: Mode, pieces: Vec<Piece>) -> Self {
        Section {
            title: title.into(),
            mode,
            show_title: false,
            collapsible: false,
            collapsed: false,
            line_above: false,
            hide_in_icons: false,
            action: None,
            items: pieces.into_iter().map(Item::new).collect(),
        }
    }

    fn titled(mut self) -> Self {
        self.show_title = true;
        self
    }

    fn folding(mut self, collapsed: bool) -> Self {
        self.show_title = true;
        self.collapsible = true;
        self.collapsed = collapsed;
        self
    }

    fn with_action(mut self, a: TitleAction) -> Self {
        self.action = Some(a);
        self
    }
}

/// An item's place: its segment, and its index in it.
pub type Path = (usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// Search, inbox and projects; the rest folded away.
    Focused,
    /// Inbox, projects, and the git tools in their own segment.
    Developer,
    /// Pages first, Notion style: favorites, then projects.
    Notion,
    /// A strip of icons.
    Compact,
    /// Every view, as the rail was before it could be arranged.
    Everything,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Focused,
        Preset::Developer,
        Preset::Notion,
        Preset::Compact,
        Preset::Everything,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Focused => "Focused",
            Preset::Developer => "Developer",
            Preset::Notion => "Notion",
            Preset::Compact => "Icons",
            Preset::Everything => "Everything",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Preset::Focused => "Inbox and projects; the rest folded away",
            Preset::Developer => "Projects, then runs and branches in a segment of their own",
            Preset::Notion => "Search and inbox up top, your pins, projects, account below",
            Preset::Compact => "Narrowed to icons; names show on hover",
            Preset::Everything => "Every view, as the rail used to be",
        }
    }

    pub fn layout(self) -> Sidebar {
        let views = |vs: &[View]| vs.iter().map(|v| Piece::view(*v)).collect::<Vec<_>>();
        let more = [
            View::Notes,
            View::Memory,
            View::Usage,
            View::Extensions,
            View::Cloudflare,
            View::PostHog,
            #[cfg(feature = "coderlabs")]
            View::Site,
            View::Isometric,
            View::Phone,
            View::Automations,
            #[cfg(feature = "team")]
            View::Team,
        ];
        let top = Section::new("Top", Mode::Fixed, vec![Piece::Search]);
        let projects = Section::new("Projects", Mode::Scroll, vec![Piece::projects()])
            .titled()
            .with_action(TitleAction::AddProject);
        let account = |pieces| Section::new("Account", Mode::Fixed, pieces);
        let mut style = Style::default();
        let sections = match self {
            Preset::Focused => vec![
                Section::new(
                    "Top",
                    Mode::Fixed,
                    vec![Piece::Search, Piece::view(View::Inbox)],
                ),
                projects,
                Section::new(
                    "Tools",
                    Mode::Scroll,
                    views(
                        &[
                            &[
                                #[cfg(feature = "coderlabs")]
                                View::Tasks,
                                View::Runs,
                                View::Branches,
                            ][..],
                            &more,
                        ]
                        .concat(),
                    ),
                )
                .folding(true),
                account(vec![Piece::Usage, Piece::Settings]),
            ],
            Preset::Developer => vec![
                Section::new(
                    "Top",
                    Mode::Fixed,
                    vec![
                        Piece::Search,
                        Piece::view(View::Inbox),
                        #[cfg(feature = "coderlabs")]
                        Piece::view(View::Tasks),
                    ],
                ),
                projects,
                Section::new("Git", Mode::Scroll, views(&[View::Runs, View::Branches])).titled(),
                Section::new("More", Mode::Scroll, views(&more)).folding(true),
                account(vec![Piece::Usage, Piece::User, Piece::Settings]),
            ],
            Preset::Notion => vec![
                Section::new(
                    "Top",
                    Mode::Fixed,
                    vec![Piece::User, Piece::Search, Piece::view(View::Inbox)],
                ),
                Section::new(
                    "Pinned",
                    Mode::Scroll,
                    views(&[
                        View::Notes,
                        #[cfg(feature = "coderlabs")]
                        View::Tasks,
                    ]),
                )
                .folding(false),
                projects,
                Section::new(
                    "More",
                    Mode::Scroll,
                    views(&[&[View::Runs, View::Branches][..], &more[1..]].concat()),
                )
                .folding(true),
                Section::new("Bottom", Mode::Fixed, vec![Piece::Settings]),
            ],
            Preset::Compact => {
                style.collapse = Collapse::Icons;
                style.density = Density::Compact;
                vec![
                    Section::new(
                        "Top",
                        Mode::Fixed,
                        vec![Piece::Search, Piece::view(View::Inbox)],
                    ),
                    Section::new(
                        "Views",
                        Mode::Scroll,
                        views(&[
                            #[cfg(feature = "coderlabs")]
                            View::Tasks,
                            View::Runs,
                            View::Branches,
                            View::Notes,
                        ]),
                    ),
                    projects,
                    account(vec![Piece::Settings]),
                ]
            }
            Preset::Everything => vec![
                top,
                Section::new("Views", Mode::Scroll, views(View::ALL)),
                projects,
                account(vec![Piece::Usage, Piece::Settings]),
            ],
        };
        Sidebar {
            style,
            sections,
            known: View::ALL.to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sidebar {
    #[serde(default)]
    pub style: Style,
    pub sections: Vec<Section>,
    /// Every view this layout has been offered. A view Arakun adds later is
    /// not in it, so the builder can mark it new instead of forcing it in.
    #[serde(default)]
    pub known: Vec<View>,
}

impl Default for Sidebar {
    /// Before anyone arranged it: the rail as it always looked, so an update
    /// moves nothing.
    fn default() -> Self {
        Preset::Everything.layout()
    }
}

impl Sidebar {
    pub fn load() -> Self {
        std::fs::read(file())
            .ok()
            .and_then(|b| Self::parse(&b))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = crate::durable::write(&file(), &bytes);
        }
    }

    /// Reads a layout, skipping pieces this version does not know (from a
    /// newer Arakun, or edited by hand) instead of dropping it all.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let mut v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let sections = v.get_mut("sections")?.as_array_mut()?;
        for s in sections.iter_mut() {
            if let Some(items) = s.get_mut("items").and_then(|i| i.as_array_mut()) {
                items.retain(|i| serde_json::from_value::<Item>(i.clone()).is_ok());
            }
        }
        if let Some(known) = v.get_mut("known").and_then(|k| k.as_array_mut()) {
            known.retain(|k| serde_json::from_value::<View>(k.clone()).is_ok());
        }
        let mut s: Sidebar = serde_json::from_value(v).ok()?;
        s.tidy();
        Some(s)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn get(&self, (s, i): Path) -> Option<&Item> {
        self.sections.get(s)?.items.get(i)
    }

    pub fn get_mut(&mut self, (s, i): Path) -> Option<&mut Item> {
        self.sections.get_mut(s)?.items.get_mut(i)
    }

    pub fn find(&self, piece: &Piece) -> Option<Path> {
        self.sections.iter().enumerate().find_map(|(s, sec)| {
            sec.items
                .iter()
                .position(|i| i.piece.same(piece))
                .map(|i| (s, i))
        })
    }

    pub fn has(&self, piece: &Piece) -> bool {
        self.find(piece).is_some()
    }

    /// Views not in the rail.
    #[cfg_attr(not(test), allow(dead_code))] // only the tests ask for it so far
    pub fn hidden_views(&self) -> Vec<View> {
        View::ALL
            .iter()
            .copied()
            .filter(|v| !self.has(&Piece::view(*v)))
            .collect()
    }

    /// A view this layout has never offered.
    pub fn is_new(&self, view: View) -> bool {
        !self.known.contains(&view)
    }

    pub fn mark_known(&mut self) -> bool {
        let before = self.known.len();
        for &v in View::ALL {
            if !self.known.contains(&v) {
                self.known.push(v);
            }
        }
        self.known.len() != before
    }

    pub fn remove(&mut self, (s, i): Path) -> Option<Item> {
        let items = &mut self.sections.get_mut(s)?.items;
        (i < items.len()).then(|| items.remove(i))
    }

    /// Puts an item at `to` (index may be the segment's length, for last).
    /// A unique piece already in the rail moves instead of showing twice.
    pub fn insert(&mut self, item: Item, to: Path) -> bool {
        if to.0 >= self.sections.len() {
            return false;
        }
        let to = match item
            .piece
            .unique()
            .then(|| self.find(&item.piece))
            .flatten()
        {
            Some(from) => {
                self.remove(from);
                shift(to, from)
            }
            None => to,
        };
        let items = &mut self.sections[to.0].items;
        items.insert(to.1.min(items.len()), item);
        true
    }

    pub fn move_item(&mut self, from: Path, to: Path) -> bool {
        if to.0 >= self.sections.len() {
            return false;
        }
        let Some(item) = self.remove(from) else {
            return false;
        };
        let to = shift(to, from);
        let items = &mut self.sections[to.0].items;
        items.insert(to.1.min(items.len()), item);
        true
    }

    /// Moves a segment so it lands at `to` (counted before the move).
    pub fn move_section(&mut self, from: usize, to: usize) -> bool {
        if from >= self.sections.len() || to > self.sections.len() {
            return false;
        }
        let s = self.sections.remove(from);
        let to = if from < to { to - 1 } else { to };
        self.sections.insert(to.min(self.sections.len()), s);
        true
    }

    pub fn add_section(&mut self, at: usize) -> usize {
        let n = self.sections.len() + 1;
        let mut s = Section::new(&format!("Segment {n}"), Mode::Scroll, vec![]);
        s.show_title = true;
        let at = at.min(self.sections.len());
        self.sections.insert(at, s);
        at
    }

    pub fn remove_section(&mut self, s: usize) -> Option<Section> {
        (s < self.sections.len()).then(|| self.sections.remove(s))
    }

    /// One of each unique piece.
    fn tidy(&mut self) {
        let mut seen: Vec<Piece> = Vec::new();
        for sec in &mut self.sections {
            sec.items.retain(|i| {
                if !i.piece.unique() {
                    return true;
                }
                if seen.iter().any(|p| p.same(&i.piece)) {
                    return false;
                }
                seen.push(i.piece.clone());
                true
            });
        }
    }
}

/// Where `to` points once the item at `removed` is gone.
fn shift(to: Path, removed: Path) -> Path {
    if to.0 == removed.0 && removed.1 < to.1 {
        (to.0, to.1 - 1)
    } else {
        to
    }
}

fn file() -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name("sidebar.json"),
        None => crate::state::data_dir().join("sidebar.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &Sidebar) -> Vec<String> {
        s.sections
            .iter()
            .map(|sec| {
                format!(
                    "{}[{}]",
                    sec.title,
                    sec.items
                        .iter()
                        .map(|i| i.name())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect()
    }

    fn small() -> Sidebar {
        Sidebar {
            style: Style::default(),
            sections: vec![
                Section::new(
                    "A",
                    Mode::Fixed,
                    vec![Piece::Search, Piece::view(View::Inbox)],
                ),
                Section::new(
                    "B",
                    Mode::Scroll,
                    vec![Piece::projects(), Piece::view(View::Runs)],
                ),
                Section::new("C", Mode::Fixed, vec![Piece::Settings]),
            ],
            known: View::ALL.to_vec(),
        }
    }

    #[test]
    fn the_default_is_the_rail_as_it_was() {
        let s = Sidebar::default();
        assert!(View::ALL.iter().all(|v| s.has(&Piece::view(*v))));
        assert!(s.hidden_views().is_empty());
        assert_eq!(s.style, Style::default());
    }

    #[test]
    fn items_move_within_and_across_segments() {
        let mut s = small();
        assert!(s.move_item((1, 1), (1, 0)));
        assert_eq!(
            names(&s),
            ["A[Search,Inbox]", "B[Runs,Projects]", "C[Settings]"]
        );
        // Down past itself lands where it was aimed.
        assert!(s.move_item((0, 0), (0, 2)));
        assert_eq!(
            names(&s),
            ["A[Inbox,Search]", "B[Runs,Projects]", "C[Settings]"]
        );
        assert!(s.move_item((2, 0), (0, 0)));
        assert_eq!(
            names(&s),
            ["A[Settings,Inbox,Search]", "B[Runs,Projects]", "C[]"]
        );
    }

    #[test]
    fn a_unique_piece_moves_instead_of_doubling() {
        let mut s = small();
        assert!(s.insert(Item::new(Piece::view(View::Runs)), (0, 0)));
        assert_eq!(
            names(&s),
            ["A[Runs,Search,Inbox]", "B[Projects]", "C[Settings]"]
        );
        assert!(s.insert(Item::new(Piece::Separator), (2, 0)));
        assert!(s.insert(Item::new(Piece::Separator), (2, 0)));
        assert_eq!(names(&s)[2], "C[Separator,Separator,Settings]");
    }

    #[test]
    fn segments_reorder_add_and_go() {
        let mut s = small();
        assert!(s.move_section(2, 0));
        assert_eq!(
            names(&s),
            ["C[Settings]", "A[Search,Inbox]", "B[Projects,Runs]"]
        );
        assert!(s.move_section(0, 3));
        assert_eq!(
            names(&s),
            ["A[Search,Inbox]", "B[Projects,Runs]", "C[Settings]"]
        );
        let at = s.add_section(1);
        assert_eq!(at, 1);
        assert_eq!(names(&s)[1], "Segment 4[]");
        s.remove_section(1);
        assert_eq!(names(&s), names(&small()));
        assert!(s.hidden_views().contains(&View::Notes));
    }

    #[test]
    fn a_layout_survives_unknown_pieces_and_duplicates() {
        let json = br#"{"style":{"variant":"floating","side":"right","glow":3},"sections":[
            {"title":"A","items":[{"type":"view","view":"inbox"},{"type":"hologram"},{"type":"view","view":"teleporter"}]},
            {"title":"B","mode":"fixed","items":[{"type":"view","view":"inbox","label":"Mail"},{"type":"projects"}]}
        ],"known":["inbox","teleporter"]}"#;
        let s = Sidebar::parse(json).unwrap();
        assert_eq!(names(&s), ["A[Inbox]", "B[Projects]"]);
        assert_eq!(s.style.variant, Variant::Floating);
        assert_eq!(s.style.side, Side::Right);
        assert!(s.style.badges);
        assert!(s.is_new(View::Phone));
    }

    #[test]
    fn presets_round_trip() {
        for p in Preset::ALL {
            let s = p.layout();
            assert_eq!(
                Sidebar::parse(s.to_json().as_bytes()),
                Some(s.clone()),
                "{}",
                p.label()
            );
            assert!(s.has(&Piece::projects()), "{}", p.label());
            // Every view once at most: a preset can't show one twice.
            let views: Vec<_> = s
                .sections
                .iter()
                .flat_map(|x| &x.items)
                .filter(|i| matches!(i.piece, Piece::View { .. }))
                .collect();
            assert_eq!(
                views.len(),
                View::ALL
                    .iter()
                    .filter(|v| s.has(&Piece::view(**v)))
                    .count(),
                "{}",
                p.label()
            );
        }
    }
}
