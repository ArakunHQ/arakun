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
    /// snake_case would write `git_hub`.
    #[serde(rename = "github")]
    GitHub,
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
    Servers,
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
        View::GitHub,
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
        View::Servers,
        View::Automations,
        #[cfg(feature = "team")]
        View::Team,
    ];

    /// Views that stay out of the rail until turned on in the Marketplace.
    /// The rest are the core every sidebar can hold.
    pub const EXTENSIONS: &[View] = &[
        View::GitHub,
        View::Notes,
        View::Memory,
        View::Usage,
        View::Cloudflare,
        View::PostHog,
        View::Isometric,
        View::Phone,
        View::Servers,
        View::Automations,
    ];

    pub fn is_extension(self) -> bool {
        View::EXTENSIONS.contains(&self)
    }

    pub fn label(self) -> &'static str {
        match self {
            View::Inbox => "Inbox",
            #[cfg(feature = "coderlabs")]
            View::Tasks => "Tasks",
            View::Runs => "Runs",
            View::Branches => "Branches",
            View::GitHub => "GitHub",
            View::Notes => "Notes",
            View::Memory => "Memory",
            View::Usage => "Usage",
            View::Extensions => "Marketplace",
            View::Cloudflare => "Cloudflare",
            View::PostHog => "PostHog",
            #[cfg(feature = "coderlabs")]
            View::Site => "arakun.dev",
            View::Isometric => "Isometric",
            #[cfg(feature = "coderlabs")]
            View::Canvas => "Canvas",
            View::Phone => "Phone",
            View::Servers => "Servers",
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
            View::GitHub => "github",
            View::Notes => "file01",
            View::Memory => "ai-idea",
            View::Usage => "dashboard-square01",
            View::Extensions => "store01",
            View::Cloudflare => "internet",
            View::PostHog => "chart-breakout-square",
            #[cfg(feature = "coderlabs")]
            View::Site => "globe",
            View::Isometric => "cube",
            #[cfg(feature = "coderlabs")]
            View::Canvas => "artboard",
            View::Phone => "signal-full02",
            View::Servers => "computer",
            View::Automations => "flash",
            #[cfg(feature = "team")]
            View::Team => "ungroup-items",
        }
    }

    /// One line on what the view is, for the Marketplace.
    pub fn blurb(self) -> &'static str {
        match self {
            View::Inbox => "Sessions waiting for you, and past notifications",
            #[cfg(feature = "coderlabs")]
            View::Tasks => "Your task list",
            View::Runs => "CI runs and their logs",
            View::Branches => "Every branch and its PR",
            View::GitHub => "Your review queue on GitHub",
            View::Notes => "Project notes",
            View::Memory => "What agents remember here",
            View::Usage => "Plan limits and spend",
            View::Extensions => "Plugins, MCP servers and skills",
            View::Cloudflare => "Workers, tunnels and DNS",
            View::PostHog => "Product analytics for each repo",
            #[cfg(feature = "coderlabs")]
            View::Site => "Your arakun.dev account",
            View::Isometric => "Animated isometric scenes",
            #[cfg(feature = "coderlabs")]
            View::Canvas => "Design frames, drawn natively",
            View::Phone => "Arakun on your phone",
            View::Servers => "Sessions on your other machines",
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
    /// A card beside the content.
    Card,
    /// No card: the rail is part of the window's background.
    #[default]
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
            variant: Variant::Flush,
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
    /// What is live in every project: sessions that wait on the user or
    /// failed, then those that run.
    Active,
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

    pub fn active() -> Self {
        Piece::Active
    }

    /// One of a kind: placing it again moves it.
    pub fn unique(&self) -> bool {
        !matches!(
            self,
            Piece::Pin { .. } | Piece::Separator | Piece::Title { .. } | Piece::Spacer
        )
    }

    /// Shapes the sidebar rather than taking you anywhere: not counted as
    /// one of a folded segment's pieces.
    pub fn is_decoration(&self) -> bool {
        matches!(
            self,
            Piece::Separator | Piece::Title { .. } | Piece::Spacer | Piece::Mascot
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
            Piece::Active => "Active".into(),
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
            Piece::Active => "clock01",
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
    /// Folded, its title says how many pieces it holds, with a dot when one
    /// of them wants the user.
    #[serde(default = "yes")]
    pub peek: bool,
    #[serde(default)]
    pub line_above: bool,
    /// Not drawn while the rail is narrowed to icons.
    #[serde(default)]
    pub hide_in_icons: bool,
    /// Its pieces as one row of icons, names in their tooltips.
    #[serde(default)]
    pub row: bool,
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
            peek: true,
            line_above: false,
            hide_in_icons: false,
            row: false,
            action: None,
            items: pieces.into_iter().map(Item::new).collect(),
        }
    }

    /// Where Inbox sits right before or after Search: the two then share a
    /// row, Inbox as a small button at the end of the search box. Returns
    /// the index of Search, then of Inbox.
    pub fn inbox_by_search(&self) -> Option<(usize, usize)> {
        let search = self.items.iter().position(|i| i.piece == Piece::Search)?;
        let is_inbox = |i: usize| {
            self.items
                .get(i)
                .is_some_and(|it| it.piece == Piece::view(View::Inbox))
        };
        [search + 1, search.wrapping_sub(1)]
            .into_iter()
            .find(|&i| is_inbox(i))
            .map(|inbox| (search, inbox))
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

    fn in_row(mut self) -> Self {
        self.row = true;
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
    /// The open project and its environments; the views as a row of icons.
    Switcher,
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
    pub const ALL: [Preset; 6] = [
        Preset::Switcher,
        Preset::Focused,
        Preset::Developer,
        Preset::Notion,
        Preset::Compact,
        Preset::Everything,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Switcher => "Switcher",
            Preset::Focused => "Focused",
            Preset::Developer => "Developer",
            Preset::Notion => "Notion",
            Preset::Compact => "Icons",
            Preset::Everything => "Everything",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Preset::Switcher => "One project at a time, its environments, the views as icons",
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
            View::Servers,
            View::Automations,
            #[cfg(feature = "team")]
            View::Team,
        ];
        let projects = Section::new("Projects", Mode::Scroll, vec![Piece::projects()])
            .titled()
            .with_action(TitleAction::AddProject);
        let account = |pieces| Section::new("Account", Mode::Fixed, pieces);
        let active = || Section::new("Active", Mode::Scroll, vec![Piece::active()]);
        let mut style = Style::default();
        let sections = match self {
            Preset::Switcher => vec![
                Section::new(
                    "Top",
                    Mode::Fixed,
                    vec![Piece::Search, Piece::view(View::Inbox)],
                ),
                active(),
                Section::new(
                    "Projects",
                    Mode::Scroll,
                    vec![Piece::Projects {
                        envs: true,
                        changes: true,
                        only_open: true,
                    }],
                ),
                account(vec![Piece::Usage]),
                Section::new(
                    "Views",
                    Mode::Fixed,
                    [
                        views(&[
                            #[cfg(feature = "coderlabs")]
                            View::Tasks,
                            View::Runs,
                            View::Branches,
                            View::GitHub,
                        ]),
                        views(
                            &more
                                .iter()
                                .copied()
                                .filter(|v| *v != View::Extensions)
                                .collect::<Vec<_>>(),
                        ),
                        vec![
                            Piece::view(View::Extensions),
                            Piece::Spacer,
                            Piece::Settings,
                        ],
                    ]
                    .concat(),
                )
                .in_row(),
            ],
            Preset::Focused => vec![
                Section::new(
                    "Top",
                    Mode::Fixed,
                    vec![Piece::Search, Piece::view(View::Inbox)],
                ),
                active(),
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
                                View::GitHub,
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
                active(),
                projects,
                Section::new(
                    "Git",
                    Mode::Scroll,
                    views(&[View::Runs, View::Branches, View::GitHub]),
                )
                .titled(),
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
                active(),
                projects,
                Section::new(
                    "More",
                    Mode::Scroll,
                    views(&[&[View::Runs, View::Branches, View::GitHub][..], &more[1..]].concat()),
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
                            View::GitHub,
                            View::Notes,
                        ]),
                    ),
                    active(),
                    projects,
                    account(vec![Piece::Settings]),
                ]
            }
            // Inbox sits by Search, where it shares the search box's row.
            Preset::Everything => vec![
                Section::new(
                    "Top",
                    Mode::Fixed,
                    vec![Piece::Search, Piece::view(View::Inbox)],
                ),
                Section::new(
                    "Views",
                    Mode::Scroll,
                    views(
                        &View::ALL
                            .iter()
                            .copied()
                            .filter(|v| *v != View::Inbox)
                            .collect::<Vec<_>>(),
                    ),
                ),
                active(),
                projects,
                account(vec![Piece::Usage, Piece::Settings]),
            ],
        };
        Sidebar {
            style,
            sections,
            known: View::ALL.to_vec(),
            offered_active: true,
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
    /// The Active list was put in once. A layout saved before it existed gets
    /// it on load; one where the user took it out keeps it out.
    #[serde(default)]
    pub offered_active: bool,
}

impl Default for Sidebar {
    /// Before anyone arranged it. A layout someone arranged is saved and
    /// keeps its look.
    fn default() -> Self {
        Preset::Switcher.layout()
    }
}

impl Sidebar {
    pub fn load() -> Self {
        Self::load_from(&file())
    }

    fn load_from(path: &std::path::Path) -> Self {
        crate::stored::load_with(path, |b| {
            Self::parse(b).ok_or_else(|| "not a sidebar layout".to_string())
        })
        .unwrap_or_default()
    }

    pub fn save(&self) {
        self.save_to(&file());
    }

    fn save_to(&self, path: &std::path::Path) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = crate::stored::write(path, &bytes);
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
        s.offer_active();
        Some(s)
    }

    #[cfg(test)]
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
        // Born titled and foldable: a group is how a long sidebar keeps
        // room for the projects.
        let s = Section::new(&format!("Segment {n}"), Mode::Scroll, vec![]).folding(false);
        let at = at.min(self.sections.len());
        self.sections.insert(at, s);
        at
    }

    pub fn remove_section(&mut self, s: usize) -> Option<Section> {
        (s < self.sections.len()).then(|| self.sections.remove(s))
    }

    /// Removes the command pins, and returns how many went. A command runs on
    /// a click and the rail shows only its title, so one in a layout someone
    /// else wrote must be added by hand, where its text can be read.
    pub fn drop_commands(&mut self) -> usize {
        let mut gone = 0;
        for s in &mut self.sections {
            let before = s.items.len();
            s.items.retain(|i| {
                !matches!(
                    i.piece,
                    Piece::Pin {
                        pin: Pin::Command { .. }
                    }
                )
            });
            gone += before - s.items.len();
        }
        gone
    }

    /// Removes segment `s` but keeps its pieces: they join the end of the
    /// segment above it, or the start of the one below when it is the
    /// first. False when there is no other segment to take them.
    pub fn ungroup_section(&mut self, s: usize) -> bool {
        if s >= self.sections.len() || self.sections.len() < 2 {
            return false;
        }
        let items = self.sections.remove(s).items;
        if s > 0 {
            self.sections[s - 1].items.extend(items);
        } else {
            self.sections[0].items.splice(0..0, items);
        }
        true
    }

    /// Puts a view the rail does not hold at the end of the segment with the
    /// most views, else of the first scrolling one. False when it is there.
    pub fn place(&mut self, view: View) -> bool {
        let piece = Piece::view(view);
        if self.has(&piece) {
            return false;
        }
        let views = |s: &Section| {
            s.items
                .iter()
                .filter(|i| matches!(i.piece, Piece::View { .. }))
                .count()
        };
        // On a tie the first segment wins: max_by_key would take the last.
        let most = self
            .sections
            .iter()
            .enumerate()
            .filter(|(_, s)| views(s) > 0)
            .min_by_key(|(i, s)| (std::cmp::Reverse(views(s)), *i))
            .map(|(i, _)| i);
        let Some(s) = most.or_else(|| self.sections.iter().position(|s| s.mode == Mode::Scroll))
        else {
            return false;
        };
        let end = self.sections[s].items.len();
        self.insert(Item::new(piece), (s, end))
    }

    /// One of each unique piece.
    /// Puts the Active list in its own segment right above the projects (or
    /// first, with no projects), once per layout.
    fn offer_active(&mut self) {
        if std::mem::replace(&mut self.offered_active, true) || self.has(&Piece::active()) {
            return;
        }
        let at = self
            .find(&Piece::projects())
            .map(|(s, _)| s)
            .unwrap_or(0)
            .min(self.sections.len());
        self.sections.insert(
            at,
            Section::new("Active", Mode::Scroll, vec![Piece::active()]),
        );
    }

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
    near_state("sidebar.json")
}

fn near_state(name: &str) -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name(name),
        None => crate::state::data_dir().join(name),
    }
}

/// The extension views turned on in the Marketplace. Kept apart from the
/// layout, so a template or an undo in the builder leaves them as they are.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Extensions {
    #[serde(default)]
    pub on: Vec<View>,
}

impl Extensions {
    /// The saved choice. The first time, it keeps what `layout` already
    /// shows for someone who used Arakun before, so an update hides nothing,
    /// and starts with none on a new install. The choice is saved at once.
    pub fn load(layout: &Sidebar) -> Self {
        Self::load_from(&extensions_file(), layout, crate::state::exists())
    }

    fn load_from(path: &std::path::Path, layout: &Sidebar, used_before: bool) -> Self {
        let saved = crate::stored::load_with(path, |b| {
            Self::parse(b).ok_or_else(|| "not a list of extensions".to_string())
        });
        if let Some(saved) = saved {
            return saved;
        }
        let first = Self::first(layout, used_before);
        first.save_to(path);
        first
    }

    pub fn save(&self) {
        self.save_to(&extensions_file());
    }

    fn save_to(&self, path: &std::path::Path) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = crate::stored::write(path, &bytes);
        }
    }

    /// Skips views this version does not know.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let on = v
            .get("on")?
            .as_array()?
            .iter()
            .filter_map(|x| serde_json::from_value::<View>(x.clone()).ok())
            .filter(|v| v.is_extension())
            .collect();
        Some(Extensions { on })
    }

    fn first(layout: &Sidebar, used_before: bool) -> Self {
        let on = if used_before {
            View::EXTENSIONS
                .iter()
                .copied()
                .filter(|v| layout.has(&Piece::view(*v)))
                .collect()
        } else {
            Vec::new()
        };
        Extensions { on }
    }

    /// Whether the rail shows `view`: always for the core, and for an
    /// extension once it is on.
    pub fn shows(&self, view: View) -> bool {
        !view.is_extension() || self.on.contains(&view)
    }

    /// Turns an extension on or off. False when nothing changed.
    pub fn set(&mut self, view: View, on: bool) -> bool {
        if !view.is_extension() || self.on.contains(&view) == on {
            return false;
        }
        if on {
            self.on.push(view);
        } else {
            self.on.retain(|v| *v != view);
        }
        true
    }
}

fn extensions_file() -> PathBuf {
    near_state("extensions.json")
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
            offered_active: true,
        }
    }

    #[test]
    fn the_default_holds_every_view() {
        let s = Sidebar::default();
        // An extension turned on in the Marketplace shows without a trip to the builder.
        assert!(View::ALL.iter().all(|v| s.has(&Piece::view(*v))));
        assert!(s.hidden_views().is_empty());
        assert_eq!(s.style, Style::default());
        assert_eq!(
            s.sections.iter().map(|x| x.row).collect::<Vec<_>>(),
            [false, false, false, false, true],
            "the views sit in a row at the bottom"
        );
    }

    #[test]
    fn a_layout_from_before_rows_has_none() {
        let json = br#"{"sections":[{"title":"A","items":[{"type":"settings"}]}]}"#;
        let s = Sidebar::parse(json).unwrap();
        assert!(!s.sections[0].row);
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
    fn a_new_segment_is_titled_and_folds() {
        let mut s = small();
        let at = s.add_section(0);
        let sec = &s.sections[at];
        assert!(sec.show_title && sec.collapsible && sec.peek);
        assert!(!sec.collapsed);
    }

    #[test]
    fn an_imported_layout_loses_its_commands_only() {
        let json = br#"{"sections":[{"title":"A","items":[
            {"type":"pin","pin":{"kind":"command","title":"Build","command":"rm -rf ~"}},
            {"type":"search"},
            {"type":"pin","pin":{"kind":"url","title":"Docs","url":"https://example.com"}}
        ]}],"offered_active":true}"#;
        let mut s = Sidebar::parse(json).unwrap();
        let before = s.sections[0].items.len();
        assert_eq!(s.drop_commands(), 1);
        assert_eq!(s.sections[0].items.len(), before - 1);
        assert!(s.sections[0].items.iter().all(|i| !matches!(
            i.piece,
            Piece::Pin {
                pin: Pin::Command { .. }
            }
        )));
        assert!(s.has(&Piece::Search));
        assert_eq!(s.drop_commands(), 0);
    }

    #[test]
    fn ungrouping_keeps_the_pieces() {
        let mut s = small();
        assert!(s.ungroup_section(1));
        assert_eq!(names(&s), ["A[Search,Inbox,Projects,Runs]", "C[Settings]"]);
        // The first segment's pieces go to the start of the next one.
        assert!(s.ungroup_section(0));
        assert_eq!(names(&s), ["C[Search,Inbox,Projects,Runs,Settings]"]);
        // The last segment has nowhere to put them.
        assert!(!s.ungroup_section(0));
        assert!(!s.ungroup_section(5));
        assert_eq!(s.sections.len(), 1);
    }

    #[test]
    fn a_segment_saved_before_peek_shows_what_is_inside() {
        let json = br#"{"sections":[{"title":"Tools","collapsible":true,"collapsed":true,"items":[{"type":"settings"}]}]}"#;
        let s = Sidebar::parse(json).unwrap();
        let tools = s.sections.iter().find(|x| x.title == "Tools").unwrap();
        assert!(tools.peek);
        let json =
            br#"{"sections":[{"title":"Tools","peek":false,"items":[{"type":"settings"}]}]}"#;
        let s = Sidebar::parse(json).unwrap();
        assert!(!s.sections.iter().find(|x| x.title == "Tools").unwrap().peek);
    }

    #[test]
    fn a_layout_survives_unknown_pieces_and_duplicates() {
        let json = br#"{"style":{"variant":"floating","side":"right","glow":3},"sections":[
            {"title":"A","items":[{"type":"view","view":"inbox"},{"type":"hologram"},{"type":"view","view":"teleporter"}]},
            {"title":"B","mode":"fixed","items":[{"type":"view","view":"inbox","label":"Mail"},{"type":"projects"}]}
        ],"known":["inbox","teleporter"]}"#;
        let s = Sidebar::parse(json).unwrap();
        // A layout from before the Active list gets it once, over the projects.
        assert_eq!(names(&s), ["A[Inbox]", "Active[Active]", "B[Projects]"]);
        assert_eq!(s.style.variant, Variant::Floating);
        assert_eq!(s.style.side, Side::Right);
        assert!(s.style.badges);
        assert!(s.is_new(View::Phone));
        // One word in the file, where snake_case would split it.
        assert_eq!(serde_json::to_string(&View::GitHub).unwrap(), "\"github\"");
    }

    #[test]
    fn the_active_list_is_offered_once() {
        let mut s = small();
        s.offered_active = false;
        let s = Sidebar::parse(s.to_json().as_bytes()).unwrap();
        assert_eq!(
            names(&s),
            [
                "A[Search,Inbox]",
                "Active[Active]",
                "B[Projects,Runs]",
                "C[Settings]"
            ]
        );
        // Taken out, it stays out.
        let mut s = s;
        s.remove((1, 0));
        let s = Sidebar::parse(s.to_json().as_bytes()).unwrap();
        assert!(!s.has(&Piece::active()));
        // An old file that already holds it does not get a second one.
        let json =
            br#"{"sections":[{"title":"A","items":[{"type":"active"},{"type":"projects"}]}]}"#;
        let s = Sidebar::parse(json).unwrap();
        assert_eq!(names(&s), ["A[Active,Projects]"]);
        assert_eq!(s.get((0, 0)).unwrap().piece, Piece::Active);
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

    #[test]
    fn the_core_views_are_not_extensions() {
        for v in [View::Inbox, View::Runs, View::Branches, View::Extensions] {
            assert!(!v.is_extension(), "{}", v.label());
        }
        assert!(View::EXTENSIONS.iter().all(|v| View::ALL.contains(v)));
    }

    #[test]
    fn an_update_keeps_the_extensions_a_rail_had() {
        let mut layout = small();
        layout.insert(Item::new(Piece::view(View::Cloudflare)), (1, 0));
        let before = Extensions::first(&layout, true);
        assert_eq!(before.on, [View::Cloudflare]);
        assert!(before.shows(View::Cloudflare));
        assert!(!before.shows(View::PostHog));
        // The core shows whether or not anything is on.
        assert!(before.shows(View::Runs));

        let new = Extensions::first(&Sidebar::default(), false);
        assert!(new.on.is_empty());
        assert!(View::EXTENSIONS.iter().all(|v| !new.shows(*v)));
        assert!(new.shows(View::Extensions));
    }

    #[test]
    fn extensions_turn_on_and_off_once() {
        let mut e = Extensions::default();
        assert!(e.set(View::PostHog, true));
        assert!(!e.set(View::PostHog, true));
        assert!(!e.set(View::Runs, true), "the core is not an extension");
        assert_eq!(e.on, [View::PostHog]);
        assert!(e.set(View::PostHog, false));
        assert!(e.on.is_empty());
    }

    #[test]
    fn saved_extensions_skip_what_this_version_lacks() {
        let e = Extensions::parse(br#"{"on":["cloudflare","teleporter","runs"]}"#).unwrap();
        assert_eq!(e.on, [View::Cloudflare]);
        assert_eq!(
            Extensions::parse(br#"{"on":[]}"#),
            Some(Extensions::default())
        );
        assert_eq!(Extensions::parse(b"{}"), None);
    }

    #[test]
    fn a_view_turned_on_lands_beside_the_other_views() {
        let mut s = small();
        assert!(s.place(View::Cloudflare));
        // A and B hold one view each; the first of them wins.
        assert_eq!(
            names(&s),
            [
                "A[Search,Inbox,Cloudflare]",
                "B[Projects,Runs]",
                "C[Settings]"
            ]
        );
        assert!(!s.place(View::Cloudflare), "already there");
        // With no view anywhere, it goes to the first scrolling segment.
        let mut bare = Sidebar {
            style: Style::default(),
            sections: vec![
                Section::new("A", Mode::Fixed, vec![Piece::Search]),
                Section::new("B", Mode::Scroll, vec![Piece::projects()]),
            ],
            known: View::ALL.to_vec(),
            offered_active: true,
        };
        assert!(bare.place(View::Notes));
        assert_eq!(names(&bare), ["A[Search]", "B[Projects,Notes]"]);
        let mut none = Sidebar {
            style: Style::default(),
            sections: vec![],
            known: vec![],
            offered_active: true,
        };
        assert!(!none.place(View::Notes));
    }

    #[test]
    fn inbox_joins_search_only_when_next_to_it() {
        let sec = |pieces| Section::new("A", Mode::Fixed, pieces);
        let inbox = || Piece::view(View::Inbox);
        let runs = || Piece::view(View::Runs);
        assert_eq!(
            sec(vec![Piece::Search, inbox()]).inbox_by_search(),
            Some((0, 1))
        );
        assert_eq!(
            sec(vec![runs(), inbox(), Piece::Search]).inbox_by_search(),
            Some((2, 1))
        );
        assert_eq!(
            sec(vec![Piece::Search, runs(), inbox()]).inbox_by_search(),
            None
        );
        assert_eq!(sec(vec![Piece::Search]).inbox_by_search(), None);
        assert_eq!(sec(vec![inbox()]).inbox_by_search(), None);
    }

    #[test]
    fn an_unreadable_layout_is_kept() {
        let dir = crate::stored::tests::scratch("sidebar");
        let path = dir.join("sidebar.json");
        // `sections` is required.
        let edited = r#"{"style":{}}"#;
        std::fs::write(&path, edited).unwrap();
        assert_eq!(Sidebar::load_from(&path), Sidebar::default());
        small().save_to(&path);
        assert_eq!(Sidebar::load_from(&path), small());
        assert_eq!(crate::stored::tests::kept(&path).as_deref(), Some(edited));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_extensions_are_kept_when_the_first_choice_is_saved() {
        let dir = crate::stored::tests::scratch("extensions");
        let path = dir.join("extensions.json");
        std::fs::write(&path, "{}").unwrap();
        let e = Extensions::load_from(&path, &Sidebar::default(), false);
        assert!(e.on.is_empty());
        assert_eq!(Extensions::load_from(&path, &Sidebar::default(), true), e);
        assert_eq!(crate::stored::tests::kept(&path).as_deref(), Some("{}"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
