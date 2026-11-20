//! GitHub Actions runs of each project, CI and deploys, read with `gh`: the list
//! is polled, a run's jobs, annotations and failing log are read when it opens.

pub mod graph;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use gpui::{AppContext as _, Context, EventEmitter, SharedString, Task};
use serde::Deserialize;

use crate::git;
use crate::model::unix_now;
use crate::platform::GIT_REMOTE;

const POLL_ACTIVE: Duration = Duration::from_secs(60);
const POLL_RUNNING: Duration = Duration::from_secs(15);
const POLL_IDLE: Duration = Duration::from_secs(5 * 60);
/// Runs listed per project; the branch history and the badge come from these.
const LIST_LIMIT: usize = 80;
/// A branch graph is read again at most this often (or when the runs change).
const GRAPH_TTL: Duration = Duration::from_secs(60);
/// A running run's page is read again this often while it shows.
const DETAIL_LIVE: Duration = Duration::from_secs(8);
/// Open pull requests are read at most this often.
const PRS_TTL: Duration = Duration::from_secs(90);
/// How long a push waits for its run to show up before arakun stops looking.
const EXPECT_FOR: Duration = Duration::from_secs(3 * 60);
/// The Logs tab keeps the end of long logs.
const LOG_LINES: usize = 4000;
const RUN_FIELDS: &str = "databaseId,number,displayTitle,workflowName,headBranch,headSha,event,status,conclusion,createdAt,startedAt,updatedAt,url,attempt";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Queued,
    Running,
    Passed,
    Failed,
    Cancelled,
    Skipped,
}

impl State {
    /// From GitHub's `status` and `conclusion` (null or "" while running).
    pub fn of(status: &str, conclusion: Option<&str>) -> Self {
        match status {
            "completed" => match conclusion.unwrap_or("") {
                "success" => Self::Passed,
                "cancelled" => Self::Cancelled,
                "skipped" | "neutral" => Self::Skipped,
                _ => Self::Failed,
            },
            "in_progress" => Self::Running,
            _ => Self::Queued,
        }
    }

    pub fn done(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Skipped => "Skipped",
        }
    }

    /// Which of several states speaks for a branch: a failure beats work in
    /// progress, which beats a pass.
    fn weight(self) -> u8 {
        match self {
            Self::Failed => 5,
            Self::Running => 4,
            Self::Queued => 3,
            Self::Cancelled => 2,
            Self::Passed => 1,
            Self::Skipped => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    #[serde(rename = "databaseId")]
    pub id: u64,
    pub number: u64,
    pub display_title: String,
    pub workflow_name: String,
    pub head_branch: String,
    pub head_sha: String,
    pub event: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    pub updated_at: String,
    pub url: String,
    #[serde(default)]
    pub attempt: u32,
}

impl Run {
    pub fn state(&self) -> State {
        State::of(&self.status, self.conclusion.as_deref())
    }

    /// Release workflows and anything named like a deploy.
    pub fn is_deploy(&self) -> bool {
        let name = self.workflow_name.to_lowercase();
        self.event == "release" || name.contains("deploy")
    }

    pub fn short_sha(&self) -> &str {
        self.head_sha.get(..7).unwrap_or(&self.head_sha)
    }

    pub fn created(&self) -> u64 {
        parse_time(&self.created_at).unwrap_or(0)
    }

    /// Seconds it ran (so far, while it runs).
    pub fn duration(&self) -> Option<u64> {
        let start = parse_time(self.started_at.as_deref().unwrap_or(&self.created_at))?;
        let end = if self.state().done() {
            parse_time(&self.updated_at)?
        } else {
            unix_now()
        };
        Some(end.saturating_sub(start))
    }

    /// Failed at first, then passed when run again: likely a flaky test.
    pub fn passed_on_retry(&self) -> bool {
        self.attempt > 1 && self.state() == State::Passed
    }

    pub fn trigger(&self) -> &str {
        match self.event.as_str() {
            "push" => "Push",
            "pull_request" | "pull_request_target" => "Pull request",
            "release" => "Release",
            "workflow_dispatch" => "Manual",
            "schedule" => "Schedule",
            "merge_group" => "Merge queue",
            other => other,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Step {
    pub name: String,
    pub number: u32,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

impl Step {
    pub fn state(&self) -> State {
        State::of(&self.status, self.conclusion.as_deref())
    }

    /// `npx tsc` for GitHub's "Run npx tsc".
    pub fn title(&self) -> &str {
        self.name.strip_prefix("Run ").unwrap_or(&self.name)
    }

    pub fn duration(&self) -> Option<u64> {
        span(self.started_at.as_deref(), self.completed_at.as_deref())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

impl Job {
    pub fn state(&self) -> State {
        State::of(&self.status, self.conclusion.as_deref())
    }

    pub fn duration(&self) -> Option<u64> {
        span(self.started_at.as_deref(), self.completed_at.as_deref())
    }

    pub fn failed_step(&self) -> Option<&Step> {
        self.steps.iter().find(|s| s.state() == State::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Failure,
    Warning,
    Notice,
}

/// A problem GitHub (or the log) points at a line of code.
#[derive(Clone, Debug)]
pub struct Annotation {
    pub job: String,
    pub path: String,
    pub line: u32,
    pub end_line: u32,
    pub level: Level,
    pub title: String,
    pub message: String,
    /// Lines around it (number, text) from the commit that ran.
    pub snippet: Vec<(u32, String)>,
}

impl Annotation {
    /// Has a file of the repository behind it (GitHub puts some on `.github`).
    pub fn in_code(&self) -> bool {
        self.line > 0 && !self.path.is_empty() && self.path != ".github"
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Artifact {
    pub name: String,
    #[serde(default)]
    pub size_in_bytes: u64,
    #[serde(default)]
    pub expired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Plain,
    /// `##[group]` header: the start of a step's command.
    Group,
    /// Inside a group: the command's script, shell and env.
    Setup,
    Command,
    Error,
    Warning,
    Notice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    pub kind: LineKind,
    pub text: String,
}

/// The end of the failing step's log.
#[derive(Clone, Debug)]
pub struct Failure {
    pub job: u64,
    pub lines: Vec<LogLine>,
}

/// The last run of the same workflow and branch that passed, and what landed since.
#[derive(Clone, Debug)]
pub struct SinceGreen {
    pub run: u64,
    pub number: u64,
    /// (sha, title), newest first; empty when the same commit passed.
    pub commits: Vec<(String, String)>,
    /// Not every commit in between is in the local clone.
    pub partial: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Detail {
    pub jobs: Vec<Job>,
    pub annotations: Vec<Annotation>,
    pub failure: Option<Failure>,
    pub artifacts: Vec<Artifact>,
    /// First line of the commit message, and its author.
    pub commit: Option<(String, String)>,
    pub actor: Option<String>,
    pub since_green: Option<SinceGreen>,
}

/// An open pull request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub head_ref_name: String,
    pub url: String,
    #[serde(default)]
    pub is_draft: bool,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED, or empty.
    #[serde(default)]
    pub review_decision: Option<String>,
    /// MERGEABLE, CONFLICTING, UNKNOWN.
    #[serde(default)]
    pub mergeable: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review {
    Draft,
    Approved,
    ChangesRequested,
    Required,
    None,
}

impl Pr {
    pub fn review(&self) -> Review {
        if self.is_draft {
            return Review::Draft;
        }
        match self.review_decision.as_deref().unwrap_or("") {
            "APPROVED" => Review::Approved,
            "CHANGES_REQUESTED" => Review::ChangesRequested,
            "REVIEW_REQUIRED" => Review::Required,
            _ => Review::None,
        }
    }

    pub fn conflicts(&self) -> bool {
        self.mergeable.as_deref() == Some("CONFLICTING")
    }
}

pub fn list_prs(dir: &Path) -> Result<Vec<Pr>> {
    gh_json(
        dir,
        &[
            "pr",
            "list",
            "--state",
            "open",
            "-L",
            "50",
            "--json",
            "number,title,headRefName,url,isDraft,reviewDecision,mergeable",
        ],
    )
}

/// What landed between a green commit and a failing one, from the local clone.
fn since_green(dir: &Path, green: &Run, failing: &Run) -> SinceGreen {
    let mut s = SinceGreen {
        run: green.id,
        number: green.number,
        commits: Vec::new(),
        partial: false,
    };
    if green.head_sha == failing.head_sha {
        return s;
    }
    match git::read(
        dir,
        &[
            "log",
            "--format=%H%x1f%s",
            "-n",
            "30",
            &format!("{}..{}", green.head_sha, failing.head_sha),
        ],
    ) {
        Ok(out) => {
            s.commits = out
                .lines()
                .filter_map(|l| l.split_once('\x1f'))
                .map(|(sha, title)| (sha.to_string(), title.to_string()))
                .collect();
            s.partial = s.commits.len() == 30;
        }
        Err(_) => s.partial = true,
    }
    s
}

impl Detail {
    pub fn failed_job(&self) -> Option<&Job> {
        self.jobs.iter().find(|j| j.state() == State::Failed)
    }
}

pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(SharedString),
}

/// `YYYY-MM-DD` → days since 1970-01-01.
pub fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// "2026-10-13T03:42:18Z" (fractions ignored) → unix seconds.
pub fn parse_time(s: &str) -> Option<u64> {
    let days = day_number(s)?;
    let t = s.get(11..19)?;
    let mut hms = t.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, sec) = (hms.next()??, hms.next()??, hms.next()??);
    u64::try_from(days * 86400 + h * 3600 + m * 60 + sec).ok()
}

fn span(start: Option<&str>, end: Option<&str>) -> Option<u64> {
    let start = parse_time(start?)?;
    let end = match end {
        Some(e) => parse_time(e)?,
        None => unix_now(),
    };
    Some(end.saturating_sub(start))
}

/// "48s", "1m 12s", "1h 3m".
pub fn duration_label(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

/// "0:44", the clock-style duration of a step.
pub fn clock(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub fn size_label(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.0} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

// ---------------------------------------------------------------------------
// Logs

/// One line of a job log without its timestamp and colors, classified by the
/// runner's `##[…]` markers. Returns None for markers with nothing to show.
fn classify(raw: &str, in_group: bool) -> Option<LogLine> {
    let line = raw.trim_start_matches('\u{feff}');
    // "2026-10-13T03:39:11.1374619Z text"
    let line = match line.split_once(' ') {
        Some((ts, rest)) if ts.len() >= 20 && ts.ends_with('Z') && ts.as_bytes()[4] == b'-' => rest,
        _ if line.len() >= 20 && line.ends_with('Z') && line.as_bytes().get(4) == Some(&b'-') => "",
        _ => line,
    };
    let text = git::strip_ansi(line);
    let (kind, text) = if let Some(t) = text.strip_prefix("##[group]") {
        (LineKind::Group, t.to_string())
    } else if text.starts_with("##[endgroup]") || text.starts_with("##[debug]") {
        return None;
    } else if let Some(t) = text.strip_prefix("##[error]") {
        (LineKind::Error, t.to_string())
    } else if let Some(t) = text.strip_prefix("##[warning]") {
        (LineKind::Warning, t.to_string())
    } else if let Some(t) = text.strip_prefix("##[notice]") {
        (LineKind::Notice, t.to_string())
    } else if let Some(t) = text.strip_prefix("[command]") {
        (LineKind::Command, t.to_string())
    } else if in_group {
        (LineKind::Setup, text)
    } else {
        (LineKind::Plain, text)
    };
    Some(LogLine { kind, text })
}

pub fn parse_log(raw: &str) -> Vec<LogLine> {
    let mut out = Vec::new();
    let mut in_group = false;
    for raw in raw.lines() {
        let marker = raw.find("##[").map(|i| &raw[i..]);
        let Some(line) = classify(raw, in_group) else {
            if marker.is_some_and(|m| m.starts_with("##[endgroup]")) {
                in_group = false;
            }
            continue;
        };
        if line.kind == LineKind::Group {
            in_group = true;
        }
        out.push(line);
    }
    out
}

/// The failing step's output: its command, then what it printed up to the
/// runner's errors, without the group's script and env.
pub fn failure_excerpt(lines: &[LogLine]) -> Vec<LogLine> {
    const MAX: usize = 30;
    let Some(first_error) = lines.iter().position(|l| l.kind == LineKind::Error) else {
        return lines[lines.len().saturating_sub(20)..].to_vec();
    };
    let end = lines[first_error..]
        .iter()
        .position(|l| l.kind != LineKind::Error)
        .map_or(lines.len(), |n| first_error + n);
    let start = lines[..first_error]
        .iter()
        .rposition(|l| l.kind == LineKind::Group)
        .unwrap_or(0);
    let header =
        (lines.get(start).is_some_and(|l| l.kind == LineKind::Group)).then(|| lines[start].clone());
    let body: Vec<LogLine> = lines[start..end]
        .iter()
        .filter(|l| !matches!(l.kind, LineKind::Group | LineKind::Setup))
        .cloned()
        .collect();
    let mut out: Vec<LogLine> = header.into_iter().collect();
    out.extend_from_slice(&body[body.len().saturating_sub(MAX)..]);
    out
}

/// Errors printed as `file(line,col): error …` (tsc) or `file:line:col: error …`
/// (most compilers and linters), for logs GitHub drew no annotations from.
pub fn errors_in_log(lines: &[LogLine], job: &str) -> Vec<Annotation> {
    let mut out = Vec::new();
    for l in lines {
        if out.len() >= 10 {
            break;
        }
        if let Some((path, line, message)) = error_at(l.text.trim()) {
            out.push(Annotation {
                job: job.to_string(),
                path,
                line,
                end_line: line,
                level: Level::Failure,
                title: String::new(),
                message,
                snippet: Vec::new(),
            });
        }
    }
    out
}

/// An error line naming where it is: `file(line,col): error …` or `file:line:col: error …`.
pub fn error_at(t: &str) -> Option<(String, u32, String)> {
    tsc_error(t).or_else(|| colon_error(t))
}

fn looks_like_path(p: &str) -> bool {
    !p.is_empty()
        && !p.contains(' ')
        && p.rsplit_once('.')
            .is_some_and(|(_, ext)| (1..=5).contains(&ext.len()))
}

fn tsc_error(t: &str) -> Option<(String, u32, String)> {
    let (path, rest) = t.split_once('(')?;
    let (pos, rest) = rest.split_once("):")?;
    let line: u32 = pos.split(',').next()?.parse().ok()?;
    let message = rest.trim().strip_prefix("error")?.trim().to_string();
    looks_like_path(path).then(|| (path.trim_start_matches("./").to_string(), line, message))
}

fn colon_error(t: &str) -> Option<(String, u32, String)> {
    // A Windows drive (`C:\w\lib.c:2:3: error`) belongs to the path.
    let b = t.as_bytes();
    let drive =
        b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    let (drive, t) = t.split_at(if drive { 2 } else { 0 });
    let mut parts = t.splitn(4, ':');
    let (path, line, col, rest) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let line: u32 = line.trim().parse().ok()?;
    col.trim().parse::<u32>().ok()?;
    let rest = rest.trim();
    let message = rest
        .strip_prefix("error")
        .or_else(|| rest.strip_prefix("Error"))?;
    let message = message.trim_start_matches(':').trim().to_string();
    looks_like_path(path).then(|| {
        (
            format!("{drive}{}", path.trim_start_matches("./")),
            line,
            message,
        )
    })
}

/// The lines of `source` around an annotation: four before, through one after.
pub fn snippet(source: &str, line: u32, end_line: u32) -> Vec<(u32, String)> {
    let from = line.saturating_sub(4).max(1);
    let to = end_line.max(line) + 1;
    source
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u32 + 1, l.trim_end().to_string()))
        .filter(|(n, _)| (from..=to).contains(n))
        .collect()
}

// ---------------------------------------------------------------------------
// gh

/// Runs `gh` with `args` in `dir`, stopped after [`GIT_REMOTE`]: a refresh holds the
/// project's `busy` flag until it ends.
fn gh_out(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    crate::github::gh_run(dir, args, None, GIT_REMOTE).map_err(|p| anyhow!(p.text()))
}

fn gh_json<T: serde::de::DeserializeOwned>(dir: &Path, args: &[&str]) -> Result<T> {
    crate::github::gh_json(dir, args, GIT_REMOTE).map_err(|p| anyhow!(p.text()))
}

/// What a failed list read means for the project.
#[derive(Debug, PartialEq, Eq)]
pub enum ListError {
    /// No GitHub remote: nothing to poll.
    NotGitHub,
    Other(String),
}

impl From<crate::guard::Panicked> for ListError {
    fn from(panicked: crate::guard::Panicked) -> Self {
        ListError::Other(panicked.to_string())
    }
}

pub fn list_runs(dir: &Path) -> std::result::Result<Vec<Run>, ListError> {
    let limit = LIST_LIMIT.to_string();
    gh_json(dir, &["run", "list", "-L", &limit, "--json", RUN_FIELDS]).map_err(|e| {
        let msg = e.to_string();
        let lower = msg.to_lowercase();
        if lower.contains("no git remotes")
            || lower.contains("known github host")
            || lower.contains("not a git repository")
        {
            ListError::NotGitHub
        } else if lower.contains("gh auth login") {
            ListError::Other(crate::github::SIGNED_OUT.into())
        } else {
            ListError::Other(msg)
        }
    })
}

#[derive(Deserialize)]
struct RawRun {
    #[serde(default)]
    head_commit: Option<RawCommit>,
    #[serde(default)]
    actor: Option<RawActor>,
}

#[derive(Deserialize)]
struct RawCommit {
    message: String,
    #[serde(default)]
    author: Option<RawAuthor>,
}

#[derive(Deserialize)]
struct RawAuthor {
    name: String,
}

#[derive(Deserialize)]
struct RawActor {
    login: String,
}

#[derive(Deserialize)]
struct RawJobs {
    jobs: Vec<Job>,
}

#[derive(Deserialize)]
struct RawArtifacts {
    artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
struct RawAnnotation {
    #[serde(default)]
    path: String,
    #[serde(default)]
    start_line: u32,
    #[serde(default)]
    end_line: u32,
    #[serde(default)]
    annotation_level: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    message: String,
}

pub fn job_log(dir: &Path, job: u64) -> Result<Vec<LogLine>> {
    let out = gh_out(
        dir,
        &[
            "api",
            "--allow-escape-sequences",
            &format!("repos/{{owner}}/{{repo}}/actions/jobs/{job}/logs"),
        ],
    )?;
    let mut lines = parse_log(&String::from_utf8_lossy(&out));
    if lines.len() > LOG_LINES {
        lines.drain(..lines.len() - LOG_LINES);
    }
    Ok(lines)
}

/// Everything the run page shows. `checkouts` are the project's folders, to
/// read annotated code from when the commit isn't in the clone.
pub fn run_detail(
    dir: &Path,
    run: &Run,
    green: Option<&Run>,
    checkouts: &[PathBuf],
) -> Result<Detail> {
    let base = format!("repos/{{owner}}/{{repo}}/actions/runs/{}", run.id);
    let raw: RawRun = gh_json(dir, &["api", &base])?;
    let jobs: RawJobs = gh_json(dir, &["api", &format!("{base}/jobs?per_page=100")])?;
    let artifacts = gh_json::<RawArtifacts>(dir, &["api", &format!("{base}/artifacts")])
        .map(|a| a.artifacts)
        .unwrap_or_default();
    let mut annotations = Vec::new();
    for job in jobs.jobs.iter().take(12) {
        let path = format!("repos/{{owner}}/{{repo}}/check-runs/{}/annotations", job.id);
        let Ok(list) = gh_json::<Vec<RawAnnotation>>(dir, &["api", &path]) else {
            continue;
        };
        annotations.extend(list.into_iter().map(|a| Annotation {
            job: job.name.clone(),
            path: a.path,
            line: a.start_line,
            end_line: a.end_line.max(a.start_line),
            level: match a.annotation_level.as_str() {
                "failure" => Level::Failure,
                "warning" => Level::Warning,
                _ => Level::Notice,
            },
            title: a.title.unwrap_or_default(),
            message: a.message,
            snippet: Vec::new(),
        }));
    }
    let failure = jobs
        .jobs
        .iter()
        .find(|j| j.state() == State::Failed)
        .and_then(|job| {
            let lines = job_log(dir, job.id).ok()?;
            if !annotations.iter().any(|a| a.in_code() && a.job == job.name) {
                annotations.extend(errors_in_log(&lines, &job.name));
            }
            Some(Failure {
                job: job.id,
                lines: failure_excerpt(&lines),
            })
        });
    let mut sources: HashMap<String, Option<String>> = HashMap::new();
    for a in annotations.iter_mut().filter(|a| a.in_code()) {
        let source = sources.entry(a.path.clone()).or_insert_with(|| {
            git::show_file(dir, &run.head_sha, &a.path).or_else(|| {
                checkouts
                    .iter()
                    .find_map(|c| std::fs::read_to_string(c.join(&a.path)).ok())
            })
        });
        if let Some(src) = source {
            a.snippet = snippet(src, a.line, a.end_line);
        }
    }
    Ok(Detail {
        jobs: jobs.jobs,
        annotations,
        failure,
        artifacts,
        commit: raw.head_commit.map(|c| {
            let title = c.message.lines().next().unwrap_or_default().to_string();
            (title, c.author.map(|a| a.name).unwrap_or_default())
        }),
        actor: raw.actor.map(|a| a.login),
        since_green: green
            .filter(|_| run.state() == State::Failed)
            .map(|g| since_green(dir, g, run)),
    })
}

fn gh_action(dir: &Path, args: &[&str]) -> Result<()> {
    gh_out(dir, args).map(|_| ())
}

/// The prompt that hands a failed run to an agent.
pub fn fix_prompt(run: &Run, detail: Option<&Detail>, checkout_branch: &str) -> String {
    let mut p = format!(
        "The GitHub Actions run #{} ({}) failed on branch `{}` at {}.\nRun: {}\n",
        run.number,
        run.workflow_name,
        run.head_branch,
        run.short_sha(),
        run.url
    );
    if let Some(d) = detail {
        if let Some(job) = d.failed_job() {
            p.push_str(&format!("Failed job: {}", job.name));
            if let Some(step) = job.failed_step() {
                p.push_str(&format!(", step `{}`", step.title()));
            }
            p.push('\n');
        }
        let notes: Vec<&Annotation> = d
            .annotations
            .iter()
            .filter(|a| a.level != Level::Notice)
            .take(8)
            .collect();
        if !notes.is_empty() {
            p.push_str("\nProblems reported:\n");
            for a in notes {
                if a.in_code() {
                    p.push_str(&format!(
                        "- {}:{}: {}\n",
                        a.path,
                        a.line,
                        a.message.lines().next().unwrap_or_default()
                    ));
                } else {
                    p.push_str(&format!(
                        "- {}\n",
                        a.message.lines().next().unwrap_or_default()
                    ));
                }
            }
        }
        if let Some(f) = &d.failure {
            p.push_str("\nEnd of the failing step's log:\n```\n");
            for l in &f.lines {
                p.push_str(&l.text);
                p.push('\n');
            }
            p.push_str("```\n");
        }
    }
    if checkout_branch != run.head_branch {
        p.push_str(&format!(
            "\nThis checkout is on `{checkout_branch}`, not `{}`; look at that branch's code (`git show {}`) and say so if the fix belongs there.\n",
            run.head_branch,
            run.short_sha()
        ));
    }
    p.push_str(
        "\nFind the cause and fix it in this checkout. Run the same command locally to confirm it passes, then summarize what changed. Don't push.",
    );
    p
}

/// Asks the agent of a fix to commit and push it, for CI to judge. `staged`
/// are the files of the session arakun staged for it.
pub fn ship_prompt(branch: &str, staged: &[String]) -> String {
    let what = if staged.is_empty() {
        "Commit the fix".to_string()
    } else {
        format!(
            "I staged the {} you changed ({}). Commit only what is staged (`git commit`, no `-a`; leave other changes alone)",
            if staged.len() == 1 { "file" } else { "files" },
            staged
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "Looks good. {what} with a short, clear message and push `{branch}` (`git push`). \
         CI will run on it; I'll tell you how it ends."
    )
}

/// Hands a fix's agent the run that still fails after its push.
pub fn retry_prompt(run: &Run, detail: Option<&Detail>, round: u8, rounds: u8) -> String {
    let mut p = format!(
        "CI still fails on `{}` after your push (try {round} of {rounds}): run #{} ({}) at {}.\nRun: {}\n",
        run.head_branch,
        run.number,
        run.workflow_name,
        run.short_sha(),
        run.url
    );
    match detail {
        Some(d) => {
            let full = fix_prompt(run, Some(d), &run.head_branch);
            // fix_prompt's two header lines are said above already.
            let rest = full.splitn(3, '\n').nth(2).unwrap_or("");
            p.push_str(rest);
        }
        None => p.push_str(&format!(
            "\nRead the failure with `gh run view {} --log-failed`, find the cause, fix it, and run the same command locally to confirm. Don't push; tell me when it's ready.",
            run.id
        )),
    }
    p
}

// ---------------------------------------------------------------------------
// The polling model

pub enum CiEvent {
    /// A run finished since the last read; `prev` is how the one before it on
    /// the same workflow and branch ended.
    /// `watched`: the branch was pushed from arakun, and this is its first result.
    Finished {
        root: PathBuf,
        run: Run,
        prev: Option<State>,
        watched: bool,
    },
}

/// A job's log as the Logs tab reads it.
type JobLog = Load<Arc<Vec<LogLine>>>;

#[derive(Default)]
pub struct Repo {
    pub runs: Vec<Run>,
    pub loaded: bool,
    pub busy: bool,
    pub error: Option<SharedString>,
    pub not_github: bool,
    last_fetch: Option<Instant>,
    /// Run id → the `updated_at` it was read at, and the read.
    details: HashMap<u64, (String, Load<Arc<Detail>>)>,
    /// Job id → the log, and the `logs_gen` of the read it came from.
    logs: HashMap<u64, (u64, JobLog)>,
    /// Counts `clear_logs`, to tell the reads that started before one.
    logs_gen: u64,
    /// Branch → its graph and when it was read.
    graphs: HashMap<String, (Instant, Load<Arc<graph::Graph>>)>,
    /// When each run's page was last read, to keep a running one moving.
    detail_at: HashMap<u64, Instant>,
    /// Open pull requests, and when they were read.
    pub prs: Vec<Pr>,
    prs_at: Option<Instant>,
    /// Branches pushed from arakun whose CI hasn't shown up yet, and when.
    expected: HashMap<String, u64>,
    /// Branches pushed from arakun: their next finished run is news either way.
    watched: HashMap<String, u64>,
    /// Runs with a re-run or cancel on its way.
    pub acting: HashSet<u64>,
    /// Runs seen and whether they had finished, to tell what just finished.
    seen: Option<HashMap<u64, bool>>,
}

impl Repo {
    pub fn run(&self, id: u64) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == id)
    }

    /// The newest run of each CI workflow on `branch`, worst first.
    pub fn branch_state(&self, branch: &str) -> Option<(State, &Run)> {
        let mut seen = HashSet::new();
        self.runs
            .iter()
            .filter(|r| r.head_branch == branch && !r.is_deploy())
            .filter(|r| seen.insert(r.workflow_name.as_str()))
            .map(|r| (r.state(), r))
            .max_by_key(|(s, _)| s.weight())
    }

    pub fn latest_deploy(&self) -> Option<&Run> {
        self.runs.iter().find(|r| r.is_deploy())
    }

    /// The run before `run` on its workflow and branch.
    pub fn previous(&self, run: &Run) -> Option<&Run> {
        self.runs
            .iter()
            .filter(|r| r.workflow_name == run.workflow_name && r.head_branch == run.head_branch)
            .find(|r| {
                r.created() < run.created() || (r.created() == run.created() && r.id < run.id)
            })
    }

    pub fn detail(&self, id: u64) -> Option<&Load<Arc<Detail>>> {
        self.details.get(&id).map(|(_, d)| d)
    }

    pub fn log(&self, job: u64) -> Option<&Load<Arc<Vec<LogLine>>>> {
        self.logs.get(&job).map(|(_, l)| l)
    }

    /// Marks a job's log as being read, and gives the read's generation for
    /// `finish_log`; `None` when it is read, being read or failed. A failed read
    /// stays until the user tries again or the run's page is read again
    /// (`clear_logs`): the Logs tab asks on every render, and reading again each
    /// time calls `gh` in a loop and uses up the GitHub rate limit.
    fn start_log(&mut self, job: u64) -> Option<u64> {
        if self.logs.contains_key(&job) {
            return None;
        }
        self.logs.insert(job, (self.logs_gen, Load::Loading));
        Some(self.logs_gen)
    }

    /// Keeps a read's result. A read that started before the logs were last
    /// cleared keeps only a log, and only over a read in progress or an older
    /// one: a running run's page is read again every few seconds, so a slow read
    /// must still count, but its failure is old and would otherwise stay until
    /// the user tries again.
    fn finish_log(&mut self, job: u64, generation: u64, load: JobLog) {
        let keep = generation == self.logs_gen
            || matches!(load, Load::Ready(_))
                && self
                    .logs
                    .get(&job)
                    .is_none_or(|(at, held)| matches!(held, Load::Loading) || *at < generation);
        if keep {
            self.logs.insert(job, (generation, load));
        }
    }

    fn clear_logs(&mut self) {
        self.logs.clear();
        self.logs_gen += 1;
    }

    /// Forgets a log that failed to read, so the next `start_log` reads it again.
    fn forget_failed_log(&mut self, job: u64) {
        if matches!(self.logs.get(&job), Some((_, Load::Failed(_)))) {
            self.logs.remove(&job);
        }
    }

    pub fn graph(&self, branch: &str) -> Option<&Load<Arc<graph::Graph>>> {
        self.graphs.get(branch).map(|(_, g)| g)
    }

    /// The state CI gave a commit: the newest run of each workflow on it, worst first.
    pub fn commit_state(&self, sha: &str) -> Option<(State, &Run)> {
        let mut seen = HashSet::new();
        self.runs
            .iter()
            .filter(|r| r.head_sha == sha && !r.is_deploy())
            .filter(|r| seen.insert(r.workflow_name.as_str()))
            .map(|r| (r.state(), r))
            .max_by_key(|(s, _)| s.weight())
    }

    /// Deploys of a commit (release tags), newest first.
    pub fn deploys_of(&self, sha: &str) -> impl Iterator<Item = &Run> {
        self.runs
            .iter()
            .filter(move |r| r.head_sha == sha && r.is_deploy())
    }

    /// The last run of `run`'s workflow and branch, before it, that passed.
    pub fn last_green(&self, run: &Run) -> Option<&Run> {
        self.runs
            .iter()
            .filter(|r| {
                r.workflow_name == run.workflow_name
                    && r.head_branch == run.head_branch
                    && r.id != run.id
            })
            .filter(|r| r.created() <= run.created())
            .find(|r| r.state() == State::Passed)
    }

    pub fn pr_of(&self, branch: &str) -> Option<&Pr> {
        self.prs.iter().find(|p| p.head_ref_name == branch)
    }

    /// Durations of the workflow's finished runs that passed or failed, oldest
    /// first, with their ids.
    pub fn durations(&self, workflow: &str) -> Vec<(u64, u64, State)> {
        let mut out: Vec<(u64, u64, State)> = self
            .runs
            .iter()
            .filter(|r| {
                r.workflow_name == workflow && matches!(r.state(), State::Passed | State::Failed)
            })
            .filter_map(|r| Some((r.id, r.duration()?, r.state())))
            .take(24)
            .collect();
        out.reverse();
        out
    }

    /// A push from arakun is waiting for its CI to show up.
    pub fn expecting(&self, branch: &str) -> bool {
        self.expected
            .get(branch)
            .is_some_and(|at| unix_now().saturating_sub(*at) < EXPECT_FOR.as_secs())
    }

    fn any_running(&self) -> bool {
        self.runs.iter().any(|r| !r.state().done())
            || self.expected.keys().any(|b| self.expecting(b))
    }
}

pub struct Ci {
    repos: HashMap<PathBuf, Repo>,
    /// Folders of each project's checkouts, for reading annotated code.
    checkouts: HashMap<PathBuf, Vec<PathBuf>>,
    window_active: bool,
    _poll: Task<()>,
}

impl EventEmitter<CiEvent> for Ci {}

impl Ci {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.read_with(cx, |this, _| this.poll_interval()) else {
                    break;
                };
                cx.background_executor().timer(wait).await;
                if this.update(cx, |this, cx| this.refresh_all(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            repos: HashMap::new(),
            checkouts: HashMap::new(),
            window_active: true,
            _poll: poll,
        }
    }

    fn poll_interval(&self) -> Duration {
        if !self.window_active {
            POLL_IDLE
        } else if self.repos.values().any(Repo::any_running) {
            POLL_RUNNING
        } else {
            POLL_ACTIVE
        }
    }

    pub fn disabled() -> bool {
        std::env::var("ARAKUN_CI").is_ok_and(|v| v == "off")
    }

    /// The projects to follow (main checkout → every checkout); new ones are read now.
    pub fn watch(&mut self, projects: Vec<(PathBuf, Vec<PathBuf>)>, cx: &mut Context<Self>) {
        let roots: HashSet<PathBuf> = projects.iter().map(|(r, _)| r.clone()).collect();
        self.repos.retain(|r, _| roots.contains(r));
        let mut fresh = Vec::new();
        for (root, checkouts) in projects {
            if !self.repos.contains_key(&root) {
                self.repos.insert(root.clone(), Repo::default());
                fresh.push(root.clone());
            }
            self.checkouts.insert(root, checkouts);
        }
        for root in fresh {
            self.refresh(&root, cx);
        }
    }

    pub fn repo(&self, root: &Path) -> Option<&Repo> {
        self.repos.get(root)
    }

    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        let was = self.window_active;
        self.window_active = active;
        if active && !was {
            let stale: Vec<PathBuf> = self
                .repos
                .iter()
                .filter(|(_, r)| {
                    r.last_fetch
                        .is_none_or(|t| t.elapsed() > Duration::from_secs(30))
                })
                .map(|(p, _)| p.clone())
                .collect();
            for root in stale {
                self.refresh(&root, cx);
            }
        }
    }

    pub fn refresh_all(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<PathBuf> = self.repos.keys().cloned().collect();
        for root in roots {
            self.refresh(&root, cx);
        }
    }

    pub fn refresh_if_stale(&mut self, root: &Path, max_age: Duration, cx: &mut Context<Self>) {
        if self
            .repos
            .get(root)
            .is_some_and(|r| r.last_fetch.is_none_or(|t| t.elapsed() > max_age))
        {
            self.refresh(root, cx);
        }
    }

    pub fn refresh(&mut self, root: &Path, cx: &mut Context<Self>) {
        if Self::disabled() {
            return;
        }
        let Some(repo) = self.repos.get_mut(root) else {
            return;
        };
        if repo.busy || repo.not_github {
            return;
        }
        repo.busy = true;
        let want_prs = repo.prs_at.is_none_or(|t| t.elapsed() > PRS_TTL);
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let dir = root.clone();
            let (result, prs) = cx
                .background_spawn(crate::guard::task(async move {
                    let runs = list_runs(&dir);
                    let prs = (want_prs && runs.is_ok())
                        .then(|| list_prs(&dir).ok())
                        .flatten();
                    (runs, prs)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(repo) = this.repos.get_mut(&root) else {
                    return;
                };
                repo.busy = false;
                repo.last_fetch = Some(Instant::now());
                if let Some(prs) = prs {
                    repo.prs = prs;
                    repo.prs_at = Some(Instant::now());
                }
                match result {
                    Ok(runs) => {
                        repo.error = None;
                        repo.loaded = true;
                        let finished = Self::finished(repo, &runs);
                        // A run that changed since its page was read gets read again.
                        repo.details.retain(|id, (at, _)| {
                            runs.iter().any(|r| r.id == *id && r.updated_at == *at)
                        });
                        if repo.runs != runs {
                            repo.graphs.clear();
                        }
                        repo.expected.retain(|branch, at| {
                            unix_now().saturating_sub(*at) < EXPECT_FOR.as_secs()
                                && !runs
                                    .iter()
                                    .any(|r| &r.head_branch == branch && r.created() + 30 >= *at)
                        });
                        repo.runs = runs;
                        for run in finished {
                            let prev = repo.previous(&run).map(Run::state);
                            let watched = !run.is_deploy()
                                && repo
                                    .watched
                                    .get(&run.head_branch)
                                    .is_some_and(|at| run.created() + 30 >= *at)
                                && repo.watched.remove(&run.head_branch).is_some();
                            cx.emit(CiEvent::Finished {
                                root: root.clone(),
                                run,
                                prev,
                                watched,
                            });
                        }
                    }
                    Err(ListError::NotGitHub) => {
                        repo.not_github = true;
                        repo.loaded = true;
                    }
                    Err(ListError::Other(e)) => repo.error = Some(e.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs that finished since the last read (none on the first read).
    fn finished(repo: &mut Repo, runs: &[Run]) -> Vec<Run> {
        let now: HashMap<u64, bool> = runs.iter().map(|r| (r.id, r.state().done())).collect();
        let out = match &repo.seen {
            None => Vec::new(),
            Some(seen) => runs
                .iter()
                .filter(|r| r.state().done() && seen.get(&r.id) != Some(&true))
                // A run first seen already finished, long after it ended, isn't news.
                .filter(|r| {
                    seen.contains_key(&r.id)
                        || unix_now().saturating_sub(parse_time(&r.updated_at).unwrap_or(0))
                            < 15 * 60
                })
                .cloned()
                .collect(),
        };
        let mut seen = repo.seen.take().unwrap_or_default();
        seen.extend(now);
        repo.seen = Some(seen);
        out
    }

    /// Reads a run's page unless it is already read (or being read).
    pub fn load_detail(&mut self, root: &Path, id: u64, cx: &mut Context<Self>) {
        let checkouts = self.checkouts.get(root).cloned().unwrap_or_default();
        let Some(repo) = self.repos.get_mut(root) else {
            return;
        };
        let Some(run) = repo.run(id).cloned() else {
            return;
        };
        if let Some((at, load)) = repo.details.get(&id)
            && (*at == run.updated_at || matches!(load, Load::Loading))
        {
            return;
        }
        self.read_detail(root, run, checkouts, cx);
    }

    /// Reads a running run's page again now and then, so its steps move.
    pub fn poll_detail(&mut self, root: &Path, id: u64, cx: &mut Context<Self>) {
        let checkouts = self.checkouts.get(root).cloned().unwrap_or_default();
        let Some(repo) = self.repos.get_mut(root) else {
            return;
        };
        let Some(run) = repo.run(id).cloned().filter(|r| !r.state().done()) else {
            return;
        };
        let due = match (repo.details.get(&id), repo.detail_at.get(&id)) {
            (Some((_, Load::Loading)), _) => false,
            (Some(_), Some(at)) => at.elapsed() >= DETAIL_LIVE,
            _ => true,
        };
        if due {
            self.read_detail(root, run, checkouts, cx);
        }
    }

    fn read_detail(
        &mut self,
        root: &Path,
        run: Run,
        checkouts: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repos.get_mut(root) else {
            return;
        };
        let id = run.id;
        let green = repo.last_green(&run).cloned();
        repo.detail_at.insert(id, Instant::now());
        let keep_old = repo.details.remove(&id).and_then(|(_, l)| match l {
            Load::Ready(d) => Some(d),
            _ => None,
        });
        repo.details.insert(
            id,
            (
                run.updated_at.clone(),
                keep_old.map_or(Load::Loading, Load::Ready),
            ),
        );
        // The job logs can have grown too.
        repo.clear_logs();
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let dir = root.clone();
            let r = run.clone();
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    run_detail(&dir, &r, green.as_ref(), &checkouts)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(repo) = this.repos.get_mut(&root) {
                    let load = match result {
                        Ok(d) => Load::Ready(Arc::new(d)),
                        Err(e) => Load::Failed(e.to_string().into()),
                    };
                    repo.details.insert(id, (run.updated_at.clone(), load));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads (or re-reads, when old) the graph of a branch.
    pub fn load_graph(&mut self, root: &Path, branch: &str, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.get_mut(root) else {
            return;
        };
        let keep = match repo.graphs.get(branch) {
            Some((_, Load::Loading)) => return,
            Some((at, _)) if at.elapsed() < GRAPH_TTL => return,
            Some((_, Load::Ready(g))) => Some(g.clone()),
            _ => None,
        };
        repo.graphs.insert(
            branch.to_string(),
            (Instant::now(), keep.map_or(Load::Loading, Load::Ready)),
        );
        cx.notify();
        let (root, branch) = (root.to_path_buf(), branch.to_string());
        cx.spawn(async move |this, cx| {
            let (dir, b) = (root.clone(), branch.clone());
            let result = cx
                .background_spawn(crate::guard::task(async move { graph::build(&dir, &b) }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(repo) = this.repos.get_mut(&root) {
                    let load = match result {
                        Ok(g) => Load::Ready(Arc::new(g)),
                        Err(e) => Load::Failed(e.to_string().into()),
                    };
                    repo.graphs.insert(branch, (Instant::now(), load));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A branch was pushed from arakun: look for its run soon, and tell how it ends.
    pub fn pushed(&mut self, root: &Path, branch: &str, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.get_mut(root) else {
            return;
        };
        repo.expected.insert(branch.to_string(), unix_now());
        repo.watched.insert(branch.to_string(), unix_now());
        repo.graphs.clear();
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            // GitHub takes a few seconds to queue the run of a push.
            for wait in [6, 12, 25] {
                cx.background_executor()
                    .timer(Duration::from_secs(wait))
                    .await;
                if this.update(cx, |this, cx| this.refresh(&root, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Someone else (an agent) is about to push `branch`: its next result is news.
    pub fn watch_branch(&mut self, root: &Path, branch: &str) {
        if let Some(repo) = self.repos.get_mut(root) {
            repo.watched.insert(branch.to_string(), unix_now());
        }
    }

    /// Forgets the read graphs, after the branches moved (a commit, a revert).
    pub fn forget_graphs(&mut self, root: &Path, cx: &mut Context<Self>) {
        if let Some(repo) = self.repos.get_mut(root) {
            repo.graphs.clear();
            cx.notify();
        }
    }

    /// Reads a job's whole log for the Logs tab.
    pub fn load_log(&mut self, root: &Path, job: u64, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.get_mut(root) else {
            return;
        };
        let Some(generation) = repo.start_log(job) else {
            return;
        };
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let dir = root.clone();
            let result = cx
                .background_spawn(crate::guard::task(async move { job_log(&dir, job) }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(repo) = this.repos.get_mut(&root) {
                    let load = match result {
                        Ok(l) => Load::Ready(Arc::new(l)),
                        Err(e) => Load::Failed(e.to_string().into()),
                    };
                    repo.finish_log(job, generation, load);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads a job's log again after it failed to read.
    pub fn retry_log(&mut self, root: &Path, job: u64, cx: &mut Context<Self>) {
        if let Some(repo) = self.repos.get_mut(root) {
            repo.forget_failed_log(job);
        }
        self.load_log(root, job, cx);
    }

    /// `gh run rerun --failed` (every job when none failed), or `gh run cancel`.
    pub fn act(&mut self, root: &Path, id: u64, cancel: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.get_mut(root) else {
            return;
        };
        let Some(failed) = repo.run(id).map(|r| r.state() == State::Failed) else {
            return;
        };
        if !repo.acting.insert(id) {
            return;
        }
        repo.error = None;
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let dir = root.clone();
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    let id = id.to_string();
                    match (cancel, failed) {
                        (true, _) => gh_action(&dir, &["run", "cancel", &id]),
                        (false, true) => gh_action(&dir, &["run", "rerun", &id, "--failed"]),
                        (false, false) => gh_action(&dir, &["run", "rerun", &id]),
                    }
                }))
                .await;
            // GitHub takes a moment to show the new attempt.
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(repo) = this.repos.get_mut(&root) {
                    repo.acting.remove(&id);
                    if let Err(e) = result {
                        repo.error = Some(e.to_string().into());
                    }
                }
                this.refresh(&root, cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64, workflow: &str, branch: &str, status: &str, conclusion: &str, at: &str) -> Run {
        Run {
            id,
            number: id,
            display_title: format!("run {id}"),
            workflow_name: workflow.into(),
            head_branch: branch.into(),
            head_sha: "5a7019c0ffee".into(),
            event: "push".into(),
            status: status.into(),
            conclusion: Some(conclusion.into()),
            created_at: at.into(),
            started_at: Some(at.into()),
            updated_at: at.into(),
            url: String::new(),
            attempt: 1,
        }
    }

    #[test]
    fn finds_the_last_green_run_and_words_a_retry() {
        let repo = Repo {
            runs: vec![
                run(
                    4,
                    "CI",
                    "feat",
                    "completed",
                    "failure",
                    "2026-10-13T04:00:00Z",
                ),
                run(
                    3,
                    "CI",
                    "feat",
                    "completed",
                    "failure",
                    "2026-10-13T03:00:00Z",
                ),
                run(
                    2,
                    "Lint",
                    "feat",
                    "completed",
                    "success",
                    "2026-10-13T02:30:00Z",
                ),
                run(
                    1,
                    "CI",
                    "feat",
                    "completed",
                    "success",
                    "2026-10-13T02:00:00Z",
                ),
            ],
            ..Repo::default()
        };
        assert_eq!(repo.last_green(&repo.runs[0]).map(|r| r.id), Some(1));
        assert_eq!(
            repo.durations("CI").iter().map(|d| d.0).collect::<Vec<_>>(),
            vec![1, 3, 4]
        );

        let detail = Detail::default();
        let p = retry_prompt(&repo.runs[0], Some(&detail), 2, 3);
        assert!(p.starts_with("CI still fails on `feat` after your push (try 2 of 3): run #4"));
        assert_eq!(p.matches("Run: ").count(), 1, "{p}");
        assert!(p.ends_with("Don't push."));
        assert!(retry_prompt(&repo.runs[0], None, 1, 3).contains("gh run view 4 --log-failed"));
    }

    #[test]
    fn reads_gh_pr_list() {
        let json = r#"[{"headRefName":"feat","isDraft":false,"mergeable":"CONFLICTING","number":12,"reviewDecision":"APPROVED","title":"Feat","url":"u"}]"#;
        let prs: Vec<Pr> = serde_json::from_str(json).unwrap();
        assert_eq!(prs[0].review(), Review::Approved);
        assert!(prs[0].conflicts());
    }

    #[test]
    fn reads_gh_run_list() {
        let json = r#"[{"attempt":1,"conclusion":"","createdAt":"2026-10-13T03:42:18Z","databaseId":38342961341,"displayTitle":"v1.9.0","event":"release","headBranch":"v1.9.0","headSha":"4ab3c3b86f9c","number":21,"startedAt":"2026-10-13T03:42:18Z","status":"in_progress","updatedAt":"2026-10-13T03:43:11Z","url":"https://github.com/o/r/actions/runs/1","workflowName":"Deploy release"}]"#;
        let runs: Vec<Run> = serde_json::from_str(json).unwrap();
        assert_eq!(runs[0].id, 38342961341);
        assert_eq!(runs[0].state(), State::Running);
        assert!(runs[0].is_deploy());
        assert_eq!(runs[0].short_sha(), "4ab3c3b");
        assert_eq!(parse_time("1970-01-02T00:01:05Z"), Some(86465));
        assert_eq!(
            parse_time("2026-10-13T03:43:11.1234Z").unwrap()
                - parse_time("2026-10-13T03:42:18Z").unwrap(),
            53
        );
    }

    #[test]
    fn states_and_labels() {
        assert_eq!(State::of("completed", Some("success")), State::Passed);
        assert_eq!(State::of("completed", Some("failure")), State::Failed);
        assert_eq!(State::of("completed", Some("timed_out")), State::Failed);
        assert_eq!(State::of("completed", Some("cancelled")), State::Cancelled);
        assert_eq!(State::of("queued", None), State::Queued);
        assert_eq!(duration_label(48), "48s");
        assert_eq!(duration_label(108), "1m 48s");
        assert_eq!(clock(44), "0:44");
        assert_eq!(size_label(2_500_000), "2.4 MB");
    }

    #[test]
    fn branch_state_takes_the_newest_run_of_each_workflow() {
        let repo = Repo {
            runs: vec![
                run(
                    4,
                    "CI",
                    "feat",
                    "completed",
                    "success",
                    "2026-10-13T03:00:04Z",
                ),
                run(
                    3,
                    "Lint",
                    "feat",
                    "completed",
                    "failure",
                    "2026-10-13T03:00:03Z",
                ),
                run(
                    2,
                    "CI",
                    "feat",
                    "completed",
                    "failure",
                    "2026-10-13T03:00:02Z",
                ),
                run(
                    1,
                    "CI",
                    "main",
                    "completed",
                    "success",
                    "2026-10-13T03:00:01Z",
                ),
            ],
            ..Default::default()
        };
        let (state, r) = repo.branch_state("feat").unwrap();
        assert_eq!((state, r.id), (State::Failed, 3));
        assert_eq!(repo.branch_state("main").unwrap().0, State::Passed);
        assert!(repo.branch_state("other").is_none());
        assert_eq!(repo.previous(&repo.runs[0]).map(|r| r.id), Some(2));
        assert!(repo.previous(&repo.runs[3]).is_none());
    }

    #[test]
    fn a_failed_log_is_read_again_only_when_asked() {
        let mut repo = Repo::default();
        let generation = repo.start_log(7).unwrap();
        assert!(repo.start_log(7).is_none(), "it is being read");
        repo.finish_log(7, generation, Load::Failed("gone".into()));
        assert!(
            repo.start_log(7).is_none(),
            "every render would call gh again"
        );
        repo.forget_failed_log(7);
        let generation = repo.start_log(7).unwrap();
        repo.finish_log(7, generation, Load::Ready(Arc::default()));
        repo.forget_failed_log(7);
        assert!(repo.start_log(7).is_none(), "a read log stays");
    }

    #[test]
    fn a_log_read_from_before_a_clear_keeps_only_a_log() {
        let mut repo = Repo::default();
        let old = repo.start_log(7).unwrap();
        repo.clear_logs();
        repo.finish_log(7, old, Load::Failed("not there yet".into()));
        assert!(repo.start_log(7).is_some(), "the old failure doesn't stay");

        // A slow read that ends after the page of a running run is read again.
        let mut repo = Repo::default();
        let old = repo.start_log(7).unwrap();
        repo.clear_logs();
        repo.finish_log(7, old, Load::Ready(Arc::default()));
        assert!(matches!(repo.log(7), Some(Load::Ready(_))));

        // It doesn't replace the newer read's log.
        let newer = Arc::new(Vec::new());
        let mut repo = Repo::default();
        let old = repo.start_log(7).unwrap();
        repo.clear_logs();
        let generation = repo.start_log(7).unwrap();
        repo.finish_log(7, generation, Load::Ready(newer.clone()));
        repo.finish_log(7, old, Load::Ready(Arc::default()));
        assert!(matches!(repo.log(7), Some(Load::Ready(l)) if Arc::ptr_eq(l, &newer)));

        // Two clears with two reads on their way: the newer read's log wins,
        // whichever ends first.
        let newer = Arc::new(Vec::new());
        let mut repo = Repo::default();
        let a = repo.start_log(7).unwrap();
        repo.clear_logs();
        let b = repo.start_log(7).unwrap();
        repo.clear_logs();
        repo.finish_log(7, a, Load::Ready(Arc::default()));
        repo.finish_log(7, b, Load::Ready(newer.clone()));
        assert!(matches!(repo.log(7), Some(Load::Ready(l)) if Arc::ptr_eq(l, &newer)));
    }

    #[test]
    fn finished_runs_are_news_only_after_the_first_read() {
        let mut repo = Repo::default();
        let first = vec![run(
            1,
            "CI",
            "feat",
            "in_progress",
            "",
            "2026-10-13T03:00:00Z",
        )];
        assert!(Ci::finished(&mut repo, &first).is_empty());
        let second = vec![run(
            1,
            "CI",
            "feat",
            "completed",
            "failure",
            "2026-10-13T03:00:00Z",
        )];
        assert_eq!(Ci::finished(&mut repo, &second).len(), 1);
        assert!(Ci::finished(&mut repo, &second).is_empty());
    }

    const LOG: &str = "\u{feff}2026-10-13T03:39:11.1374619Z Current runner version: '2.337.0'
2026-10-13T03:39:17.2254157Z ##[group]Run npm ci
2026-10-13T03:39:17.2254157Z npm ci
2026-10-13T03:39:17.2254157Z shell: /usr/bin/bash -e {0}
2026-10-13T03:39:17.3701320Z ##[endgroup]
2026-10-13T03:39:29.5000000Z added 312 packages
2026-10-13T03:39:29.5742706Z ##[group]Run npx tsc --noEmit -p .
2026-10-13T03:39:29.5742706Z npx tsc --noEmit -p .
2026-10-13T03:39:29.5796004Z ##[endgroup]
2026-10-13T03:39:36.1000000Z studio/worker/mcp.ts(340,30): error TS2551: Property 'lenght' does not exist on type 'AgentMention[]'.
2026-10-13T03:39:36.2000000Z ##[error]Process completed with exit code 2.
2026-10-13T03:39:36.3000000Z Post job cleanup.";

    #[test]
    fn parses_logs_and_finds_the_failure() {
        let lines = parse_log(LOG);
        assert_eq!(lines[0].text, "Current runner version: '2.337.0'");
        assert_eq!(
            lines[1],
            LogLine {
                kind: LineKind::Group,
                text: "Run npm ci".into()
            }
        );
        assert_eq!(lines[2].kind, LineKind::Setup);
        assert_eq!(lines[4].kind, LineKind::Plain);
        let excerpt = failure_excerpt(&lines);
        let texts: Vec<&str> = excerpt.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Run npx tsc --noEmit -p .",
                "studio/worker/mcp.ts(340,30): error TS2551: Property 'lenght' does not exist on type 'AgentMention[]'.",
                "Process completed with exit code 2.",
            ]
        );
        let found = errors_in_log(&lines, "module");
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].path.as_str(), found[0].line),
            ("studio/worker/mcp.ts", 340)
        );
        assert!(found[0].message.starts_with("TS2551"));
    }

    #[test]
    fn reads_compiler_errors_and_snippets() {
        assert_eq!(
            colon_error("src/main.rs:12:5: error: expected `;`"),
            Some(("src/main.rs".into(), 12, "expected `;`".into()))
        );
        assert_eq!(colon_error("Error: something: else"), None);
        assert_eq!(
            colon_error(r"C:\w\lib.c:2:3: error: x"),
            Some((r"C:\w\lib.c".into(), 2, "x".into()))
        );
        assert_eq!(tsc_error("Found 1 error (in 1 file)"), None);
        let src = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let s = snippet(&src, 6, 6);
        assert_eq!(s.first().map(|l| l.0), Some(2));
        assert_eq!(s.last().map(|l| l.0), Some(7));
        assert_eq!(snippet(&src, 1, 1).first().map(|l| l.0), Some(1));
    }

    #[test]
    fn fix_prompt_carries_the_failure() {
        let r = run(
            57,
            "CI",
            "feat",
            "completed",
            "failure",
            "2026-10-13T03:00:00Z",
        );
        let detail = Detail {
            failure: Some(Failure {
                job: 1,
                lines: vec![LogLine {
                    kind: LineKind::Error,
                    text: "boom".into(),
                }],
            }),
            ..Default::default()
        };
        let p = fix_prompt(&r, Some(&detail), "feat");
        assert!(p.contains("#57") && p.contains("`feat`") && p.contains("boom"));
        assert!(!p.contains("This checkout is on"));
        assert!(fix_prompt(&r, None, "main").contains("This checkout is on `main`"));
    }
}
