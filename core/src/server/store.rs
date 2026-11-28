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
    // Not a secret: the setting's name.
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

/// An environment whose deletion started. It is saved before git touches
/// anything, so a server stopped part way finishes the deletion when it
/// starts again instead of leaving the folder, its branch or its place.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Deletion {
    /// The worktree's folder, as the place names it.
    pub place: PathBuf,
    /// The repository's shared git files: once the folder is gone, only they
    /// still say where the branch is.
    pub repo: PathBuf,
    /// The branch that goes with it, when the user chose to delete it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Store {
    #[serde(default)]
    pub places: Vec<Place>,
    /// Newest first.
    #[serde(default)]
    pub sessions: Vec<Record>,
    /// Deletions started and not finished yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleting: Vec<Deletion>,
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

    /// Where the whole conversation of session `id` is kept as markdown once
    /// another agent continued it: the new agent is pointed there.
    pub fn linked_path(&self, id: &str) -> PathBuf {
        self.dir.join("linked").join(format!("{id}.md"))
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

    /// Notes that the deletion of `d.place` started. Saved by the caller.
    pub fn begin_deletion(&mut self, d: Deletion) {
        self.deleting.retain(|o| o.place != d.place);
        self.deleting.push(d);
    }

    /// The deletion of `place` is over, done or not; `false` when none was noted.
    pub fn end_deletion(&mut self, place: &Path) -> bool {
        let before = self.deleting.len();
        self.deleting.retain(|d| d.place != place);
        self.deleting.len() != before
    }

    /// Forgets a session and its transcript. Its linked note stays: the
    /// session that took over from it reads it.
    pub fn remove_session(&mut self, id: &str) {
        self.sessions.retain(|s| s.id != id);
        let _ = std::fs::remove_file(self.transcript_path(id));
    }

    /// The sessions done and idle for `days`: not working or waiting for an
    /// answer, and last used that long ago. The daemon leaves out the ones a
    /// device shows or whose agent still runs.
    pub fn idle_for(&self, days: u16, now: u64) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|s| !matches!(s.status, Status::Working | Status::NeedsInput))
            .filter(|s| days > 0 && now.saturating_sub(s.updated) >= u64::from(days) * 86_400)
            .map(|s| s.id.clone())
            .collect()
    }

    /// What [`Housekeeping::run`] needs, taken now so it can run off the
    /// daemon's thread. `also` are sessions running that the list may not
    /// hold yet.
    pub fn housekeeping<'a>(&self, also: impl IntoIterator<Item = &'a String>) -> Housekeeping {
        let mut listed: std::collections::HashSet<String> =
            self.sessions.iter().map(|s| s.id.clone()).collect();
        listed.extend(also.into_iter().cloned());
        Housekeeping {
            dir: self.dir.clone(),
            listed,
        }
    }
}

/// How long a photo sent to the server stays. A prompt that named it has been
/// read by its agent long before.
pub const UPLOAD_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 86_400);
/// How long the transcript of a session the server no longer lists stays,
/// such as one a crash left between writing it and saving the list.
pub const ORPHAN_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 86_400);

/// The server's files no session needs any more.
pub struct Housekeeping {
    dir: PathBuf,
    listed: std::collections::HashSet<String>,
}

impl Housekeeping {
    /// Removes old uploads, the transcripts of sessions the server does not
    /// list, and the temp files of writes that died. Linked notes stay: a
    /// session that took over from one no longer listed still reads its note.
    /// Slow on a big folder: call it off the daemon's thread.
    pub fn run(self) -> usize {
        use crate::housekeep::{remove_old, remove_write_leftovers};
        let unlisted = |name: &str| {
            name.strip_suffix(".json")
                .is_some_and(|id| is_id(id) && !self.listed.contains(id))
        };
        let (sessions, linked, uploads) = (
            self.dir.join("sessions"),
            self.dir.join("linked"),
            self.dir.join("uploads"),
        );
        let mut removed = remove_old(&uploads, UPLOAD_AGE, |n| n.starts_with("photo-"));
        removed += remove_old(&sessions, ORPHAN_AGE, unlisted);
        for dir in [&self.dir, &sessions, &linked, &uploads] {
            removed += remove_write_leftovers(dir);
        }
        removed
    }
}

/// Whether `id` is one [`new_id`] makes (`s<hex>-<n>`), so a file someone
/// else put in the folder is never taken for a session's.
fn is_id(id: &str) -> bool {
    id.strip_prefix('s')
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(millis, n)| {
            !millis.is_empty()
                && millis.bytes().all(|b| b.is_ascii_hexdigit())
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
        })
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
    let head = std::fs::read_to_string(git_dir(dir)?.join("HEAD")).ok()?;
    Some(head.trim().strip_prefix("ref: refs/heads/")?.to_string())
}

/// The folder git keeps `dir`'s checkout in: `.git`, or for a worktree the
/// folder its `.git` file names.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    Some(dir.join(text.trim().strip_prefix("gitdir:")?.trim()))
}

/// The repository a folder is a checkout of: its main folder, which every
/// worktree of it shares, and the URL of its `origin`. Read from git's files,
/// as `branch` is: the inbox is built often.
pub fn repo(dir: &Path) -> Option<(PathBuf, Option<String>)> {
    let git = git_dir(dir)?;
    // A worktree's folder says where the shared one is (`../..`).
    let common = match std::fs::read_to_string(git.join("commondir")) {
        Ok(rel) => git.join(rel.trim()),
        Err(_) => git,
    };
    let common = common.canonicalize().unwrap_or(common);
    // Only `<root>/.git` sits in its checkout. A bare repository's own folder,
    // or a submodule's under `.git/modules`, is the repository itself: its
    // parent holds other repositories too.
    let root = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent()?.to_path_buf()
    } else {
        common.clone()
    };
    let config = std::fs::read_to_string(common.join("config")).unwrap_or_default();
    Some((root, origin_url(&config)))
}

/// What the repository at `root` (from [`repo`]) is called: its folder's
/// name, without the `.git` a bare one or one kept apart ends in. New
/// worktrees of it go in a folder of this name.
pub fn repo_name(root: &Path) -> Option<String> {
    let name = root.file_name()?.to_string_lossy();
    let name = name.strip_suffix(".git").unwrap_or(&name);
    (!name.is_empty()).then(|| name.to_string())
}

/// The `url` of `[remote "origin"]` in a git config.
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line.replace(' ', "") == "[remote\"origin\"]";
        } else if in_origin
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            return Some(without_userinfo(value.trim()));
        }
    }
    None
}

/// `url` without a user or token before its host: the origin goes to every
/// paired device. An scp-style `git@host:o/r` has no scheme and stays as is.
fn without_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(host_end);
    match authority.rfind('@') {
        Some(at) => format!("{scheme}://{}{path}", &authority[at + 1..]),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_idle_for_the_days_picked_are_found() {
        const DAY: u64 = 86_400;
        let now = 400 * DAY;
        let record = |id: &str, updated: u64, status: Status| Record {
            id: id.into(),
            title: String::new(),
            agent: AgentKind::Claude,
            place: PathBuf::from("/p"),
            created: 0,
            updated,
            added: 0,
            removed: 0,
            meta: None,
            failed: false,
            status,
        };
        let store = Store {
            sessions: vec![
                record("old", now - 31 * DAY, Status::Idle),
                record("failed", now - 31 * DAY, Status::Error),
                record("recent", now - 2 * DAY, Status::Idle),
                record("working", now - 31 * DAY, Status::Working),
                record("asking", now - 31 * DAY, Status::NeedsInput),
            ],
            ..Store::default()
        };
        assert_eq!(store.idle_for(30, now), ["old", "failed"]);
        assert!(store.idle_for(90, now).is_empty());
        assert!(store.idle_for(0, now).is_empty(), "off");
    }

    #[test]
    fn an_origin_leaves_its_token_on_the_server() {
        let config = "[remote \"origin\"]\n\turl = https://me:ghp_secret@github.com/o/r.git\n";
        assert_eq!(
            origin_url(config).as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(
            without_userinfo("ssh://git@host:22/o/r.git"),
            "ssh://host:22/o/r.git"
        );
        assert_eq!(
            without_userinfo("git@github.com:o/r.git"),
            "git@github.com:o/r.git"
        );
        assert_eq!(
            without_userinfo("https://github.com/o/r@v2.git"),
            "https://github.com/o/r@v2.git"
        );
    }

    #[test]
    fn a_worktree_belongs_to_the_repository_it_was_made_from() {
        let main = temp("repo-main");
        let git = main.join(".git");
        std::fs::create_dir_all(git.join("worktrees/fix")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            git.join("config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://x/u.git\n[remote \"origin\"]\n\turl = git@github.com:o/app.git\n",
        )
        .unwrap();
        let tree = temp("repo-tree");
        let tree_git = git.join("worktrees/fix");
        std::fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", tree_git.display()),
        )
        .unwrap();
        std::fs::write(tree_git.join("HEAD"), "ref: refs/heads/fix\n").unwrap();
        std::fs::write(tree_git.join("commondir"), "../..\n").unwrap();

        let root = main.canonicalize().unwrap();
        let origin = Some("git@github.com:o/app.git".to_string());
        assert_eq!(repo(&main), Some((root.clone(), origin.clone())));
        assert_eq!(repo(&tree), Some((root, origin)));
        assert_eq!(branch(&tree).as_deref(), Some("fix"));
        // Not a checkout at all.
        assert_eq!(repo(&temp("repo-none")), None);
        for dir in ["repo-main", "repo-tree", "repo-none"] {
            let _ = std::fs::remove_dir_all(temp(dir));
        }
    }

    #[test]
    fn a_worktree_of_a_bare_repository_belongs_to_that_repository() {
        let parent = temp("repo-bare");
        let bare = parent.join("app.git");
        let tree_git = bare.join("worktrees/fix");
        std::fs::create_dir_all(&tree_git).unwrap();
        std::fs::write(bare.join("config"), "[core]\n\tbare = true\n").unwrap();
        std::fs::write(tree_git.join("HEAD"), "ref: refs/heads/fix\n").unwrap();
        std::fs::write(tree_git.join("commondir"), "../..\n").unwrap();
        let tree = temp("repo-bare-tree");
        std::fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", tree_git.display()),
        )
        .unwrap();

        // Not the folder the bare repository is in, which other ones share.
        assert_eq!(repo(&tree), Some((bare.canonicalize().unwrap(), None)));
        for dir in ["repo-bare", "repo-bare-tree"] {
            let _ = std::fs::remove_dir_all(temp(dir));
        }
    }

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
    fn a_deletion_started_is_read_back_until_it_ends() {
        let dir = temp("deleting");
        let mut store = Store::load(&dir).unwrap();
        let fix = Deletion {
            place: "/w/fix".into(),
            repo: "/w/app/.git".into(),
            branch: Some("fix".into()),
        };
        store.begin_deletion(fix.clone());
        // Started again: noted once.
        store.begin_deletion(fix.clone());
        store.save().unwrap();
        let mut back = Store::load(&dir).unwrap();
        assert_eq!(back.deleting, [fix]);
        assert!(back.end_deletion(Path::new("/w/fix")));
        assert!(!back.end_deletion(Path::new("/w/fix")));
        back.save().unwrap();
        // Nothing in progress leaves nothing in the file.
        let text = std::fs::read_to_string(dir.join("state.json")).unwrap();
        assert!(!text.contains("deleting"), "{text}");
        assert!(Store::load(&dir).unwrap().deleting.is_empty());
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

    fn aged(path: &Path, days: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let f = std::fs::File::create(path).unwrap();
        f.set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400),
        )
        .unwrap();
    }

    #[test]
    fn housekeeping_keeps_what_a_session_still_needs() {
        let dir = temp("housekeep");
        let mut store = Store::load(&dir).unwrap();
        store.sessions.push(record("s1-0", Status::Idle));
        let listed = store.transcript_path("s1-0");
        let running = store.transcript_path("s2-0");
        let orphan = store.transcript_path("s3-0");
        let young = store.transcript_path("s4-0");
        let foreign = dir.join("sessions").join("notes.json");
        let note = store.linked_path("s3-0");
        let old_photo = store.uploads_dir().join("photo-1.png");
        let new_photo = store.uploads_dir().join("photo-2.png");
        let leftover = dir.join(".state.json.1-0.tmp");
        for p in [&listed, &running, &orphan, &foreign, &note] {
            aged(p, 30);
        }
        aged(&young, 1);
        aged(&old_photo, 40);
        aged(&new_photo, 2);
        aged(&leftover, 2);

        let running_id = "s2-0".to_string();
        assert_eq!(store.housekeeping([&running_id]).run(), 3);
        assert!(listed.exists() && running.exists() && young.exists());
        assert!(foreign.exists() && note.exists() && new_photo.exists());
        assert!(!orphan.exists() && !old_photo.exists() && !leftover.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_server_ids_count_as_sessions() {
        assert!(is_id("s18f3a2b-0"));
        assert!(is_id(&new_id()));
        assert!(!is_id("s-0"));
        assert!(!is_id("s18f3a2b"));
        assert!(!is_id("notes"));
        assert!(!is_id("x18f-0"));
    }
}
