//! Where the Changes panel runs git for an environment: in its folder here,
//! or on the paired server that has the folder ([`GitAt`]). A server runs the
//! same [`Call`] in its folder and answers with a `git` message that carries
//! the request's `ref`; [`Asks`] pairs the two up.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::client::Sender;
use crate::git;
use crate::git_call::{Call, CommitSource, DiffText, NewPr, PrSource, Status};

/// How much longer than the server's own limit the app waits for an answer:
/// the way there and back.
const TRAVEL: Duration = Duration::from_secs(30);

/// Said when a server knows `git` requests, but not this one yet.
pub const TOO_OLD_FOR: &str =
    "This server runs an older Arakun that can't do this. Update it first.";

/// Said when a server does not know the `git` request yet.
pub const OLDER: &str =
    "This server runs an older Arakun. Update it to see and commit its changes from here.";

/// Where git runs for an environment. Each method blocks: call it off the
/// main thread, as the local git calls are.
#[derive(Clone)]
pub enum GitAt {
    Here(PathBuf),
    Server(ServerGit),
}

/// A folder on a paired server, and the way to ask it.
#[derive(Clone)]
pub struct ServerGit {
    pub server: String,
    pub place: String,
    /// None once the server is no longer paired.
    pub link: Option<Sender>,
    pub asks: Asks,
}

impl GitAt {
    pub fn on_server(&self) -> bool {
        matches!(self, GitAt::Server(_))
    }

    /// Runs `call` where the folder is and reads what it found.
    fn call<T: DeserializeOwned>(&self, call: Call) -> Result<T> {
        let value = match self {
            GitAt::Here(dir) => call.run(dir)?,
            GitAt::Server(s) => s.ask(call)?,
        };
        Ok(serde_json::from_value(value)?)
    }

    /// The changes and where the branch stands, each with its own error
    /// here. A server reads both at once.
    pub fn status(&self) -> (Result<Vec<git::Change>>, Result<git::Sync>) {
        match self {
            GitAt::Here(dir) => (git::status(dir), git::sync(dir)),
            GitAt::Server(_) => match self.call::<Status>(Call::Status) {
                Ok(s) => (Ok(s.changes), Ok(s.sync)),
                Err(e) => {
                    let again = anyhow!("{e}");
                    (Err(e), Err(again))
                }
            },
        }
    }

    pub fn diff(&self, path: &str, untracked: bool, limit: Option<u64>) -> Result<DiffText> {
        self.call(Call::Diff {
            path: path.into(),
            untracked,
            limit,
        })
    }

    pub fn stage(&self, path: &str) -> Result<()> {
        self.call(Call::Stage { path: path.into() })
    }

    pub fn unstage(&self, path: &str) -> Result<()> {
        self.call(Call::Unstage { path: path.into() })
    }

    pub fn discard(&self, change: &git::Change) -> Result<()> {
        self.call(Call::Discard {
            change: change.clone(),
        })
    }

    pub fn commit(&self, message: &str, all: bool) -> Result<()> {
        self.call(Call::Commit {
            message: message.into(),
            all,
        })
    }

    pub fn push(&self, remote: &str, branch: &str) -> Result<()> {
        self.call(Call::Push {
            remote: remote.into(),
            branch: branch.into(),
        })
    }

    pub fn pull(&self) -> Result<()> {
        self.call(Call::Pull)
    }

    pub fn fetch(&self) -> Result<()> {
        self.call(Call::Fetch)
    }

    /// The branch's pull request, or None when it has none. An error says
    /// only that the server could not be asked.
    pub fn pr_view(&self) -> Result<Option<git::PullRequest>> {
        self.call(Call::PrView)
    }

    /// What a new pull request starts with. Where git can't tell, the
    /// default one, as `git::pr_plan` gives; an error only when the server
    /// could not be asked.
    pub fn pr_plan(&self, remote: &str, branch: &str) -> Result<git::PrPlan> {
        self.call(Call::PrPlan {
            remote: remote.into(),
            branch: branch.into(),
            onto: None,
        })
    }

    /// The plan for base `onto`, or None when the remote has no such branch.
    pub fn pr_plan_onto(
        &self,
        remote: &str,
        branch: &str,
        onto: &str,
    ) -> Result<Option<git::PrPlan>> {
        self.call(Call::PrPlan {
            remote: remote.into(),
            branch: branch.into(),
            onto: Some(onto.into()),
        })
    }

    /// Opens the pull request; with `web`, GitHub's form in the browser
    /// instead, which only works here.
    pub fn create_pr(&self, pr: NewPr, web: bool) -> Result<Option<String>> {
        match self {
            GitAt::Here(dir) => crate::git_call::create_pr(dir, &pr, web),
            GitAt::Server(_) if web => Err(anyhow!(
                "GitHub's form opens in a browser on this computer, and the branch is on the server."
            )),
            GitAt::Server(_) => self.call(Call::PrCreate(pr)),
        }
    }

    pub fn pr_source(
        &self,
        remote: &str,
        base: &str,
        branch: &str,
        max_diff: usize,
    ) -> Result<PrSource> {
        match self {
            GitAt::Here(dir) => Ok(crate::git_call::pr_source(
                dir, remote, base, branch, max_diff,
            )),
            GitAt::Server(_) => self.call(Call::PrSource {
                remote: remote.into(),
                base: base.into(),
                branch: branch.into(),
                max_diff,
            }),
        }
    }

    /// What a commit of all changes, or of the staged ones, would hold.
    pub fn commit_source(&self, all: bool, max_diff: usize) -> Result<CommitSource> {
        self.call(Call::CommitSource { all, max_diff })
    }
}

impl From<&Path> for GitAt {
    fn from(dir: &Path) -> GitAt {
        GitAt::Here(dir.to_path_buf())
    }
}

impl ServerGit {
    fn ask(&self, call: Call) -> Result<Value> {
        let Some(link) = &self.link else {
            return Err(anyhow!("This server is no longer paired."));
        };
        let wait = call.limit() + TRAVEL;
        let op = serde_json::to_value(&call)?["op"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (reference, answer) = self.asks.open(&self.server, &op)?;
        let msg = json!({
            "type": "git", "ref": reference, "place": self.place, "call": call,
        });
        link.send(&msg);
        match answer.recv_timeout(wait) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(Failed { why, timed_out })) if timed_out => Err(late(why)),
            Ok(Err(Failed { why, .. })) => Err(anyhow!("{why}")),
            Err(_) => {
                self.asks.forget(reference);
                // The server may still finish a change it is slow to answer.
                Err(late(if reads(&msg["call"]) {
                    "The server did not answer in time.".into()
                } else {
                    "The server did not answer in time, and may still finish. Check its changes before you try again.".into()
                }))
            }
        }
    }
}

type Answer = std::result::Result<Value, Failed>;

/// Why a request failed, and whether git (or the wait for it) ran out of
/// time: then the status reads pause, as they do for a folder here.
#[derive(Debug, PartialEq, Eq)]
struct Failed {
    why: String,
    timed_out: bool,
}

impl From<&str> for Failed {
    fn from(why: &str) -> Failed {
        Failed {
            why: why.into(),
            timed_out: false,
        }
    }
}

/// An error `git::timed_out` knows, saying `why`.
fn late(why: String) -> anyhow::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, why).into()
}

/// The `git` requests out to servers, by `ref`, until each is answered.
/// Shared between the shell, which hears the answers, and the threads that
/// wait on them.
#[derive(Clone, Default)]
pub struct Asks(Arc<Mutex<Waiting>>);

#[derive(Default)]
struct Waiting {
    next: u64,
    /// Each request's server, its `op`, and where its answer goes.
    out: HashMap<u64, (String, String, mpsc::Sender<Answer>)>,
    /// Servers that said they do not know the request: none is sent there
    /// until they connect again, maybe updated.
    older: HashSet<String>,
}

impl Asks {
    fn lock(&self) -> std::sync::MutexGuard<'_, Waiting> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new request `op` to `server`: its `ref`, and where its answer comes.
    fn open(&self, server: &str, op: &str) -> Result<(u64, mpsc::Receiver<Answer>)> {
        let mut w = self.lock();
        if w.older.contains(server) {
            return Err(anyhow!(OLDER));
        }
        w.next += 1;
        let reference = w.next;
        let (tx, rx) = mpsc::channel();
        w.out
            .insert(reference, (server.to_string(), op.to_string(), tx));
        Ok((reference, rx))
    }

    fn forget(&self, reference: u64) {
        self.lock().out.remove(&reference);
    }

    /// A `git` message from `server`: the answer to one of its requests.
    pub fn answered(&self, server: &str, msg: &Value) {
        let Some(reference) = msg["ref"].as_u64() else {
            return;
        };
        let mut w = self.lock();
        // Only the server it went to answers it.
        if w.out.get(&reference).is_none_or(|(s, _, _)| s != server) {
            return;
        }
        let Some((_, _, tx)) = w.out.remove(&reference) else {
            return;
        };
        let answer = match msg["error"].as_str() {
            Some(error) => Err(Failed {
                why: error.to_string(),
                timed_out: msg["timed_out"] == true,
            }),
            None => Ok(msg["ok"].clone()),
        };
        let _ = tx.send(answer);
    }

    /// A request that did not reach `server`, or may not have.
    pub fn undelivered(&self, server: &str, msg: &Value) {
        let Some(reference) = msg["ref"].as_u64() else {
            return;
        };
        let mut w = self.lock();
        if w.out.get(&reference).is_none_or(|(s, _, _)| s != server) {
            return;
        }
        if let Some((_, _, tx)) = w.out.remove(&reference) {
            let _ = tx.send(Err(dropped(&msg["call"]).as_str().into()));
        }
    }

    /// Every request out to `server` ends with `why`: its connection dropped,
    /// or it is no longer paired.
    pub fn fail(&self, server: &str, why: &str) {
        let mut w = self.lock();
        let ended: Vec<u64> = w
            .out
            .iter()
            .filter(|(_, (s, _, _))| s == server)
            .map(|(r, _)| *r)
            .collect();
        for reference in ended {
            if let Some((_, _, tx)) = w.out.remove(&reference) {
                let _ = tx.send(Err(why.into()));
            }
        }
    }

    /// `server` does not know the request: the ones out fail, and none is
    /// sent there until it connects again.
    pub fn older(&self, server: &str) {
        self.lock().older.insert(server.to_string());
        self.fail(server, OLDER);
    }

    /// Whether `message` is `server` refusing a request it is too old to
    /// know (`bad message: unknown variant `commit_source``): those requests
    /// fail at once, and the others still go there.
    pub fn refused(&self, server: &str, message: &str) -> bool {
        let Some(op) = message
            .strip_prefix("bad message: unknown variant `")
            .and_then(|rest| rest.split_once('`'))
            .map(|(op, _)| op)
        else {
            return false;
        };
        let mut w = self.lock();
        let ended: Vec<u64> = w
            .out
            .iter()
            .filter(|(_, (s, o, _))| s == server && o == op)
            .map(|(r, _)| *r)
            .collect();
        for reference in &ended {
            if let Some((_, _, tx)) = w.out.remove(reference) {
                let _ = tx.send(Err(TOO_OLD_FOR.into()));
            }
        }
        !ended.is_empty()
    }

    /// `server` is connected again, maybe updated.
    pub fn back(&self, server: &str) {
        self.lock().older.remove(server);
    }
}

/// Why a request that may not have reached the server failed. One that
/// changes something may have run all the same.
fn dropped(call: &Value) -> String {
    if reads(call) {
        "The server could not be reached.".into()
    } else {
        "The connection to the server dropped. Check its changes before you try again.".into()
    }
}

/// Whether `call` (as sent) only reads, so trying again changes nothing.
fn reads(call: &Value) -> bool {
    let reads = [
        "status",
        "diff",
        "pr_view",
        "pr_plan",
        "pr_source",
        "commit_source",
        "fetch",
    ];
    call["op"].as_str().is_some_and(|op| reads.contains(&op))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_goes_to_the_request_with_its_ref() {
        let asks = Asks::default();
        let (a, ra) = asks.open("s1", "status").unwrap();
        let (b, rb) = asks.open("s1", "status").unwrap();
        asks.answered("s1", &json!({ "type": "git", "ref": b, "ok": 2 }));
        asks.answered("s1", &json!({ "type": "git", "ref": a, "error": "no" }));
        assert_eq!(rb.try_recv().unwrap(), Ok(json!(2)));
        assert_eq!(ra.try_recv().unwrap(), Err("no".into()));
    }

    #[test]
    fn another_server_cannot_answer_a_request() {
        let asks = Asks::default();
        let (a, ra) = asks.open("s1", "status").unwrap();
        asks.answered("s2", &json!({ "type": "git", "ref": a, "ok": 1 }));
        assert!(ra.try_recv().is_err());
        asks.undelivered("s2", &json!({ "type": "git", "ref": a }));
        assert!(ra.try_recv().is_err());
        asks.answered("s1", &json!({ "type": "git", "ref": a, "ok": 1 }));
        assert_eq!(ra.try_recv().unwrap(), Ok(json!(1)));
    }

    #[test]
    fn a_drop_ends_the_requests_of_that_server_only() {
        let asks = Asks::default();
        let (_, r1) = asks.open("s1", "status").unwrap();
        let (_, r2) = asks.open("s2", "status").unwrap();
        asks.fail("s1", "dropped");
        assert_eq!(r1.try_recv().unwrap(), Err("dropped".into()));
        assert!(r2.try_recv().is_err());
    }

    #[test]
    fn an_older_server_is_not_asked_until_it_is_back() {
        let asks = Asks::default();
        let (_, r) = asks.open("s1", "status").unwrap();
        asks.older("s1");
        assert_eq!(r.try_recv().unwrap(), Err(OLDER.into()));
        assert!(asks.open("s1", "status").is_err());
        assert!(asks.open("s2", "status").is_ok());
        asks.back("s1");
        assert!(asks.open("s1", "status").is_ok());
    }

    #[test]
    fn an_undelivered_change_says_to_look_first() {
        let asks = Asks::default();
        let (a, ra) = asks.open("s1", "status").unwrap();
        let (b, rb) = asks.open("s1", "status").unwrap();
        asks.undelivered(
            "s1",
            &json!({ "type": "git", "ref": a, "call": { "op": "status" } }),
        );
        asks.undelivered(
            "s1",
            &json!({ "type": "git", "ref": b, "call": { "op": "push" } }),
        );
        assert_eq!(
            ra.try_recv().unwrap(),
            Err("The server could not be reached.".into())
        );
        assert!(
            rb.try_recv()
                .unwrap()
                .unwrap_err()
                .why
                .contains("Check its changes")
        );
    }

    #[test]
    fn a_server_timeout_reads_as_one() {
        let asks = Asks::default();
        let (a, ra) = asks.open("s1", "status").unwrap();
        asks.answered(
            "s1",
            &json!({ "type": "git", "ref": a, "error": "slow", "timed_out": true }),
        );
        let failed = ra.try_recv().unwrap().unwrap_err();
        assert!(failed.timed_out);
        assert!(git::timed_out(&late(failed.why)));
    }

    #[test]
    fn a_folder_here_runs_git_itself() {
        let dir = std::env::temp_dir().join(format!("arakun-git-at-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git::init(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let at = GitAt::from(dir.as_path());
        let (changes, sync) = at.status();
        assert_eq!(changes.unwrap().len(), 1);
        assert!(sync.is_ok());
        at.stage("a.txt").unwrap();
        assert!(at.status().0.unwrap()[0].staged);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_too_old_for_one_request_fails_only_that_one() {
        let asks = Asks::default();
        let (_, status) = asks.open("s1", "status").unwrap();
        let (_, write) = asks.open("s1", "commit_source").unwrap();
        let (_, elsewhere) = asks.open("s2", "commit_source").unwrap();
        let refusal = "bad message: unknown variant `commit_source`, expected one of `status`";
        assert!(asks.refused("s1", refusal));
        assert_eq!(write.try_recv().unwrap(), Err(TOO_OLD_FOR.into()));
        assert!(status.try_recv().is_err() && elsewhere.try_recv().is_err());
        // The server still takes the requests it knows.
        assert!(asks.open("s1", "status").is_ok());
        assert!(!asks.refused("s1", refusal));
        assert!(!asks.refused("s1", "bad message: missing field `ref`"));
        assert!(reads(&json!({ "op": "commit_source" })));
    }
}
