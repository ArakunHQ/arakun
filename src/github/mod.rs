//! GitHub notifications of the signed-in user: the inbox behind the GitHub tab.
//! Read and marked read through the `gh` CLI and its own session, so there is
//! no token to keep. `GitHub` polls them and says which ones are new; the alert
//! rule and the watch list (`github.json` next to arakun's state) decide which
//! of those deserve a notice.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::pin;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::future::{Either, select};
use gpui::{AppContext as _, Context, EventEmitter, Task};
use serde::{Deserialize, Serialize};

use crate::git;

pub mod avatars;
pub mod board;
pub mod pilot;
pub mod pr;
pub mod pulse;
pub mod queue;

/// How many notifications one read asks for.
const PER_PAGE: usize = 50;
/// The least time between two inbox reads Arakun starts on its own, while
/// the window is active. GitHub's `X-Poll-Interval` can ask for more, never less.
const POLL_ACTIVE: Duration = Duration::from_secs(60);
const POLL_IDLE: Duration = Duration::from_secs(300);
/// How many pull requests or issues are read at once for the queue.
const DETAILS_AT_ONCE: usize = 3;
/// How far back a mention still counts.
const MENTIONS_DAYS: i32 = 30;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Notification {
    /// The thread's id: what marking it read needs.
    pub id: String,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub reason: String,
    /// ISO 8601, UTC. Changes when the thread gets new activity.
    #[serde(default)]
    pub updated_at: String,
    pub subject: Subject,
    pub repository: Repository,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Subject {
    #[serde(default)]
    pub title: String,
    /// The API address of the pull request, issue or commit. Missing for check
    /// suites and discussions.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub latest_comment_url: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Repository {
    pub full_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub owner: Owner,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Owner {
    #[serde(default)]
    pub login: String,
}

/// What a notification is about, as far as its icon and its wording go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    PullRequest,
    Issue,
    Ci,
    Release,
    Commit,
    Discussion,
    Other,
}

impl Kind {
    pub fn icon(self) -> &'static str {
        match self {
            Kind::PullRequest => "git-pull-request",
            Kind::Issue => "circle-dot",
            Kind::Ci => "activity01",
            Kind::Release => "tag01",
            Kind::Commit => "git-commit",
            Kind::Discussion => "comment01",
            Kind::Other => "circle",
        }
    }

    /// The thing in a sentence: "the pull request …".
    pub fn noun(self) -> &'static str {
        match self {
            Kind::PullRequest => "pull request",
            Kind::Issue => "issue",
            Kind::Ci => "CI run",
            Kind::Release => "release",
            Kind::Commit => "commit",
            Kind::Discussion => "discussion",
            Kind::Other => "notification",
        }
    }
}

impl Notification {
    pub fn kind(&self) -> Kind {
        match self.subject.kind.as_str() {
            "PullRequest" => Kind::PullRequest,
            "Issue" => Kind::Issue,
            "CheckSuite" | "WorkflowRun" => Kind::Ci,
            "Release" => Kind::Release,
            "Commit" => Kind::Commit,
            "Discussion" => Kind::Discussion,
            _ => Kind::Other,
        }
    }

    /// Unix seconds of the last activity (0 when GitHub's date can't be read).
    pub fn updated(&self) -> u64 {
        crate::ci::parse_time(&self.updated_at).unwrap_or(0)
    }

    /// The page to open for this notification. The API only gives its own
    /// address for the subject, so the web one is built from it; a subject with
    /// no address, or one of a kind not known here, opens the closest page of
    /// the repository.
    pub fn web_url(&self) -> String {
        let repo = &self.repository;
        // The repository's own address also works on GitHub Enterprise hosts.
        let base = if repo.html_url.is_empty() {
            format!("https://github.com/{}", repo.full_name)
        } else {
            repo.html_url.trim_end_matches('/').to_string()
        };
        let subject = self.subject.url.as_deref().and_then(|url| {
            let mut parts = url.split_once("/repos/")?.1.split('/').skip(2);
            let (what, id) = (parts.next()?, parts.next()?);
            if id.is_empty() || parts.next().is_some() {
                return None;
            }
            let page = match what {
                "pulls" => "pull",
                "issues" => "issues",
                "commits" => "commit",
                "discussions" => "discussions",
                // A release's API id isn't its tag, so its own page can't be named.
                _ => return None,
            };
            Some(format!("{base}/{page}/{id}"))
        });
        subject.unwrap_or_else(|| match self.subject.kind.as_str() {
            "PullRequest" => format!("{base}/pulls"),
            "Issue" => format!("{base}/issues"),
            "Release" => format!("{base}/releases"),
            "Discussion" => format!("{base}/discussions"),
            "CheckSuite" | "WorkflowRun" => format!("{base}/actions"),
            "Commit" => format!("{base}/commits"),
            "RepositoryVulnerabilityAlert" | "RepositoryDependabotAlertsThread" => {
                format!("{base}/security")
            }
            _ => base,
        })
    }
}

/// Reasons that mean someone asked for the user by name.
const DIRECT: [&str; 5] = [
    "review_requested",
    "mention",
    "team_mention",
    "assign",
    "security_alert",
];

pub fn direct(reason: &str) -> bool {
    DIRECT.contains(&reason)
}

/// What an island notice about a notification says under its title:
/// `arakun #61 · rik asked for your review`. `who` is the item's author when
/// known, empty otherwise.
pub fn notice_line(n: &Notification, number: Option<u64>, who: &str) -> String {
    let what = match n.kind() {
        Kind::PullRequest => "pull request",
        Kind::Issue => "issue",
        Kind::Ci => "workflow run",
        Kind::Release => "release",
        Kind::Commit => "commit",
        Kind::Discussion => "discussion",
        Kind::Other => "thread",
    };
    let by = |verb: &str, alone: &str| {
        if who.is_empty() {
            alone.to_string()
        } else {
            format!("{who} {verb}")
        }
    };
    let why = match n.reason.as_str() {
        "review_requested" => by("asked for your review", "Your review was requested"),
        "mention" | "team_mention" => by("mentioned you", "You were mentioned"),
        "assign" => "Assigned to you".to_string(),
        "author" => format!("New activity on your {what}"),
        "comment" => "New reply in a thread you're in".to_string(),
        "ci_activity" => "A workflow run you started finished".to_string(),
        "approval_requested" => "A deployment waits for your approval".to_string(),
        other => {
            let label = reason_label(other);
            let mut c = label.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    };
    let repo = n
        .repository
        .full_name
        .rsplit('/')
        .next()
        .unwrap_or_default();
    match number {
        Some(num) => format!("{repo} #{num} · {why}"),
        None => format!("{repo} · {why}"),
    }
}

/// The reason as a badge: "review requested", "mentioned", "assigned".
pub fn reason_label(reason: &str) -> String {
    match reason {
        "review_requested" => "review requested",
        "mention" => "mentioned",
        "team_mention" => "team mentioned",
        "assign" => "assigned",
        "security_alert" => "security alert",
        "author" => "your thread",
        "comment" => "you commented",
        "state_change" => "state changed",
        "subscribed" => "watching",
        "manual" => "subscribed",
        "ci_activity" => "CI activity",
        "approval_requested" => "approval requested",
        "invitation" => "invited",
        "member_feature_requested" => "feature requested",
        other => return other.replace('_', " "),
    }
    .to_string()
}

/// The reason as the end of "It is in my inbox because …".
fn reason_sentence(reason: &str) -> String {
    match reason {
        "review_requested" => "my review was requested",
        "mention" => "I was mentioned",
        "team_mention" => "a team I am on was mentioned",
        "assign" => "it was assigned to me",
        "security_alert" => "GitHub found a security problem in the repository",
        "author" => "I opened it",
        "comment" => "I commented on it",
        "state_change" => "I changed its state",
        "subscribed" => "I watch the repository",
        "manual" => "I subscribed to it",
        "ci_activity" => "a workflow run I started finished",
        "approval_requested" => "a deployment waits for my approval",
        other => return format!("of the reason GitHub calls `{other}`"),
    }
    .to_string()
}

/// Whether `repo` is on the watch list: an entry `owner` covers everything of
/// that owner, an entry `owner/repo` that repository alone.
pub fn watched(repo: &Repository, watch: &[String]) -> bool {
    watch.iter().any(|entry| {
        if entry.contains('/') {
            entry.eq_ignore_ascii_case(&repo.full_name)
        } else {
            entry.eq_ignore_ascii_case(&repo.owner.login)
        }
    })
}

/// The ids of `list` inside the focus; threads hidden by it stay unread.
fn unread_ids_in_focus(list: &[Notification], focus: &[String]) -> Vec<String> {
    list.iter()
        .filter(|n| in_focus(&n.repository.full_name, focus))
        .map(|n| n.id.clone())
        .collect()
}

/// Whether `repo` (`owner/name`) is inside the focus: an entry `owner` covers
/// everything of that owner, `owner/repo` that repository alone. An empty
/// focus covers everything.
pub fn in_focus(repo: &str, focus: &[String]) -> bool {
    let owner = repo.split('/').next().unwrap_or_default();
    focus.is_empty()
        || focus.iter().any(|entry| {
            if entry.contains('/') {
                entry.eq_ignore_ascii_case(repo)
            } else {
                entry.eq_ignore_ascii_case(owner)
            }
        })
}

/// Names with how many queue items each has.
pub type Counts = Vec<(String, usize)>;

/// The owners and repositories of `repos`, each with how many items it has:
/// what the focus menu offers. Owners first, then repositories, by name.
pub fn scopes<'a>(repos: impl IntoIterator<Item = &'a str>) -> (Counts, Counts) {
    let mut owners: Vec<(String, usize)> = Vec::new();
    let mut names: Vec<(String, usize)> = Vec::new();
    let bump = |list: &mut Vec<(String, usize)>, key: &str| match list
        .iter_mut()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
    {
        Some((_, n)) => *n += 1,
        None => list.push((key.to_string(), 1)),
    };
    for repo in repos {
        if !repo.contains('/') {
            continue;
        }
        bump(&mut owners, repo.split('/').next().unwrap_or_default());
        bump(&mut names, repo);
    }
    owners.sort_by_key(|(k, _)| k.to_ascii_lowercase());
    names.sort_by_key(|(k, _)| k.to_ascii_lowercase());
    (owners, names)
}

/// Whether a new notification deserves a notice: someone asked for the user by
/// name, wherever it is, or it comes from a place the user chose to watch.
/// Everything else waits in the inbox.
pub fn alerts(n: &Notification, watch: &[String]) -> bool {
    direct(&n.reason) || watched(&n.repository, watch)
}

/// What the user typed as a watch-list entry, cleaned up: `owner` or
/// `owner/repo`. A pasted GitHub address works too.
pub fn watch_entry(text: &str) -> Option<String> {
    let t = text.trim();
    let t = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(t);
    let t = t
        .strip_prefix("github.com/")
        .unwrap_or(t)
        .trim_start_matches('@')
        .trim_matches('/');
    let parts: Vec<&str> = t.split('/').collect();
    let ok = |p: &&str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    ((1..=2).contains(&parts.len()) && parts.iter().all(ok)).then(|| t.to_string())
}

/// The session title for a notification handed to an agent.
pub fn ask_title(n: &Notification) -> String {
    let what = match n.kind() {
        Kind::PullRequest => "PR",
        Kind::Issue => "Issue",
        Kind::Ci => "CI",
        Kind::Release => "Release",
        Kind::Commit => "Commit",
        Kind::Discussion => "Discussion",
        Kind::Other => "GitHub",
    };
    format!("{what}: {}", n.subject.title)
}

/// The prompt that hands a notification to an agent: read it, say what it
/// needs, and touch nothing on GitHub without asking.
pub fn ask_prompt(n: &Notification) -> String {
    let kind = n.kind();
    let url = n.web_url();
    let mut p = format!(
        "A GitHub notification needs a look: the {} \"{}\" in {}.\nIt is in my inbox because {}.\nURL: {url}\n\n",
        kind.noun(),
        n.subject.title,
        n.repository.full_name,
        reason_sentence(&n.reason),
    );
    p.push_str("Read it with `gh` first: the description, the diff, the comments and the checks");
    match kind {
        Kind::PullRequest => p.push_str(&format!(
            " (`gh pr view {url} --comments`, `gh pr diff {url}`, `gh pr checks {url}`)"
        )),
        Kind::Issue => p.push_str(&format!(" (`gh issue view {url} --comments`)")),
        _ => p.push_str(&format!(
            " (`gh api` or `gh browse` with `-R {}`)",
            n.repository.full_name
        )),
    }
    p.push_str(
        ". Use the URL: this checkout may be another repository.\n\nThen tell me what it is about and what it needs from me.\n\nDon't post comments, approve, request changes, merge or close anything without asking me first.",
    );
    p
}

// ---------------------------------------------------------------------------
// Settings

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Owners (`arakunhq`) and repositories (`owner/repo`) whose every new
    /// notification gets a notice.
    #[serde(default)]
    pub watch: Vec<String>,
    /// Owners and repositories the tab shows, in the same form; empty shows
    /// everything. What is left out doesn't show, count or alert.
    #[serde(default)]
    pub focus: Vec<String>,
    /// The GitHub tab was opened at least once: until then nothing polls gh.
    #[serde(default)]
    pub opened: bool,
}

fn file() -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name("github.json"),
        None => crate::state::data_dir().join("github.json"),
    }
}

pub fn load() -> Settings {
    std::fs::read(file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn store(settings: &Settings) {
    let path = file();
    if let Err(e) = crate::durable::write(
        &path,
        &serde_json::to_vec_pretty(settings).unwrap_or_default(),
    ) {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
}

// ---------------------------------------------------------------------------
// gh

/// Why the inbox can't be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The `gh` CLI isn't installed.
    NoGh,
    /// `gh` has no session.
    SignedOut,
    Other(String),
}

impl Problem {
    pub fn text(&self) -> String {
        match self {
            Problem::NoGh => "GitHub CLI (gh) is not installed".into(),
            Problem::SignedOut => "Sign in to GitHub: run `gh auth login` in a terminal.".into(),
            Problem::Other(e) => e.clone(),
        }
    }
}

/// Whether to read GitHub in the background. Most people never open the
/// GitHub tab, and starting gh every minute for them costs processes, battery
/// and API quota for nothing; so the reads start once the tab was opened.
/// Without gh, or signed out, the reads would only fail again: they wait
/// until the tab is opened again, which reads once and resumes them on success.
fn polls(opened: bool, problem: Option<&Problem>) -> bool {
    opened && !matches!(problem, Some(Problem::NoGh | Problem::SignedOut))
}

/// What `gh` wrote on a failed call, as a problem.
fn problem(stderr: &str) -> Problem {
    let err = git::strip_ansi(stderr);
    let err = err.trim();
    if err.to_lowercase().contains("gh auth login") {
        Problem::SignedOut
    } else if err.is_empty() {
        Problem::Other("gh failed".into())
    } else {
        Problem::Other(err.to_string())
    }
}

/// An open pull request of a repository, whoever it involves: what Ctrl+K
/// lists under `#` besides the user's own queue.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoPull {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub author: RepoPullAuthor,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub review_decision: String,
    #[serde(default)]
    pub head_ref_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct RepoPullAuthor {
    #[serde(default)]
    pub login: String,
}

/// The JSON `gh pr list --json …` prints.
pub fn parse_repo_pulls(json: &[u8]) -> Result<Vec<RepoPull>, Problem> {
    serde_json::from_slice(json).map_err(|e| Problem::Other(format!("unexpected gh output: {e}")))
}

/// How long a repository's open pull requests are reused before reading them again.
const REPO_PULLS_FRESH: Duration = Duration::from_secs(60);

fn gh_out(args: &[&str]) -> Result<Vec<u8>, Problem> {
    // These calls name no repository, so gh runs away from any checkout: a
    // folder's remotes must not change which host it talks to.
    let out = git::gh(&std::env::temp_dir())
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Problem::NoGh
            } else {
                Problem::Other(format!("could not run gh: {e}"))
            }
        })?;
    if !out.status.success() {
        return Err(problem(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(out.stdout)
}

/// The newest notifications: the unread ones, or with `all` the read ones too.
/// Also how long GitHub asks clients to wait before the next read, when it says.
pub fn list(all: bool) -> Result<(Vec<Notification>, Option<Duration>), Problem> {
    let mut path = format!("notifications?per_page={PER_PAGE}");
    if all {
        path.push_str("&all=true");
    }
    let out = gh_out(&["api", "--include", &path])?;
    let (interval, body) = split_response(&out);
    let list = serde_json::from_slice(body)
        .map_err(|e| Problem::Other(format!("unexpected answer from gh: {e}")))?;
    Ok((list, interval))
}

/// Splits what `gh api --include` prints into the `X-Poll-Interval` header
/// and the body. Output without headers is all body.
fn split_response(out: &[u8]) -> (Option<Duration>, &[u8]) {
    if !out.starts_with(b"HTTP/") {
        return (None, out);
    }
    // gh ends each header line with CRLF; take a bare LF too.
    let blank = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| out.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)));
    let Some((at, len)) = blank else {
        return (None, out);
    };
    let interval = String::from_utf8_lossy(&out[..at])
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("x-poll-interval") {
                return None;
            }
            value.trim().parse().ok().map(Duration::from_secs)
        });
    (interval, &out[at + len..])
}

/// How long to wait between automatic inbox reads: what GitHub asks for, but
/// never less than `POLL_ACTIVE`, or `POLL_IDLE` when the window is inactive.
fn poll_gap(asked: Option<Duration>, window_active: bool) -> Duration {
    let floor = if window_active {
        POLL_ACTIVE
    } else {
        POLL_IDLE
    };
    asked.map_or(floor, |a| a.max(floor))
}

/// How long the poll sleeps: until `gap` has passed since the last read
/// started, or a whole `gap` when it has already (a read is running, or none
/// is allowed), so the loop never spins.
fn poll_wait(since_start: Option<Duration>, gap: Duration) -> Duration {
    match since_start {
        Some(e) if e < gap => gap - e,
        _ => gap,
    }
}

/// Whether an automatic read may start: none is running, and the last one
/// started at least `gap` ago.
fn read_due(busy: bool, since_start: Option<Duration>, gap: Duration) -> bool {
    !busy && since_start.is_none_or(|e| e >= gap)
}

/// Sleeps until `sleep` ends or `wake` says the poll gap changed, so a wait
/// counted with the old gap is computed again. False once `wake` is closed.
async fn sleep_or_wake(sleep: impl Future, wake: &mut mpsc::UnboundedReceiver<()>) -> bool {
    match select(pin!(sleep), wake.next()).await {
        Either::Left(_) => true,
        Either::Right((woke, _)) => woke.is_some(),
    }
}

/// Puts back as unread the threads in `marked` whose mark didn't reach
/// GitHub. One a read brought back meanwhile (with new activity) isn't doubled,
/// and one marked again at a newer version stays read: that mark is the current one.
fn restore_unread(
    unread: &mut Vec<Notification>,
    all: &mut [Notification],
    read_marks: &mut HashMap<String, String>,
    marked: Vec<Notification>,
) {
    for n in marked {
        if read_marks.get(&n.id).is_some_and(|at| *at != n.updated_at) {
            continue;
        }
        if read_marks.get(&n.id) == Some(&n.updated_at) {
            read_marks.remove(&n.id);
        }
        for a in all.iter_mut() {
            if a.id == n.id && a.updated_at == n.updated_at {
                a.unread = true;
            }
        }
        if !unread.iter().any(|u| u.id == n.id) {
            unread.push(n);
        }
    }
    // GitHub lists the newest first; ISO 8601 dates sort as text.
    unread.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
}

/// The ids of the notifications in `list` about the item `key` (`queue::key_of`).
fn notifications_of(list: &[Notification], key: &str) -> Vec<String> {
    list.iter()
        .filter(|n| pr::ItemRef::from_notification(n).is_some_and(|i| queue::key_of(&i) == key))
        .map(|n| n.id.clone())
        .collect()
}

pub fn mark_thread_read(id: &str) -> Result<(), Problem> {
    // A test build shares the person's real gh session, and GitHub's API cannot
    // mark a thread unread again: there the change stays in memory.
    if read_only() {
        return Ok(());
    }
    gh_out(&["api", "-X", "PATCH", &format!("notifications/threads/{id}")]).map(|_| ())
}

/// `ARAKUN_GITHUB=readonly`: notifications are read but never marked read on
/// GitHub. `arakun dev` sets it for the test Arakun.
pub fn read_only() -> bool {
    std::env::var("ARAKUN_GITHUB").is_ok_and(|v| v == "readonly")
}

// ---------------------------------------------------------------------------
// The command line

pub const USAGE: &str = "arakun github draft <owner/repo>#<n> --verdict <approve|request-changes|comment> --body-file <file|-> [--head <sha>] [--agent <name>]
    save a review as a draft for the GitHub tab; nothing is sent to GitHub.
    `--body-file -` reads the review from stdin.
    `--head` is the commit you read (`gh pr view <url> --json headRefOid`); an approval needs it.";

/// `arakun github …`. Only `draft`: it writes the draft file and needs no
/// running Arakun.
pub fn cli(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("draft") => match save_draft_cli(&args[1..]) {
            Ok(msg) => {
                println!("{msg}");
                0
            }
            Err(e) => {
                eprintln!("arakun github draft: {e}");
                1
            }
        },
        Some("help" | "-h" | "--help") | None => {
            println!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("arakun github: unknown command `{other}`\n{USAGE}");
            2
        }
    }
}

fn save_draft_cli(args: &[String]) -> Result<String, String> {
    let parsed = pr::parse_draft_args(args)?;
    // The text is read here, in the caller's process and so inside its
    // sandbox; the running app never opens a path a caller names.
    let body = if parsed.body_file.as_os_str() == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .map_err(|e| format!("could not read stdin: {e}"))?;
        text
    } else {
        std::fs::read_to_string(&parsed.body_file)
            .map_err(|e| format!("{}: {e}", parsed.body_file.display()))?
    };
    if body.trim().is_empty() && parsed.verdict != pr::Verdict::Approve {
        return Err("the review is empty; only an approval may say nothing".into());
    }
    // The draft says which commit it was written for, so the tab can warn
    // when the pull request moved on, and approves only that commit.
    let head = parsed.reviewed_head(|| {
        pr::head_sha(&parsed.item)
            .map_err(|p| format!("could not read {}: {}", parsed.item.label(), p.text()))
    })?;
    let draft = parsed.draft(body, head, crate::model::unix_now());
    pr::save_draft(&draft).map_err(|e| format!("could not save the draft: {e}"))?;
    Ok(format!(
        "Saved a draft {} for {} (written for {}). Nothing was sent to GitHub: the user posts it from the GitHub tab.",
        parsed.verdict.name(),
        parsed.item.label(),
        pr::short_sha(&draft.head_sha),
    ))
}

/// Whether to read an item's details: not yet asked at this date, or the last
/// read failed. Once per inbox read at most (`tried_since_poll`): a failure
/// isn't retried in a loop while offline, and new activity on an item waits
/// for the next poll like the inbox does.
fn ask_details(asked: bool, failed: bool, tried_since_poll: bool) -> bool {
    !tried_since_poll && (!asked || failed)
}

// ---------------------------------------------------------------------------
// The model

/// What earlier reads showed, to tell which notifications are new.
#[derive(Default)]
struct Seen {
    /// Thread id → its `updated_at` when last listed.
    at: HashMap<String, String>,
    /// The newest `updated_at` of any read.
    newest: String,
}

/// The unread notifications that are new since the last read: a thread not
/// listed before, or one with new activity. None on the first read, which only
/// shows what was already waiting.
fn news(seen: &mut Option<Seen>, unread: &[Notification]) -> Vec<Notification> {
    let out = match seen.as_ref() {
        None => Vec::new(),
        Some(s) => unread
            .iter()
            .filter(|n| s.at.get(&n.id) != Some(&n.updated_at))
            // Marking one read lets an older one into the page; that isn't news.
            .filter(|n| n.updated_at >= s.newest)
            .cloned()
            .collect(),
    };
    let s = seen.get_or_insert_with(Seen::default);
    for n in unread {
        s.at.insert(n.id.clone(), n.updated_at.clone());
        if n.updated_at > s.newest {
            s.newest = n.updated_at.clone();
        }
    }
    out
}

pub enum GitHubEvent {
    /// Unread notifications that weren't there at the last read.
    New(Vec<Notification>),
}

pub struct GitHub {
    unread: Vec<Notification>,
    /// Read and unread, while the "All" filter is shown.
    all: Vec<Notification>,
    all_loaded: bool,
    show_all: bool,
    loaded: bool,
    busy: bool,
    /// The user asked for a read while one was going; another follows it.
    again: bool,
    problem: Option<Problem>,
    /// Why the last "mark read" didn't reach GitHub.
    mark_problem: Option<Problem>,
    /// When the last read started: the gap between reads counts from here.
    last_start: Option<Instant>,
    /// How long GitHub asked to wait between reads (`X-Poll-Interval`).
    poll_interval: Option<Duration>,
    seen: Option<Seen>,
    /// Threads marked read here (id → `updated_at`): a read that started before
    /// GitHub knew would bring them back.
    read_marks: HashMap<String, String>,
    settings: Settings,
    window_active: bool,
    /// The login gh is signed in as, once known.
    me: Option<String>,
    /// A read of `me` alone is on its way (`want_me`).
    me_reading: bool,
    /// What the searches found at the last read.
    found: Vec<(queue::Why, queue::Found)>,
    searched: bool,
    search_problem: Option<Problem>,
    /// Pull requests and issues read for the queue and the item view, by `queue::key_of`.
    details: HashMap<String, queue::Details>,
    /// Every open pull request of the projects' repositories, by `owner/name`.
    repo_pulls: HashMap<String, Vec<RepoPull>>,
    /// When each repository's open pull requests were last asked for, read or
    /// not: a repository that always fails isn't read on every palette open.
    repo_pulls_started: HashMap<String, Instant>,
    /// Repositories whose open pull requests are being read.
    repo_pulls_busy: HashSet<String>,
    /// The entry's `updated_at` when its details were last asked for, so a
    /// date that never matches the details' own doesn't read it again and again.
    asked_at: HashMap<String, String>,
    fetching: HashSet<String>,
    detail_problems: HashMap<String, Problem>,
    /// When an item's last read started: it is read once per inbox read at most.
    tried_at: HashMap<String, Instant>,
    queue: queue::Queue,
    /// Profile pictures by lower-case login.
    avatars: HashMap<String, Avatar>,
    avatars_busy: bool,
    /// Tells the poll the gap changed (the window's activity, `X-Poll-Interval`).
    poll_wake: mpsc::UnboundedSender<()>,
    /// People shown elsewhere in the tab (the agent's cards).
    extra_people: Vec<String>,
    _poll: Task<()>,
}

/// Where a profile picture is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Avatar {
    /// Asked for; an old copy may already show.
    Pending(Option<PathBuf>),
    Ready(PathBuf),
    /// Not to be asked again this run: the initials show.
    Failed,
}

impl EventEmitter<GitHubEvent> for GitHub {}

impl GitHub {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (poll_wake, mut woken) = mpsc::unbounded();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let polled = this.update(cx, |this, cx| {
                    if this.polls() {
                        this.refresh_if_stale(cx);
                    }
                });
                if polled.is_err() {
                    break;
                }
                // Sleeps until the next read is due, counted from the last
                // one, so a Refresh in between doesn't make a tick miss. A
                // shorter gap (the window comes back) wakes it to count again.
                let Ok(wait) = this.read_with(cx, |this, _| {
                    poll_wait(this.last_start.map(|t| t.elapsed()), this.poll_gap())
                }) else {
                    break;
                };
                let timer = cx.background_executor().timer(wait);
                if !sleep_or_wake(timer, &mut woken).await {
                    break;
                }
            }
        });
        Self {
            unread: Vec::new(),
            all: Vec::new(),
            all_loaded: false,
            show_all: false,
            loaded: false,
            busy: false,
            again: false,
            problem: None,
            mark_problem: None,
            last_start: None,
            poll_interval: None,
            seen: None,
            read_marks: HashMap::new(),
            settings: load(),
            window_active: true,
            me: None,
            me_reading: false,
            found: Vec::new(),
            searched: false,
            search_problem: None,
            details: HashMap::new(),
            repo_pulls: HashMap::new(),
            repo_pulls_started: HashMap::new(),
            repo_pulls_busy: HashSet::new(),
            asked_at: HashMap::new(),
            fetching: HashSet::new(),
            detail_problems: HashMap::new(),
            tried_at: HashMap::new(),
            queue: queue::Queue::default(),
            avatars: HashMap::new(),
            avatars_busy: false,
            poll_wake,
            extra_people: Vec::new(),
            _poll: poll,
        }
    }

    /// Whether the background reads run: the timer and the window coming back.
    fn polls(&self) -> bool {
        polls(self.settings.opened, self.problem.as_ref())
    }

    /// The GitHub tab is in front: from now on the inbox is kept fresh.
    pub fn opened(&mut self, cx: &mut Context<Self>) {
        if !self.settings.opened {
            self.settings.opened = true;
            store(&self.settings);
        }
        self.refresh_if_stale(cx);
    }

    fn poll_gap(&self) -> Duration {
        poll_gap(self.poll_interval, self.window_active)
    }

    /// The poll gap changed: the pending wait is counted again.
    fn wake_poll(&self) {
        let _ = self.poll_wake.unbounded_send(());
    }

    /// `ARAKUN_GITHUB=off`: nothing is read (tests, screenshots of a dev build).
    pub fn disabled() -> bool {
        std::env::var("ARAKUN_GITHUB").is_ok_and(|v| v == "off")
    }

    /// The unread threads inside the focus: what "Mark all read" marks.
    pub fn unread_in_focus(&self) -> Vec<String> {
        unread_ids_in_focus(&self.unread, &self.settings.focus)
    }

    pub fn find(&self, id: &str) -> Option<&Notification> {
        self.unread.iter().chain(&self.all).find(|n| n.id == id)
    }

    /// Whether any unread notification is one the alert rule cares about.
    pub fn wants_attention(&self) -> bool {
        self.unread.iter().any(|n| {
            alerts(n, &self.settings.watch)
                && in_focus(&n.repository.full_name, &self.settings.focus)
        })
    }

    /// Whether the list for this filter was read at least once.
    pub fn loaded(&self, all: bool) -> bool {
        if all { self.all_loaded } else { self.loaded }
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Why the inbox can't be read, or why the last mark didn't go through.
    pub fn problem(&self) -> Option<&Problem> {
        self.problem.as_ref().or(self.mark_problem.as_ref())
    }

    pub fn watch(&self) -> &[String] {
        &self.settings.watch
    }

    /// Adds an owner or `owner/repo` to the watch list; false when the text is
    /// neither, or is already there.
    pub fn add_watch(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(entry) = watch_entry(text) else {
            return false;
        };
        if self
            .settings
            .watch
            .iter()
            .any(|w| w.eq_ignore_ascii_case(&entry))
        {
            return false;
        }
        self.settings.watch.push(entry);
        store(&self.settings);
        cx.notify();
        true
    }

    pub fn remove_watch(&mut self, entry: &str, cx: &mut Context<Self>) {
        self.settings.watch.retain(|w| w != entry);
        store(&self.settings);
        cx.notify();
    }

    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        let was = self.window_active;
        self.window_active = active;
        if active != was {
            self.wake_poll();
        }
        if active && !was && self.polls() {
            self.refresh_if_stale(cx);
            // Views read what may have changed on disk meanwhile (a review draft).
            cx.notify();
        }
    }

    /// A read Arakun starts on its own: only when the last one started at
    /// least the poll gap ago (GitHub's `X-Poll-Interval`), and not while one
    /// runs, since that one is fresh.
    fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        // The gap for the window's state now: coming back to it is already
        // active, and a poll counted while active waits the idle gap once
        // the window is left.
        let since_start = self.last_start.map(|t| t.elapsed());
        if read_due(self.busy, since_start, self.poll_gap()) {
            self.refresh(cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if Self::disabled() {
            return;
        }
        if self.busy {
            self.again = true;
            return;
        }
        self.busy = true;
        self.last_start = Some(Instant::now());
        cx.notify();
        let want_all = self.show_all;
        let now = crate::model::unix_now();
        let today = queue::day_key(queue::day_start(now, 0));
        let since = queue::day_key(queue::day_start(now, MENTIONS_DAYS));
        cx.spawn(async move |this, cx| {
            let (unread, all, (found, failed, search_problem), me) = cx
                .background_spawn(async move {
                    std::thread::scope(|s| {
                        let searches = s.spawn(|| queue::search_all(&today, &since));
                        let me = s.spawn(|| pr::me().ok());
                        let unread = list(false);
                        let all = (want_all && unread.is_ok())
                            .then(|| list(true).ok().map(|(all, _)| all))
                            .flatten();
                        let found = searches.join().unwrap_or_else(|_| {
                            (
                                Vec::new(),
                                queue::Why::ALL.to_vec(),
                                Some(Problem::Other("the searches failed".into())),
                            )
                        });
                        (unread, all, found, me.join().ok().flatten())
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if me.is_some() {
                    this.me = me;
                }
                // A failed search keeps what it found last time; the others update.
                this.found = queue::merge_found(std::mem::take(&mut this.found), found, &failed);
                this.searched = true;
                this.search_problem = search_problem;
                match unread {
                    Ok((raw, interval)) => {
                        this.problem = None;
                        if interval.is_some() && interval != this.poll_interval {
                            this.poll_interval = interval;
                            this.wake_poll();
                        }
                        this.loaded = true;
                        let fresh = this.take_unread(raw);
                        if let Some(all) = all.filter(|_| this.show_all) {
                            this.all = all;
                            this.all_loaded = true;
                            this.settle_all();
                        }
                        if !fresh.is_empty() {
                            cx.emit(GitHubEvent::New(fresh));
                        }
                    }
                    Err(p) => this.problem = Some(p),
                }
                this.rebuild();
                this.fetch_details(cx);
                this.fetch_avatars(cx);
                cx.notify();
                // Only a person sets it: they asked, so it reads at once.
                if std::mem::take(&mut this.again) {
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    // -----------------------------------------------------------------------
    // The review queue

    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    /// Reads the login gh is signed in as, when it isn't known yet. For a
    /// sheet opened from the island before the tab ever was: it needs to know
    /// which pull requests are the user's. Unlike `opened`, it turns on no
    /// background reads.
    pub fn want_me(&mut self, cx: &mut Context<Self>) {
        if self.me.is_some() || self.me_reading || Self::disabled() {
            return;
        }
        self.me_reading = true;
        cx.spawn(async move |this, cx| {
            let me = cx.background_spawn(async { pr::me().ok() }).await;
            let _ = this.update(cx, |this, cx| {
                this.me_reading = false;
                if this.me.is_none() && me.is_some() {
                    this.me = me;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn queue(&self) -> &queue::Queue {
        &self.queue
    }

    /// Whether the searches ran at least once.
    pub fn searched(&self) -> bool {
        self.searched
    }

    pub fn search_problem(&self) -> Option<&Problem> {
        self.search_problem.as_ref()
    }

    /// How many things wait on the user: the rail badge.
    pub fn on_you(&self) -> usize {
        self.queue.on_you
    }

    /// Reads the open pull requests of `repos` (`owner/name`) that weren't
    /// asked for in the last minute, in the background.
    pub fn refresh_repo_pulls(&mut self, repos: Vec<String>, cx: &mut Context<Self>) {
        if Self::disabled() {
            return;
        }
        for repo in repos {
            let fresh = self
                .repo_pulls_started
                .get(&repo)
                .is_some_and(|at| at.elapsed() < REPO_PULLS_FRESH);
            if fresh || !self.repo_pulls_busy.insert(repo.clone()) {
                continue;
            }
            self.repo_pulls_started.insert(repo.clone(), Instant::now());
            cx.spawn(async move |this, cx| {
                let r = repo.clone();
                let read = cx
                    .background_spawn(async move {
                        let json = gh_out(&[
                            "pr",
                            "list",
                            "-R",
                            &r,
                            "--state",
                            "open",
                            "--limit",
                            "100",
                            "--json",
                            "number,title,author,isDraft,updatedAt,reviewDecision,headRefName",
                        ])?;
                        parse_repo_pulls(&json)
                    })
                    .await;
                let _ = this.update(cx, |g, cx| {
                    g.repo_pulls_busy.remove(&repo);
                    if let Ok(list) = read {
                        g.repo_pulls.insert(repo, list);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Every open pull request read for a repository, with its `owner/name`.
    pub fn repo_pulls(&self) -> impl Iterator<Item = (&str, &RepoPull)> {
        self.repo_pulls
            .iter()
            .flat_map(|(repo, list)| list.iter().map(move |p| (repo.as_str(), p)))
    }

    /// Every pull request or issue read in full, with its reference: the
    /// queue's and any asked for by number.
    pub fn all_details(&self) -> impl Iterator<Item = (pr::ItemRef, &queue::Details)> {
        self.details.iter().filter_map(|(key, d)| {
            let (repo, n) = key.rsplit_once('#')?;
            let kind = match d {
                queue::Details::Pull(_) => pr::ItemKind::Pull,
                queue::Details::Issue(_) => pr::ItemKind::Issue,
            };
            Some((
                pr::ItemRef {
                    repo: repo.to_string(),
                    number: n.parse().ok()?,
                    kind,
                },
                d,
            ))
        })
    }

    pub fn details(&self, item: &pr::ItemRef) -> Option<&queue::Details> {
        self.details.get(&queue::key_of(item))
    }

    pub fn detail_problem(&self, item: &pr::ItemRef) -> Option<&Problem> {
        self.detail_problems.get(&queue::key_of(item))
    }

    pub fn fetching(&self, item: &pr::ItemRef) -> bool {
        self.fetching.contains(&queue::key_of(item))
    }

    pub fn focus(&self) -> &[String] {
        &self.settings.focus
    }

    /// Every owner and repository with something in the queue, focus or not.
    pub fn scopes(&self) -> (Counts, Counts) {
        let entries = queue::gather(&self.unread, &self.found);
        let repos: Vec<String> = entries.iter().map(queue::Entry::repo).collect();
        scopes(repos.iter().map(String::as_str))
    }

    /// Adds `entry` to the focus, or takes it out when it is there. `None`
    /// clears it: everything shows again.
    pub fn toggle_focus(&mut self, entry: Option<&str>, cx: &mut Context<Self>) {
        match entry {
            None => self.settings.focus.clear(),
            Some(e)
                if self
                    .settings
                    .focus
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(e)) =>
            {
                self.settings.focus.retain(|f| !f.eq_ignore_ascii_case(e));
            }
            Some(e) => self.settings.focus.push(e.to_string()),
        }
        store(&self.settings);
        self.rebuild();
        self.fetch_details(cx);
        cx.notify();
    }

    /// An island notice for a notification: its title, the line under it and
    /// a picture (the author's, or the owner's when the author isn't known
    /// yet), when one is on disk.
    pub fn notice(&self, n: &Notification) -> (String, String, Option<std::path::PathBuf>) {
        let item = pr::ItemRef::from_notification(n);
        let key = item.as_ref().map(queue::key_of);
        let who = self
            .queue
            .rows
            .iter()
            .find(|(e, _)| {
                e.notification.as_ref().is_some_and(|m| m.id == n.id)
                    || key.as_ref().is_some_and(|k| *k == e.key())
            })
            .map(|(e, p)| {
                if p.author.is_empty() {
                    e.author.clone()
                } else {
                    p.author.clone()
                }
            })
            .unwrap_or_default();
        let image = [who.as_str(), n.repository.owner.login.as_str()]
            .into_iter()
            .filter(|l| !l.is_empty())
            .find_map(|l| self.avatar(l))
            .map(std::path::Path::to_path_buf);
        (
            n.subject.title.clone(),
            notice_line(n, item.map(|i| i.number), &who),
            image,
        )
    }

    /// A login's profile picture on disk, once there is one.
    pub fn avatar(&self, login: &str) -> Option<&std::path::Path> {
        match self.avatars.get(&login.to_ascii_lowercase())? {
            Avatar::Ready(p) | Avatar::Pending(Some(p)) => Some(p),
            _ => None,
        }
    }

    /// Pictures for people the tab shows outside the queue.
    pub fn want_people(&mut self, people: Vec<String>, cx: &mut Context<Self>) {
        if people != self.extra_people {
            self.extra_people = people;
            self.fetch_avatars(cx);
        }
    }

    /// Everyone the tab can show: the user, the queue's authors, the people
    /// of every item read, and the agent's cards and board.
    fn shown_people(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        out.extend(self.me.clone());
        out.extend(self.extra_people.iter().cloned());
        // Owners too: an island notice shows the organization's picture when
        // the author isn't known yet.
        out.extend(self.unread.iter().map(|n| n.repository.owner.login.clone()));
        out.extend(self.queue.rows.iter().map(|(e, p)| {
            if p.author.is_empty() {
                e.author.clone()
            } else {
                p.author.clone()
            }
        }));
        for d in self.details.values() {
            out.extend(avatars::people(d));
        }
        out
    }

    /// Downloads, one batch at a time, the pictures not on disk yet or older
    /// than a week. What is on disk shows meanwhile.
    fn fetch_avatars(&mut self, cx: &mut Context<Self>) {
        if self.avatars_busy {
            return;
        }
        let dir = avatars::dir();
        let mut want: Vec<String> = Vec::new();
        for login in self.shown_people() {
            let key = login.to_ascii_lowercase();
            if key.is_empty() || self.avatars.contains_key(&key) || want.contains(&key) {
                continue;
            }
            match avatars::cached(&dir, &key) {
                Some((path, false)) => {
                    self.avatars.insert(key, Avatar::Ready(path));
                }
                Some((path, true)) => {
                    self.avatars
                        .insert(key.clone(), Avatar::Pending(Some(path)));
                    want.push(key);
                }
                None => {
                    self.avatars.insert(key.clone(), Avatar::Pending(None));
                    want.push(key);
                }
            }
        }
        if want.is_empty() {
            return;
        }
        self.avatars_busy = true;
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(async move {
                    // A few at once: a queue's worth of people arrives in a second or two.
                    std::thread::scope(|s| {
                        let jobs: Vec<_> = want
                            .chunks(want.len().div_ceil(4))
                            .map(|part| {
                                let dir = &dir;
                                s.spawn(move || {
                                    part.iter()
                                        .map(|l| (l.clone(), avatars::download(dir, l).ok()))
                                        .collect::<Vec<_>>()
                                })
                            })
                            .collect();
                        jobs.into_iter()
                            .flat_map(|j| j.join().unwrap_or_default())
                            .collect::<Vec<_>>()
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.avatars_busy = false;
                for (login, path) in got {
                    let state = match (path, this.avatars.get(&login)) {
                        (Some(p), _) => Avatar::Ready(p),
                        // A failed refresh keeps the old picture.
                        (None, Some(Avatar::Pending(Some(old)))) => Avatar::Ready(old.clone()),
                        (None, _) => Avatar::Failed,
                    };
                    this.avatars.insert(login, state);
                }
                cx.notify();
                // People that showed up while this batch ran.
                this.fetch_avatars(cx);
            });
        })
        .detach();
    }

    /// Lays the queue out again from what is known, and notes an inbox-zero day.
    fn rebuild(&mut self) {
        let mut entries = queue::gather(&self.unread, &self.found);
        entries.retain(|e| in_focus(&e.repo(), &self.settings.focus));
        let now = crate::model::unix_now();
        let me = self.me.clone().unwrap_or_default();
        self.queue = queue::build(&entries, &self.details, &me, queue::day_start(now, 0));
    }

    /// Reads, a few at a time, the pull requests and issues of the queue whose
    /// details are missing or older than what the inbox or a search says.
    fn fetch_details(&mut self, cx: &mut Context<Self>) {
        let mut entries = queue::gather(&self.unread, &self.found);
        entries.retain(|e| in_focus(&e.repo(), &self.settings.focus));
        for e in entries {
            if self.fetching.len() >= DETAILS_AT_ONCE {
                break;
            }
            let Some(item) = e.item.clone() else { continue };
            let key = queue::key_of(&item);
            let asked = self.asked_at.get(&key) == Some(&e.updated_at);
            let failed = self.detail_problems.contains_key(&key);
            // Read since this inbox read started; before any, every read counts.
            let tried_since_poll = self
                .tried_at
                .get(&key)
                .is_some_and(|&t| self.last_start.is_none_or(|s| t >= s));
            if self.fetching.contains(&key) || !ask_details(asked, failed, tried_since_poll) {
                continue;
            }
            if self
                .details
                .get(&key)
                .is_some_and(|d| d.updated_at() >= e.updated_at.as_str())
            {
                continue;
            }
            self.asked_at.insert(key, e.updated_at.clone());
            self.fetch_item(item, cx);
        }
    }

    /// Reads one pull request or issue now, whatever is known of it.
    pub fn fetch_item(&mut self, item: pr::ItemRef, cx: &mut Context<Self>) {
        if Self::disabled() {
            return;
        }
        let key = queue::key_of(&item);
        if !self.fetching.insert(key.clone()) {
            return;
        }
        self.tried_at.insert(key.clone(), Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    match item.kind {
                        pr::ItemKind::Pull => {
                            pr::fetch_pull(&item).map(|p| queue::Details::Pull(Box::new(p)))
                        }
                        pr::ItemKind::Issue => {
                            pr::fetch_issue(&item).map(|i| queue::Details::Issue(Box::new(i)))
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.fetching.remove(&key);
                match read {
                    Ok(d) => {
                        this.detail_problems.remove(&key);
                        let merged = matches!(&d, queue::Details::Pull(p) if p.merged());
                        this.details.insert(key.clone(), d);
                        if merged {
                            this.settle_merged(&key, cx);
                        }
                    }
                    Err(p) => {
                        // Not retried here, which would loop while the
                        // network is down; the next poll asks again.
                        this.detail_problems.insert(key, p);
                    }
                }
                this.rebuild();
                this.fetch_details(cx);
                this.fetch_avatars(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Keeps what a read brought, minus what was marked read meanwhile, and
    /// returns the new ones.
    fn take_unread(&mut self, raw: Vec<Notification>) -> Vec<Notification> {
        // GitHub no longer lists it, or it has new activity: the mark did its job.
        self.read_marks
            .retain(|id, at| raw.iter().any(|n| &n.id == id && &n.updated_at == at));
        let list: Vec<Notification> = raw
            .into_iter()
            .filter(|n| n.unread && self.read_marks.get(&n.id) != Some(&n.updated_at))
            .collect();
        let fresh = news(&mut self.seen, &list);
        self.unread = list;
        fresh
    }

    /// Shows as read, in the "All" list, what was marked read here.
    fn settle_all(&mut self) {
        for n in &mut self.all {
            if self.read_marks.get(&n.id) == Some(&n.updated_at) {
                n.unread = false;
            }
        }
    }

    /// Takes the threads out of the unread list now and returns them; GitHub
    /// is told after.
    fn mark_locally(&mut self, ids: &[String]) -> Vec<Notification> {
        let mut marked = Vec::new();
        for id in ids {
            if let Some(n) = self.unread.iter().find(|n| &n.id == id) {
                self.read_marks.insert(n.id.clone(), n.updated_at.clone());
                marked.push(n.clone());
            }
        }
        self.unread.retain(|n| !marked.iter().any(|m| m.id == n.id));
        self.settle_all();
        self.rebuild();
        marked
    }

    /// Marks read the unread notifications of a merged pull request (`key`,
    /// as `queue::key_of`), when settling after a merge is on.
    pub fn settle_merged(&mut self, key: &str, cx: &mut Context<Self>) {
        if !crate::settle::on() {
            return;
        }
        let ids = notifications_of(&self.unread, key);
        self.mark_all_read(ids, cx);
    }

    pub fn mark_read(&mut self, id: &str, cx: &mut Context<Self>) {
        self.mark_all_read(vec![id.to_string()], cx);
    }

    /// Marks each of `ids` read here and on GitHub. The inbox is read again
    /// only if the poll gap allows; the marks already show.
    pub fn mark_all_read(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let marked = self.mark_locally(&ids);
        if marked.is_empty() {
            return;
        }
        let ids: Vec<String> = marked.iter().map(|n| n.id.clone()).collect();
        self.mark_problem = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let failed: Vec<(String, Problem)> = cx
                .background_spawn(async move {
                    ids.into_iter()
                        .filter_map(|id| mark_thread_read(&id).err().map(|e| (id, e)))
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Not read after all: shown again now, since the next inbox
                // read may be a poll gap away.
                let back: Vec<Notification> = marked
                    .into_iter()
                    .filter(|n| failed.iter().any(|(id, _)| *id == n.id))
                    .collect();
                if !back.is_empty() {
                    restore_unread(&mut this.unread, &mut this.all, &mut this.read_marks, back);
                    this.rebuild();
                }
                this.mark_problem = failed.into_iter().next().map(|(_, p)| p);
                this.refresh_if_stale(cx);
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_is_read_at_most_once_per_poll() {
        assert!(ask_details(false, false, false));
        assert!(!ask_details(true, false, false));
        // A failure isn't retried at once, which would loop while offline,
        // but on the next poll, whatever time the failure took.
        assert!(!ask_details(true, true, true));
        assert!(ask_details(true, true, false));
        // New activity on an item read this poll waits for the next one.
        assert!(!ask_details(false, false, true));
    }

    #[test]
    fn the_poll_obeys_github_s_poll_interval() {
        let s = Duration::from_secs;
        assert_eq!(poll_gap(None, true), POLL_ACTIVE);
        assert_eq!(poll_gap(None, false), POLL_IDLE);
        assert_eq!(poll_gap(Some(s(120)), true), s(120));
        // Never more often than before, whatever GitHub says.
        assert_eq!(poll_gap(Some(s(10)), true), POLL_ACTIVE);
        assert_eq!(poll_gap(Some(s(120)), false), POLL_IDLE);
    }

    #[test]
    fn the_poll_sleeps_until_the_next_read_is_due() {
        let s = Duration::from_secs;
        // A Refresh at 100 s moves the next read to 160 s, not 200 s.
        assert_eq!(poll_wait(Some(s(0)), s(60)), s(60));
        assert_eq!(poll_wait(Some(s(20)), s(60)), s(40));
        // Due, or a read still running: a whole gap, never a spin.
        assert_eq!(poll_wait(Some(s(60)), s(60)), s(60));
        assert_eq!(poll_wait(Some(s(90)), s(60)), s(60));
        assert_eq!(poll_wait(None, s(60)), s(60));
    }

    #[test]
    fn a_pending_poll_wakes_when_the_gap_changes() {
        let (tx, mut rx) = mpsc::unbounded();
        // A long idle wait that hasn't ended: the wake ends it.
        tx.unbounded_send(()).unwrap();
        assert!(futures::executor::block_on(sleep_or_wake(
            futures::future::pending::<()>(),
            &mut rx
        )));
        // Counted again with the active gap, the read is due at 60 s, not
        // at the end of the 300 s idle wait it was in.
        let s = Duration::from_secs;
        assert_eq!(poll_wait(Some(s(45)), poll_gap(None, false)), s(255));
        assert_eq!(poll_wait(Some(s(45)), poll_gap(None, true)), s(15));
        // The timer ending still ends the wait.
        assert!(futures::executor::block_on(sleep_or_wake(
            futures::future::ready(()),
            &mut rx
        )));
        drop(tx);
        assert!(!futures::executor::block_on(sleep_or_wake(
            futures::future::pending::<()>(),
            &mut rx
        )));
    }

    #[test]
    fn a_poll_counted_while_active_waits_the_idle_gap_once_left() {
        let s = Duration::from_secs;
        // Started 70 s ago while active; the window was left since.
        assert!(!read_due(false, Some(s(70)), poll_gap(None, false)));
        assert!(read_due(false, Some(s(300)), poll_gap(None, false)));
        assert!(read_due(false, Some(s(70)), poll_gap(None, true)));
        assert!(read_due(false, None, poll_gap(None, false)));
        assert!(!read_due(true, None, poll_gap(None, true)));
    }

    #[test]
    fn a_merged_pull_requests_notifications_are_found_by_its_key() {
        let api = |n: u64| format!("https://api.github.com/repos/O/R/pulls/{n}");
        let list = [
            note(
                "1",
                "PullRequest",
                "author",
                Some(&api(7)),
                "O/R",
                "2025-01-01T00:00:00Z",
            ),
            note(
                "2",
                "PullRequest",
                "comment",
                Some(&api(8)),
                "O/R",
                "2025-01-01T00:00:00Z",
            ),
            note(
                "3",
                "PullRequest",
                "comment",
                Some(&api(7)),
                "O/R",
                "2025-01-02T00:00:00Z",
            ),
            note(
                "4",
                "CheckSuite",
                "ci_activity",
                None,
                "O/R",
                "2025-01-02T00:00:00Z",
            ),
        ];
        assert_eq!(notifications_of(&list, "o/r#7"), ["1", "3"]);
        assert!(notifications_of(&list, "o/r#9").is_empty());
    }

    #[test]
    fn a_failed_mark_shows_the_thread_unread_again() {
        let a = note(
            "1",
            "PullRequest",
            "review_requested",
            None,
            "o/r",
            "2025-01-03T00:00:00Z",
        );
        let b = note("2", "Issue", "mention", None, "o/r", "2025-01-02T00:00:00Z");
        let c = note("3", "Issue", "mention", None, "o/r", "2025-01-01T00:00:00Z");
        // `b` and `c` were marked read here; only `b`'s mark failed.
        let mut unread = vec![a.clone()];
        let mut all = [a.clone(), b.clone(), c.clone()];
        for n in &mut all[1..] {
            n.unread = false;
        }
        let mut marks = HashMap::from([
            (b.id.clone(), b.updated_at.clone()),
            (c.id.clone(), c.updated_at.clone()),
        ]);
        restore_unread(&mut unread, &mut all, &mut marks, vec![b.clone()]);
        assert_eq!(unread, vec![a.clone(), b.clone()]);
        assert!(all[1].unread && !all[2].unread);
        assert_eq!(marks.keys().collect::<Vec<_>>(), vec![&c.id]);

        // A read brought it back with new activity meanwhile: not doubled.
        let newer = note("2", "Issue", "mention", None, "o/r", "2025-01-04T00:00:00Z");
        let mut unread = vec![newer.clone(), a.clone()];
        let mut marks = HashMap::new();
        restore_unread(&mut unread, &mut [], &mut marks, vec![b.clone()]);
        assert_eq!(unread, vec![newer.clone(), a.clone()]);

        // The newer version was marked read too, and that mark held: the
        // older mark failing doesn't bring the thread back.
        let mut unread = vec![a.clone()];
        let mut all = [a.clone(), newer.clone()];
        all[1].unread = false;
        let mut marks = HashMap::from([(newer.id.clone(), newer.updated_at.clone())]);
        restore_unread(&mut unread, &mut all, &mut marks, vec![b]);
        assert_eq!(unread, vec![a]);
        assert!(!all[1].unread);
        assert_eq!(marks.get(&newer.id), Some(&newer.updated_at));
    }

    #[test]
    fn reads_github_s_poll_interval_from_the_headers() {
        let out =
            b"HTTP/2.0 200 OK\r\nContent-Type: application/json\r\nX-Poll-Interval: 60\r\n\r\n[]";
        assert_eq!(
            split_response(out),
            (Some(Duration::from_secs(60)), &b"[]"[..])
        );
        let out = b"HTTP/1.1 200 OK\nx-poll-interval: 90\n\n[1]";
        assert_eq!(
            split_response(out),
            (Some(Duration::from_secs(90)), &b"[1]"[..])
        );
        let out = b"HTTP/2.0 200 OK\r\nEtag: x\r\n\r\n[]";
        assert_eq!(split_response(out), (None, &b"[]"[..]));
        // Without headers, all of it is the body.
        assert_eq!(split_response(b"[]"), (None, &b"[]"[..]));
    }

    #[test]
    fn polls_only_after_the_tab_was_opened_and_gh_works() {
        assert!(!polls(false, None));
        assert!(polls(true, None));
        assert!(!polls(true, Some(&Problem::NoGh)));
        assert!(!polls(true, Some(&Problem::SignedOut)));
        // A passing failure (the network) doesn't stop the reads.
        assert!(polls(true, Some(&Problem::Other("timeout".into()))));
        // Settings written before the flag existed read as never opened.
        let old: Settings = serde_json::from_str(r#"{"watch": ["arakunhq"]}"#).unwrap();
        assert!(!old.opened);
    }

    #[test]
    fn a_repositorys_open_pull_requests_are_read() {
        let json = br#"[{"number":86,"title":"fix(palette): answer through Claude Code","author":{"login":"KarloAldrete","is_bot":false},"isDraft":false,"updatedAt":"2026-11-15T03:00:00Z","reviewDecision":"REVIEW_REQUIRED","headRefName":"pr/palette-oauth"},{"number":61,"title":"ci: pin","author":{"login":"rikvisser-dev"}}]"#;
        let list = parse_repo_pulls(json).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].number, list[0].author.login.as_str()),
            (86, "KarloAldrete")
        );
        assert_eq!(list[1].review_decision, "");
        assert!(parse_repo_pulls(b"not json").is_err());
    }

    #[test]
    fn focus_keeps_owners_and_repositories() {
        assert!(in_focus("rikvisser-dev/x", &[]));
        let focus = vec!["ArakunHQ".to_string(), "KarloAldrete/hpo".to_string()];
        assert!(in_focus("arakunhq/arakun", &focus));
        assert!(in_focus("karloaldrete/HPO", &focus));
        assert!(!in_focus("KarloAldrete/other", &focus));
        assert!(!in_focus("arakunhq-fork/arakun", &focus));
        let (owners, repos) = scopes(["b/x", "a/y", "b/x", "b/z", ""]);
        assert_eq!(owners, vec![("a".to_string(), 1), ("b".to_string(), 3)]);
        assert_eq!(
            repos,
            vec![
                ("a/y".to_string(), 1),
                ("b/x".to_string(), 2),
                ("b/z".to_string(), 1)
            ]
        );
    }

    fn note(
        id: &str,
        kind: &str,
        reason: &str,
        url: Option<&str>,
        repo: &str,
        at: &str,
    ) -> Notification {
        Notification {
            id: id.into(),
            unread: true,
            reason: reason.into(),
            updated_at: at.into(),
            subject: Subject {
                title: "ci: run only the jobs a change can affect".into(),
                url: url.map(str::to_string),
                latest_comment_url: None,
                kind: kind.into(),
            },
            repository: Repository {
                full_name: repo.into(),
                html_url: format!("https://github.com/{repo}"),
                owner: Owner {
                    login: repo.split('/').next().unwrap_or_default().into(),
                },
            },
        }
    }

    #[test]
    fn mark_all_read_keeps_to_the_focus() {
        let list = [
            note("1", "PullRequest", "mention", None, "ArakunHQ/arakun", AT),
            note("2", "PullRequest", "mention", None, "other/repo", AT),
        ];
        assert_eq!(unread_ids_in_focus(&list, &[]), ["1", "2"]);
        assert_eq!(unread_ids_in_focus(&list, &["arakunhq".to_string()]), ["1"]);
    }

    #[test]
    fn notices_say_who_wants_what() {
        let n = note(
            "1",
            "PullRequest",
            "review_requested",
            None,
            "ArakunHQ/arakun",
            AT,
        );
        assert_eq!(
            notice_line(&n, Some(61), "rik"),
            "arakun #61 · rik asked for your review"
        );
        assert_eq!(
            notice_line(&n, Some(61), ""),
            "arakun #61 · Your review was requested"
        );
        let n = note("2", "Issue", "author", None, "ArakunHQ/arakun", AT);
        assert_eq!(
            notice_line(&n, None, "rik"),
            "arakun · New activity on your issue"
        );
        let n = note("3", "PullRequest", "state_change", None, "a/b", AT);
        assert_eq!(notice_line(&n, Some(2), ""), "b #2 · State changed");
    }

    const AT: &str = "2026-11-14T21:20:45Z";

    #[test]
    fn parses_what_gh_returns() {
        let json = r#"[{"id":"27219075342","unread":true,"reason":"review_requested","updated_at":"2026-11-14T21:20:45Z",
            "last_read_at":null,"subject":{"title":"ci: run only the jobs a change can affect",
            "url":"https://api.github.com/repos/ArakunHQ/arakun/pulls/39",
            "latest_comment_url":"https://api.github.com/repos/ArakunHQ/arakun/pulls/39","type":"PullRequest"},
            "repository":{"id":1458074519,"name":"arakun","full_name":"ArakunHQ/arakun","private":false,
            "owner":{"login":"ArakunHQ","id":356445173},"html_url":"https://github.com/ArakunHQ/arakun"}},
            {"id":"2","unread":false,"reason":"ci_activity","updated_at":"2026-11-14T20:00:00Z",
            "subject":{"title":"CI failed","url":null,"latest_comment_url":null,"type":"CheckSuite"},
            "repository":{"full_name":"k/app","html_url":"https://github.com/k/app","owner":{"login":"k"}}}]"#;
        let list: Vec<Notification> = serde_json::from_str(json).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].kind(), Kind::PullRequest);
        assert_eq!(list[0].repository.owner.login, "ArakunHQ");
        assert_eq!(
            list[0].web_url(),
            "https://github.com/ArakunHQ/arakun/pull/39"
        );
        assert_eq!(
            list[0].updated(),
            crate::ci::parse_time("2026-11-14T21:20:45Z").unwrap()
        );
        assert_eq!(
            (
                list[1].kind(),
                list[1].unread,
                list[1].subject.url.as_deref()
            ),
            (Kind::Ci, false, None)
        );
        assert_eq!(list[1].web_url(), "https://github.com/k/app/actions");
    }

    #[test]
    fn a_notification_opens_its_web_page() {
        let url = |kind, api: Option<&str>| note("1", kind, "subscribed", api, "O/R", AT).web_url();
        assert_eq!(
            url(
                "PullRequest",
                Some("https://api.github.com/repos/O/R/pulls/12")
            ),
            "https://github.com/O/R/pull/12"
        );
        assert_eq!(
            url("Issue", Some("https://api.github.com/repos/O/R/issues/7")),
            "https://github.com/O/R/issues/7"
        );
        assert_eq!(
            url(
                "Commit",
                Some("https://api.github.com/repos/O/R/commits/2af8316")
            ),
            "https://github.com/O/R/commit/2af8316"
        );
        // A release's API id is not its tag; the list of releases is as close as it gets.
        assert_eq!(
            url(
                "Release",
                Some("https://api.github.com/repos/O/R/releases/9911")
            ),
            "https://github.com/O/R/releases"
        );
        assert_eq!(
            url("Discussion", None),
            "https://github.com/O/R/discussions"
        );
        assert_eq!(url("CheckSuite", None), "https://github.com/O/R/actions");
        assert_eq!(url("RepositoryInvitation", None), "https://github.com/O/R");
        assert_eq!(url("PullRequest", None), "https://github.com/O/R/pulls");
        assert_eq!(
            url(
                "Mystery",
                Some("https://api.github.com/repos/O/R/mysteries/3")
            ),
            "https://github.com/O/R"
        );
        assert_eq!(
            url("Issue", Some("not an address")),
            "https://github.com/O/R/issues"
        );

        // GitHub Enterprise: the page lives where the repository does.
        let mut n = note(
            "1",
            "PullRequest",
            "mention",
            Some("https://ghe.corp/api/v3/repos/O/R/pulls/4"),
            "O/R",
            AT,
        );
        n.repository.html_url = "https://ghe.corp/O/R".into();
        assert_eq!(n.web_url(), "https://ghe.corp/O/R/pull/4");
        n.repository.html_url.clear();
        assert_eq!(n.web_url(), "https://github.com/O/R/pull/4");
    }

    #[test]
    fn alerts_for_direct_reasons_and_watched_places() {
        let n = |reason, repo| note("1", "PullRequest", reason, None, repo, AT);
        for reason in [
            "review_requested",
            "mention",
            "team_mention",
            "assign",
            "security_alert",
        ] {
            assert!(alerts(&n(reason, "stranger/repo"), &[]), "{reason}");
        }
        for reason in [
            "subscribed",
            "author",
            "comment",
            "state_change",
            "ci_activity",
            "manual",
        ] {
            assert!(!alerts(&n(reason, "stranger/repo"), &[]), "{reason}");
        }
        let watch = ["arakunhq".to_string(), "Karlo/Dotfiles".to_string()];
        assert!(alerts(&n("subscribed", "ArakunHQ/arakun"), &watch));
        assert!(alerts(&n("comment", "karlo/dotfiles"), &watch));
        // An owner entry is the whole owner, never a prefix; a repository entry is that one only.
        assert!(!alerts(&n("subscribed", "arakunhq-labs/arakun"), &watch));
        assert!(!alerts(&n("subscribed", "karlo/other"), &watch));
        assert!(!alerts(&n("subscribed", "other/arakunhq"), &watch));
        assert!(alerts(&n("mention", "karlo/other"), &watch));
    }

    #[test]
    fn watch_entries_are_owners_or_repositories() {
        assert_eq!(watch_entry(" arakunhq ").as_deref(), Some("arakunhq"));
        assert_eq!(watch_entry("@ArakunHQ").as_deref(), Some("ArakunHQ"));
        assert_eq!(
            watch_entry("zed-industries/zed").as_deref(),
            Some("zed-industries/zed")
        );
        assert_eq!(
            watch_entry("https://github.com/arakunhq/arakun/").as_deref(),
            Some("arakunhq/arakun")
        );
        assert_eq!(
            watch_entry("github.com/arakunhq").as_deref(),
            Some("arakunhq")
        );
        for bad in ["", "  ", "a/b/c", "a//b", "two words", "a/b?tab=x", "/"] {
            assert_eq!(watch_entry(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn notifications_are_news_only_after_the_first_read() {
        let mut seen = None;
        let a = note(
            "1",
            "PullRequest",
            "review_requested",
            None,
            "O/R",
            "2026-11-14T10:00:00Z",
        );
        let b = note("2", "Issue", "mention", None, "O/R", "2026-11-14T11:00:00Z");
        assert!(news(&mut seen, std::slice::from_ref(&a)).is_empty());
        let both = [b.clone(), a.clone()];
        assert_eq!(news(&mut seen, &both), std::slice::from_ref(&b));
        assert!(news(&mut seen, &both).is_empty());

        // New activity on a thread already listed is news again.
        let mut a2 = a.clone();
        a2.updated_at = "2026-11-14T12:00:00Z".into();
        assert_eq!(news(&mut seen, &[a2.clone(), b.clone()]), [a2.clone()]);

        // An older thread that slides into the page when another is read is not.
        let old = note(
            "0",
            "Issue",
            "subscribed",
            None,
            "O/R",
            "2026-11-01T09:00:00Z",
        );
        assert!(news(&mut seen, &[a2, old]).is_empty());

        // An empty inbox still counts as a read: what comes next is news.
        let mut seen = None;
        assert!(news(&mut seen, &[]).is_empty());
        assert_eq!(news(&mut seen, std::slice::from_ref(&b)), [b]);
    }

    /// Reads this machine's own inbox through gh; nothing is marked read.
    #[test]
    #[ignore]
    fn reads_the_real_inbox() {
        for all in [false, true] {
            let (list, interval) = list(all).unwrap_or_else(|p| panic!("{}", p.text()));
            println!("X-Poll-Interval: {interval:?}");
            for n in &list {
                assert!(
                    !n.id.is_empty() && !n.repository.full_name.is_empty(),
                    "{n:?}"
                );
                assert!(n.updated() > 0, "{n:?}");
                assert!(n.web_url().starts_with("https://"), "{n:?}");
            }
            println!("all={all}: {} notifications", list.len());
        }
    }

    #[test]
    fn reasons_read_as_plain_words() {
        assert_eq!(reason_label("review_requested"), "review requested");
        assert_eq!(reason_label("mention"), "mentioned");
        assert_eq!(reason_label("assign"), "assigned");
        assert_eq!(reason_label("some_new_reason"), "some new reason");
    }

    #[test]
    fn gh_failures_say_what_to_do() {
        assert_eq!(
            problem("To get started with GitHub CLI, please run:  gh auth login\n"),
            Problem::SignedOut
        );
        assert_eq!(
            problem("gh: Bad credentials (HTTP 401)\nTry authenticating with:  gh auth login"),
            Problem::SignedOut
        );
        assert_eq!(
            problem("\u{1b}[31mgh: Not Found (HTTP 404)\u{1b}[0m\n"),
            Problem::Other("gh: Not Found (HTTP 404)".into())
        );
        assert_eq!(problem("  "), Problem::Other("gh failed".into()));
        assert_eq!(
            Problem::SignedOut.text(),
            "Sign in to GitHub: run `gh auth login` in a terminal."
        );
    }

    #[test]
    fn the_prompt_hands_over_the_notification_and_its_limits() {
        let n = note(
            "1",
            "PullRequest",
            "review_requested",
            Some("https://api.github.com/repos/ArakunHQ/arakun/pulls/39"),
            "ArakunHQ/arakun",
            AT,
        );
        let p = ask_prompt(&n);
        assert!(
            p.contains(
                "the pull request \"ci: run only the jobs a change can affect\" in ArakunHQ/arakun"
            ),
            "{p}"
        );
        assert!(p.contains("because my review was requested."), "{p}");
        assert!(
            p.contains("URL: https://github.com/ArakunHQ/arakun/pull/39"),
            "{p}"
        );
        assert!(
            p.contains("`gh pr diff https://github.com/ArakunHQ/arakun/pull/39`"),
            "{p}"
        );
        assert!(
            p.contains("the description, the diff, the comments and the checks"),
            "{p}"
        );
        assert!(p.contains("what it needs from me"), "{p}");
        assert!(p.ends_with("Don't post comments, approve, request changes, merge or close anything without asking me first."), "{p}");
        assert_eq!(
            ask_title(&n),
            "PR: ci: run only the jobs a change can affect"
        );

        let issue = note(
            "2",
            "Issue",
            "assign",
            Some("https://api.github.com/repos/k/app/issues/7"),
            "k/app",
            AT,
        );
        let p = ask_prompt(&issue);
        assert!(
            p.contains("the issue \"") && p.contains("because it was assigned to me."),
            "{p}"
        );
        assert!(
            p.contains("`gh issue view https://github.com/k/app/issues/7 --comments`"),
            "{p}"
        );

        let ci = note("3", "CheckSuite", "ci_activity", None, "k/app", AT);
        let p = ask_prompt(&ci);
        assert!(
            p.contains("the CI run \"") && p.contains("URL: https://github.com/k/app/actions"),
            "{p}"
        );
        assert!(p.contains("-R k/app"), "{p}");
    }
}
