//! The checks of a pull request: counted once each, their logs, and running failed ones again.

use super::*;
use crate::github::Problem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    Failed,
    /// Queued, waiting or in progress.
    Running,
    /// Skipped or neutral: it says nothing against the change.
    Skipped,
}

/// A check run or a commit status, in one shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRun {
    pub name: String,
    /// The workflow it belongs to; empty for a commit status.
    pub workflow: String,
    pub state: CheckState,
    pub url: String,
    /// The GitHub Actions job behind it, when its address names one: what
    /// a re-run and the failing log need.
    pub actions: Option<ActionsJob>,
}

/// The run (and the job in it) a check's address points at:
/// `https://github.com/o/r/actions/runs/<run>/job/<job>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionsJob {
    /// Lower case. A check's address may be on any site, so the job only
    /// counts on the host `gh` talks to.
    pub host: String,
    pub repo: String,
    pub run: u64,
    pub job: Option<u64>,
}

impl ActionsJob {
    pub fn from_url(url: &str) -> Option<Self> {
        let rest = url.split_once("://")?.1;
        let (before, after) = rest.split_once("/actions/runs/")?;
        // `host/owner/repo`: the last two parts name the repository.
        let mut parts = before.rsplitn(3, '/');
        let (name, owner) = (parts.next()?, parts.next()?);
        let host = parts.next()?.to_ascii_lowercase();
        if host.is_empty() || host.contains('/') {
            return None;
        }
        let repo = format!("{owner}/{name}");
        if !valid_repo(&repo) {
            return None;
        }
        let number = |text: &str| -> Option<u64> {
            let end = text
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(text.len());
            text[..end].parse().ok()
        };
        Some(ActionsJob {
            host,
            repo,
            run: number(after)?,
            job: after.split_once("/job/").and_then(|(_, j)| number(j)),
        })
    }

    /// Whether the job ran in `repo` on `host`, the host `gh` talks to
    /// (`gh_host`): only those are read or run again. The same ids on
    /// another host name another repository's job and run.
    pub fn in_repo(&self, repo: &str, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        (self.host == host || self.host.strip_prefix("www.") == Some(host.as_str()))
            && self.repo.eq_ignore_ascii_case(repo)
    }
}

impl CheckRun {
    /// The Actions job of `repo` on `host` whose log says why this check failed.
    pub fn failed_job(&self, repo: &str, host: &str) -> Option<u64> {
        if self.state != CheckState::Failed {
            return None;
        }
        self.actions.as_ref().filter(|a| a.in_repo(repo, host))?.job
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChecksSummary {
    pub passed: usize,
    pub failed: usize,
    pub running: usize,
    pub skipped: usize,
    /// One entry per check, in GitHub's order.
    pub checks: Vec<CheckRun>,
}

impl ChecksSummary {
    pub fn total(&self) -> usize {
        self.checks.len()
    }

    /// All the checks as one state: any failing fails, any running runs.
    /// None when nothing ran or all were skipped.
    pub fn overall(&self) -> Option<CheckState> {
        if self.failed > 0 {
            Some(CheckState::Failed)
        } else if self.running > 0 {
            Some(CheckState::Running)
        } else if self.passed > 0 {
            Some(CheckState::Passed)
        } else {
            None
        }
    }

    pub fn names(&self, state: CheckState) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| c.state == state)
            .map(|c| c.name.clone())
            .collect()
    }
}

impl Check {
    fn normalised(&self) -> CheckRun {
        // A commit status has a `context` and no `name`.
        if self.name.is_empty() && !self.context.is_empty() {
            let state = match self.state.as_str() {
                "SUCCESS" => CheckState::Passed,
                "PENDING" | "EXPECTED" => CheckState::Running,
                _ => CheckState::Failed,
            };
            return CheckRun {
                name: self.context.clone(),
                workflow: String::new(),
                state,
                url: self.target_url.clone(),
                actions: None,
            };
        }
        let state = if self.status != "COMPLETED" {
            CheckState::Running
        } else {
            match self.conclusion.as_str() {
                "SUCCESS" => CheckState::Passed,
                "SKIPPED" | "NEUTRAL" => CheckState::Skipped,
                _ => CheckState::Failed,
            }
        };
        CheckRun {
            name: self.name.clone(),
            workflow: self.workflow_name.clone(),
            state,
            url: self.details_url.clone(),
            actions: ActionsJob::from_url(&self.details_url),
        }
    }

    fn started(&self) -> u64 {
        secs(&self.started_at).max(secs(&self.completed_at))
    }
}

/// The checks of the head commit, one per check: a check that ran again shows
/// its latest run only. Two workflows may both have a job called `build`; those
/// are two checks, so the workflow is part of what makes a run "the same".
pub fn checks_summary(pull: &Pull) -> ChecksSummary {
    let mut latest: Vec<(u64, CheckRun)> = Vec::new();
    for check in &pull.status_check_rollup {
        let (at, run) = (check.started(), check.normalised());
        match latest
            .iter_mut()
            .find(|(_, c)| c.name == run.name && c.workflow == run.workflow)
        {
            // GitHub lists a rerun after the run it replaces, so a tie goes to the later entry.
            Some(seen) if at >= seen.0 => *seen = (at, run),
            Some(_) => {}
            None => latest.push((at, run)),
        }
    }
    let mut sum = ChecksSummary {
        checks: latest.into_iter().map(|(_, c)| c).collect(),
        ..Default::default()
    };
    for check in &sum.checks {
        match check.state {
            CheckState::Passed => sum.passed += 1,
            CheckState::Failed => sum.failed += 1,
            CheckState::Running => sum.running += 1,
            CheckState::Skipped => sum.skipped += 1,
        }
    }
    sum
}

/// The Actions runs of `repo` on `host` that "Re-run failed" starts again:
/// those with a failed check and none still running (GitHub refuses a run in
/// progress).
pub fn rerunnable(sum: &ChecksSummary, repo: &str, host: &str) -> Vec<u64> {
    let of = |state: CheckState| {
        sum.checks
            .iter()
            .filter(move |c| c.state == state)
            .filter_map(|c| c.actions.as_ref())
            .filter(|a| a.in_repo(repo, host))
            .map(|a| a.run)
    };
    let running: Vec<u64> = of(CheckState::Running).collect();
    let mut runs: Vec<u64> = Vec::new();
    for run in of(CheckState::Failed) {
        if !running.contains(&run) && !runs.contains(&run) {
            runs.push(run);
        }
    }
    runs
}

/// How long reading a job's log or starting a run again may take. A `gh`
/// that hangs would keep the log on "Loading", or every other write waiting.
const CHECK_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Runs `cmd` away from any checkout, killing `gh` after `timeout`.
pub(super) fn gh_run(cmd: &Cmd, timeout: std::time::Duration) -> Result<Vec<u8>, Problem> {
    let stdin = cmd.stdin.as_deref().map(str::as_bytes);
    crate::github::gh_run(&std::env::temp_dir(), &cmd.args, stdin, timeout)
}

/// Log lines kept of a failing job: the end is where it failed.
const CHECK_LOG_LINES: usize = 60;

/// The end of the failing step's log of one job, as the Runs tab shows it.
pub fn check_log(item: &ItemRef, job: u64) -> Result<Vec<crate::ci::LogLine>, Problem> {
    if !valid_repo(&item.repo) {
        return Err(invalid("Not a repository name."));
    }
    let path = format!("repos/{}/actions/jobs/{job}/logs", item.repo);
    let out = crate::github::gh_run(
        &std::env::temp_dir(),
        &["api", "--allow-escape-sequences", &path],
        None,
        CHECK_WAIT,
    )?;
    let lines = crate::ci::parse_log(&String::from_utf8_lossy(&out));
    let mut end = crate::ci::failure_excerpt(&lines);
    if end.len() > CHECK_LOG_LINES {
        end.drain(..end.len() - CHECK_LOG_LINES);
    }
    Ok(end)
}

pub(super) fn rerun_cmd(item: &ItemRef, run: u64) -> Result<Cmd, Problem> {
    pulls_only(item)?;
    if !valid_repo(&item.repo) {
        return Err(invalid("Not a repository name."));
    }
    Ok(Cmd {
        args: strings(&[
            "run",
            "rerun",
            &run.to_string(),
            "--failed",
            "-R",
            &item.repo,
        ]),
        stdin: None,
    })
}

/// Starts the failed jobs of each of `runs` again (`rerunnable`).
pub fn rerun_failed(item: &ItemRef, runs: &[u64]) -> Result<(), Problem> {
    if runs.is_empty() {
        return Err(invalid("No failed run to start again."));
    }
    rerun_each(runs, |run| {
        act_with(crate::github::read_only(), rerun_cmd(item, run), |cmd| {
            gh_run(cmd, CHECK_WAIT)
        })
        .map(|_| ())
    })
}

/// Tries every run, also after one fails: the runs don't depend on each
/// other. When none starts, the first problem is the answer (no `gh`, signed
/// out); when some do, the answer says which started and which didn't.
fn rerun_each(
    runs: &[u64],
    mut start: impl FnMut(u64) -> Result<(), Problem>,
) -> Result<(), Problem> {
    let mut started: Vec<String> = Vec::new();
    let mut failed: Vec<(u64, Problem)> = Vec::new();
    for run in runs {
        match start(*run) {
            Ok(()) => started.push(run.to_string()),
            Err(problem) => failed.push((*run, problem)),
        }
    }
    if failed.is_empty() {
        return Ok(());
    }
    if started.is_empty() {
        return Err(failed.remove(0).1);
    }
    let failed: Vec<String> = failed
        .iter()
        .map(|(run, problem)| format!("run {run} ({})", problem.text()))
        .collect();
    Err(Problem::Other(format!(
        "Started run {} again, but not {}.",
        started.join(", "),
        failed.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::fixtures::*;

    #[test]
    fn checks_are_counted_once_each() {
        let sum = checks_summary(&pull());
        let shape: Vec<(&str, &str, CheckState)> = sum
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.workflow.as_str(), c.state))
            .collect();
        assert_eq!(
            shape,
            [
                // The rerun that passed replaces the run that failed.
                ("Clippy", "CI", CheckState::Passed),
                ("Tests", "CI", CheckState::Running),
                ("Windows", "CI", CheckState::Skipped),
                // Same name, another workflow: another check.
                ("Tests", "Nightly", CheckState::Failed),
                ("license/cla", "", CheckState::Passed),
                ("deploy/preview", "", CheckState::Running),
            ]
        );
        assert_eq!(
            (
                sum.passed,
                sum.failed,
                sum.running,
                sum.skipped,
                sum.total()
            ),
            (2, 1, 2, 1, 6)
        );
        assert_eq!(
            sum.checks[0].url,
            "https://github.com/ArakunHQ/arakun/actions/runs/1/job/2"
        );
        assert_eq!(sum.checks[4].url, "https://cla.example/41");
        assert_eq!(sum.names(CheckState::Running), ["Tests", "deploy/preview"]);
        assert_eq!(sum.overall(), Some(CheckState::Failed));
        let only = |passed, running| ChecksSummary {
            passed,
            running,
            ..Default::default()
        };
        assert_eq!(only(3, 1).overall(), Some(CheckState::Running));
        assert_eq!(only(3, 0).overall(), Some(CheckState::Passed));
        assert_eq!(only(0, 0).overall(), None);

        // The later run wins wherever GitHub lists it.
        let mut p = pull();
        p.status_check_rollup.swap(0, 1);
        assert_eq!(checks_summary(&p).checks[0].state, CheckState::Passed);

        let state = |json: &str| read::<Check>(json.as_bytes()).unwrap().normalised().state;
        assert_eq!(
            state(r#"{"name": "a", "status": "QUEUED"}"#),
            CheckState::Running
        );
        assert_eq!(
            state(r#"{"name": "a", "status": "COMPLETED", "conclusion": "NEUTRAL"}"#),
            CheckState::Skipped
        );
        assert_eq!(
            state(r#"{"name": "a", "status": "COMPLETED", "conclusion": "CANCELLED"}"#),
            CheckState::Failed
        );
        assert_eq!(
            state(r#"{"name": "a", "status": "COMPLETED", "conclusion": "ACTION_REQUIRED"}"#),
            CheckState::Failed
        );
        assert_eq!(
            state(r#"{"context": "a", "state": "ERROR"}"#),
            CheckState::Failed
        );
        assert_eq!(
            state(r#"{"context": "a", "state": "EXPECTED"}"#),
            CheckState::Running
        );
        assert_eq!(checks_summary(&Pull::default()), ChecksSummary::default());
    }

    #[test]
    fn a_run_that_fails_does_not_stop_the_runs_after_it() {
        let mut tried = Vec::new();
        let result = rerun_each(&[7, 8, 9], |run| {
            tried.push(run);
            if run == 8 {
                Err(Problem::Other("it is still running".into()))
            } else {
                Ok(())
            }
        });
        assert_eq!(tried, [7, 8, 9]);
        let Err(Problem::Other(text)) = result else {
            panic!("the failure of run 8 is reported");
        };
        assert_eq!(
            text,
            "Started run 7, 9 again, but not run 8 (it is still running)."
        );
        // Nothing started: the problem itself, such as a missing gh.
        assert!(matches!(
            rerun_each(&[7, 8], |_| Err(Problem::NoGh)),
            Err(Problem::NoGh)
        ));
        assert!(rerun_each(&[7, 8], |_| Ok(())).is_ok());
    }

    #[test]
    fn a_check_names_its_actions_job() {
        let a = ActionsJob::from_url(
            "https://github.com/ArakunHQ/arakun/actions/runs/12/job/34?pr=41#step:3:1",
        )
        .unwrap();
        assert_eq!(
            (a.repo.as_str(), a.run, a.job),
            ("ArakunHQ/arakun", 12, Some(34))
        );
        assert!(a.in_repo("arakunhq/ARAKUN", "GitHub.com"));
        assert!(!a.in_repo("ArakunHQ/other", "github.com"));
        // The same repository name and ids on another host are another job.
        assert!(!a.in_repo("ArakunHQ/arakun", "ghe.example"));
        // A run's own page names no job; another host still names its repository.
        let run = ActionsJob::from_url("https://ghe.example/o/r/actions/runs/7").unwrap();
        assert_eq!((run.repo.as_str(), run.run, run.job), ("o/r", 7, None));
        // Enterprise stays valid when it is the host gh talks to.
        assert!(run.in_repo("o/r", "ghe.example"));
        assert!(!run.in_repo("o/r", "github.com"));
        for other in [
            "https://x/5",
            "https://cla.example/41",
            "https://github.com/actions/runs/1",
            "https://github.com/o/r/actions/runs/latest",
            "github.com/o/r/actions/runs/1",
            "",
        ] {
            assert_eq!(ActionsJob::from_url(other), None, "{other}");
        }
    }

    #[test]
    fn only_finished_failed_runs_start_again() {
        let check = |name: &str, status: &str, conclusion: &str, url: &str| Check {
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.into(),
            details_url: url.into(),
            workflow_name: "CI".into(),
            ..Default::default()
        };
        let here = "https://github.com/ArakunHQ/arakun/actions/runs";
        let p = Pull {
            head_ref_name: "pr/resize".into(),
            head_ref_oid: SHA.into(),
            status_check_rollup: vec![
                check(
                    "Clippy",
                    "COMPLETED",
                    "FAILURE",
                    &format!("{here}/7/job/70"),
                ),
                check(
                    "Tests",
                    "COMPLETED",
                    "CANCELLED",
                    &format!("{here}/7/job/71"),
                ),
                check("Fmt", "COMPLETED", "SUCCESS", &format!("{here}/7/job/72")),
                // Run 8 still works: GitHub would refuse to start it again.
                check("Docs", "COMPLETED", "FAILURE", &format!("{here}/8/job/80")),
                check("Site", "IN_PROGRESS", "", &format!("{here}/8/job/81")),
                // Not this repository's run, and not an Actions job at all.
                check(
                    "Fork",
                    "COMPLETED",
                    "FAILURE",
                    "https://github.com/else/where/actions/runs/9/job/90",
                ),
                check("Vendor", "COMPLETED", "FAILURE", "https://ci.example/1"),
            ],
            ..Default::default()
        };
        let sum = checks_summary(&p);
        assert_eq!(rerunnable(&sum, "arakunhq/arakun", "github.com"), [7]);
        assert!(rerunnable(&sum, "arakunhq/arakun", "ghe.example").is_empty());
        let jobs: Vec<(u64, &str)> = sum
            .checks
            .iter()
            .filter_map(|c| {
                Some((
                    c.failed_job("ArakunHQ/arakun", "github.com")?,
                    c.name.as_str(),
                ))
            })
            .collect();
        // Not the job that passed, the one still running or the fork's.
        assert_eq!(jobs, [(70, "Clippy"), (71, "Tests"), (80, "Docs")]);
        // The fixture's only failure isn't an Actions job: nothing to start.
        assert!(rerunnable(&checks_summary(&pull()), "ArakunHQ/arakun", "github.com").is_empty());

        let pr = item(ItemKind::Pull);
        assert_eq!(
            rerun_cmd(&pr, 7).unwrap().args.join(" "),
            "run rerun 7 --failed -R ArakunHQ/arakun"
        );
        assert!(rerun_cmd(&item(ItemKind::Issue), 7).is_err());
        assert!(rerun_failed(&pr, &[]).is_err());

        let log = [crate::ci::LogLine {
            kind: crate::ci::LineKind::Error,
            text: "error: unused variable `x`".into(),
        }];
        let failed: Vec<(&CheckRun, Option<&[crate::ci::LogLine]>)> = sum
            .checks
            .iter()
            .filter(|c| c.state == CheckState::Failed)
            .map(|c| (c, (c.name == "Clippy").then_some(&log[..])))
            .collect();
        let prompt = fix_checks_prompt(&pr, "Keep the tab name", &p, &failed);
        assert!(
            prompt.contains("failed on branch `pr/resize` at 0123456"),
            "{prompt}"
        );
        assert!(
            prompt.contains(&format!("- Clippy (CI): {here}/7/job/70")),
            "{prompt}"
        );
        assert!(
            prompt.contains("- Vendor (CI): https://ci.example/1"),
            "{prompt}"
        );
        assert!(
            prompt.contains("log of Clippy:\n```\nerror: unused variable `x`\n```"),
            "{prompt}"
        );
        assert!(!prompt.contains("log of Tests"), "{prompt}");
        assert!(
            prompt.contains("`git switch -c pr/resize FETCH_HEAD`"),
            "{prompt}"
        );
        assert!(prompt.ends_with("Don't push."), "{prompt}");
    }
}
