//! What the palette lists: each row's kind, how it looks, where Enter takes
//! it and what its preview shows. The shell fills these from its state
//! (`shell/palette.rs`); the palette only filters, ranks and draws them.

use std::path::PathBuf;

use gpui::SharedString;

use super::clip::ErrorText;
use crate::ui::Tint;

/// Where Enter takes a row. Everything the old switcher reached is here.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Project(usize),
    Env(usize, usize),
    Session(usize, usize, SharedString),
    NewSession,
    NewEnvironment,
    NewTerminal,
    NewBrowserTab,
    AddProject,
    /// The setup from first launch, again.
    Onboarding,
    ToggleDock,
    ToggleSidebar,
    TogglePanel,
    Customize,
    ToggleSubagents,
    Account,
    Settings,
    Extensions,
    #[cfg(feature = "coderlabs")]
    Tasks,
    Runs,
    Branches,
    GitHub,
    #[cfg(feature = "coderlabs")]
    Canvas,
    /// A branch of the visible project, shown in the Branches tab.
    Branch(String),
    /// A CoderSpace task, by id.
    #[cfg(feature = "coderlabs")]
    Task(String),
    SignIn(crate::agent::AgentKind),
    // New in the palette.
    View(crate::sidebar::View),
    /// A pull request or issue, in the GitHub tab.
    Pull(crate::github::pr::ItemRef),
    /// A to-do: Enter ticks it off, or opens it again.
    Todo(u64),
    /// A run in the Runs tab of the project at `root`.
    Run {
        root: PathBuf,
        id: u64,
    },
    File {
        path: PathBuf,
        line: Option<u32>,
    },
    Url(String),
    Setting(Setting),
    Memory,
    /// Re-run a CI run.
    Rerun {
        root: PathBuf,
        id: u64,
    },
    /// Start a session on a failed run.
    FixRun {
        root: PathBuf,
        id: u64,
    },
    Copy(String),
    /// A new agent session: the prompt, a model id, a branch to work on.
    Ask {
        prompt: String,
        model: Option<String>,
        branch: Option<String>,
    },
    FixError(ErrorText),
    /// Merge, behind the palette's own confirm.
    Merge(crate::github::pr::ItemRef),
    /// "Send to the cloud agent…": the palette's own task form.
    CloudTask,
    /// The form that connects arakun-pilot or changes its settings.
    PilotConnect,
    /// Rows that only show something.
    Nothing,
}

/// Settings the palette turns on and off in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Setting {
    Redacted,
    Island,
    Glass,
    Arcade,
    Dock,
    Sidebar,
    Panel,
    Subagents,
}

/// What a row is; decides its tab, its icon and its actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Session,
    Pull,
    Person,
    Run,
    File,
    Symbol,
    Command,
    View,
    Setting,
    Project,
    Branch,
    Memory,
    Rule,
    /// One of the person's to-dos.
    Todo,
    /// Cards made for this query: clipboard, ask an agent, answer, rule.
    Card,
}

/// The filter tabs, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    All,
    Sessions,
    GitHub,
    Files,
    Commands,
    Memory,
    Settings,
    People,
    Views,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::All => "All",
            Tab::Sessions => "Sessions",
            Tab::GitHub => "GitHub",
            Tab::Files => "Files",
            Tab::Commands => "Commands",
            Tab::Memory => "Memory",
            Tab::Settings => "Settings",
            Tab::People => "People",
            Tab::Views => "Views",
        }
    }

    pub fn icon(self) -> Option<&'static str> {
        match self {
            Tab::All => None,
            Tab::Sessions => Some("message-multiple01"),
            Tab::GitHub => Some("github"),
            Tab::Files => Some("folder01"),
            Tab::Commands => Some("flash"),
            Tab::Memory => Some("bot"),
            Tab::Settings => Some("settings01"),
            Tab::People => Some("user"),
            Tab::Views => Some("dashboard-square01"),
        }
    }
}

/// What sits before a row's title.
#[derive(Clone, Debug, PartialEq)]
pub enum Lead {
    Icon(&'static str, Tint),
    Avatar(String, Option<PathBuf>),
    Working,
    Idle,
    Dot(Tint, bool),
}

/// What the preview pane shows for a row.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Preview {
    #[default]
    None,
    Session {
        place: String,
        agent: String,
        status: crate::model::Status,
        age: String,
        doing: Option<String>,
        added: u32,
        removed: u32,
        files: Option<usize>,
        cost: Option<String>,
        quote: Option<String>,
    },
    Pull {
        repo: String,
        number: u64,
        author: String,
        avatar: Option<PathBuf>,
        waiting: Option<(String, bool)>,
        checks: Option<(usize, usize, Vec<Tint>)>,
        merge: Option<(String, Tint)>,
        changes: Option<(u32, u32, u32)>,
        opened: String,
        diff: Vec<String>,
    },
    Person {
        login: String,
        avatar: Option<PathBuf>,
        for_you: usize,
        you_owe: usize,
        theirs: usize,
    },
    Run {
        title: String,
        place: String,
        state: String,
        tint: Tint,
        ago: String,
        duration: Option<String>,
        commit: String,
        log: Vec<String>,
    },
    File {
        path: PathBuf,
        rel: String,
        line: Option<u32>,
    },
    Command {
        blurb: String,
        keys: Vec<&'static str>,
        cli: Option<String>,
    },
    View {
        blurb: String,
        stats: Vec<(String, String)>,
    },
    Setting {
        path: String,
        blurb: String,
        on: bool,
    },
    Project {
        path: String,
        branch: String,
        envs: usize,
        sessions: usize,
    },
    Branch {
        name: String,
        detail: String,
    },
    Memory {
        scope: String,
        id: String,
        kind: String,
        body: String,
        files: Vec<String>,
        age: String,
        hits: u32,
    },
    Error(ErrorText),
}

/// One row.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Stable across opens, for frecency and pins: `session:<id>`.
    pub key: String,
    pub kind: Kind,
    pub title: SharedString,
    /// Quieter text after the title.
    pub sub: SharedString,
    /// Small mono text at the end: `12m`, `L1496`.
    pub meta: SharedString,
    /// A shortcut shown at the end, as key caps.
    pub keys: Vec<&'static str>,
    pub pill: Option<(SharedString, Tint)>,
    pub lead: Lead,
    /// Its group in the All tab when nothing is typed or as a section label.
    pub section: &'static str,
    /// More text to search in besides the title and sub.
    pub extra: String,
    pub target: Target,
    pub preview: Preview,
    /// On or off, for settings.
    pub toggle: Option<bool>,
    /// Shown dimmed (finished or old).
    pub dim: bool,
    /// Added to its score: what needs the person comes first.
    pub boost: i32,
    /// Names a `#` search can narrow to, lowercase: the pull request's
    /// repository (`arakunhq/arakun`, `arakun`), its owner and its project.
    pub tags: Vec<String>,
}

impl Item {
    pub fn new(
        key: impl Into<String>,
        kind: Kind,
        title: impl Into<SharedString>,
        target: Target,
    ) -> Self {
        Item {
            key: key.into(),
            kind,
            title: title.into(),
            sub: SharedString::default(),
            meta: SharedString::default(),
            keys: Vec::new(),
            pill: None,
            lead: Lead::Icon("search01", Tint::Neutral),
            section: "",
            extra: String::new(),
            target,
            preview: Preview::None,
            toggle: None,
            dim: false,
            boost: 0,
            tags: Vec::new(),
        }
    }

    /// The repository (`owner/name`) and project a pull request belongs to, so
    /// `#arakun 86` or `#app` narrows to them.
    pub fn repo_tags(mut self, repo: &str, project: Option<&str>) -> Self {
        let repo = repo.to_ascii_lowercase();
        if let Some((owner, name)) = repo.split_once('/') {
            self.tags.extend([owner.to_string(), name.to_string()]);
        }
        self.tags.push(repo);
        self.tags.extend(project.map(str::to_ascii_lowercase));
        self
    }

    pub fn sub(mut self, s: impl Into<SharedString>) -> Self {
        self.sub = s.into();
        self
    }

    pub fn meta(mut self, s: impl Into<SharedString>) -> Self {
        self.meta = s.into();
        self
    }

    pub fn lead(mut self, l: Lead) -> Self {
        self.lead = l;
        self
    }

    pub fn icon(self, name: &'static str, tint: Tint) -> Self {
        self.lead(Lead::Icon(name, tint))
    }

    pub fn section(mut self, s: &'static str) -> Self {
        self.section = s;
        self
    }

    pub fn keys(mut self, k: &[&'static str]) -> Self {
        self.keys = k.to_vec();
        self
    }

    pub fn preview(mut self, p: Preview) -> Self {
        self.preview = p;
        self
    }

    pub fn extra(mut self, e: impl Into<String>) -> Self {
        self.extra = e.into();
        self
    }

    pub fn boost(mut self, b: i32) -> Self {
        self.boost = b;
        self
    }

    pub fn pill(mut self, text: impl Into<SharedString>, tint: Tint) -> Self {
        self.pill = Some((text.into(), tint));
        self
    }

    /// The tabs this row counts in.
    pub fn tab(&self) -> Tab {
        match self.kind {
            Kind::Session => Tab::Sessions,
            Kind::Pull | Kind::Run => Tab::GitHub,
            Kind::Person => Tab::People,
            Kind::File | Kind::Symbol => Tab::Files,
            Kind::Command => Tab::Commands,
            Kind::Memory => Tab::Memory,
            Kind::Setting => Tab::Settings,
            Kind::View | Kind::Project | Kind::Branch => Tab::Views,
            Kind::Rule | Kind::Todo | Kind::Card => Tab::All,
        }
    }
}
