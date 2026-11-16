//! What `arakun server` keeps across runs, in `state.json` of its folder: the
//! folders sessions can start in (its places) and the sessions it ran. Each
//! session's transcript is a file of its own in `sessions/`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use crate::agent::{AgentKind, SessionMeta, Setting};
use crate::model::Status;

/// A folder sessions can start in.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Place {
    pub path: PathBuf,
    /// The agent a session here starts with when none is named.
    pub agent: AgentKind,
    /// The settings the next session of each agent starts with here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub picks: Vec<Pick>,
}

/// A setting picked for an agent: `key` is [`Setting::key`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pick {
    pub agent: AgentKind,
    pub key: String,
    pub value: String,
}

impl Place {
    /// The folder's name, which stands for the project.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// What the next `agent` session starts with here, the model first: the
    /// reasoning levels an agent offers depend on its model.
    pub fn picked(&self, agent: AgentKind) -> Vec<(Setting, String)> {
        let mut picked: Vec<(Setting, String)> = self
            .picks
            .iter()
            .filter(|p| p.agent == agent)
            .filter_map(|p| Some((Setting::from_key(&p.key)?, p.value.clone())))
            .collect();
        picked.sort_by_key(|(s, _)| *s != Setting::Model);
        picked
    }

    /// Remembers a setting for the next `agent` session here. Another model
    /// forgets the reasoning picked for the old one.
    pub fn pick(&mut self, agent: AgentKind, s: Setting, value: &str) {
        self.picks.retain(|p| {
            p.agent != agent
                || (p.key != s.key() && !(s == Setting::Model && p.key == Setting::Effort.key()))
        });
        self.picks.push(Pick {
            agent,
            key: s.key().into(),
            value: value.into(),
        });
    }
}

fn idle() -> Status {
    Status::Idle
}

/// A session, as the list shows it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Record {
    pub id: String,
    pub title: String,
    pub agent: AgentKind,
    /// The folder it works in.
    pub place: PathBuf,
    pub created: u64,
    pub updated: u64,
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub removed: u32,
    /// The agent's own session, which is resumed when this one runs again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<SessionMeta>,
    /// Its last turn failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    /// Never read from the file: nothing runs when the server starts, so a
    /// session is idle then, or failed.
    #[serde(skip, default = "idle")]
    pub status: Status,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Store {
    #[serde(default)]
    pub places: Vec<Place>,
    /// Newest first.
    #[serde(default)]
    pub sessions: Vec<Record>,
    #[serde(skip)]
    dir: PathBuf,
}

impl Store {
    /// What `dir` holds; empty when it holds nothing yet. A file that is there
    /// but cannot be read is an error, never an empty store: the next save
    /// would write that emptiness over every session and folder on record.
    pub fn load(dir: &Path) -> Result<Store, String> {
        let path = dir.join("state.json");
        let mut store: Store = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                format!(
                    "{} is not what the server wrote ({e}); move it aside to start with nothing",
                    path.display()
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        store.dir = dir.to_path_buf();
        for s in &mut store.sessions {
            s.status = if s.failed {
                Status::Error
            } else {
                Status::Idle
            };
        }
        Ok(store)
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        for s in &mut self.sessions {
            s.failed = s.status == Status::Error;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        // Prompts and titles are the user's own: no other user of the machine reads them.
        crate::durable::write_private(&self.dir.join("state.json"), &bytes)
    }

    pub fn transcript_path(&self, id: &str) -> PathBuf {
        self.dir.join("sessions").join(format!("{id}.json"))
    }

    /// Where the photos that come with prompts are saved.
    pub fn uploads_dir(&self) -> PathBuf {
        self.dir.join("uploads")
    }

    pub fn place(&self, path: &Path) -> Option<&Place> {
        self.places.iter().find(|p| p.path == path)
    }

    pub fn place_mut(&mut self, path: &Path) -> Option<&mut Place> {
        self.places.iter_mut().find(|p| p.path == path)
    }

    /// Adds the folder at `path` (canonical) as a place; `false` when it is one.
    pub fn add_place(&mut self, path: &Path, agent: AgentKind) -> bool {
        if self.place(path).is_some() {
            return false;
        }
        self.places.push(Place {
            path: path.to_path_buf(),
            agent,
            picks: Vec::new(),
        });
        true
    }

    /// Takes a place off the list. Its sessions stay: they still have their
    /// transcripts, and can go on in their folder.
    pub fn remove_place(&mut self, path: &Path) -> bool {
        let before = self.places.len();
        self.places.retain(|p| p.path != path);
        self.places.len() != before
    }

    pub fn session(&self, id: &str) -> Option<&Record> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn session_mut(&mut self, id: &str) -> Option<&mut Record> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// The id of the session `id` names: its id, or the start of exactly one.
    /// What a client sends never becomes a file name before it passed here.
    pub fn find(&self, id: &str) -> Result<String, String> {
        if id.is_empty() {
            return Err(format!("which session? `{} ls` lists them", super::name()));
        }
        if let Some(s) = self.session(id) {
            return Ok(s.id.clone());
        }
        let hits: Vec<&Record> = self
            .sessions
            .iter()
            .filter(|s| s.id.starts_with(id))
            .collect();
        match hits[..] {
            [s] => Ok(s.id.clone()),
            [] => Err(format!("no session {id}")),
            _ => Err(format!("more than one session starts with {id}")),
        }
    }

    /// Forgets a session and its transcript.
    pub fn remove_session(&mut self, id: &str) {
        self.sessions.retain(|s| s.id != id);
        let _ = std::fs::remove_file(self.transcript_path(id));
    }
}

/// An id no other session of this server has: the time, and a count for two
/// that start in the same millisecond.
pub fn new_id() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("s{millis:x}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// The branch checked out in `dir`, read from git's own files: the inbox is
/// built often, and this starts no process. `None` outside a repository and
/// on a detached HEAD.
pub fn branch(dir: &Path) -> Option<String> {
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // A worktree: `.git` is a file that names its real folder.
        let text = std::fs::read_to_string(&dot_git).ok()?;
        dir.join(text.trim().strip_prefix("gitdir:")?.trim())
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    Some(head.trim().strip_prefix("ref: refs/heads/")?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-server-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(id: &str, status: Status) -> Record {
        Record {
            id: id.into(),
            title: "t".into(),
            agent: AgentKind::Claude,
            place: "/w/app".into(),
            created: 1,
            updated: 2,
            added: 0,
            removed: 0,
            meta: None,
            failed: false,
            status,
        }
    }

    #[test]
    fn a_saved_session_is_never_working_when_read_back() {
        let dir = temp("store");
        let mut store = Store::load(&dir).unwrap();
        assert!(store.add_place(Path::new("/w/app"), AgentKind::Claude));
        assert!(!store.add_place(Path::new("/w/app"), AgentKind::Droid));
        store.sessions.push(record("s1-0", Status::Working));
        store.sessions.push(record("s2-0", Status::Error));
        store.sessions.push(record("s3-0", Status::NeedsInput));
        store.save().unwrap();

        let back = Store::load(&dir).unwrap();
        assert_eq!(back.places, store.places);
        let statuses: Vec<Status> = back.sessions.iter().map(|s| s.status).collect();
        assert_eq!(statuses, [Status::Idle, Status::Error, Status::Idle]);
        assert_eq!(
            back.transcript_path("s1-0"),
            dir.join("sessions").join("s1-0.json")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_state_that_cannot_be_read_is_left_alone() {
        let dir = temp("broken");
        let file = dir.join("state.json");
        std::fs::write(&file, b"{ \"sessions\": [ half a file").unwrap();
        let e = Store::load(&dir).err().unwrap();
        assert!(
            e.contains("state.json") && e.contains("move it aside"),
            "{e}"
        );
        // Nothing was written over it: the sessions can still be recovered by hand.
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"{ \"sessions\": [ half a file"
        );
        // No file at all is a server that has not run yet.
        std::fs::remove_file(&file).unwrap();
        assert!(Store::load(&dir).unwrap().sessions.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_is_found_by_its_id_or_the_start_of_one() {
        let mut store = Store::default();
        store.sessions.push(record("s1a-0", Status::Idle));
        store.sessions.push(record("s1b-0", Status::Idle));
        store.sessions.push(record("s2-0", Status::Idle));
        assert_eq!(store.find("s2").unwrap(), "s2-0");
        assert_eq!(store.find("s1a-0").unwrap(), "s1a-0");
        assert!(store.find("s1").unwrap_err().contains("more than one"));
        assert!(store.find("x").unwrap_err().contains("no session"));
        // Never a path: only what the list holds comes back.
        assert!(store.find("../s2-0").is_err());
        assert!(store.find("").is_err());
    }

    #[test]
    fn picks_are_kept_per_agent_with_the_model_first() {
        let mut place = Place {
            path: "/w/app".into(),
            agent: AgentKind::Claude,
            picks: Vec::new(),
        };
        place.pick(AgentKind::Claude, Setting::Effort, "high");
        place.pick(AgentKind::Claude, Setting::Model, "opus");
        place.pick(AgentKind::Droid, Setting::Model, "gpt");
        // The model changed: the reasoning picked for the old one is gone.
        assert_eq!(
            place.picked(AgentKind::Claude),
            [(Setting::Model, "opus".to_string())]
        );
        place.pick(AgentKind::Claude, Setting::Effort, "low");
        place.pick(AgentKind::Claude, Setting::Effort, "max");
        assert_eq!(
            place.picked(AgentKind::Claude),
            [
                (Setting::Model, "opus".to_string()),
                (Setting::Effort, "max".to_string())
            ]
        );
        assert_eq!(
            place.picked(AgentKind::Droid),
            [(Setting::Model, "gpt".to_string())]
        );
        assert_eq!(place.name(), "app");
    }

    #[test]
    fn the_branch_is_read_from_a_checkout_and_from_a_worktree() {
        let dir = temp("branch");
        assert_eq!(branch(&dir), None);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(branch(&repo).as_deref(), Some("main"));

        let wt = dir.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        std::fs::write(
            repo.join(".git/worktrees/wt/HEAD"),
            "ref: refs/heads/fix/a-b\n",
        )
        .unwrap();
        assert_eq!(branch(&wt).as_deref(), Some("fix/a-b"));

        // Detached: a commit, not a branch.
        std::fs::write(repo.join(".git/HEAD"), "0123abcd\n").unwrap();
        assert_eq!(branch(&repo), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ids_differ_within_a_millisecond() {
        assert_ne!(new_id(), new_id());
    }
}
