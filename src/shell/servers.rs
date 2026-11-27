//! The servers' side of the shell: the connections to the machines running
//! `arakun server` (`crate::servers`), and their folders and sessions shown
//! as the app's own.
//!
//! Each folder a server offers is an environment here (`model::Remote`): under
//! the local project with the same `origin`, so one project holds its work on
//! every machine, or under a project of the servers' own. Their sessions open
//! in the same tabs, with the same composer, as sessions here. The agent runs
//! over there: the tab shows it through a thread that only holds a transcript,
//! fed with the entries the server sends whole (`transcript`), and what the
//! user does in it goes back as the phone's messages ([`RemoteAction`]).
//!
//! The app connects at start when any server is paired. Until a server
//! answers, its environments are the ones it listed last
//! (`servers::load_inboxes`), so a restart or a server that is away loses none.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{App, AppContext as _, Context, Entity, SharedString, Task, Window};
use serde_json::{Value, json};

use super::{Modal, Shell};
use crate::agent::{
    AgentKind, Entry, RemoteAction, SessionMeta, Setting, SettingOption, SubagentRecord, Thread,
    ThreadStatus, Transcript, Usage,
};
use crate::dialogs::{DeleteEnvDialog, DialogEvent};
use crate::extensions_view::ServerTarget;
use crate::model::{self, Environment, Project, Remote, Session, Status, TabKind, unix_now};
use crate::phone::{ClientId, wire};
use crate::servers::balance::{self, Balance, Candidate};
use crate::servers::client::{Client, Event, Events, Status as LinkStatus};
use crate::servers::{self, Saved};
use crate::servers_view::{
    PlaceRow, Reach, ServerRow, ServersEvent, ServersState, SessionRow, Via, YourServers,
};

mod account;
pub(super) use account::fixture_machines;
mod files;
mod idle;
mod mirror;
mod terms;

/// One server: its connection and what it last said it holds.
struct Conn {
    client: Client,
    reach: Reach,
    sessions: Vec<SessionRow>,
    places: Vec<PlaceRow>,
    /// It sends several sessions to one connection (`open` with `keep`), as
    /// its inbox says. One from before sends the last one opened only.
    watches_several: bool,
    /// It is from before it sent images: none are asked of it.
    no_images: bool,
    /// For a server of the account, what `client` was started with, so a
    /// new list of the account's machines leaves it alone when nothing
    /// changed. `None` for the others.
    start: Option<account::Start>,
    /// Unix seconds it connected at, while it is connected.
    since: Option<u64>,
    /// Unix seconds it was last connected, since this app started.
    seen: Option<u64>,
    /// Its agents and their limits, from its last `usage` message.
    agents: Vec<crate::servers_view::AgentRow>,
    /// The idle time it was last told, and when, on this connection.
    idle_told: Option<(u64, std::time::Instant)>,
    /// It is from before it took idle time: none is sent to it.
    no_idle: bool,
}

impl Conn {
    fn new(client: Client) -> Conn {
        Conn {
            client,
            reach: Reach::Connecting,
            sessions: Vec::new(),
            places: Vec::new(),
            watches_several: false,
            no_images: false,
            start: None,
            since: None,
            seen: None,
            agents: Vec::new(),
            idle_told: None,
            no_idle: false,
        }
    }

    /// The connection to a server of the account, started with `start`.
    fn through_account(client: Client, start: account::Start) -> Conn {
        Conn {
            reach: start.reach(),
            start: Some(start),
            ..Conn::new(client)
        }
    }
}

/// The server's session on screen, which the server sends every change of,
/// and how much of it has arrived. A server follows one session for a
/// connection; the others show what they had and their state from the inbox.
struct Following {
    server: String,
    session: String,
    /// The environment it is in here.
    env_path: PathBuf,
    /// Its entries so far, whole, as the server's transcript holds them.
    entries: Vec<Entry>,
    /// Its subagents that ran as child sessions, as the server last sent them.
    /// Their transcripts are not among the entries.
    subagents: Vec<SubagentRecord>,
    agent: AgentKind,
    cwd: PathBuf,
    title: String,
    status: Status,
    /// A `transcript` was asked for and has not come back.
    asking: bool,
    /// The index the one that is out asked from.
    asked: usize,
    /// Entries changed from this index on while one was out: asked for next.
    again: Option<usize>,
    /// The whole session being read again, from its start, into here: on
    /// opening it and after each reconnect. It takes the place of `entries`
    /// only once all of it is here, so the view never falls back to a first
    /// page and fills up again.
    refill: Option<Vec<Entry>>,
    /// Why the session could not be read in full, when it could not. Nothing
    /// more is asked for until it is opened again.
    broken: Option<String>,
    /// Who looks at it: the session in front here, phones through this app.
    /// It is followed while anyone does.
    by: Vec<Watcher>,
    /// The prompts waiting there for the running turn, as its last `state`
    /// said: the server keeps the queue.
    queue: RemoteQueue,
    /// The images its agent named or its prompts mentioned that are on the
    /// server, by link as written.
    images: HashMap<String, ImageCopy>,
}

/// An image a server's session names, as asked of the server.
#[derive(Clone, Debug, Default, PartialEq)]
struct ImageCopy {
    /// The last entry that named it when it was asked for. An entry after it
    /// naming it again may mean a new image under the same name: it is asked
    /// for again.
    at: usize,
    /// The copy here, once one came: the newest answer's.
    file: Option<PathBuf>,
    /// The `at` of the answer `file` came from.
    file_at: usize,
    /// Asked and not answered yet.
    waiting: bool,
}

/// A server session's queue, as its `state` says it. An older server says
/// none: it is shown empty.
#[derive(Clone, Debug, Default, PartialEq)]
struct RemoteQueue {
    prompts: Vec<String>,
    paused: bool,
    sending_now: usize,
}

impl RemoteQueue {
    fn of(msg: &Value) -> Self {
        let prompts = msg["queue"]
            .as_array()
            .map(|q| {
                q.iter()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            prompts,
            paused: msg["queue_paused"] == true,
            sending_now: msg["sending_now"].as_u64().unwrap_or(0) as usize,
        }
    }
}

impl Following {
    /// The whole session is asked for again, from its start, and read to the side.
    fn read_again(&mut self) {
        (self.asking, self.asked, self.again) = (true, 0, None);
        (self.refill, self.broken) = (Some(Vec::new()), None);
        // Answers the old connection never brought are asked for again; the
        // copies that came stay on show.
        self.images.retain(|_, i| !i.waiting || i.file.is_some());
        for i in self.images.values_mut() {
            i.waiting = false;
        }
    }
}

/// Who a server's session is followed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Watcher {
    /// The session in front in this app's window.
    Desktop,
    /// A phone that opened it through this app.
    Phone(ClientId),
}

/// Settings with their current value and choices, as `wire::settings` sends them.
type SettingList = Vec<(Setting, (String, Vec<SettingOption>))>;

/// A server's folder and an agent there: what its `options` are about.
type OptionsKey = (String, String, AgentKind);

pub(super) struct ServersLink {
    saved: Vec<Saved>,
    conns: HashMap<String, Conn>,
    /// Where every connection's thread tells how it goes.
    events: Events,
    /// The servers' sessions followed, each for whoever looks at it.
    following: Vec<Following>,
    /// Sessions nobody follows any more, the last let go of last, kept with
    /// their entries so opening one again shows it at once while it is read
    /// again.
    rested: Vec<Following>,
    /// What each server listed last, as kept on disk (`servers::kept_inbox`).
    inboxes: HashMap<String, Value>,
    /// The last write of `inboxes` failed: the next inbox writes them again.
    inboxes_unsaved: bool,
    /// Waits for the newest write of `inboxes` to set `inboxes_unsaved`. A
    /// newer write drops it: only how the last one went counts.
    inboxes_saving: Option<Task<()>>,
    /// What each local project's repository is known by (`repo_key`), read once.
    origins: HashMap<PathBuf, Option<String>>,
    /// Sessions asked of servers, until each says its session's id.
    starts: Starts,
    /// That session, by server and id: its tab opens once the inbox has it.
    starting_id: Option<(String, String)>,
    /// Environments of folders a server no longer offers, kept for the
    /// sessions that ran there: nothing new starts in them.
    retired: HashSet<PathBuf>,
    /// Servers asked to clone or add a folder, until they answer.
    adding: HashSet<String>,
    /// Of those, the ones asked from the project picker. It is closed by
    /// then, so a failure is told in a notice, not in the Computers tab.
    from_picker: HashSet<String>,
    /// Failures of those, by server, waiting to be told.
    picker_failed: Vec<(String, String)>,
    /// A folder a server just added, by server and path: it opens once listed.
    added: Option<(String, String)>,
    /// A new environment asked of each server, until it answers.
    new_env: HashMap<String, NewEnv>,
    /// Servers from before they made worktrees: a first prompt starts where
    /// it is written there, the main folder too.
    no_new_env: HashSet<String>,
    /// The first prompts of sessions asked of servers, by the start's `ref`,
    /// until the server answers: they name the branch once it started, and
    /// go back in the composer if it did not. Each with its server.
    first_prompts: HashMap<u64, (String, FirstPrompt)>,
    /// The model and reasoning each server offers a new session of an agent
    /// in one of its folders, by server, place and agent, as it last said.
    options: HashMap<OptionsKey, SettingList>,
    /// The slash commands each server's agent offered last in one of its
    /// folders: a session opened there has them before its agent runs again.
    commands: HashMap<OptionsKey, Vec<crate::agent::Choice>>,
    /// Phones that asked for the options of a server's folder, by server,
    /// place and agent, with the environment and agent they asked about:
    /// the server's answer goes to them too.
    phone_options: HashMap<OptionsKey, Vec<(ClientId, PathBuf, AgentKind)>>,
    /// The environments servers were asked to delete, by server and place,
    /// until each answers, with the Delete environment dialog that waits on it.
    deleting: Vec<(String, String, gpui::EntityId)>,
    /// The id a pairing under way will carry once it works.
    pairing: Option<String>,
    /// Gives a pairing up when its link has run out.
    pairing_timeout: Option<Task<()>>,
    /// The pairing under way copies the MCP sign-ins there.
    pair_sign_ins: bool,
    error: Option<String>,
    /// The terminals open in servers' environments.
    terms: terms::Terms,
    /// Whether new environments are spread across machines, and each one's preference.
    balance: Balance,
    /// Git asked of servers for their environments' Changes panels.
    git_asks: servers::git::Asks,
    /// The account's machines, for its servers.
    machines: account::Machines,
    _listen: Task<()>,
    /// Tells the connected servers the idle time here (`idle`).
    _idle: Task<()>,
}

/// The folders a server's sessions ran in that it no longer offers. Their
/// sessions stay in its list, so they stay openable here. An older server
/// names a session's folder only (`env`): those sessions share one place per
/// name, under a path no folder has ([`GONE`]).
fn gone_places(sessions: &[SessionRow], places: &[PlaceRow]) -> Vec<PlaceRow> {
    let mut gone: Vec<PlaceRow> = Vec::new();
    for s in sessions {
        if (s.place.is_empty() && s.env.is_empty()) || places.iter().any(|p| s.in_place(p)) {
            continue;
        }
        let path = if s.place.is_empty() {
            format!("{GONE}{}", s.env)
        } else {
            s.place.clone()
        };
        if gone.iter().any(|g| g.path == path) {
            continue;
        }
        let name = if s.env.is_empty() {
            Path::new(&s.place)
                .file_name()
                .map_or_else(|| s.place.clone(), |n| n.to_string_lossy().into_owned())
        } else {
            s.env.clone()
        };
        gone.push(PlaceRow {
            path,
            project: name,
            branch: String::new(),
            agent: s.agent.clone(),
            repo: String::new(),
            origin: String::new(),
        });
    }
    gone
}

/// Said to a phone whose prompt did not reach a server's session.
const PHONE_PROMPT_LOST: &str =
    "the server was not connected, so your message did not reach it. Send it again once it is back";

/// What the place of an older server's session starts with, whose folder is
/// known by name only. Not an absolute path, so it never names a folder there.
const GONE: &str = "gone:";

/// Whether `message` is a server refusing one of `kinds` because it is older
/// than them: it cannot read the message (`bad message: unknown variant ...`).
/// What the Computers tab says of a server too old for a Linear mapping.
const LINEAR_TOO_OLD: &str = "This server is too old to take work from Linear. Update Arakun on it, then map the project again.";

fn older_server_refused(message: &str, kinds: &[&str]) -> bool {
    // The variant refused comes first; the ones expected after it may name
    // a kind here without being about it.
    kinds
        .iter()
        .any(|k| message.starts_with(&format!("bad message: unknown variant `{k}`")))
}

/// Whether `server` was asked to delete `place` and has not answered yet.
fn deleting_already<T>(deleting: &[(String, String, T)], server: &str, place: &str) -> bool {
    deleting.iter().any(|(s, p, _)| s == server && p == place)
}

/// A folder request (`add_place` or `clone`) to `server` that never left
/// this app: the form can ask again, and says why. None when there was none.
fn folder_request_lost(adding: &mut HashSet<String>, server: &str, kind: &str) -> Option<String> {
    if !matches!(kind, "add_place" | "clone") || !adding.remove(server) {
        return None;
    }
    Some("The request did not reach the server. Try again once it is connected.".into())
}

impl ServersLink {
    /// A folder `server` was asked for is not coming: said where it was asked.
    fn folder_failed(&mut self, server: &str, why: String) {
        if let Some(why) =
            route_folder_failure(&mut self.from_picker, &mut self.picker_failed, server, why)
        {
            self.error = Some(why);
        }
    }
}

/// Queues the failure for a notice when the project picker asked for the
/// folder, else hands it back for the Computers tab.
fn route_folder_failure(
    from_picker: &mut HashSet<String>,
    picker_failed: &mut Vec<(String, String)>,
    server: &str,
    why: String,
) -> Option<String> {
    if from_picker.remove(server) {
        picker_failed.push((server.to_string(), why));
        None
    } else {
        Some(why)
    }
}

/// The first prompt of a session a server was asked to start.
pub(super) struct FirstPrompt {
    pub env: PathBuf,
    /// As the user wrote it, without the note to the agent.
    pub text: String,
    pub agent: AgentKind,
    /// The rename note that went with it, kept for the next prompt if it did not go.
    pub note: Option<String>,
    /// The folder it was written in, when another: kept with it if it did not go.
    pub mentions_from: Option<PathBuf>,
    /// Typed here: it goes back in the composer if the server did not start it.
    pub desktop: bool,
}

/// A new environment a server was asked for.
struct NewEnv {
    branch: String,
    /// Its first prompt names it, as one made here.
    auto_name: bool,
    /// The New environment dialog that asked, which waits on the answer. One
    /// opened after it was dismissed is not told about it.
    dialog: Option<gpui::EntityId>,
    /// The first prompt that waits for it: its session starts there.
    prompt: Option<super::sessions::QueuedPrompt>,
}

/// `who` no longer looks at what it followed, but for `keep` (server and
/// session): what nobody looks at any more is taken out and returned.
fn let_go(
    following: &mut Vec<Following>,
    who: Watcher,
    keep: Option<(&str, &str)>,
) -> Vec<Following> {
    let kept = |f: &Following| keep == Some((f.server.as_str(), f.session.as_str()));
    for f in following.iter_mut().filter(|f| !kept(f)) {
        f.by.retain(|w| *w != who);
    }
    let (gone, stay) = std::mem::take(following)
        .into_iter()
        .partition(|f| f.by.is_empty() && !kept(f));
    *following = stay;
    gone
}

/// What following a session asks of its server.
struct Followed {
    /// The session is asked for, whole: it is new here, or read again.
    ask: bool,
    /// What nobody looks at any more: its server stops sending it.
    close: Vec<Following>,
    /// What a server that sends one session at a time no longer sends,
    /// since it sends this one in its place.
    displaced: Vec<Following>,
}

/// `who` follows `fresh`'s session, and lets go of the one it looked at
/// before. On a server that watches one session per connection
/// (`several` false), the others it sent are given up.
fn follow_in(
    following: &mut Vec<Following>,
    who: Watcher,
    fresh: Following,
    several: bool,
) -> Followed {
    let (server, session) = (fresh.server.clone(), fresh.session.clone());
    let same = |f: &Following| f.server == server && f.session == session;
    let close = let_go(following, who, Some((&server, &session)));
    let displaced = if several {
        Vec::new()
    } else {
        following
            .extract_if(.., |f| f.server == server && !same(f))
            .collect()
    };
    let ask = match following.iter_mut().find(|f| same(f)) {
        Some(f) => {
            if !f.by.contains(&who) {
                f.by.push(who);
            }
            // One that could not be read is read again when someone opens
            // it. Who else looks at it still does.
            let again = f.broken.is_some();
            if again {
                f.read_again();
            }
            again
        }
        None => {
            following.push(Following {
                by: vec![who],
                ..fresh
            });
            true
        }
    };
    Followed {
        ask,
        close,
        displaced,
    }
}

/// The folder name of a server's repository at `repo`, as the server names
/// it (`store::repo_name`): without a bare repository's `.git`. The path is
/// the server's, which may be Windows' while this is not, or the other way.
fn repo_folder(repo: &str) -> Option<&str> {
    let last = repo
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty() && !name.ends_with(':')).then_some(name)
}

/// `server`'s starts that wait on no answer any more: their first prompts
/// go. Another server's stay, kept until its own drop is told back.
fn settle_first_prompts(
    first_prompts: &mut HashMap<u64, (String, FirstPrompt)>,
    starts: &Starts,
    server: &str,
) {
    first_prompts.retain(|r, (s, _)| s != server || starts.waiting(*r));
}

/// Takes `server`'s session `id` out of what is followed. Another server's
/// session may have the same id: it stays.
fn unfollow(following: &mut Vec<Following>, server: &str, id: &str) -> Vec<Following> {
    following
        .extract_if(.., |f| f.server == server && f.session == id)
        .collect()
}

/// What phones call the session `id` of `server`: the two together, since
/// two servers may give a session the same id. No id has an `@`.
pub(super) fn phone_session_id(server: &str, id: &str) -> String {
    format!("{id}@{server}")
}

/// The session id and, for a server's session, the server in what phones
/// call a session ([`phone_session_id`]).
pub(super) fn split_phone_id(pid: &str) -> (&str, Option<&str>) {
    match pid.rsplit_once('@') {
        Some((id, server)) => (id, Some(server)),
        None => (pid, None),
    }
}

/// Tells each server to stop sending the sessions in `gone`, and keeps them
/// to show again.
fn close_follows(link: &mut ServersLink, gone: Vec<Following>) {
    for f in gone {
        if let Some(conn) = link.conns.get(&f.server)
            && conn.reach == Reach::Connected
        {
            conn.client
                .send(&json!({ "type": "close", "session": f.session }));
        }
        rest(&mut link.rested, f);
    }
}

/// How many sessions let go of are kept to show again.
const RESTED: usize = 4;

/// Keeps `f` in `rested`, the newest of at most [`RESTED`]. One that has
/// nothing to show, or could not be read, is not worth keeping.
fn rest(rested: &mut Vec<Following>, mut f: Following) {
    rested.retain(|r| !(r.server == f.server && r.session == f.session));
    if f.entries.is_empty() || f.broken.is_some() {
        return;
    }
    f.by.clear();
    rested.push(f);
    let over = rested.len().saturating_sub(RESTED);
    rested.drain(..over);
}

/// The kept copy of `server`'s session `id`, if there is one.
fn rested_of<'a>(rested: &'a [Following], server: &str, id: &str) -> Option<&'a Following> {
    rested
        .iter()
        .find(|r| r.server == server && r.session == id)
}

/// Takes the kept copy of `server`'s session `id` out of `rested`, to be
/// followed again: it shows until the session is read again in full.
fn wake(rested: &mut Vec<Following>, server: &str, id: &str) -> Option<Following> {
    let i = rested
        .iter()
        .position(|r| r.server == server && r.session == id)?;
    let mut f = rested.remove(i);
    f.read_again();
    Some(f)
}

/// Whether the error `msg` answers a start. A server from before errors said
/// what they were about answers one with this message only, which it sends
/// for nothing else.
fn about_start(msg: &Value) -> bool {
    msg["about"] == "start"
        || (msg["about"].is_null()
            && msg["id"].is_null()
            && msg["message"] == "that project is gone")
}

/// A server reached over plain http whose key is not pinned yet: paired
/// before its socket was sealed, it trusts whatever key answers next, and its
/// token goes to that machine. Once pinned, every connection is checked and
/// sealed end to end, http or not.
fn in_the_clear(s: &Saved) -> bool {
    s.way == servers::Way::Direct && s.origin.starts_with("http://") && s.key.is_empty()
}

impl Shell {
    /// Whether this app reaches `server` over plain http, or does not know it.
    fn server_in_the_clear(&self, server: &str) -> bool {
        self.servers
            .as_ref()
            .and_then(|l| l.saved.iter().find(|s| s.id == server))
            .is_none_or(in_the_clear)
    }
}

/// Whether building the link connects this server: at start, not over plain http.
fn connects_now(s: &Saved, startup: bool) -> bool {
    !startup || !in_the_clear(s)
}

/// The kept lists shown when the link is built: those of the servers it connects.
fn seeded_inboxes(
    saved: &[Saved],
    inboxes: &HashMap<String, Value>,
    startup: bool,
) -> Vec<(String, Value)> {
    saved
        .iter()
        .filter(|s| connects_now(s, startup))
        .filter_map(|s| Some((s.id.clone(), inboxes.get(&s.id)?.clone())))
        .collect()
}

/// Who asked a server for a session, and so hears its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Asker {
    /// This app: the session's tab opens here.
    Desktop,
    /// A phone, through this app: the session opens there.
    Phone(ClientId),
    /// A phone that has left since. Its answer goes nowhere: not here, and
    /// not to a phone that comes later with the same id.
    Gone,
}

/// The starts sent to servers and not answered yet, oldest first. A server
/// answers its starts in the order they came, with `started` or an error
/// about the start, so its next answer is for its oldest one.
/// Each carries a number of this app's own, which goes out with it as `ref`
/// (the server reads past it), so a start that never went out is known.
#[derive(Default)]
struct Starts(Vec<(String, Asker, u64)>, u64);

impl Starts {
    /// Notes a start for `server`; what it goes out with as `ref`.
    fn sent(&mut self, server: &str, by: Asker) -> u64 {
        self.1 += 1;
        self.0.push((server.to_string(), by, self.1));
        self.1
    }

    /// Who the answer `server` just gave is for.
    #[cfg(test)]
    fn answered(&mut self, server: &str) -> Option<Asker> {
        self.answered_ref(server).map(|(by, _)| by)
    }

    /// Who the answer `server` just gave is for, and the `ref` it went with.
    fn answered_ref(&mut self, server: &str) -> Option<(Asker, u64)> {
        let at = self.0.iter().position(|(s, _, _)| s == server)?;
        let (_, by, reference) = self.0.remove(at);
        Some((by, reference))
    }

    /// Whether the start `reference` still waits on its answer.
    fn waiting(&self, reference: u64) -> bool {
        self.0.iter().any(|(_, _, r)| *r == reference)
    }

    /// The start `reference` never went out: who asked for it. No answer comes for it.
    fn undelivered(&mut self, reference: u64) -> Option<Asker> {
        let at = self.0.iter().position(|(_, _, r)| *r == reference)?;
        Some(self.0.remove(at).1)
    }

    /// The connection to `server` dropped, and the answers with it: the
    /// phones that were waiting for one.
    fn dropped(&mut self, server: &str) -> Vec<ClientId> {
        let mut phones = Vec::new();
        self.0.retain(|(s, by, _)| {
            if s != server {
                return true;
            }
            if let Asker::Phone(client) = by {
                phones.push(*client);
            }
            false
        });
        phones
    }

    /// Phone `client` left, or every phone with `None`.
    fn phone_left(&mut self, client: Option<ClientId>) {
        for (_, by, _) in &mut self.0 {
            if matches!(by, Asker::Phone(c) if client.is_none_or(|left| left == *c)) {
                *by = Asker::Gone;
            }
        }
    }
}

fn status_from(word: &str) -> Status {
    match word {
        "working" => Status::Working,
        "needs_input" => Status::NeedsInput,
        "error" => Status::Error,
        _ => Status::Idle,
    }
}

/// The `settings` of a start without those the server does not offer, when
/// it said what it offers. The reasoning goes with the model it was picked
/// for: without that model it would be checked against another.
fn offered_picks(
    settings: &[(String, String)],
    offered: Option<&SettingList>,
) -> Vec<(String, String)> {
    let Some(offered) = offered else {
        return settings.to_vec();
    };
    let takes = |key: &str, value: &str| {
        let choices = Setting::from_key(key)
            .and_then(|s| offered.iter().find(|(d, _)| *d == s))
            .map(|(_, (_, o))| o);
        choices.is_none_or(|o| o.is_empty() || o.iter().any(|c| c.id == value))
    };
    let (model, effort) = (Setting::Model.key(), Setting::Effort.key());
    let model_dropped = settings.iter().any(|(k, v)| k == model && !takes(k, v));
    // The levels offered are those of the model the server would start on.
    // Another model it offers may have others: its reasoning goes as picked,
    // for the server to check against that model.
    let other_model = settings.iter().any(|(k, v)| {
        k == model
            && offered
                .iter()
                .find(|(d, _)| *d == Setting::Model)
                .is_some_and(|(_, (current, _))| current != v)
    });
    settings
        .iter()
        .filter(|(k, v)| (other_model && k == effort) || takes(k, v))
        .filter(|(k, _)| !(model_dropped && k == effort))
        .cloned()
        .collect()
}

/// A setting of a new session on a server, from what the server offers and
/// what the environment picked (`draft`).
fn draft_on_server(
    offered: &SettingList,
    agent: AgentKind,
    draft: &[(Setting, String)],
    s: Setting,
) -> Option<(String, Vec<SettingOption>)> {
    let on_offer = |s: Setting| {
        offered
            .iter()
            .find(|(d, (_, o))| *d == s && !o.is_empty())
            .map(|(_, v)| v)
    };
    let picked = |s: Setting| draft.iter().find(|(d, _)| *d == s).map(|(_, v)| v);
    // OpenCode's and Codex's reasoning levels are per model: the ones offered
    // are those of the model the server would start on. Claude's are the same
    // for every model, as the server sends them.
    if matches!(agent, AgentKind::OpenCode | AgentKind::Codex)
        && s == Setting::Effort
        && let Some(model) = picked(Setting::Model)
        && on_offer(Setting::Model)
            .is_some_and(|(current, o)| current != model && o.iter().any(|c| c.id == *model))
    {
        return None;
    }
    let (mut current, options) = on_offer(s)?.clone();
    if let Some(v) = picked(s).filter(|v| options.iter().any(|o| o.id == **v)) {
        current = v.clone();
    }
    Some((current, options))
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

use crate::clone_source::repo_key;

/// Where a server's folder goes: the local project its repository is, else a
/// project of the servers' own. Without an `origin`, a server's repository is
/// its own and no other machine's.
fn project_for(
    place: &PlaceRow,
    server: &str,
    locals: &[(PathBuf, Option<String>)],
) -> (PathBuf, bool) {
    let key = repo_key(&place.origin);
    if let Some(key) = &key
        && let Some((path, _)) = locals.iter().find(|(_, k)| k.as_ref() == Some(key))
    {
        return (path.clone(), true);
    }
    let repo = if place.repo.is_empty() {
        &place.path
    } else {
        &place.repo
    };
    let key = key.unwrap_or_else(|| format!("{server}{repo}"));
    (Remote::project_path(&key), false)
}

/// Shows the followed session in `thread` as it is now, and why it stopped
/// there if it did. The reason goes after what is shown, which would replace it.
fn show_following(f: &Following, status: Status, thread: &Entity<Thread>, cx: &mut App) {
    let (status, transcript) = (thread_status(status), transcript_of(f));
    let why = f.broken.clone();
    // Copies that came for a view of it closed since.
    let fetched: Vec<(String, PathBuf)> = f
        .images
        .iter()
        .filter_map(|(l, i)| Some((l.clone(), i.file.clone()?)))
        .collect();
    thread.update(cx, |t, cx| {
        t.show_transcript(transcript, status, cx);
        for (link, file) in fetched {
            if t.fetched_images().get(&link) != Some(&file) {
                t.image_fetched(link, file, cx);
            }
        }
        if let Some(why) = why {
            t.post_notice(why, true, cx);
        }
    });
}

/// The transcript a viewer thread is handed: the entries so far, as this session.
fn transcript_of(f: &Following) -> Transcript {
    Transcript {
        version: 1,
        meta: SessionMeta {
            agent: f.agent,
            session_id: None,
            title: f.title.clone(),
            cwd: f.cwd.clone(),
            created: unix_now(),
            updated: unix_now(),
            pr: None,
            seat: None,
            seat_key: None,
        },
        entries: f.entries.clone(),
        usage: Usage::default(),
        fork_point: None,
        interrupted: false,
        resume: false,
        queue: f.queue.prompts.clone(),
        handoff: None,
        subagents: f.subagents.clone(),
    }
}

/// What the viewer shows as the session's state. A failed turn left its
/// reason in the transcript, so the error needs no words of its own here.
fn thread_status(status: Status) -> ThreadStatus {
    match status {
        Status::Working => ThreadStatus::Working,
        Status::NeedsInput => ThreadStatus::NeedsInput,
        Status::Idle | Status::Error => ThreadStatus::Idle,
    }
}

/// The thread of a session that is on screen, if it is.
fn thread_of(projects: &[Project], env_path: &Path, id: &str) -> Option<Entity<Thread>> {
    projects
        .iter()
        .flat_map(|p| &p.environments)
        .find(|e| e.path == env_path)?
        .sessions
        .iter()
        .find(|s| s.id.as_ref() == id)?
        .live
        .as_ref()
        .map(|l| l.thread.clone())
}

fn agent_of(name: &str) -> AgentKind {
    AgentKind::parse(name).unwrap_or(AgentKind::Claude)
}

/// A link is good for two minutes; a pairing still under way a little after
/// that will not finish.
const PAIRING_WAIT: std::time::Duration = std::time::Duration::from_secs(150);

/// A `transcript` answer: entries `from..to` of the session's `count`.
struct Page {
    from: usize,
    to: usize,
    count: usize,
    entries: Vec<Entry>,
}

/// Reads a `transcript` answer. An entry this app does not know (a server one
/// version ahead with a new kind) fails the whole page: half a transcript
/// with holes in it would be shown as if it were the session.
fn page_of(msg: &Value) -> Result<Page, String> {
    let number = |key: &str| msg[key].as_u64().unwrap_or(0) as usize;
    let entries = serde_json::from_value(msg["entries"].clone()).map_err(|e| e.to_string())?;
    Ok(Page {
        from: number("from"),
        to: number("to"),
        count: number("count"),
        entries,
    })
}

/// What to do after a page was put in.
#[derive(Debug, PartialEq)]
enum Next {
    /// All of it is here.
    Done,
    /// More is out: ask from this index.
    Ask(usize),
    /// The server answered with nothing new where more was due. Asking again
    /// would get the same answer, forever and as fast as the network goes.
    Stuck,
    /// An answer to a question no longer asked: the one that is out comes yet.
    Stale,
}

/// Puts a page into the entries so far. `asked` is the index it was asked from.
fn merge(entries: &mut Vec<Entry>, page: Page, asked: usize) -> Next {
    // The server answers from where it was asked, or from its last entry when
    // it has fewer. A page from further on answers an earlier question: one
    // sent before the session was opened again, or before a reconnect.
    if page.from > asked {
        return Next::Stale;
    }
    if page.from > entries.len() {
        // A gap: what is missing comes first.
        return Next::Ask(entries.len());
    }
    let brought = !page.entries.is_empty();
    entries.truncate(page.from);
    entries.extend(page.entries);
    if page.to >= page.count || entries.len() >= page.count {
        Next::Done
    } else if brought {
        Next::Ask(entries.len())
    } else {
        Next::Stuck
    }
}

/// Writes the kept lists off the UI thread, and sets `inboxes_unsaved` once
/// the disk says how it went.
fn save_inboxes(link: &mut ServersLink, cx: &mut Context<Shell>) {
    let saved = servers::save_inboxes(&link.inboxes);
    link.inboxes_saving = Some(cx.spawn(async move |this, cx| {
        // The writer gone without a word is a write that did not happen.
        let saved = saved.await.unwrap_or(false);
        let _ = this.update(cx, |this, _| {
            if let Some(link) = &mut this.servers {
                link.inboxes_unsaved = !saved;
            }
        });
    }));
}

/// The images `entries` name or mention to ask the server for, each with the last entry
/// naming it: the ones not asked for yet, and the ones named again since they
/// were. The files are the server's: one at the same path here is another.
fn images_to_ask(entries: &[Entry], asked: &HashMap<String, ImageCopy>) -> Vec<(String, usize)> {
    let mut last: Vec<(String, usize)> = Vec::new();
    for (at, e) in entries.iter().enumerate() {
        let links = match e {
            Entry::Agent { text } => crate::agent::markdown::image_files(text),
            // As the prompt is shown: a mention in a pasted block is not one.
            Entry::User { text, .. } => {
                crate::agent::attach::image_mentions(&crate::agent::attach::split_pastes(text).0)
            }
            _ => continue,
        };
        for link in links {
            match last.iter_mut().find(|(l, _)| *l == link) {
                Some(seen) => seen.1 = at,
                None => last.push((link, at)),
            }
        }
    }
    last.retain(|(link, at)| asked.get(link).is_none_or(|i| *at > i.at));
    last
}

/// Keeps the image an `image` answer carries in `dir`, named by its bytes so
/// a changed image is never mistaken for the one shown before.
fn keep_image(msg: &Value, dir: &Path) -> Option<PathBuf> {
    use base64::Engine as _;
    use sha2::Digest as _;
    let ext = match msg["mime"].as_str()? {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(msg["data"].as_str()?)
        .ok()?;
    let digest = sha2::Sha256::digest(&bytes);
    let name: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let file = dir.join(format!("{name}.{ext}"));
    if !file.is_file() {
        std::fs::create_dir_all(dir).ok()?;
        crate::durable::write_private(&file, &bytes).ok()?;
    }
    Some(file)
}

/// Where the copies of servers' images are kept. They are fetched again
/// after a restart, so a temporary folder is enough.
pub(super) fn images_dir() -> PathBuf {
    std::env::temp_dir().join("arakun-server-images")
}

impl Shell {
    /// Opens (or focuses) the Computers tab, and connects to the paired servers
    /// the first time.
    pub(super) fn open_servers_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if self.has_projects() {
            self.show_view_tab("Computers", TabKind::Servers, |k| {
                matches!(k, TabKind::Servers)
            });
            self.note_active_tab(cx);
        } else {
            // Following servers needs no project here: the tab shows in the
            // welcome's place, as the GitHub tab does.
            self.show_alone(Some(super::home::Alone::Servers), cx);
        }
        self.servers_connect(window, cx);
        // A server signed in since shows up.
        self.servers_list_machines(window, cx);
        self.sync_servers_view(cx);
        self.views.servers.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// At start: the paired servers are connected, and their projects show
    /// what they listed last until they answer. Not one over plain http with
    /// no key pinned yet: whatever answers on the network this is now would
    /// get its token. It waits for the Computers tab.
    pub(super) fn restore_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if servers::load().iter().any(|s| !in_the_clear(s)) {
            self.servers_connect_with(true, window, cx);
            self.sync_servers_view(cx);
        }
    }

    pub(super) fn servers_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.servers_connect_with(false, window, cx)
    }

    /// With `startup`, only the servers whose link is encrypted.
    fn servers_connect_with(&mut self, startup: bool, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.servers_dial_session(cx);
        if let Some(link) = &mut self.servers {
            let mut late = Vec::new();
            if !startup {
                for s in &link.saved {
                    if !link.conns.contains_key(&s.id) {
                        let conn = Self::servers_first_conn(
                            s,
                            session.as_ref(),
                            &link.machines,
                            link.events.clone(),
                        );
                        link.conns.insert(s.id.clone(), conn);
                        late.extend(link.inboxes.get(&s.id).map(|i| (s.id.clone(), i.clone())));
                    }
                }
            }
            // Left out at start: what it listed last shows until it answers.
            for (id, inbox) in &late {
                self.servers_inbox(id, inbox);
            }
            if !late.is_empty() {
                self.sync_remote_projects(window, cx);
            }
            return;
        }
        let (events, mut heard) = mpsc::unbounded::<(String, Event)>();
        let listen = cx.spawn_in(window, async move |this, cx| {
            while let Some((id, event)) = heard.next().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.servers_heard(&id, event, window, cx);
                        this.servers_tell_picker_failed(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let saved = servers::load();
        let mut inboxes = servers::load_inboxes();
        // A server forgotten while it was kept is not shown again.
        inboxes.retain(|id, _| saved.iter().any(|s| s.id == *id));
        let seeded = seeded_inboxes(&saved, &inboxes, startup);
        let conns = saved
            .iter()
            .filter(|s| connects_now(s, startup))
            .map(|s| {
                // No list was asked yet: a server of the account is held
                // until it is.
                let conn = Self::servers_first_conn(
                    s,
                    session.as_ref(),
                    &Default::default(),
                    events.clone(),
                );
                (s.id.clone(), conn)
            })
            .collect();
        self.servers = Some(ServersLink {
            saved,
            conns,
            events,
            following: Vec::new(),
            rested: Vec::new(),
            inboxes,
            inboxes_unsaved: false,
            inboxes_saving: None,
            origins: HashMap::new(),
            starts: Starts::default(),
            starting_id: None,
            retired: HashSet::new(),
            adding: HashSet::new(),
            from_picker: HashSet::new(),
            picker_failed: Vec::new(),
            added: None,
            new_env: HashMap::new(),
            no_new_env: HashSet::new(),
            first_prompts: HashMap::new(),
            options: HashMap::new(),
            commands: HashMap::new(),
            phone_options: HashMap::new(),
            deleting: Vec::new(),
            pairing: None,
            pairing_timeout: None,
            pair_sign_ins: false,
            error: None,
            terms: terms::Terms::new(window, cx),
            balance: balance::load(),
            git_asks: Default::default(),
            machines: Default::default(),
            _listen: listen,
            _idle: Self::servers_time_idle(window, cx),
        });
        self.servers_list_machines(window, cx);
        for (id, inbox) in &seeded {
            self.servers_inbox(id, inbox);
        }
        // What they listed last shows until they answer, offline ones too.
        // Not run with nothing kept: it reads every local project's origin.
        if !seeded.is_empty() {
            self.sync_remote_projects(window, cx);
        }
    }

    /// The name a server goes by.
    fn server_name(s: &Saved) -> String {
        if s.name.is_empty() {
            s.origin.clone()
        } else {
            s.name.clone()
        }
    }

    pub(super) fn sync_servers_view(&mut self, cx: &mut Context<Self>) {
        let targets = self.server_targets();
        self.views
            .extensions
            .update(cx, |v, cx| v.set_servers(targets, cx));
        let yours = self.servers_yours();
        let Some(link) = &self.servers else {
            return;
        };
        // When the relay last saw each server of the account.
        let listed_seen = |label: Option<&str>| match (&yours, label) {
            (YourServers::Listed(rows), Some(label)) => rows
                .iter()
                .find(|m| m.label == label)
                .map(|m| m.last_seen)
                .filter(|t| *t > 0),
            _ => None,
        };
        let servers = link
            .saved
            .iter()
            .filter_map(|s| {
                let conn = link.conns.get(&s.id)?;
                let seen = conn.seen.max(listed_seen(s.label()));
                Some(ServerRow {
                    id: s.id.clone(),
                    name: Self::server_name(s),
                    origin: s.origin.clone(),
                    reach: conn.reach.clone(),
                    sessions: conn.sessions.clone(),
                    places: conn.places.clone(),
                    via: match s.way {
                        servers::Way::Direct => Via::Direct {
                            clear: in_the_clear(s),
                        },
                        servers::Way::Account { .. } => Via::Account,
                        servers::Way::Relay { .. } => Via::OldRelay,
                    },
                    label: s.label().map(str::to_string),
                    since: conn.since,
                    seen,
                    agents: conn.agents.clone(),
                    sign_ins: s.sign_ins,
                    holds_sign_ins: s.holds_sign_ins,
                })
            })
            .collect();
        let state = ServersState {
            servers,
            yours,
            pairing: link.pairing.is_some(),
            adding: link.adding.clone(),
            error: link.error.clone(),
            balance: link.balance.clone(),
        };
        self.views
            .servers
            .update(cx, |v, cx| v.set_state(state, cx));
        self.sync_this_computer(cx);
        cx.notify();
    }

    /// What the Computers tab shows of this computer: its environments with
    /// their latest sessions, and the agents signed in here. Asked as the
    /// tab draws, since sessions here start and stop all the time.
    pub(super) fn sync_this_computer(&self, cx: &mut Context<Self>) {
        const SHOWN: usize = 4;
        let mut envs = Vec::new();
        for p in self.projects.iter().filter(|p| !p.is_remote()) {
            for e in &p.environments {
                let mut sessions: Vec<&Session> =
                    e.sessions.iter().filter(|s| !s.settled).collect();
                sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
                let more = sessions.len().saturating_sub(SHOWN);
                envs.push(crate::servers_view::EnvRow {
                    project: p.name.to_string(),
                    branch: e.branch.to_string(),
                    path: model::pretty_path(&e.path),
                    here: Some(e.path.clone()),
                    sessions: sessions
                        .into_iter()
                        .take(SHOWN)
                        .map(|s| SessionRow {
                            id: s.id.to_string(),
                            title: s.title.to_string(),
                            agent: s.agent.label().to_string(),
                            status: s.shown_status(),
                            updated: s.updated,
                            place: String::new(),
                            env: String::new(),
                        })
                        .collect(),
                    more,
                });
            }
        }
        let this = crate::servers_view::ThisComputer {
            name: crate::phone::host_name(),
            envs,
            agents: crate::servers_view::agents_of(&self.local_usage(cx)),
        };
        self.views.servers.update(cx, |v, cx| v.set_this(this, cx));
    }

    /// The paired servers, for the Marketplace to install on. Before the
    /// first connection they are read from the file, and not connected.
    pub(super) fn server_targets(&self) -> Vec<ServerTarget> {
        let target = |s: &Saved, reach: Reach| ServerTarget {
            id: s.id.clone(),
            name: if s.name.is_empty() {
                s.origin.clone()
            } else {
                s.name.clone()
            },
            reach,
            in_the_clear: in_the_clear(s),
        };
        // A server left out at start has no connection yet; picking it connects it.
        match &self.servers {
            Some(link) => link
                .saved
                .iter()
                .map(|s| {
                    let reach = link
                        .conns
                        .get(&s.id)
                        .map_or(Reach::Connecting, |c| c.reach.clone());
                    target(s, reach)
                })
                .collect(),
            None => servers::load()
                .iter()
                .map(|s| target(s, Reach::Connecting))
                .collect(),
        }
    }

    /// A message from the Marketplace to one server.
    pub(super) fn server_send_raw(&self, id: &str, msg: &Value) {
        if let Some(conn) = self.servers.as_ref().and_then(|l| l.conns.get(id)) {
            conn.client.send(msg);
        }
    }

    /// Whether a server answers right now.
    pub(super) fn server_reach(&self, server: &str) -> Option<Reach> {
        let link = self.servers.as_ref()?;
        link.conns.get(server).map(|c| c.reach.clone())
    }

    /// The connected servers, for the project picker to offer a clone on.
    pub(super) fn clone_servers(&self) -> Vec<crate::picker::CloneServer> {
        let Some(link) = &self.servers else {
            return Vec::new();
        };
        link.saved
            .iter()
            .filter(|s| {
                link.conns
                    .get(&s.id)
                    .is_some_and(|c| c.reach == Reach::Connected)
            })
            .map(|s| crate::picker::CloneServer {
                id: s.id.clone(),
                name: Self::server_name(s),
            })
            .collect()
    }

    /// Asks `server` to clone `url`, for the project picker. The new folder
    /// opens once the server lists it (`servers_open_added`).
    pub(super) fn servers_clone_from_picker(
        &mut self,
        server: &str,
        url: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let machine = link
            .saved
            .iter()
            .find(|s| s.id == server)
            .map(Self::server_name)
            .unwrap_or_default();
        let problem = match link.conns.get(server) {
            Some(c) if c.reach == Reach::Connected => {
                // One at a time per server: its answer says only which folder.
                link.adding.contains(server).then(|| {
                    format!("A folder is being made on {machine}; try again once it is there.")
                })
            }
            _ => Some(format!("{machine} is not connected right now.")),
        };
        if problem.is_none()
            && let Some(conn) = link.conns.get(server)
        {
            conn.client.send(&json!({ "type": "clone", "url": url }));
            link.adding.insert(server.to_string());
            link.from_picker.insert(server.to_string());
            self.sync_servers_view(cx);
            return;
        }
        if let Some(why) = problem {
            self.notify(
                crate::island::AlertKind::Warning,
                &format!("Could not clone on {machine}"),
                &why,
                None,
                cx,
            );
        }
    }

    /// Tells the clones asked from the project picker that failed.
    fn servers_tell_picker_failed(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let failed = std::mem::take(&mut link.picker_failed);
        for (server, why) in failed {
            let machine = self
                .servers
                .as_ref()
                .and_then(|l| l.saved.iter().find(|s| s.id == server))
                .map(Self::server_name)
                .unwrap_or_default();
            self.notify(
                crate::island::AlertKind::Warning,
                &format!("Could not clone on {machine}"),
                &why,
                None,
                cx,
            );
        }
    }

    pub(super) fn on_servers_event(
        &mut self,
        ev: &ServersEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.servers_connect(window, cx);
        match ev {
            ServersEvent::SignIn => {
                self.open_account_sign_in(window, cx);
                return;
            }
            ServersEvent::OpenHere { env, session } => {
                let found = self
                    .projects
                    .iter()
                    .enumerate()
                    .find_map(|(pi, p)| p.env_index(env).map(|ei| (pi, ei)));
                if let Some((pi, ei)) = found {
                    self.select_env(pi, ei, window, cx);
                    self.open_session(session.clone().into(), window, cx);
                }
                return;
            }
            ServersEvent::SignIns { server, on } => {
                self.server_set_sign_ins(server, *on, cx);
                return;
            }
            ServersEvent::Remove(id) => {
                // Taken off the account after the pairing is forgotten: the
                // list asked again then no longer brings it back.
                let leave = self.servers.as_ref().and_then(|l| {
                    l.saved
                        .iter()
                        .find(|s| s.id == *id)
                        .and_then(servers::account_to_leave)
                        .map(str::to_string)
                });
                self.servers_forget(id, window, cx);
                if let Some(label) = leave {
                    self.servers_remove_machine(label, window, cx);
                }
                self.sync_servers_view(cx);
                return;
            }
            _ => {}
        }
        let Some(link) = &mut self.servers else {
            return;
        };
        link.error = None;
        match ev {
            ServersEvent::Pair {
                link: text,
                sign_ins,
            } => match servers::parse_link(text) {
                Ok(invite) => {
                    link.pair_sign_ins = *sign_ins;
                    let id = servers::new_id();
                    let client = Client::pair(
                        id.clone(),
                        invite,
                        servers::device_name(),
                        link.events.clone(),
                    );
                    // A pairing still under way is dropped, which hangs it up.
                    if let Some(before) = link.pairing.take() {
                        link.conns.remove(&before);
                    }
                    link.conns.insert(id.clone(), Conn::new(client));
                    link.pairing = Some(id.clone());
                    link.pairing_timeout = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(PAIRING_WAIT).await;
                        let _ = this.update(cx, |this, cx| this.servers_pairing_ran_out(&id, cx));
                    }));
                }
                Err(e) => link.error = Some(e),
            },
            ServersEvent::CancelPairing => {
                if let Some(id) = link.pairing.take() {
                    link.conns.remove(&id);
                }
                link.pairing_timeout = None;
            }
            ServersEvent::AddPlace { server, path } => {
                if let Some(conn) = link.conns.get(server) {
                    conn.client
                        .send(&json!({ "type": "add_place", "path": path }));
                    link.adding.insert(server.clone());
                    link.from_picker.remove(server);
                }
            }
            ServersEvent::Clone { server, url } => {
                if let Some(conn) = link.conns.get(server) {
                    conn.client.send(&json!({ "type": "clone", "url": url }));
                    link.adding.insert(server.clone());
                    link.from_picker.remove(server);
                }
            }
            ServersEvent::Remove(_)
            | ServersEvent::OpenHere { .. }
            | ServersEvent::SignIn
            | ServersEvent::SignIns { .. } => {}
            ServersEvent::Open { server, session } => {
                let (server, session) = (server.clone(), session.clone());
                self.servers_open_session(&server, &session, window, cx);
            }
            ServersEvent::Spread(on) => {
                link.balance.spread = *on;
                balance::save(&link.balance);
            }
            ServersEvent::Prefer { server, preference } => {
                link.balance.set(server.as_deref(), *preference);
                balance::save(&link.balance);
            }
        }
        self.sync_servers_view(cx);
    }

    /// Forgets a server: it is told to forget this app, and all that waits
    /// on it here is let go.
    fn servers_forget(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        link.error = None;
        if let Some(conn) = link.conns.remove(id) {
            conn.client.unpair();
        }
        link.git_asks
            .fail(id, "The server was removed before it answered.");
        // Only one paired by link has a token: one of the account has none.
        if link
            .saved
            .iter()
            .any(|s| s.id == *id && s.way == servers::Way::Direct)
        {
            servers::forget_token(id);
        }
        link.saved.retain(|s| s.id != *id);
        servers::save(&link.saved);
        if link.balance.servers.contains_key(id) {
            link.balance.forget(id);
            balance::save(&link.balance);
        }
        if link.inboxes.remove(id).is_some() || link.inboxes_unsaved {
            save_inboxes(link, cx);
        }
        link.following.retain(|f| f.server != *id);
        link.rested.retain(|f| f.server != *id);
        // What it was asked is not coming: the others' requests stay.
        link.adding.remove(id);
        link.from_picker.remove(id);
        if link.added.as_ref().is_some_and(|(s, _)| s == id) {
            link.added = None;
        }
        link.deleting.retain(|(server, _, _)| server != id);
        let new = link.new_env.remove(id);
        // Its link stops without a word: the phones waiting on a start hear it here.
        let waiting = link.starts.dropped(id);
        settle_first_prompts(&mut link.first_prompts, &link.starts, id);
        self.sync_remote_projects(window, cx);
        if let Some(new) = new {
            let why = "The server was removed before it made the environment.";
            self.servers_new_env_failed(new, why.into(), cx);
        }
        for client in waiting {
            self.phone_send(client, wire::error("that server was removed"));
        }
    }

    /// The link of a pairing still under way has run out: it is given up.
    fn servers_pairing_ran_out(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        if link.pairing.as_deref() != Some(id) {
            return;
        }
        link.pairing = None;
        link.conns.remove(id);
        link.error = Some("Pairing took too long; ask the server for a new link.".into());
        self.sync_servers_view(cx);
    }

    /// The last Computers tab was closed: what went wrong was about what it showed.
    pub(super) fn servers_tab_closed(&mut self, cx: &mut Context<Self>) {
        if let Some(link) = &mut self.servers {
            link.error = None;
        }
        self.sync_servers_view(cx);
    }

    /// Brings the servers' folders and sessions into the projects: new ones
    /// in, gone ones out, the rest brought up to date. A server that cannot be
    /// reached keeps what it last said, so its work stays in sight.
    fn sync_remote_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        // What each local repository is known by, read the first time.
        for p in self.projects.iter().filter(|p| !p.is_remote()) {
            link.origins.entry(p.path.clone()).or_insert_with(|| {
                crate::git::origin_url(&p.path)
                    .as_deref()
                    .and_then(repo_key)
            });
        }
        let locals: Vec<(PathBuf, Option<String>)> = self
            .projects
            .iter()
            .filter(|p| !p.is_remote())
            .map(|p| (p.path.clone(), link.origins.get(&p.path).cloned().flatten()))
            .collect();
        // Every folder the servers offer: its project, and itself as an environment.
        let mut wanted: Vec<(PathBuf, String, Remote, PlaceRow, Vec<SessionRow>)> = Vec::new();
        let mut retired = HashSet::new();
        for saved in &link.saved {
            let Some(conn) = link.conns.get(&saved.id) else {
                continue;
            };
            let gone = gone_places(&conn.sessions, &conn.places);
            retired.extend(gone.iter().map(|p| Remote::env_path(&saved.id, &p.path)));
            for place in conn.places.iter().chain(&gone) {
                let (project, _) = project_for(place, &saved.id, &locals);
                let remote = Remote {
                    server: saved.id.clone(),
                    machine: Self::server_name(saved).into(),
                    place: place.path.clone(),
                };
                let sessions = conn
                    .sessions
                    .iter()
                    .filter(|s| s.in_place(place))
                    .cloned()
                    .collect();
                wanted.push((
                    project,
                    place.project.clone(),
                    remote,
                    place.clone(),
                    sessions,
                ));
            }
        }
        // A folder the server no longer offers says nothing of its repository:
        // it stays in the project it is in, not in one of the server's own.
        for (project, _, remote, _, _) in &mut wanted {
            let path = Remote::env_path(&remote.server, &remote.place);
            if retired.contains(&path)
                && let Some(p) = self.projects.iter().find(|p| p.env_index(&path).is_some())
            {
                *project = p.path.clone();
            }
        }
        link.retired = retired;
        // Each folder is one environment, in the one project it belongs to.
        let keep: HashMap<PathBuf, PathBuf> = wanted
            .iter()
            .map(|(project, _, r, _, _)| (Remote::env_path(&r.server, &r.place), project.clone()))
            .collect();

        // What is on screen stays on screen, wherever it moves in the lists.
        let active = self.projects.get(self.active_project).map(|p| {
            (
                p.path.clone(),
                p.environments.get(p.active_env).map(|e| e.path.clone()),
            )
        });
        let on_screen = self.sessions_on_screen();
        let mut unread = false;
        let mut moving = take_misplaced(&mut self.projects, &keep);
        self.projects
            .retain(|p| !p.is_remote() || !p.environments.is_empty());

        for (project, name, remote, place, sessions) in wanted {
            let pi = match self.projects.iter().position(|p| p.path == project) {
                Some(pi) => pi,
                None => {
                    // At the end, so the projects before it keep their indexes.
                    let mut p = crate::state::new_project(&project);
                    p.name = name.into();
                    p.environments.clear();
                    self.projects.push(p);
                    self.projects.len() - 1
                }
            };
            let env_path = Remote::env_path(&remote.server, &remote.place);
            let p = &mut self.projects[pi];
            let ei = match p.env_index(&env_path) {
                Some(ei) => ei,
                None => {
                    // Moved here from another project: its tabs come along, and
                    // its draft stays with its path in the composer.
                    let env = moving.remove(&env_path).unwrap_or_else(|| {
                        let main = !place.repo.is_empty() && place.repo == place.path;
                        let branch = (!place.branch.is_empty()).then(|| place.branch.clone());
                        let mut env = Environment::new(env_path.clone(), branch, main);
                        env.agent = AgentKind::from_id(&place.agent);
                        env.remote = Some(remote.clone());
                        env
                    });
                    p.environments.push(env);
                    p.environments.len() - 1
                }
            };
            let env = &mut p.environments[ei];
            if place.repo.is_empty() {
                // Not a repository: no branch, where git would say a detached HEAD.
                env.set_no_branch();
            } else if !place.branch.is_empty() && env.branch.as_ref() != place.branch {
                env.set_branch(Some(place.branch.clone()));
            }
            env.remote = Some(remote);
            unread |= sync_sessions(&self.strip, &on_screen, env, sessions);
        }

        // An environment on screen that moved to another project is followed there.
        if let Some((path, env)) = active
            && let Some(pi) = env
                .as_ref()
                .and_then(|e| self.projects.iter().position(|p| p.env_index(e).is_some()))
                .or_else(|| self.projects.iter().position(|p| p.path == path))
        {
            self.active_project = pi;
            if let Some(ei) = env.and_then(|e| self.projects[pi].env_index(&e)) {
                self.projects[pi].active_env = ei;
            }
        }
        self.active_project = self
            .active_project
            .min(self.projects.len().saturating_sub(1));
        // The tabs of a folder the servers no longer list close; one that
        // moved to another project keeps its tabs, which go by its path.
        self.settle_strip(cx);
        if unread {
            self.save(cx);
        }
        self.servers_open_started(window, cx);
        self.servers_open_added(window, cx);
        // A window that had no projects polls none: the server's ones need it.
        self.sync_poll(window, cx);
        // Phones list the servers' sessions too.
        self.phone_inbox_changed(cx);
        cx.notify();
    }

    /// A session of a server's environment was put on screen: a viewer for
    /// it, which the server fills once it follows it.
    pub(super) fn view_remote(
        &mut self,
        env_path: &Path,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = self.session_mut(env_path, id) else {
            return;
        };
        let (agent, title, status) = (s.agent, s.title.to_string(), s.status);
        let thread = Thread::viewer(empty_transcript(agent, &title), thread_status(status), cx);
        thread.update(cx, |t, _| t.set_remote());
        let remote = self.find_env(env_path).and_then(|e| e.remote.as_ref());
        let link = self.servers.as_ref();
        let known = remote.zip(link).and_then(|(r, link)| {
            link.commands
                .get(&(r.server.clone(), r.place.clone(), agent))
                .cloned()
        });
        if let Some(commands) = known {
            thread.update(cx, |t, cx| t.set_remote_commands(commands, cx));
        }
        // Looked at before: what it held then shows until it is read again,
        // rather than an empty thread.
        let kept = remote
            .zip(link)
            .and_then(|(r, link)| rested_of(&link.rested, &r.server, id));
        if let Some(f) = kept {
            // With its status now, as the server last listed it.
            show_following(f, status, &thread, cx);
        }
        self.attach(env_path, id, thread, window, cx);
    }

    /// The viewer of `server`'s session `id` is gone: a new one must be
    /// filled again from the start.
    pub(super) fn servers_unfollow(&mut self, server: &str, id: &str) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let gone = unfollow(&mut link.following, server, id);
        close_follows(link, gone);
    }

    /// `who` looks away from what it followed: a session nobody else looks at
    /// is no longer followed.
    pub(super) fn servers_look_away(&mut self, who: Watcher) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let gone = let_go(&mut link.following, who, None);
        close_follows(link, gone);
    }

    /// The server sends session `id`, of the environment at `env_path`, whole
    /// and then every change, for as long as `who` or anyone else looks at it.
    /// The one `who` looked at before is let go.
    pub(super) fn servers_follow(
        &mut self,
        env_path: &Path,
        id: &str,
        who: Watcher,
        cx: &mut Context<Self>,
    ) {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.clone()) else {
            return;
        };
        let session = self
            .find_env(env_path)
            .and_then(|e| e.sessions.iter().find(|s| s.id.as_ref() == id))
            .map(|s| (s.agent, s.title.to_string(), s.status));
        let Some((agent, title, status)) = session else {
            return;
        };
        let Some(link) = &mut self.servers else {
            return;
        };
        if !link.conns.contains_key(&remote.server) {
            return;
        }
        let kept = wake(&mut link.rested, &remote.server, id);
        let fresh = kept.map(|f| Following {
            env_path: env_path.to_path_buf(),
            title: title.clone(),
            status,
            ..f
        });
        let fresh = fresh.unwrap_or_else(|| Following {
            server: remote.server.clone(),
            session: id.to_string(),
            env_path: env_path.to_path_buf(),
            entries: Vec::new(),
            subagents: Vec::new(),
            agent,
            cwd: PathBuf::from(&remote.place),
            title,
            status,
            asking: true,
            asked: 0,
            again: None,
            refill: Some(Vec::new()),
            broken: None,
            by: Vec::new(),
            queue: RemoteQueue::default(),
            images: HashMap::new(),
        });
        let several = link.conns[&remote.server].watches_several;
        let followed = follow_in(&mut link.following, who, fresh, several);
        close_follows(link, followed.close);
        // Not connected: it is asked for when the server is back.
        if followed.ask
            && let Some(conn) = link.conns.get(&remote.server)
            && conn.reach == Reach::Connected
        {
            conn.client
                .send(&json!({ "type": "open", "session": id, "keep": true }));
            conn.client
                .send(&json!({ "type": "transcript", "session": id, "from": 0 }));
        }
        self.servers_displaced(&remote.machine, followed.displaced, cx);
    }

    /// Sessions an older server stopped sending, for one opened after them:
    /// whoever looked at them is told, and follows them again by opening them.
    fn servers_displaced(
        &mut self,
        machine: &str,
        displaced: Vec<Following>,
        cx: &mut Context<Self>,
    ) {
        let why = format!(
            "{machine} sends one session at a time, and another was opened there. Update arakun-server on it to follow both."
        );
        for f in displaced {
            for who in &f.by {
                match who {
                    Watcher::Desktop => {
                        if let Some(thread) = thread_of(&self.projects, &f.env_path, &f.session) {
                            let why = format!("{why} Open this session again to follow it.");
                            thread.update(cx, |t, cx| t.post_notice(why, true, cx));
                        }
                    }
                    Watcher::Phone(client) => self.phone_send(*client, wire::error(&why)),
                }
            }
        }
    }

    /// What the user did in a server's session goes to the server.
    pub(super) fn servers_remote_action(
        &mut self,
        env_path: &Path,
        id: &str,
        action: &RemoteAction,
        cx: &mut Context<Self>,
    ) {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.clone()) else {
            return;
        };
        let msg = match action {
            RemoteAction::Send { text, now } => {
                json!({ "type": "send", "text": text, "uploads": [], "now": now })
            }
            // Paused, not dropped: the queue stays on show, to resume or empty.
            RemoteAction::Cancel => json!({ "type": "cancel", "keep_queue": true }),
            RemoteAction::Unqueue { index, text, now } => {
                json!({ "type": "unqueue", "index": index, "text": text, "now": now })
            }
            RemoteAction::ResumeQueue => json!({ "type": "resume_queue" }),
            RemoteAction::Permission(option) => json!({ "type": "permission", "option": option }),
            RemoteAction::Answer(answers) => json!({ "type": "answer", "answers": answers }),
            RemoteAction::SkipQuestion => json!({ "type": "skip_question" }),
            RemoteAction::Secret { id, values } => {
                // The thread checks this too; plain http must not carry a secret.
                if values.is_some() && self.server_in_the_clear(&remote.server) {
                    return;
                }
                json!({ "type": "secret", "id": id, "values": values })
            }
            RemoteAction::Fork => json!({ "type": "fork" }),
            RemoteAction::Compact => json!({ "type": "compact" }),
            RemoteAction::RewindPlan(entry) => json!({ "type": "rewind", "entry": entry }),
            RemoteAction::Rewind(entry) => {
                json!({ "type": "rewind", "entry": entry, "confirm": true })
            }
            RemoteAction::Setting(s, value) => {
                json!({ "type": "setting", "key": s.key(), "value": value })
            }
        };
        self.servers_send(&remote.server, id, msg, cx);
    }

    /// `/stage` in a server's session: git runs there, where the files are.
    pub(super) fn servers_stage(&mut self, env_path: &Path, id: &str, cx: &mut Context<Self>) {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.clone()) else {
            return;
        };
        self.servers_send(&remote.server, id, json!({ "type": "stage" }), cx);
    }

    /// "Continue with" `agent` in a server's session: the server hands it
    /// over, and answers with `continued`.
    pub(super) fn servers_continue_with(
        &mut self,
        env_path: &Path,
        id: &str,
        agent: AgentKind,
        cx: &mut Context<Self>,
    ) {
        let Some(env) = self.find_env(env_path) else {
            return;
        };
        let Some(remote) = env.remote.clone() else {
            return;
        };
        let settings = super::sessions::server_picks(env, agent);
        // As in `servers_start`: the new session must not start in a folder
        // the server no longer offers. The old session stays as it is.
        if self.server_env_retired(env_path) {
            if let Some(thread) = thread_of(&self.projects, env_path, id) {
                let notice = format!(
                    "{} no longer offers this folder, so no new session starts in it.",
                    remote.machine
                );
                thread.update(cx, |t, cx| t.post_notice(notice, true, cx));
            }
            return;
        }
        // A pick the server does not offer would only fail the new session
        // there, after the old one is gone.
        let offered = self.servers.as_ref().and_then(|link| {
            link.options
                .get(&(remote.server.clone(), remote.place.clone(), agent))
        });
        let settings = offered_picks(&settings, offered);
        let msg = json!({ "type": "continue_with", "agent": agent.id(), "settings": settings });
        self.servers_send(&remote.server, id, msg, cx);
    }

    /// `server` handed its session `old` to another agent as session `new`:
    /// the tab showing `old` shows `new`, and `old` is gone.
    fn servers_continued(
        &mut self,
        server: &str,
        old: &str,
        new: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The server sends its inbox before this: the new session is here.
        let Some(env_path) = self.server_session_env(server, new) else {
            return;
        };
        let Some(env) = self.find_env_mut(&env_path) else {
            return;
        };
        let Some(agent) = env.session_index(new).map(|i| env.sessions[i].agent) else {
            return;
        };
        self.rename_phone_session(
            &phone_session_id(server, old),
            &phone_session_id(server, new),
        );
        let Some(env) = self.find_env_mut(&env_path) else {
            return;
        };
        if let Some(i) = env.session_index(old) {
            let gone = env.sessions.remove(i);
            if let Some(live) = gone.live {
                live.thread.update(cx, |t, _| t.shutdown());
                self.servers_unfollow(server, old);
            }
        }
        for tab in &mut self.strip.tabs {
            if tab.is_in(&env_path)
                && matches!(&tab.kind, TabKind::Session { id, .. } if id.as_ref() == old)
            {
                tab.kind = TabKind::Session {
                    agent,
                    id: new.to_string().into(),
                };
            }
        }
        self.servers_open_session(server, new, window, cx);
        self.save(cx);
    }

    /// The rewinds shown in `server`'s sessions end: what the server says of
    /// them is not coming. One under way may still have run there; its new
    /// session then shows up with the inbox.
    fn servers_drop_rewinds(&mut self, server: &str, cx: &mut Context<Self>) {
        let threads: Vec<Entity<Thread>> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .filter(|e| e.remote.as_ref().is_some_and(|r| r.server == server))
            .flat_map(|e| &e.sessions)
            .filter_map(|s| s.live.as_ref().map(|l| l.thread.clone()))
            .collect();
        for thread in threads {
            thread.update(cx, |t, cx| t.set_remote_rewind_plan(None, None, cx));
        }
    }

    /// The thread on screen for `server`'s session `session`, if any.
    fn server_thread(&self, server: &str, session: &str) -> Option<Entity<Thread>> {
        let env = self.server_session_env(server, session)?;
        thread_of(&self.projects, &env, session)
    }

    /// A prompt phone `client` sent to `server`'s session `id`. One that
    /// cannot go is said to that phone; it is not this app's to put back.
    pub(super) fn servers_phone_send(
        &mut self,
        server: &str,
        id: &str,
        text: &str,
        now: bool,
        client: ClientId,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let connected = self
            .servers
            .as_ref()
            .and_then(|l| l.conns.get(server))
            .is_some_and(|c| c.reach == Reach::Connected);
        if !connected {
            self.phone_send(client, wire::error(PHONE_PROMPT_LOST));
        }
        // `phone` is this app's own: the server reads past it.
        let msg =
            json!({ "type": "send", "text": text, "uploads": [], "now": now, "phone": client });
        self.servers_send(server, id, msg, cx);
    }

    /// A message about session `id` to its server, or a word in the session
    /// on why it cannot go.
    fn servers_send(&mut self, server: &str, id: &str, mut msg: Value, cx: &mut Context<Self>) {
        msg["session"] = json!(id);
        let sent = self.servers.as_ref().and_then(|link| {
            let conn = link.conns.get(server)?;
            (conn.reach == Reach::Connected).then(|| conn.client.send(&msg))
        });
        if sent.is_none()
            && let Some(env) = self.server_session_env(server, id)
            && let Some(thread) = thread_of(&self.projects, &env, id)
        {
            thread.update(cx, |t, cx| {
                t.remote_undelivered(cx);
                t.post_notice(
                    String::from("The server is not connected, so this did not reach it."),
                    true,
                    cx,
                )
            });
        }
    }

    /// The first prompt in a server's environment starts a session there,
    /// with the `settings` picked for it, for `by`.
    /// The `ref` it went out with, or `None` when the server cannot be reached.
    pub(super) fn servers_start(
        &mut self,
        env_path: &Path,
        agent: AgentKind,
        text: &str,
        settings: &[(String, String)],
        by: Asker,
        mentions_from: Option<&Path>,
    ) -> Option<u64> {
        let remote = self.find_env(env_path).and_then(|e| e.remote.clone())?;
        // The server finds what the prompt mentions there too, when it is
        // one of its folders other than where the session starts.
        let mentions_from = mentions_from
            .and_then(|p| self.find_env(p))
            .and_then(|e| e.remote.as_ref())
            .filter(|r| r.server == remote.server && r.place != remote.place)
            .map(|r| r.place.clone());
        let Some(link) = &mut self.servers else {
            return None;
        };
        let Some(conn) = link.conns.get(&remote.server) else {
            link.error = Some(format!(
                "{} is not paired with this app any more.",
                remote.machine
            ));
            return None;
        };
        if conn.reach != Reach::Connected {
            link.error = Some(format!("{} is not connected right now.", remote.machine));
            return None;
        }
        if link.retired.contains(env_path) {
            link.error = Some(format!(
                "{} no longer offers this folder, so no new session starts in it.",
                remote.machine
            ));
            return None;
        }
        // A pick the server does not offer (one from this machine's
        // catalog) would only fail there: it starts on its own instead.
        let offered = link
            .options
            .get(&(remote.server.clone(), remote.place.clone(), agent));
        let settings = offered_picks(settings, offered);
        let reference = link.starts.sent(&remote.server, by);
        let mut start = json!({
            "type": "start", "env": remote.place, "agent": agent.id(), "text": text,
            "settings": settings, "ref": reference,
        });
        // Left out otherwise: a server from before it reads none the same.
        if let Some(from) = mentions_from {
            start["mentions_from"] = from.into();
        }
        conn.client.send(&start);
        Some(reference)
    }

    /// Asks the server of the environment at `env_path` what its model menu
    /// offers: the model and reasoning of a new session of each agent there,
    /// and what `session` is on. Asking for a session's settings starts its
    /// agent there, as a phone's does, so it waits for the menu to open.
    pub(super) fn servers_ask_settings(&self, env_path: &Path, session: Option<&str>) {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.as_ref()) else {
            return;
        };
        let Some(conn) = self
            .servers
            .as_ref()
            .and_then(|link| link.conns.get(&remote.server))
            .filter(|c| c.reach == Reach::Connected)
        else {
            return;
        };
        for agent in AgentKind::ALL {
            conn.client
                .send(&json!({ "type": "options", "env": remote.place, "agent": agent.id() }));
        }
        if let Some(id) = session {
            conn.client
                .send(&json!({ "type": "settings", "session": id }));
        }
    }

    /// A phone's `options` for a server's environment: the server is asked,
    /// and its answer goes to `client` as well.
    pub(super) fn servers_ask_options_for(
        &mut self,
        client: ClientId,
        env_path: &Path,
        agent: AgentKind,
    ) {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.clone()) else {
            return;
        };
        let connected = self.servers.as_ref().is_some_and(|link| {
            link.conns
                .get(&remote.server)
                .is_some_and(|c| c.reach == Reach::Connected)
        });
        if !connected {
            return;
        }
        self.servers_ask_settings(env_path, None);
        if let Some(link) = &mut self.servers {
            let key = (remote.server, remote.place, agent);
            let waiting = link.phone_options.entry(key).or_default();
            if !waiting
                .iter()
                .any(|(c, p, a)| *c == client && p == env_path && *a == agent)
            {
                waiting.push((client, env_path.to_path_buf(), agent));
            }
        }
    }

    /// What a new `agent` session in a server's environment starts on for a
    /// setting, and the server's choices: the environment's pick when the
    /// server offers it. None until the server said, and for settings it
    /// takes none of from a device.
    pub(super) fn server_draft_choices(
        &self,
        env: &Environment,
        agent: AgentKind,
        s: Setting,
    ) -> Option<(String, Vec<SettingOption>)> {
        let remote = env.remote.as_ref()?;
        let key = (remote.server.clone(), remote.place.clone(), agent);
        let offered = self.servers.as_ref()?.options.get(&key)?;
        draft_on_server(offered, agent, super::sessions::agent_draft(env, agent), s)
    }

    /// Where git runs for the environment at `env_path`: in its folder here,
    /// or on the server that has it.
    ///
    /// A server reached over plain http gets its git too: its diffs and commit
    /// messages cross the network in the clear, but so does everything else on
    /// that link, sessions and token included. Refusing git alone would not
    /// protect the code, and the README says so.
    pub(super) fn git_at(&self, env_path: &Path) -> servers::git::GitAt {
        use servers::git::{GitAt, ServerGit};
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.as_ref()) else {
            return GitAt::Here(env_path.to_path_buf());
        };
        let link = self.servers.as_ref();
        GitAt::Server(ServerGit {
            server: remote.server.clone(),
            place: remote.place.clone(),
            link: link
                .and_then(|l| l.conns.get(&remote.server))
                .map(|c| c.client.sender()),
            asks: link.map(|l| l.git_asks.clone()).unwrap_or_default(),
        })
    }

    /// Keeps the first prompt of the start `reference` until its server answers.
    pub(super) fn servers_first_prompt(&mut self, reference: u64, first: FirstPrompt) {
        let Some(remote) = self.find_env(&first.env).and_then(|e| e.remote.clone()) else {
            return;
        };
        if let Some(link) = &mut self.servers {
            link.first_prompts.insert(reference, (remote.server, first));
        }
    }

    /// The start `reference` was answered: what it started with, if kept.
    fn take_first_prompt(&mut self, reference: Option<u64>) -> Option<FirstPrompt> {
        let link = self.servers.as_mut()?;
        link.first_prompts
            .remove(&reference?)
            .map(|(_, first)| first)
    }

    /// A first prompt that did not start its session goes back in the
    /// composer, with its note for the agent and the folder it was written
    /// in, unless something new was typed there since. `why` says what
    /// happened; the island tells it.
    fn servers_put_back(
        &mut self,
        env_path: &Path,
        prompt: String,
        note: Option<String>,
        mentions_from: Option<PathBuf>,
        why: &str,
        cx: &mut Context<Self>,
    ) {
        let active = self.has_projects() && self.env().path == env_path;
        let Some(env) = self.find_env(env_path) else {
            return;
        };
        let machine = env
            .remote
            .as_ref()
            .map(|r| r.machine.to_string())
            .unwrap_or_default();
        let back = self
            .composer
            .read(cx)
            .draft(env_path)
            .is_none_or(|d| d.text.trim().is_empty());
        if back {
            self.edit_draft(env_path, cx, |d| d.set(prompt));
            if let Some(note) = note {
                self.rename_notes
                    .entry(env_path.to_path_buf())
                    .or_insert(note);
            }
            if let Some(from) = mentions_from {
                self.mention_sources.insert(env_path.to_path_buf(), from);
            }
        }
        if back && active {
            self.composer_changed(cx);
        }
        let then = if back {
            "It is back in the composer."
        } else {
            "The composer already had text, so it was not put back."
        };
        self.notify(
            crate::island::AlertKind::Warning,
            &format!("The prompt did not reach {machine}"),
            &format!("{why} {then}"),
            None,
            cx,
        );
        cx.notify();
    }

    /// A prompt that never left for `server`: it goes back in its
    /// environment's composer, unless something new was typed there since.
    fn servers_undelivered(&mut self, server: &str, msg: &Value, cx: &mut Context<Self>) {
        let env_path = match msg["type"].as_str() {
            Some("start") => {
                let reference = msg["ref"].as_u64();
                let asker = self
                    .servers
                    .as_mut()
                    .and_then(|link| reference.and_then(|r| link.starts.undelivered(r)));
                let first = self.take_first_prompt(reference);
                let why = "The connection dropped first.";
                match asker {
                    // A phone's prompt: the phone hears it did not go.
                    Some(Asker::Phone(client)) => {
                        let why = wire::error("the server went away before the session started");
                        self.phone_send(client, why);
                        return;
                    }
                    Some(Asker::Gone) => return,
                    // The drop was told first, and phones heard it then: only
                    // a prompt typed here comes back.
                    None => {
                        if let Some(first) = first.filter(|f| f.desktop) {
                            self.servers_put_back(
                                &first.env,
                                first.text,
                                first.note,
                                first.mentions_from,
                                why,
                                cx,
                            );
                        }
                        return;
                    }
                    Some(Asker::Desktop) => match first {
                        Some(first) => {
                            self.servers_put_back(
                                &first.env,
                                first.text,
                                first.note,
                                first.mentions_from,
                                why,
                                cx,
                            );
                            return;
                        }
                        None => Remote::env_path(server, &text(msg, "env")),
                    },
                }
            }
            Some("send") => {
                let session = text(msg, "session");
                // A phone's: told to that phone if it still looks at the session.
                if let Some(client) = msg["phone"].as_u64() {
                    if self.phone_looks_at(client, &phone_session_id(server, &session)) {
                        self.phone_send(client, wire::error(PHONE_PROMPT_LOST));
                    }
                    return;
                }
                let Some(env) = self.server_session_env(server, &session) else {
                    return;
                };
                env
            }
            Some(kind @ ("add_place" | "clone")) => {
                if let Some(link) = &mut self.servers
                    && let Some(why) = folder_request_lost(&mut link.adding, server, kind)
                {
                    link.folder_failed(server, why);
                    self.sync_servers_view(cx);
                }
                return;
            }
            // The branch keeps the name it has; the naming says so, as when
            // the server is not connected at all.
            Some("rename_branch") => {
                let why = format!(
                    "The connection dropped before the server renamed it. It stays {}.",
                    text(msg, "from")
                );
                self.notify(
                    crate::island::AlertKind::Warning,
                    "Could not name the branch",
                    &why,
                    None,
                    cx,
                );
                return;
            }
            // The threads stay as they were; settling them again is safe.
            Some("settle_env") => {
                self.notify(
                    crate::island::AlertKind::Warning,
                    "Could not archive the environment",
                    "The connection dropped first. Try again once the server is connected.",
                    None,
                    cx,
                );
                return;
            }
            // An answer that did not go can be given again, and the rest asked again.
            Some(
                "answer" | "skip_question" | "fork" | "compact" | "rewind" | "stage" | "setting"
                | "continue_with",
            ) => {
                let session = text(msg, "session");
                if let Some(env) = self.server_session_env(server, &session)
                    && let Some(thread) = thread_of(&self.projects, &env, &session)
                {
                    thread.update(cx, |t, cx| {
                        t.remote_undelivered(cx);
                        t.post_notice(
                            String::from("The connection dropped before this reached the server."),
                            true,
                            cx,
                        );
                    });
                }
                return;
            }
            Some("term_open") => {
                self.server_term_lost(msg, cx);
                return;
            }
            Some("list_dir" | "read_file" | "list_files") => {
                self.server_files_lost(server, msg, cx);
                return;
            }
            _ => return,
        };
        let why = "The connection dropped first.";
        self.servers_put_back(&env_path, text(msg, "text"), None, None, why, cx);
    }

    /// The environment `server`'s session `session` is in.
    fn server_session_env(&self, server: &str, session: &str) -> Option<PathBuf> {
        self.projects
            .iter()
            .flat_map(|p| &p.environments)
            .find(|e| {
                e.remote.as_ref().is_some_and(|r| r.server == server)
                    && e.session_index(session).is_some()
            })
            .map(|e| e.path.clone())
    }

    /// Why the last thing asked of the servers did not go, for a word beside
    /// the composer.
    pub(super) fn servers_refusal(&self) -> Option<String> {
        self.servers.as_ref()?.error.clone()
    }

    /// A session picked in the Computers tab opens in its environment's tab.
    fn servers_open_session(
        &mut self,
        server: &str,
        session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self.projects.iter().enumerate().find_map(|(pi, p)| {
            p.environments.iter().enumerate().find_map(|(ei, e)| {
                (e.remote.as_ref().is_some_and(|r| r.server == server)
                    && e.session_index(session).is_some())
                .then_some((pi, ei))
            })
        });
        let Some((pi, ei)) = found else {
            return;
        };
        self.select_env(pi, ei, window, cx);
        self.open_session(session.to_string().into(), window, cx);
    }

    /// The form to clone a repository on a server, or add one of its folders.
    pub(super) fn servers_add_place(
        &mut self,
        server: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_servers_tab(window, cx);
        self.views
            .servers
            .update(cx, |v, cx| v.add_place(server, window, cx));
    }

    /// A folder a server added for this app is an environment now: it opens.
    fn servers_open_added(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((server, place)) = self.servers.as_ref().and_then(|l| l.added.clone()) else {
            return;
        };
        let env_path = Remote::env_path(&server, &place);
        let found = self
            .projects
            .iter()
            .enumerate()
            .find_map(|(pi, p)| p.env_index(&env_path).map(|ei| (pi, ei)));
        if let Some((pi, ei)) = found {
            let new = self.servers.as_mut().and_then(|link| {
                link.added = None;
                link.new_env.remove(&server)
            });
            if let Some(asked) = new.as_ref().and_then(|n| n.dialog)
                && self
                    .create_env_dialog()
                    .is_some_and(|d| d.entity_id() == asked)
            {
                self.close_modal(window, cx);
            }
            let (named, prompt) = match new {
                Some(n) => (n.auto_name.then_some(n.branch), n.prompt),
                None => (None, None),
            };
            let env = &mut self.projects[pi].environments[ei];
            if let Some(branch) = named.filter(|b| env.branch.as_ref() == b) {
                env.placeholder = Some(branch);
            }
            self.select_env(pi, ei, window, cx);
            if let Some(prompt) = prompt {
                self.start_queued_on_server(&env_path, prompt, cx);
            }
        }
    }

    /// Phone `client` left, or every phone with `None`: the sessions it
    /// started on servers are not told to it, nor to a phone after it.
    pub(super) fn servers_phone_left(&mut self, client: Option<ClientId>) {
        if let Some(link) = &mut self.servers {
            link.starts.phone_left(client);
        }
        match client {
            Some(c) => self.servers_look_away(Watcher::Phone(c)),
            None => {
                let phones: Vec<ClientId> = self
                    .servers
                    .iter()
                    .flat_map(|l| &l.following)
                    .flat_map(|f| &f.by)
                    .filter_map(|w| match w {
                        Watcher::Phone(c) => Some(*c),
                        Watcher::Desktop => None,
                    })
                    .collect();
                for c in phones {
                    self.servers_look_away(Watcher::Phone(c));
                }
            }
        }
    }

    /// A new environment in project `pi` on the server of its environment
    /// `from`: a worktree there on a new branch with a random name, which its
    /// first prompt names, as here.
    /// With `prompt`, its session starts in the new environment.
    pub(super) fn servers_new_env(
        &mut self,
        pi: usize,
        from: usize,
        prompt: Option<super::sessions::QueuedPrompt>,
        cx: &mut Context<Self>,
    ) {
        let remote = self
            .projects
            .get(pi)
            .and_then(|p| p.environments.get(from))
            .and_then(|e| e.remote.clone());
        let (Some(project), Some(remote)) = (self.projects.get(pi), remote) else {
            if let Some(prompt) = prompt {
                self.unqueue(prompt, cx);
            }
            return;
        };
        let taken: Vec<String> = project
            .environments
            .iter()
            .map(|e| e.branch.to_string())
            .collect();
        let branch = crate::env_name::random(|n| taken.iter().any(|t| t == n));
        self.servers_new_env_on(
            &remote.server,
            &remote.place,
            branch,
            true,
            None,
            prompt,
            cx,
        );
    }

    /// Where a new environment goes while they are spread: one of `machines`, each a server's id or `None` for this computer,
    /// listed with the open environment's machine first. `None` when they
    /// are not spread or none of them can take it.
    pub(super) fn servers_spread_pick(&self, machines: &[Option<&str>]) -> Option<usize> {
        let link = self.servers.as_ref().filter(|l| l.balance.spread)?;
        let working = |status: Status| status == Status::Working;
        // A server that refused `new_env` before, or is still making a folder,
        // would refuse this one too.
        let can_take = |m: &Option<&str>| {
            m.is_none_or(|id| !link.no_new_env.contains(id) && !link.adding.contains(id))
        };
        let eligible: Vec<usize> = (0..machines.len())
            .filter(|&i| can_take(&machines[i]))
            .collect();
        let candidates: Vec<Candidate> = eligible
            .iter()
            .map(|&i| &machines[i])
            .map(|m| Candidate {
                preference: link.balance.of(*m),
                busy: match m {
                    None => self
                        .projects
                        .iter()
                        .flat_map(|p| &p.environments)
                        .filter(|e| e.remote.is_none())
                        .flat_map(|e| &e.sessions)
                        .filter(|s| working(s.status))
                        .count(),
                    Some(id) => link.conns.get(*id).map_or(0, |c| {
                        c.sessions.iter().filter(|s| working(s.status)).count()
                    }),
                },
            })
            .collect();
        balance::pick(&candidates).map(|i| eligible[i])
    }

    /// Whether a first prompt in the server environment at `env_path` starts
    /// right there, the main folder too: its server makes no worktrees yet,
    /// or the folder is no repository to make one of.
    pub(super) fn servers_start_in_place(&self, env_path: &Path) -> bool {
        let server = self.find_env(env_path).and_then(|e| e.remote.as_ref());
        let (Some(link), Some(r)) = (&self.servers, server) else {
            return false;
        };
        link.no_new_env.contains(&r.server)
            || link.conns.get(&r.server).is_some_and(|c| {
                c.places
                    .iter()
                    .any(|p| p.path == r.place && p.repo.is_empty())
            })
    }

    /// A worktree on `branch` made on `server`, of the repository its folder
    /// `from` is a checkout of. With `auto_name`, the first prompt there names
    /// it. With `dialog`, that New environment dialog waits on the answer.
    pub(super) fn servers_new_env_on(
        &mut self,
        server: &str,
        from: &str,
        branch: String,
        auto_name: bool,
        dialog: Option<gpui::EntityId>,
        prompt: Option<super::sessions::QueuedPrompt>,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let machine = link
            .saved
            .iter()
            .find(|s| s.id == server)
            .map(Self::server_name)
            .unwrap_or_default();
        let problem = match link.conns.get(server) {
            None => Some(format!("{machine} is not paired with this app any more.")),
            Some(c) if c.reach != Reach::Connected => {
                Some(format!("{machine} is not connected right now."))
            }
            // One at a time per server: its answer says only which folder.
            _ if link.adding.contains(server) => Some(format!(
                "A folder is being made on {machine}; try again once it is there."
            )),
            _ => None,
        };
        let new = NewEnv {
            branch,
            auto_name,
            dialog,
            prompt,
        };
        if let Some(why) = problem {
            self.servers_new_env_failed(new, why, cx);
            return;
        }
        let Some(conn) = link.conns.get(server) else {
            return;
        };
        conn.client
            .send(&json!({ "type": "new_env", "from": from, "branch": new.branch }));
        link.adding.insert(server.to_string());
        link.new_env.insert(server.to_string(), new);
        self.sync_servers_view(cx);
    }

    /// A new environment did not come: said in the dialog that asked, or by
    /// the island when the sidebar did.
    /// A prompt that waited for it goes back in its composer.
    fn servers_new_env_failed(&mut self, new: NewEnv, why: String, cx: &mut Context<Self>) {
        if let Some(prompt) = new.prompt {
            self.unqueue(prompt, cx);
        }
        if let Some(dialog) = self
            .create_env_dialog()
            .filter(|d| new.dialog == Some(d.entity_id()))
        {
            dialog.update(cx, |d, cx| d.failed(why, cx));
            return;
        }
        self.notify(
            crate::island::AlertKind::Warning,
            "Could not make the environment",
            &why,
            None,
            cx,
        );
    }

    /// Whether the server environment at `env_path` is a folder its server
    /// no longer offers, kept for its sessions: nothing new starts from it.
    pub(super) fn server_env_retired(&self, env_path: &Path) -> bool {
        self.servers
            .as_ref()
            .is_some_and(|l| l.retired.contains(env_path))
    }

    /// The New environment dialog, when it is open.
    fn create_env_dialog(&self) -> Option<Entity<crate::dialogs::CreateEnvDialog>> {
        match &self.modal {
            Some((Modal::CreateEnv(d), _)) => Some(d.clone()),
            _ => None,
        }
    }

    /// The name of the repository the server's folder `place` is a checkout
    /// of, as the server calls it: new worktrees there go in a folder of it.
    pub(super) fn server_repo_folder(&self, server: &str, place: &str) -> Option<SharedString> {
        let conn = self.servers.as_ref()?.conns.get(server)?;
        let row = conn.places.iter().find(|p| p.path == place)?;
        let repo = if row.repo.is_empty() {
            &row.path
        } else {
            &row.repo
        };
        repo_folder(repo).map(|n| n.to_string().into())
    }

    /// The branch of a server's environment, renamed there. The app shows the
    /// new name once the server says it is so (`branch_renamed`). False when
    /// the server cannot be reached.
    pub(super) fn servers_rename_branch(&mut self, env_path: &Path, from: &str, to: &str) -> bool {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.clone()) else {
            return false;
        };
        let Some(conn) = self
            .servers
            .as_ref()
            .and_then(|l| l.conns.get(&remote.server))
            .filter(|c| c.reach == Reach::Connected)
        else {
            return false;
        };
        conn.client.send(&json!({
            "type": "rename_branch", "place": remote.place, "from": from, "to": to,
        }));
        true
    }

    /// The sessions of a server's environment stop asking for attention, there.
    pub(super) fn servers_settle_env(&mut self, env_path: &Path, cx: &mut Context<Self>) {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.clone()) else {
            return;
        };
        match self.connected(&remote.server) {
            // The inbox brings the sessions back settled.
            Some(client) => client.send(&json!({ "type": "settle_env", "place": remote.place })),
            None => self.notify(
                crate::island::AlertKind::Warning,
                "Could not archive the environment",
                &format!("{} is not connected right now.", remote.machine),
                None,
                cx,
            ),
        }
    }

    /// The connection to `server` when it is up.
    fn connected(&self, server: &str) -> Option<&Client> {
        let conn = self.servers.as_ref()?.conns.get(server)?;
        (conn.reach == Reach::Connected).then_some(&conn.client)
    }

    /// The Delete environment dialog for a server's environment `ei` of
    /// project `pi`. The server deletes the worktree; its answer closes the dialog.
    pub(super) fn servers_delete_env(
        &mut self,
        pi: usize,
        ei: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let Some(env) = self.projects.get(pi).and_then(|p| p.environments.get(ei)) else {
            return;
        };
        let Some(remote) = env.remote.clone().filter(|_| !env.is_checkout) else {
            return;
        };
        let branch = env.checked_out().map(str::to_string);
        let place = PathBuf::from(&remote.place);
        let machine = remote.machine.clone();
        let confirmed = branch.clone();
        let dialog = cx.new(|cx| DeleteEnvDialog::remote(place, branch, machine, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, dialog, ev: &DialogEvent, window, cx| match ev {
                DialogEvent::DeleteOnServer { delete_branch } => {
                    let asked = dialog.entity_id();
                    let branch = delete_branch.then_some(confirmed.as_deref()).flatten();
                    if let Err(why) = this.servers_ask_delete(&remote, branch, asked) {
                        dialog.update(cx, |d, cx| d.failed(why, cx));
                    }
                }
                // A deletion under way still ends as the server says, with
                // the island telling why when it fails.
                _ => this.close_modal(window, cx),
            },
        );
        self.modal = Some((Modal::DeleteEnv(dialog), sub));
        cx.notify();
    }

    /// Asks their servers to delete the environments of `remotes`, picked
    /// together in the sidebar and confirmed in the dialog `asked`. Their
    /// branches stay.
    pub(super) fn servers_delete_envs(
        &mut self,
        remotes: &[(Remote, String)],
        asked: gpui::EntityId,
        cx: &mut Context<Self>,
    ) {
        for (remote, title) in remotes {
            if let Err(why) = self.servers_ask_delete(remote, None, asked) {
                self.servers_delete_failed(asked, format!("{title}: {why}"), cx);
            }
        }
    }

    fn servers_ask_delete(
        &mut self,
        remote: &Remote,
        branch: Option<&str>,
        dialog: gpui::EntityId,
    ) -> Result<(), String> {
        // Its reply names only the server and the place: with two asked, it
        // could be taken for the other's.
        if let Some(link) = &self.servers
            && deleting_already(&link.deleting, &remote.server, &remote.place)
        {
            return Err("It is being deleted already. Wait for that to finish.".into());
        }
        let Some(client) = self.connected(&remote.server) else {
            return Err(format!("{} is not connected right now.", remote.machine));
        };
        client.send(&json!({
            "type": "delete_env", "place": remote.place,
            "delete_branch": branch.is_some(), "branch": branch,
        }));
        if let Some(link) = &mut self.servers {
            link.deleting
                .push((remote.server.clone(), remote.place.clone(), dialog));
        }
        Ok(())
    }

    fn delete_dialog(&self) -> Option<Entity<DeleteEnvDialog>> {
        match &self.modal {
            Some((Modal::DeleteEnv(d), _)) => Some(d.clone()),
            _ => None,
        }
    }

    /// A deletion did not happen: said in the dialog that asked while it is
    /// open, else by the island.
    fn servers_delete_failed(
        &mut self,
        asked: gpui::EntityId,
        why: String,
        cx: &mut Context<Self>,
    ) {
        match self.delete_dialog().filter(|d| d.entity_id() == asked) {
            Some(dialog) => dialog.update(cx, |d, cx| d.failed(why, cx)),
            None => self.notify(
                crate::island::AlertKind::Warning,
                "Could not delete the environment",
                &why,
                None,
                cx,
            ),
        }
    }

    /// The server answered a `delete_env`. The inbox came first, so a deleted
    /// environment is already gone from the sidebar.
    fn servers_env_deleted(
        &mut self,
        server: &str,
        msg: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let place = text(msg, "place");
        let Some(link) = &mut self.servers else {
            return;
        };
        let Some(at) = link
            .deleting
            .iter()
            .position(|(s, p, _)| s == server && *p == place)
        else {
            return;
        };
        let (_, _, asked) = link.deleting.remove(at);
        let error = msg["error"].as_str().map(str::to_string);
        if msg["deleted"] != true {
            let why = error.unwrap_or_else(|| "The server did not delete it.".into());
            self.servers_delete_failed(asked, why, cx);
            return;
        }
        if self.delete_dialog().is_some_and(|d| d.entity_id() == asked) {
            self.close_modal(window, cx);
        }
        if let Some(why) = error {
            self.notify(
                crate::island::AlertKind::Warning,
                "The environment is deleted",
                &why,
                None,
                cx,
            );
        }
        self.ensure_terminal(window, cx);
    }

    /// A session this app started on a server is in its inbox now: its tab opens.
    fn servers_open_started(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((server, id)) = self.servers.as_ref().and_then(|l| l.starting_id.clone()) else {
            return;
        };
        let here = self.projects.iter().flat_map(|p| &p.environments).any(|e| {
            e.remote.as_ref().is_some_and(|r| r.server == server) && e.session_index(&id).is_some()
        });
        if here {
            if let Some(link) = &mut self.servers {
                link.starting_id = None;
            }
            self.servers_open_session(&server, &id, window, cx);
        }
    }

    /// What a connection's thread says: how it stands, or a message from the server.
    fn servers_heard(
        &mut self,
        id: &str,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.servers else {
            return;
        };
        match event {
            Event::Undelivered(msg) if msg["type"] == "git" => {
                link.git_asks.undelivered(id, &msg);
                return;
            }
            Event::Undelivered(msg) => {
                self.servers_undelivered(id, &msg, cx);
                return;
            }
            Event::Paired(saved) => {
                // A pairing given up (cancelled, or another link pasted over
                // it) may still finish on its way out. Its connection is gone,
                // so it would sit in the list as a server nothing talks to,
                // and take the pairing now under way for done.
                if link.pairing.as_deref() != Some(id) || !link.conns.contains_key(id) {
                    // Its token went to the keyring all the same.
                    servers::forget_token(id);
                    return;
                }
                link.pairing = None;
                link.pairing_timeout = None;
                // "The server is not at the relay" was about the wait that just ended.
                link.error = None;
                let mut saved = saved;
                saved.sign_ins = link.pair_sign_ins;
                link.saved.push(saved);
                servers::save(&link.saved);
            }
            Event::TokenKept => {
                match link.saved.iter_mut().find(|s| s.id == id) {
                    Some(s) if !s.token.is_empty() => {
                        s.token = Default::default();
                        servers::save(&link.saved);
                    }
                    Some(_) => {}
                    // Forgotten while its token was on its way to the keyring.
                    None => servers::forget_token(id),
                }
                return;
            }
            Event::KeyPinned(label) => {
                if let Some(s) = link.saved.iter_mut().find(|s| s.id == id)
                    && s.key != label
                {
                    s.key = label;
                    servers::save(&link.saved);
                }
                return;
            }
            Event::Status(status) => {
                let pairing = link.pairing.as_deref() == Some(id);
                let reach = match status {
                    LinkStatus::Connecting => Reach::Connecting,
                    LinkStatus::Connected => Reach::Connected,
                    LinkStatus::ServerAway => {
                        // Mid-pairing this is why nothing happens: say it. The
                        // pairing goes on if the server is back before the link runs out.
                        if pairing {
                            link.error = Some(
                                "The server is not at the relay right now. Pairing goes on if it comes back while the link lasts.".into(),
                            );
                        }
                        Reach::Away("The server is not at the relay right now.".into())
                    }
                    LinkStatus::Waiting { why, .. } => Reach::Away(why),
                    // A link that did not work leaves nothing to keep.
                    LinkStatus::Refused(why) if pairing => {
                        link.pairing = None;
                        link.pairing_timeout = None;
                        link.conns.remove(id);
                        link.error = Some(why);
                        self.sync_servers_view(cx);
                        return;
                    }
                    LinkStatus::Refused(why) => {
                        // Kept in the file: after a restart nothing else
                        // would remember that the server removed this app.
                        if why == servers::client::UNPAIRED_BY_SERVER
                            && servers::mark_removed(&mut link.saved, id)
                        {
                            servers::save(&link.saved);
                        }
                        Reach::Refused(why)
                    }
                };
                let back = reach == Reach::Connected;
                // Git's answers would have come on the connection that dropped.
                if back {
                    link.git_asks.back(id);
                } else {
                    link.git_asks.fail(
                        id,
                        "The connection to the server dropped before it answered.",
                    );
                }
                // The answer would have come on the connection that dropped,
                // but the server may still finish: the form says to look first.
                let lost_env = if back { None } else { link.new_env.remove(id) };
                if !back && link.adding.remove(id) && lost_env.is_none() {
                    link.folder_failed(
                        id,
                        "The connection dropped before the server said whether the folder was added. Check its folders before you try again.".into(),
                    );
                }
                let waiting = if back {
                    Vec::new()
                } else {
                    link.starts.dropped(id)
                };
                // Whether those starts got there is not known. One that never
                // went out is told back after this, by itself: its first
                // prompt is kept for that until the server is back.
                if back {
                    settle_first_prompts(&mut link.first_prompts, &link.starts, id);
                }
                let mut lost_delete = Vec::new();
                if !back {
                    link.deleting.retain(|(server, _, asked)| {
                        let lost = server == id;
                        if lost {
                            lost_delete.push(*asked);
                        }
                        !lost
                    });
                }
                if let Some(conn) = link.conns.get_mut(id) {
                    let now = unix_now();
                    match (conn.reach == Reach::Connected, back) {
                        (false, true) => {
                            conn.since = Some(now);
                            // Its agents and their limits; it says again when they change.
                            conn.client.send(&json!({ "type": "usage" }));
                        }
                        (true, false) => {
                            conn.since = None;
                            conn.seen = Some(now);
                        }
                        _ => {}
                    }
                    conn.reach = reach;
                    // A new connection is a new client there, which heard
                    // nothing, and may be an updated server that takes idle time.
                    conn.idle_told = None;
                    if back {
                        conn.no_idle = false;
                    }
                    // Back after a drop: the followed session is asked for
                    // again, whole, since what changed meanwhile was not
                    // heard. It is read to the side and swapped in when complete.
                    let back_to = link.following.iter_mut().filter(|f| back && f.server == id);
                    for f in back_to {
                        f.read_again();
                        conn.client
                            .send(&json!({ "type": "open", "session": f.session, "keep": true }));
                        conn.client.send(
                            &json!({ "type": "transcript", "session": f.session, "from": 0 }),
                        );
                    }
                }
                // The server may have started it and lost only the answer: a
                // phone that asks again could run the prompt twice.
                for client in waiting {
                    let why = wire::error(
                        "the connection to the server dropped before it said whether the session started. Check its sessions before you try again",
                    );
                    self.phone_send(client, why);
                }
                if let Some(new) = lost_env {
                    let why = "The connection dropped before the server said whether it made the environment. Check its environments before you try again.";
                    self.servers_new_env_failed(new, why.into(), cx);
                }
                // Its answer would have come on the connection that dropped.
                for asked in lost_delete {
                    let why = "The server dropped before it answered. If the environment is still there, try again.";
                    self.servers_delete_failed(asked, why.into(), cx);
                }
                if back {
                    self.server_terms_back(id, cx);
                    self.server_files_back(id, cx);
                    self.server_mirror(id, cx);
                    // A new connection is a new client there: it hears its
                    // Linear settings again, in case they changed meanwhile.
                    self.linear_push_server(id, cx);
                } else {
                    self.servers_drop_rewinds(id, cx);
                }
            }
            Event::Message(msg) => {
                let kind = msg["type"].as_str().unwrap_or("");
                if kind == "git" {
                    link.git_asks.answered(id, &msg);
                    return;
                }
                // The refusal names `git` first: it is about the Changes
                // panel, and none of what else was asked.
                if kind == "error" && older_server_refused(&text(&msg, "message"), &["git"]) {
                    link.git_asks.older(id);
                    return;
                }
                // A server from before it took Linear's work.
                if kind == "error"
                    && older_server_refused(&text(&msg, "message"), &["linear_settings"])
                {
                    link.error = Some(LINEAR_TOO_OLD.into());
                    self.sync_servers_view(cx);
                    return;
                }
                // Knows `git`, but not this request: only that one fails.
                if kind == "error" && link.git_asks.refused(id, &text(&msg, "message")) {
                    return;
                }
                if self.server_term_message(id, &msg, cx) || self.server_files_message(id, &msg, cx)
                {
                    return;
                }
                let Some(link) = &mut self.servers else {
                    return;
                };
                if kind == "mirror_done" {
                    self.server_mirrored(id, &msg, cx);
                    return;
                }
                if kind == "sign_ins_forgotten" {
                    self.server_forgot_sign_ins(id, &msg, cx);
                    return;
                }
                // A server from before mirroring, or before it kept sign-ins
                // by device: it keeps what it has.
                if kind == "error"
                    && older_server_refused(&text(&msg, "message"), &["mirror", "forget_sign_ins"])
                {
                    return;
                }
                // A server from before it took idle time: it waits as it did.
                if kind == "error" && older_server_refused(&text(&msg, "message"), &["idle"]) {
                    if let Some(conn) = link.conns.get_mut(id) {
                        conn.no_idle = true;
                    }
                    return;
                }
                if matches!(kind, "market" | "market_done") {
                    self.views
                        .extensions
                        .update(cx, |v, cx| v.server_said(id, &msg, cx));
                    return;
                }
                // Only an error about what the Marketplace asked is taken
                // there; a start's goes on to whoever asked for it.
                if kind == "error"
                    && self
                        .views
                        .extensions
                        .update(cx, |v, cx| v.server_error(id, &msg, cx))
                {
                    return;
                }
                if kind == "usage" {
                    if let Some(conn) = link.conns.get_mut(id) {
                        conn.agents = crate::servers_view::agents_of(&msg);
                    }
                    self.sync_servers_view(cx);
                    return;
                }
                if msg["type"] == "inbox" {
                    // Kept for the next start, when what it holds changed.
                    let kept = servers::kept_inbox(&msg);
                    // One that could not be written is tried again with the next.
                    let changed = link
                        .inboxes
                        .get(id)
                        .is_none_or(|old| !servers::same_kept(old, &kept));
                    if link.saved.iter().any(|s| s.id == id) && (changed || link.inboxes_unsaved) {
                        link.inboxes.insert(id.to_string(), kept);
                        save_inboxes(link, cx);
                    }
                    self.servers_inbox(id, &msg);
                    self.sync_remote_projects(window, cx);
                } else if msg["type"] == "place_added" {
                    if link.adding.remove(id) {
                        link.from_picker.remove(id);
                        link.added = Some((id.to_string(), text(&msg, "path")));
                        self.views.servers.update(cx, |v, cx| v.place_added(id, cx));
                    }
                    // The server sends its inbox before this: the folder is here.
                    self.servers_open_added(window, cx);
                } else if msg["type"] == "env_deleted" {
                    self.servers_env_deleted(id, &msg, window, cx);
                } else if msg["type"] == "place_failed" {
                    // What was being added is not coming: the form can ask again.
                    if link.adding.remove(id) {
                        let message = text(&msg, "message");
                        // A new environment: said to whoever asked for it.
                        match link.new_env.remove(id) {
                            // Its repository has no commits to make a worktree
                            // of: the prompt starts where it was written, as
                            // it does here then.
                            Some(NewEnv {
                                prompt: Some(prompt),
                                ..
                            }) if msg["no_commits"] == true => {
                                let from = prompt.from().to_path_buf();
                                self.start_queued_on_server(&from, prompt, cx);
                            }
                            Some(new) => self.servers_new_env_failed(new, message, cx),
                            None => link.folder_failed(id, message),
                        }
                    }
                } else if msg["type"] == "error"
                    && link.adding.contains(id)
                    && older_server_refused(&text(&msg, "message"), &["add_place", "clone"])
                {
                    // A server from before folders could be added: it does not
                    // know the request, and says so in a plain error.
                    link.adding.remove(id);
                    link.folder_failed(
                        id,
                        "This server cannot add folders yet. Update arakun-server on it and try again."
                            .into(),
                    );
                } else if msg["type"] == "started" {
                    let session = text(&msg, "id");
                    let answered = link.starts.answered_ref(id);
                    let first = answered
                        .as_ref()
                        .and_then(|(_, r)| link.first_prompts.remove(r))
                        .map(|(_, first)| first);
                    // The branch is named after the first prompt that started, there as here.
                    if let Some(first) = first {
                        self.name_after_first_prompt(&first.env, &first.text, first.agent, cx);
                    }
                    let Some(link) = &mut self.servers else {
                        return;
                    };
                    match answered.map(|(by, _)| by) {
                        Some(Asker::Desktop) => link.starting_id = Some((id.to_string(), session)),
                        // Started from a phone: it opens there, not here.
                        Some(Asker::Phone(client)) => {
                            let started =
                                json!({ "type": "started", "id": phone_session_id(id, &session) });
                            self.phone_send(client, started.to_string());
                            self.phone_inbox_changed(cx);
                        }
                        Some(Asker::Gone) | None => {}
                    }
                    // The server sends its inbox before this: the session is here.
                    self.servers_open_started(window, cx);
                } else if msg["type"] == "continued" {
                    let (old, new) = (text(&msg, "id"), text(&msg, "session"));
                    self.servers_continued(id, &old, &new, window, cx);
                } else if msg["type"] == "forked" || msg["type"] == "rewound" {
                    // The server sends its inbox before this: the session is here.
                    let session = text(&msg, "session");
                    self.servers_open_session(id, &session, window, cx);
                    if msg["type"] == "rewound"
                        && let Some(env_path) = self.server_session_env(id, &session)
                        && self.find_env(&env_path).is_some()
                    {
                        self.edit_draft(&env_path, cx, |d| d.set(text(&msg, "prompt")));
                        self.composer_changed(cx);
                        self.focus_composer(window, cx);
                    }
                } else if msg["type"] == "branch_renamed" {
                    let env_path = Remote::env_path(id, &text(&msg, "place"));
                    let (from, to) = (text(&msg, "from"), text(&msg, "to"));
                    // The inbox sent after it may have told of a newer rename
                    // already: this one is then over, its note too.
                    let current = self.find_env(&env_path).map(|e| e.branch.to_string());
                    if current.is_some_and(|b| b == from || b == to) {
                        self.branch_renamed(&env_path, &from, to, cx);
                    }
                } else if msg["type"] == "error" && self.servers_answer_failed(id, &msg, cx) {
                    // Said to whoever asked.
                } else {
                    self.servers_message(id, &msg, cx);
                }
            }
        }
        self.sync_servers_view(cx);
    }

    /// An error that answers a start, a rename or a new environment, told to
    /// whoever asked for it. False for the rest, which the session on screen
    /// or the Computers tab shows; a start this app asked for among them.
    fn servers_answer_failed(&mut self, id: &str, msg: &Value, cx: &mut Context<Self>) -> bool {
        let Some(link) = &mut self.servers else {
            return false;
        };
        let message = text(msg, "message");
        let about = if about_start(msg) {
            Some("start")
        } else {
            msg["about"].as_str()
        };
        match about {
            Some("start") => {
                let answered = link.starts.answered_ref(id);
                let first = self.take_first_prompt(answered.as_ref().map(|(_, r)| *r));
                match answered.map(|(by, _)| by) {
                    Some(Asker::Phone(client)) => {
                        self.phone_send(client, wire::error(&message));
                        true
                    }
                    Some(Asker::Gone) => true,
                    // Also shown in the Computers tab, whose start it ends.
                    Some(Asker::Desktop) | None => {
                        if let Some(first) = first.filter(|f| f.desktop) {
                            let why = format!("The server said: {message}.");
                            self.servers_put_back(
                                &first.env,
                                first.text,
                                first.note,
                                first.mentions_from,
                                &why,
                                cx,
                            );
                        }
                        false
                    }
                }
            }
            Some("rename_branch") => {
                let why = format!("{message}. `arakun rename <name>` in its terminal renames it.");
                self.notify(
                    crate::island::AlertKind::Warning,
                    "Could not name the branch",
                    &why,
                    None,
                    cx,
                );
                true
            }
            // A new environment asked from the sidebar: said there, not in
            // the Computers tab. A server that does not know `new_env` yet
            // turns the message itself down.
            // Its own failures come as `place_failed`; any other error is
            // about something else and is left to the session or the tab.
            // A server from before environments could be deleted from here.
            _ if link.deleting.iter().any(|(s, _, _)| s == id)
                && older_server_refused(&message, &["delete_env"]) =>
            {
                // It refuses every one asked of it.
                let asked: Vec<gpui::EntityId> = link
                    .deleting
                    .iter()
                    .filter(|(s, _, _)| s == id)
                    .map(|(_, _, d)| *d)
                    .collect();
                link.deleting.retain(|(s, _, _)| s != id);
                for asked in asked {
                    let why = "This server runs an older Arakun. Update it to delete environments from here.";
                    self.servers_delete_failed(asked, why.into(), cx);
                }
                true
            }
            // Archive is asked from the sidebar: said there too. The
            // refusal names `settle_env` itself, so it is about that.
            // A server from before it sent images: they stay links.
            _ if older_server_refused(&message, &["image"]) => {
                if let Some(conn) = link.conns.get_mut(id) {
                    conn.no_images = true;
                }
                true
            }
            _ if older_server_refused(&message, &["settle_env"]) => {
                self.notify(
                    crate::island::AlertKind::Warning,
                    "Could not archive the environment",
                    "This server runs an older Arakun. Update it to archive from here.",
                    None,
                    cx,
                );
                true
            }
            _ if older_server_refused(
                &message,
                &["fork", "compact", "rewind", "stage", "continue_with"],
            ) =>
            {
                self.servers_drop_rewinds(id, cx);
                self.notify(
                    crate::island::AlertKind::Warning,
                    "The server cannot do that yet",
                    "This server runs an older Arakun. Update it to fork, compact, rewind, stage or hand over its sessions from here.",
                    None,
                    cx,
                );
                true
            }
            _ if link.new_env.contains_key(id) && older_server_refused(&message, &["new_env"]) => {
                link.adding.remove(id);
                link.no_new_env.insert(id.to_string());
                match link.new_env.remove(id) {
                    // A prompt that waited for the worktree starts where it
                    // was written, as on that server before.
                    Some(NewEnv {
                        prompt: Some(prompt),
                        ..
                    }) => {
                        let from = prompt.from().to_path_buf();
                        self.start_queued_on_server(&from, prompt, cx);
                    }
                    Some(new) => {
                        let why = "This server runs an older Arakun. Update it to make environments there.";
                        self.servers_new_env_failed(new, why.into(), cx);
                    }
                    None => {}
                }
                true
            }
            _ => false,
        }
    }

    /// The server's list of folders and sessions.
    fn servers_inbox(&mut self, id: &str, msg: &Value) {
        let Some(conn) = self.servers.as_mut().and_then(|l| l.conns.get_mut(id)) else {
            return;
        };
        let rows = |key: &str| msg[key].as_array().cloned().unwrap_or_default();
        conn.watches_several = msg["watch_several"] == true;
        conn.sessions = rows("sessions")
            .iter()
            .map(|s| SessionRow {
                id: text(s, "id"),
                title: text(s, "title"),
                agent: text(s, "agent"),
                status: status_from(s["status"].as_str().unwrap_or("")),
                updated: s["updated"].as_u64().unwrap_or(0),
                place: text(s, "place"),
                env: text(s, "env"),
            })
            .collect();
        conn.places = rows("places")
            .iter()
            .map(|p| PlaceRow {
                path: text(p, "path"),
                project: text(p, "project"),
                branch: text(p, "branch"),
                agent: text(p, "agent"),
                repo: text(p, "repo"),
                origin: text(p, "origin"),
            })
            .collect();
    }

    fn servers_message(&mut self, id: &str, msg: &Value, cx: &mut Context<Self>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let Some(conn) = link.conns.get_mut(id) else {
            return;
        };
        let following = link
            .following
            .iter_mut()
            .find(|f| f.server == id && msg["id"] == f.session.as_str());
        match msg["type"].as_str().unwrap_or("") {
            // A session's model and reasoning, asked for or just changed.
            "settings" => {
                let Some(f) = following else { return };
                if let Some(thread) = thread_of(&self.projects, &f.env_path, &f.session) {
                    let list = wire::read_settings(&msg["settings"]);
                    thread.update(cx, |t, cx| t.set_remote_settings(list, cx));
                }
                // The composer reads them from the thread, and draws again.
                cx.notify();
            }
            // What a new session of an agent in one of its folders starts on.
            "options" => {
                let Some(agent) = AgentKind::parse(msg["agent"].as_str().unwrap_or("")) else {
                    return;
                };
                let key = (id.to_string(), text(msg, "env"), agent);
                let waiting = link.phone_options.remove(&key).unwrap_or_default();
                link.options
                    .insert(key, wire::read_settings(&msg["settings"]));
                for (client, env_path, agent) in waiting {
                    let reply = self.phone_options(&env_path, agent);
                    self.phone_send(client, reply);
                }
                cx.notify();
            }
            "commands" => {
                let Some(f) = following else { return };
                let commands = wire::read_commands(&msg["commands"]);
                let (env_path, session, agent) = (f.env_path.clone(), f.session.clone(), f.agent);
                if let Some(place) = self
                    .find_env(&env_path)
                    .and_then(|e| e.remote.as_ref())
                    .map(|r| r.place.clone())
                    && let Some(link) = &mut self.servers
                {
                    link.commands
                        .insert((id.to_string(), place, agent), commands.clone());
                }
                if let Some(thread) = thread_of(&self.projects, &env_path, &session) {
                    thread.update(cx, |t, cx| t.set_remote_commands(commands, cx));
                }
                // A `/` typed before they came shows them now.
                self.update_completion(cx);
            }
            "session" | "state" => {
                let Some(f) = following else { return };
                f.status = status_from(msg["status"].as_str().unwrap_or(""));
                if let Some(title) = msg["title"].as_str().filter(|t| !t.is_empty()) {
                    f.title = title.to_string();
                }
                f.queue = RemoteQueue::of(msg);
                let Some(thread) = thread_of(&self.projects, &f.env_path, &f.session) else {
                    return;
                };
                let (status, transcript) = (thread_status(f.status), transcript_of(f));
                let (permission, question) = (msg["permission"].clone(), msg["question"].clone());
                let secret = msg["secret"].clone();
                let private = !link
                    .saved
                    .iter()
                    .find(|s| s.id == id)
                    .is_none_or(in_the_clear);
                let queue = f.queue.clone();
                thread.update(cx, |t, cx| {
                    t.show_transcript(transcript, status, cx);
                    t.set_remote_asks(&permission, &question, cx);
                    t.set_remote_secret(&secret, private, cx);
                    t.set_remote_queue(queue.prompts, queue.paused, queue.sending_now, cx);
                });
            }
            // Entries changed, cut down for a phone: the whole ones are asked for.
            "entries" => {
                let Some(f) = following else { return };
                if msg["older"] == true || f.broken.is_some() {
                    return;
                }
                let from = msg["from"].as_u64().unwrap_or(0) as usize;
                if f.asking {
                    f.again = Some(f.again.map_or(from, |a| a.min(from)));
                } else {
                    (f.asking, f.asked) = (true, from);
                    conn.client
                        .send(&json!({ "type": "transcript", "session": f.session, "from": from }));
                }
            }
            "transcript" => {
                let Some(f) = following else { return };
                if f.broken.is_some() {
                    return;
                }
                if let Some(agent) = AgentKind::parse(msg["agent"].as_str().unwrap_or("")) {
                    f.agent = agent;
                }
                f.cwd = PathBuf::from(text(msg, "cwd"));
                let asked = f.asked;
                let next = page_of(msg).map(|page| match f.refill.as_mut() {
                    Some(refill) => merge(refill, page, asked),
                    None => merge(&mut f.entries, page, asked),
                });
                let ask = match next {
                    // Not what was asked for: that answer is still on its way.
                    Ok(Next::Stale) => return,
                    Ok(Next::Ask(from)) => Some(from),
                    Ok(Next::Done) => {
                        // Read whole: now it takes the place of what was shown.
                        if let Some(whole) = f.refill.take() {
                            f.entries = whole;
                        }
                        // What changed while this one was out.
                        f.again.take()
                    }
                    // Asking again would bring the same answer. What is here
                    // stays on show, and the session says why it stops there.
                    failed => {
                        (f.asking, f.again, f.refill) = (false, None, None);
                        let why = match failed {
                            // The reason without serde's list of every kind it knows.
                            Err(e) => format!(
                                "A newer server? This session holds something this app cannot read ({}). Update this app to follow it.",
                                e.split(", expected").next().unwrap_or(&e)
                            ),
                            _ => "The server stopped sending this session part way.".into(),
                        };
                        f.broken = Some(why);
                        None
                    }
                };
                match ask {
                    Some(from) => {
                        f.asked = from;
                        conn.client.send(
                            &json!({ "type": "transcript", "session": f.session, "from": from }),
                        );
                    }
                    None => f.asking = false,
                }
                // A refill shows once it is whole; what is on screen stays until then.
                if f.refill.is_none()
                    && let Some(thread) = thread_of(&self.projects, &f.env_path, &f.session)
                {
                    show_following(f, f.status, &thread, cx);
                }
                if f.refill.is_none() && !conn.no_images {
                    for (link, at) in images_to_ask(&f.entries, &f.images) {
                        let copy = f.images.entry(link.clone()).or_default();
                        (copy.at, copy.waiting) = (at, true);
                        conn.client.send(&json!({
                            "type": "image", "session": f.session, "path": link, "at": at,
                        }));
                    }
                }
            }
            "subagents" => {
                let Some(f) = following else { return };
                let Ok(subagents) = wire::read_subagents(msg) else {
                    return;
                };
                f.subagents = subagents;
                if let Some(thread) = thread_of(&self.projects, &f.env_path, &f.session) {
                    show_following(f, f.status, &thread, cx);
                }
            }
            "image" => {
                let Some(f) = following else { return };
                let path = text(msg, "path");
                let at = msg["at"].as_u64().unwrap_or(0) as usize;
                let Some(copy) = f.images.get_mut(&path) else {
                    return;
                };
                if at >= copy.at {
                    copy.waiting = false;
                }
                if msg["missing"] == true {
                    return;
                }
                let (server, session, msg) = (f.server.clone(), f.session.clone(), msg.clone());
                // Decoding and writing it to disk hold up nothing else.
                let kept = cx.background_spawn(crate::guard::task_or(
                    async move { keep_image(&msg, &images_dir()) },
                    |_| None,
                ));
                cx.spawn(async move |this, cx| {
                    let Some(file) = kept.await else { return };
                    let _ = this.update(cx, |this, cx| {
                        this.servers_image_kept(&server, &session, path, at, file, cx);
                    });
                })
                .detach();
            }
            "rewind_plan" => {
                let Some(thread) = self.server_thread(id, &text(msg, "id")) else {
                    return;
                };
                let list = |k: &str| msg[k].as_array().cloned().unwrap_or_default();
                let evicted = list("evicted")
                    .iter()
                    .filter_map(|e| {
                        let why = e[1].as_str().unwrap_or("").to_string();
                        Some((e[0].as_str()?.to_string(), why))
                    })
                    .collect();
                let plan =
                    (msg["gone"] != true).then(|| (list("restore"), list("delete"), evicted));
                let entry = msg["entry"].as_u64().map(|e| e as usize);
                let error = msg["error"]
                    .as_str()
                    .map(|e| format!("Rewind is not available: {e}."));
                thread.update(cx, |t, cx| {
                    t.set_remote_rewind_plan(entry, plan, cx);
                    if let Some(error) = error {
                        t.post_notice(error, true, cx);
                    }
                });
            }
            "staged" => {
                let Some(thread) = self.server_thread(id, &text(msg, "id")) else {
                    return;
                };
                let staged = match msg["error"].as_str() {
                    Some(e) => Err(e.to_string()),
                    None => Ok(msg["files"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|f| f.as_str().map(str::to_string))
                        .collect()),
                };
                let (notice, error) = crate::agent::staged_notice(staged);
                thread.update(cx, |t, cx| t.post_notice(notice, error, cx));
            }
            // The server took its Linear settings, or says why not.
            "linear_settings" => {
                if let Some(e) = msg["error"].as_str() {
                    link.error = Some(format!("Linear: {e}"));
                }
            }
            "error" => {
                let message = text(msg, "message");
                // An error about a session names it.
                let about = msg["id"].as_str();
                // Said in that session's tab when it is open, else in the Computers tab.
                let thread = about.and_then(|session| {
                    let env = self
                        .projects
                        .iter()
                        .flat_map(|p| &p.environments)
                        .find(|e| {
                            e.remote.as_ref().is_some_and(|r| r.server == id)
                                && e.session_index(session).is_some()
                        })?;
                    thread_of(&self.projects, &env.path, session)
                });
                match thread {
                    Some(thread) => {
                        thread.update(cx, |t, cx| t.post_notice(message, true, cx));
                    }
                    None => link.error = Some(message),
                }
            }
            _ => {}
        }
    }
}

impl Shell {
    /// A copy of a server's image is on disk: the session shows it, unless
    /// it is no longer followed or a newer answer for it came first.
    fn servers_image_kept(
        &mut self,
        server: &str,
        session: &str,
        link: String,
        at: usize,
        file: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(link_state) = &mut self.servers else {
            return;
        };
        let Some(f) = link_state
            .following
            .iter_mut()
            .find(|f| f.server == server && f.session == session)
        else {
            return;
        };
        let Some(copy) = f.images.get_mut(&link) else {
            return;
        };
        if copy.file.is_some() && at < copy.file_at {
            return;
        }
        (copy.file, copy.file_at) = (Some(file.clone()), at);
        if let Some(thread) = thread_of(&self.projects, &f.env_path, &f.session) {
            thread.update(cx, |t, cx| t.image_fetched(link, file, cx));
        }
    }
}

/// Takes each server environment out of a project `keep` no longer puts it in.
/// One that belongs in another project now is handed back, so it moves there
/// with its tabs and draft; one the servers no longer list is dropped.
fn take_misplaced(
    projects: &mut [Project],
    keep: &HashMap<PathBuf, PathBuf>,
) -> HashMap<PathBuf, Environment> {
    let mut moving = HashMap::new();
    for p in projects {
        let open = p.environments.get(p.active_env).map(|e| e.path.clone());
        let misplaced = p
            .environments
            .extract_if(.., |e| {
                e.remote.is_some() && keep.get(&e.path) != Some(&p.path)
            })
            .collect::<Vec<_>>();
        for env in misplaced {
            if keep.contains_key(&env.path) {
                moving.insert(env.path.clone(), env);
            }
        }
        p.active_env = open.and_then(|o| p.env_index(&o)).unwrap_or(0);
    }
    moving
}

/// Brings an environment's sessions in line with the server's list, newest
/// first. A session with a tab open stays until its tab closes, and one that
/// finishes while the user looks elsewhere waits on them until opened. True
/// when one did.
fn sync_sessions(
    strip: &model::TabStrip,
    on_screen: &[(PathBuf, SharedString)],
    env: &mut Environment,
    rows: Vec<SessionRow>,
) -> bool {
    let mut unread = false;
    let ids: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let open: HashSet<SharedString> = strip
        .tabs
        .iter()
        .filter(|t| t.is_in(&env.path))
        .filter_map(|t| match &t.kind {
            TabKind::Session { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    env.sessions
        .retain(|s| ids.contains(s.id.as_ref()) || open.contains(&s.id));
    for (i, row) in rows.into_iter().enumerate() {
        let at = match env.session_index(&row.id) {
            Some(at) => at,
            None => {
                let s = Session::new(
                    row.id.clone().into(),
                    row.title.clone().into(),
                    agent_of(&row.agent),
                );
                env.sessions.insert(i.min(env.sessions.len()), s);
                i.min(env.sessions.len() - 1)
            }
        };
        let away = !on_screen
            .iter()
            .any(|(p, id)| *p == env.path && id.as_ref() == row.id);
        let s = &mut env.sessions[at];
        if away && s.status == Status::Working && row.status == Status::Idle {
            s.unread = true;
            unread = true;
        }
        s.status = row.status;
        s.updated = row.updated;
        if !row.title.is_empty() {
            s.title = row.title.into();
        }
    }
    unread
}

/// An empty transcript for a session that is about to be read.
fn empty_transcript(agent: AgentKind, title: &str) -> Transcript {
    Transcript {
        version: 1,
        meta: SessionMeta {
            agent,
            session_id: None,
            title: title.to_string(),
            cwd: PathBuf::new(),
            created: unix_now(),
            updated: unix_now(),
            pr: None,
            seat: None,
            seat_key: None,
        },
        entries: Vec::new(),
        usage: Usage::default(),
        fork_point: None,
        interrupted: false,
        resume: false,
        queue: Vec::new(),
        handoff: None,
        subagents: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(current: &str, ids: &[&str]) -> (String, Vec<SettingOption>) {
        let options = ids
            .iter()
            .map(|id| SettingOption {
                id: id.to_string(),
                name: id.to_uppercase(),
                detail: None,
                description: None,
                disabled: None,
            })
            .collect();
        (current.to_string(), options)
    }

    fn pair(k: Setting, v: &str) -> (String, String) {
        (k.key().to_string(), v.to_string())
    }

    #[test]
    fn a_failed_clone_from_the_picker_is_told_in_a_notice_once() {
        let mut from_picker = HashSet::from(["a".to_string()]);
        let mut failed = Vec::new();
        let why = route_folder_failure(&mut from_picker, &mut failed, "a", "no".into());
        assert_eq!(why, None);
        assert_eq!(failed, [("a".to_string(), "no".to_string())]);
        // The next failure there came from the Computers tab.
        let why = route_folder_failure(&mut from_picker, &mut failed, "a", "again".into());
        assert_eq!(why.as_deref(), Some("again"));
        assert_eq!(failed.len(), 1);
    }

    #[test]
    fn images_are_asked_for_once_and_again_when_named_again() {
        let agent = |text: &str| Entry::Agent { text: text.into() };
        let mut entries = vec![
            agent("![a](shot.png)\n\n![c](https://x.dev/c.png)"),
            agent("![again](shot.png)\n\n![d](/srv/out/d.png)"),
        ];
        let mut asked = HashMap::new();
        assert_eq!(
            images_to_ask(&entries, &asked),
            [
                ("shot.png".to_string(), 1),
                ("/srv/out/d.png".to_string(), 1)
            ]
        );
        for (link, at) in images_to_ask(&entries, &asked) {
            asked.insert(
                link,
                ImageCopy {
                    at,
                    waiting: true,
                    ..ImageCopy::default()
                },
            );
        }
        assert_eq!(images_to_ask(&entries, &asked), []);
        // A later turn shows the same name: it may be a new image.
        entries.push(agent("Redrawn:\n\n![a](shot.png)"));
        assert_eq!(
            images_to_ask(&entries, &asked),
            [("shot.png".to_string(), 2)]
        );
        // The images a prompt mentions are the server's too.
        entries.push(Entry::User {
            text: "compare @shot.png with @\"my shots/b.jpg\" and @notes.md".into(),
            at: 1,
            elapsed_ms: None,
            message_id: None,
            tokens: None,
            shell_changes: Vec::new(),
        });
        assert_eq!(
            images_to_ask(&entries, &asked),
            [
                ("shot.png".to_string(), 3),
                ("my shots/b.jpg".to_string(), 3)
            ]
        );
    }

    #[test]
    fn a_reconnect_asks_again_for_the_images_still_waited_for() {
        let mut f = following("srv", "s1", Vec::new());
        let copy = |file: Option<&str>, waiting| ImageCopy {
            at: 0,
            file: file.map(PathBuf::from),
            file_at: 0,
            waiting,
        };
        f.images.insert("lost.png".into(), copy(None, true));
        f.images
            .insert("kept.png".into(), copy(Some("/tmp/k.png"), true));
        f.images.insert("none.png".into(), copy(None, false));
        f.read_again();
        assert!(!f.images.contains_key("lost.png"));
        assert_eq!(f.images["kept.png"], copy(Some("/tmp/k.png"), false));
        assert_eq!(f.images["none.png"], copy(None, false));
    }

    #[test]
    fn an_image_answer_is_kept_by_its_bytes() {
        use base64::Engine as _;
        let dir = std::env::temp_dir().join(format!("arakun-servers-kept-{}", std::process::id()));
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let answer =
            |data: &[u8]| json!({ "type": "image", "mime": "image/png", "data": b64(data) });
        let one = keep_image(&answer(b"one"), &dir).unwrap();
        assert_eq!(std::fs::read(&one).unwrap(), b"one");
        assert_eq!(one.extension().unwrap(), "png");
        // A changed image gets a file of its own, which the view loads afresh.
        let two = keep_image(&answer(b"two"), &dir).unwrap();
        assert_ne!(one, two);
        assert_eq!(keep_image(&answer(b"one"), &dir).unwrap(), one);
        assert_eq!(
            keep_image(&json!({ "type": "image", "missing": true }), &dir),
            None
        );
        let svg = json!({ "type": "image", "mime": "image/svg+xml", "data": b64(b"<svg/>") });
        assert_eq!(keep_image(&svg, &dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_start_sends_only_the_picks_its_server_offers() {
        let offered: SettingList = vec![
            (Setting::Model, offer("m1", &["m1", "m2"])),
            (Setting::Effort, offer("low", &["low", "high"])),
        ];
        let picks = vec![pair(Setting::Model, "m2"), pair(Setting::Effort, "high")];
        assert_eq!(offered_picks(&picks, Some(&offered)), picks);
        // Not said yet: they all go, as before.
        assert_eq!(offered_picks(&picks, None), picks);
        // A model from this machine's catalog is left out, its reasoning too.
        let local = vec![pair(Setting::Model, "local"), pair(Setting::Effort, "high")];
        assert!(offered_picks(&local, Some(&offered)).is_empty());
        let unknown_effort = vec![pair(Setting::Model, "m1"), pair(Setting::Effort, "max")];
        assert_eq!(
            offered_picks(&unknown_effort, Some(&offered)),
            vec![pair(Setting::Model, "m1")]
        );
        // Another of its models: the levels offered are not its own, so its
        // reasoning is not checked against them.
        let other = vec![pair(Setting::Model, "m2"), pair(Setting::Effort, "max")];
        assert_eq!(offered_picks(&other, Some(&offered)), other);
    }

    #[test]
    fn a_server_draft_shows_the_pick_among_the_servers_choices() {
        let offered: SettingList = vec![
            (Setting::Model, offer("m1", &["m1", "m2"])),
            (Setting::Effort, offer("low", &["low", "high"])),
        ];
        let current = |draft: &[(Setting, String)], s| {
            draft_on_server(&offered, AgentKind::Codex, draft, s).map(|(c, _)| c)
        };
        assert_eq!(current(&[], Setting::Model).as_deref(), Some("m1"));
        assert_eq!(current(&[], Setting::Mode), None);
        let picked = [
            (Setting::Model, "m1".to_string()),
            (Setting::Effort, "high".into()),
        ];
        assert_eq!(current(&picked, Setting::Effort).as_deref(), Some("high"));
        // Not offered there: the server's own stays.
        let local = [(Setting::Model, "local".to_string())];
        assert_eq!(current(&local, Setting::Model).as_deref(), Some("m1"));
        // Another of its models: the levels offered are the first one's.
        let other = [(Setting::Model, "m2".to_string())];
        assert_eq!(current(&other, Setting::Model).as_deref(), Some("m2"));
        assert_eq!(current(&other, Setting::Effort), None);
        // Claude's levels are the same for every model, so they stay.
        let claude = draft_on_server(&offered, AgentKind::Claude, &other, Setting::Effort);
        assert_eq!(claude.map(|(c, _)| c).as_deref(), Some("low"));
    }

    #[test]
    fn an_older_server_refusing_a_settle_is_told_apart() {
        let refused =
            "bad message: unknown variant `settle_env`, expected one of `open`, `delete_env`";
        assert!(older_server_refused(refused, &["settle_env"]));
        // Named only among the ones it expected: about something else.
        let other = "bad message: unknown variant `rename_branch`, expected one of `settle_env`";
        assert!(!older_server_refused(other, &["settle_env"]));
    }

    #[test]
    fn a_place_is_asked_to_go_only_once_at_a_time() {
        let deleting = vec![("box".to_string(), "/w/fix".to_string(), 1)];
        assert!(deleting_already(&deleting, "box", "/w/fix"));
        assert!(!deleting_already(&deleting, "box", "/w/fix2"));
        assert!(!deleting_already(&deleting, "other", "/w/fix"));
    }

    fn said(text: &str) -> Entry {
        Entry::Agent { text: text.into() }
    }

    fn texts(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .map(|e| match e {
                Entry::Agent { text } => text.as_str(),
                _ => "?",
            })
            .collect()
    }

    fn page(from: usize, to: usize, count: usize, new: &[&str]) -> Page {
        Page {
            from,
            to,
            count,
            entries: new.iter().map(|t| said(t)).collect(),
        }
    }

    #[test]
    fn each_start_hears_its_own_answer() {
        let mut starts = Starts::default();
        starts.sent("a", Asker::Phone(7));
        starts.sent("b", Asker::Desktop);
        starts.sent("a", Asker::Desktop);
        starts.sent("a", Asker::Phone(8));
        // A server answers its starts in order, `started` or an error alike.
        assert_eq!(starts.answered("a"), Some(Asker::Phone(7)));
        assert_eq!(starts.answered("a"), Some(Asker::Desktop));
        assert_eq!(starts.answered("b"), Some(Asker::Desktop));
        assert_eq!(starts.answered("a"), Some(Asker::Phone(8)));
        // An answer nobody waits for opens nothing.
        assert_eq!(starts.answered("a"), None);
    }

    #[test]
    fn a_phone_that_left_is_not_told_its_start() {
        let mut starts = Starts::default();
        starts.sent("a", Asker::Phone(7));
        starts.sent("a", Asker::Phone(8));
        starts.sent("a", Asker::Desktop);
        starts.phone_left(Some(7));
        assert_eq!(starts.answered("a"), Some(Asker::Gone));
        // A new phone link: none of the old phones is told, nor a new one with its id.
        starts.phone_left(None);
        assert_eq!(starts.answered("a"), Some(Asker::Gone));
        assert_eq!(starts.answered("a"), Some(Asker::Desktop));
    }

    #[test]
    fn a_dropped_server_lets_its_phones_know() {
        let mut starts = Starts::default();
        starts.sent("a", Asker::Phone(7));
        starts.sent("b", Asker::Phone(8));
        starts.sent("a", Asker::Desktop);
        assert_eq!(starts.dropped("a"), vec![7]);
        assert_eq!(starts.answered("a"), None);
        assert_eq!(starts.answered("b"), Some(Asker::Phone(8)));
    }

    #[test]
    fn an_older_servers_start_error_is_known_by_its_words() {
        assert!(about_start(
            &json!({ "type": "error", "about": "start", "message": "x" })
        ));
        let old = json!({ "type": "error", "message": "that project is gone" });
        assert!(about_start(&old));
        assert!(!about_start(
            &json!({ "type": "error", "message": "that session is gone" })
        ));
        // Said about a session, it is that session's.
        let session = json!({ "type": "error", "id": "s1", "message": "that project is gone" });
        assert!(!about_start(&session));
    }

    #[test]
    fn a_start_that_never_went_out_waits_for_no_answer() {
        let mut starts = Starts::default();
        let first = starts.sent("a", Asker::Phone(7));
        let second = starts.sent("a", Asker::Desktop);
        assert_ne!(first, second);
        assert_eq!(starts.undelivered(second), Some(Asker::Desktop));
        assert_eq!(starts.undelivered(second), None);
        // The answer that comes is the one that went.
        assert_eq!(starts.answered("a"), Some(Asker::Phone(7)));
        assert_eq!(starts.answered("a"), None);
    }

    #[test]
    fn a_transcript_arrives_in_answers_and_follows_what_changes() {
        let mut entries = Vec::new();
        // The first answer held two of five: the rest is asked for from there.
        assert_eq!(
            merge(&mut entries, page(0, 2, 5, &["a", "b"]), 0),
            Next::Ask(2)
        );
        assert_eq!(
            merge(&mut entries, page(2, 5, 5, &["c", "d", "e"]), 2),
            Next::Done
        );
        assert_eq!(texts(&entries), ["a", "b", "c", "d", "e"]);
        // The last entry grew and one came after it: the tail is replaced.
        assert_eq!(
            merge(&mut entries, page(4, 6, 6, &["e!", "f"]), 4),
            Next::Done
        );
        assert_eq!(texts(&entries), ["a", "b", "c", "d", "e!", "f"]);
        // An answer that starts past what is here joins nothing: the gap first.
        assert_eq!(
            merge(&mut entries, page(9, 10, 10, &["j"]), 9),
            Next::Ask(6)
        );
        assert_eq!(entries.len(), 6);
        // The server had fewer than asked for and answers from its end.
        assert_eq!(merge(&mut entries, page(6, 6, 6, &[]), 9), Next::Done);
        // The session was rewound on the server: fewer entries than before.
        assert_eq!(merge(&mut entries, page(1, 2, 2, &["b2"]), 1), Next::Done);
        assert_eq!(texts(&entries), ["a", "b2"]);
    }

    #[test]
    fn an_answer_that_brings_nothing_is_not_asked_for_again() {
        let mut entries = vec![said("a"), said("b")];
        // More is due, and the page is empty: the same question would get it again.
        assert_eq!(merge(&mut entries, page(2, 2, 5, &[]), 2), Next::Stuck);
        assert_eq!(texts(&entries), ["a", "b"]);
    }

    #[test]
    fn an_answer_to_an_earlier_question_is_left_alone() {
        // The session was opened again while page two of the last reading was
        // still out: it arrives first, for a reading that starts over.
        let mut fresh = Vec::new();
        assert_eq!(
            merge(&mut fresh, page(2, 4, 5, &["c", "d"]), 0),
            Next::Stale
        );
        assert!(fresh.is_empty());
        // The answer that was asked for follows, and is taken.
        assert_eq!(
            merge(&mut fresh, page(0, 2, 5, &["a", "b"]), 0),
            Next::Ask(2)
        );
        assert_eq!(texts(&fresh), ["a", "b"]);
        // The late one again, now past what was asked: still not this answer.
        assert_eq!(merge(&mut fresh, page(4, 5, 5, &["e"]), 2), Next::Stale);
        assert_eq!(texts(&fresh), ["a", "b"]);
    }

    #[test]
    fn a_page_with_an_entry_this_app_does_not_know_fails_whole() {
        let good = json!({
            "type": "transcript", "id": "s1", "from": 3, "to": 5, "count": 9,
            "entries": [{ "type": "agent", "text": "a" }, { "type": "agent", "text": "b" }],
        });
        let read = page_of(&good).unwrap();
        assert_eq!((read.from, read.to, read.count), (3, 5, 9));
        assert_eq!(texts(&read.entries), ["a", "b"]);
        // A server one version ahead: never an empty page, which asks again forever.
        let newer = json!({
            "type": "transcript", "id": "s1", "from": 0, "to": 2, "count": 2,
            "entries": [{ "type": "agent", "text": "a" }, { "type": "hologram", "shape": "cube" }],
        });
        assert!(page_of(&newer).err().unwrap().contains("hologram"));
    }

    fn place(path: &str, repo: &str, origin: &str) -> PlaceRow {
        PlaceRow {
            path: path.into(),
            project: "app".into(),
            branch: "main".into(),
            agent: "claude".into(),
            repo: repo.into(),
            origin: origin.into(),
        }
    }

    #[test]
    fn a_server_folder_joins_the_local_project_with_its_origin() {
        let locals = vec![
            (
                PathBuf::from("/home/me/code/app"),
                repo_key("https://github.com/Owner/App"),
            ),
            (PathBuf::from("/home/me/code/site"), None),
        ];
        // The same repository by ssh, in another case: the local project.
        let (path, linked) = project_for(
            &place("/srv/app", "/srv/app", "git@github.com:owner/app.git"),
            "s1",
            &locals,
        );
        assert_eq!((path, linked), (PathBuf::from("/home/me/code/app"), true));
        // Another repository: a project of the servers', the same on each server.
        let other = place("/srv/wt/x", "/srv/x", "https://gitlab.com/g/x");
        let (a, linked) = project_for(&other, "s1", &locals);
        let (b, _) = project_for(&other, "s2", &locals);
        assert!(!linked && a == b && Remote::is_path(&a));
        // Without an origin, a server's repository is its own.
        let bare = place("/srv/wt/fix", "/srv/notes", "");
        let (a, _) = project_for(&bare, "s1", &locals);
        let (b, _) = project_for(&place("/srv/notes", "/srv/notes", ""), "s1", &locals);
        let (c, _) = project_for(&bare, "s2", &locals);
        assert!(a == b && a != c);
    }

    #[test]
    fn a_server_folder_that_changes_project_moves_with_its_draft() {
        let remote_env = |place: &str| {
            let path = Remote::env_path("s1", place);
            let mut env = Environment::new(path, None, false);
            env.remote = Some(Remote {
                server: "s1".into(),
                machine: "box".into(),
                place: place.into(),
            });
            env
        };
        let local = PathBuf::from("/home/me/code/app");
        let theirs = Remote::project_path("s1/srv/app");
        let mut old = crate::state::new_project(&theirs);
        old.environments = vec![remote_env("/srv/app"), remote_env("/srv/gone")];
        old.active_env = 1;
        let mut projects = vec![crate::state::new_project(&local), old];
        let moved = Remote::env_path("s1", "/srv/app");
        // A local project with the same origin was added: the folder is its now.
        let keep = HashMap::from([(moved.clone(), local.clone())]);

        let mut moving = take_misplaced(&mut projects, &keep);
        assert!(projects[1].environments.is_empty());
        assert_eq!(projects[1].active_env, 0);
        // The local project's own folder stays where it is.
        assert_eq!(projects[0].environments.len(), 1);
        let env = moving.remove(&moved).expect("handed back to be moved");
        assert_eq!(env.path, moved);
        // A folder no server lists any more is not kept for anywhere.
        assert!(moving.is_empty());
    }

    fn following(server: &str, session: &str, by: Vec<Watcher>) -> Following {
        Following {
            server: server.into(),
            session: session.into(),
            env_path: PathBuf::new(),
            entries: Vec::new(),
            subagents: Vec::new(),
            agent: AgentKind::Claude,
            cwd: PathBuf::new(),
            title: String::new(),
            status: Status::Idle,
            asking: false,
            asked: 0,
            again: None,
            refill: None,
            broken: None,
            by,
            queue: RemoteQueue::default(),
            images: HashMap::new(),
        }
    }

    fn with_entries(server: &str, session: &str) -> Following {
        let mut f = following(server, session, vec![Watcher::Desktop]);
        f.entries = vec![said("hi")];
        f
    }

    #[test]
    fn the_last_sessions_let_go_of_are_kept() {
        let mut rested = Vec::new();
        for id in ["a", "b", "c", "d", "e"] {
            rest(&mut rested, with_entries("s1", id));
        }
        let ids: Vec<&str> = rested.iter().map(|f| f.session.as_str()).collect();
        assert_eq!(ids, ["b", "c", "d", "e"]);
        assert!(rested.iter().all(|f| f.by.is_empty()));
        // Let go of again: one copy, now the newest.
        rest(&mut rested, with_entries("s1", "b"));
        let ids: Vec<&str> = rested.iter().map(|f| f.session.as_str()).collect();
        assert_eq!(ids, ["c", "d", "e", "b"]);
    }

    #[test]
    fn empty_or_broken_sessions_are_not_kept() {
        let mut rested = vec![with_entries("s1", "a")];
        // Nothing to show: an older copy of it goes too.
        rest(&mut rested, following("s1", "a", Vec::new()));
        assert!(rested.is_empty());
        let mut broken = with_entries("s1", "b");
        broken.broken = Some("unreadable".into());
        rest(&mut rested, broken);
        assert!(rested.is_empty());
    }

    #[test]
    fn a_kept_session_wakes_to_be_read_again_to_the_side() {
        let mut rested = vec![with_entries("s1", "a"), with_entries("s2", "a")];
        assert!(wake(&mut rested, "s3", "a").is_none());
        let f = wake(&mut rested, "s2", "a").unwrap();
        // What it held shows until the server sent it whole again.
        assert_eq!(f.entries.len(), 1);
        assert!(f.asking && f.asked == 0);
        assert!(f.refill.as_ref().is_some_and(Vec::is_empty));
        assert_eq!(rested.len(), 1);
        assert_eq!(rested[0].server, "s1");
    }

    #[test]
    fn the_viewer_gets_the_subagents_the_server_sent() {
        use crate::agent::SubagentState;
        let mut f = following("s1", "a", Vec::new());
        assert!(transcript_of(&f).subagents.is_empty());
        f.subagents = vec![SubagentRecord {
            id: "child".into(),
            name: "Explore".into(),
            task: String::new(),
            state: SubagentState::Completed,
            started: 1,
            ended: Some(2),
            host: None,
            call: None,
            earlier: Vec::new(),
            entries: Vec::new(),
        }];
        let ids: Vec<String> = transcript_of(&f)
            .subagents
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["child"]);
    }

    #[test]
    fn a_session_is_followed_while_anyone_looks_at_it() {
        let follow = |session: &str, by: Vec<Watcher>| following("s1", session, by);
        let phone = Watcher::Phone(7);
        let mut following = vec![
            follow("a", vec![Watcher::Desktop]),
            follow("b", vec![phone, Watcher::Desktop]),
        ];
        // The phone opens `a`: `b` stays, the desktop has it in front.
        let gone = let_go(&mut following, phone, Some(("s1", "a")));
        assert!(gone.is_empty());
        assert_eq!(following[1].by, vec![Watcher::Desktop]);
        // The desktop moves to `c`: nobody looks at `a` or `b` any more.
        let gone = let_go(&mut following, Watcher::Desktop, Some(("s1", "c")));
        let names: Vec<&str> = gone.iter().map(|f| f.session.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(following.is_empty());
    }

    #[test]
    fn a_session_read_again_keeps_who_looks_at_it() {
        let mut broken = following("s1", "a", vec![Watcher::Desktop, Watcher::Phone(7)]);
        broken.broken = Some("the server stopped".into());
        let mut list = vec![broken];
        let fresh = following("s1", "a", Vec::new());
        let followed = follow_in(&mut list, Watcher::Phone(8), fresh, true);
        assert!(followed.ask, "read again");
        assert!(followed.close.is_empty() && followed.displaced.is_empty());
        assert_eq!(list.len(), 1);
        assert!(list[0].broken.is_none() && list[0].refill.is_some());
        let by = vec![Watcher::Desktop, Watcher::Phone(7), Watcher::Phone(8)];
        assert_eq!(list[0].by, by);
        // The last phone goes back to its inbox: the others still look.
        assert!(let_go(&mut list, Watcher::Phone(8), None).is_empty());
        // Opened again while it reads fine: nothing is asked again.
        let fresh = following("s1", "a", Vec::new());
        assert!(!follow_in(&mut list, Watcher::Phone(8), fresh, true).ask);
    }

    #[test]
    fn an_older_server_follows_the_last_session_opened_only() {
        let mut list = vec![following("old", "a", vec![Watcher::Desktop])];
        list.push(following("new", "x", vec![Watcher::Phone(9)]));
        // A phone opens `b` on the older server: it stops sending `a`.
        let fresh = following("old", "b", Vec::new());
        let followed = follow_in(&mut list, Watcher::Phone(7), fresh, false);
        assert!(followed.ask && followed.close.is_empty());
        let shown: Vec<&str> = followed
            .displaced
            .iter()
            .map(|f| f.session.as_str())
            .collect();
        assert_eq!(shown, ["a"]);
        let left: Vec<&str> = list.iter().map(|f| f.session.as_str()).collect();
        assert_eq!(left, ["x", "b"]);
        // The desktop shows `a` again: it is asked for again, in `b`'s place.
        let fresh = following("old", "a", Vec::new());
        let followed = follow_in(&mut list, Watcher::Desktop, fresh, false);
        assert!(followed.ask);
        assert_eq!(followed.displaced[0].session, "b");
        // A newer server sends both.
        let fresh = following("new", "y", Vec::new());
        let followed = follow_in(&mut list, Watcher::Phone(7), fresh, true);
        assert!(followed.displaced.is_empty());
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn a_servers_repository_is_named_by_its_own_paths() {
        assert_eq!(repo_folder("/home/me/code/app"), Some("app"));
        assert_eq!(repo_folder("/srv/git-files/app.git"), Some("app"));
        assert_eq!(repo_folder("C:\\code\\app"), Some("app"));
        assert_eq!(repo_folder("D:\\git\\app.git\\"), Some("app"));
        assert_eq!(repo_folder("C:\\"), None);
        assert_eq!(repo_folder("/"), None);
    }

    #[test]
    fn a_server_back_keeps_another_servers_untold_first_prompt() {
        let first = |server: &str| {
            let prompt = FirstPrompt {
                env: PathBuf::new(),
                text: "fix it".into(),
                agent: AgentKind::Claude,
                note: None,
                mentions_from: None,
                desktop: true,
            };
            (server.to_string(), prompt)
        };
        let mut starts = Starts::default();
        let mut prompts = HashMap::new();
        prompts.insert(starts.sent("b", Asker::Desktop), first("b"));
        let a = starts.sent("a", Asker::Desktop);
        prompts.insert(a, first("a"));
        // `b` drops; its start may come back undelivered after `a` is back.
        starts.dropped("b");
        settle_first_prompts(&mut prompts, &starts, "a");
        assert_eq!(prompts.len(), 2, "b's prompt waits for its own word");
        // `b` is back: what it was asked is over.
        settle_first_prompts(&mut prompts, &starts, "b");
        assert_eq!(prompts.keys().collect::<Vec<_>>(), [&a]);
    }

    #[test]
    fn a_viewer_that_goes_lets_go_of_its_own_servers_session_only() {
        let mut list = vec![
            following("srv1", "s19a-0", vec![Watcher::Desktop]),
            following("srv2", "s19a-0", vec![Watcher::Phone(7)]),
        ];
        let gone = unfollow(&mut list, "srv1", "s19a-0");
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].server, "srv1");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].server, "srv2");
    }

    #[test]
    fn a_phone_tells_two_servers_sessions_apart() {
        let a = phone_session_id("srv1", "s19a-0");
        let b = phone_session_id("srv2", "s19a-0");
        assert_ne!(a, b);
        assert_eq!(split_phone_id(&a), ("s19a-0", Some("srv1")));
        assert_eq!(split_phone_id(&b), ("s19a-0", Some("srv2")));
        // This computer's sessions keep their own ids.
        assert_eq!(split_phone_id("h19a2b"), ("h19a2b", None));
    }

    #[test]
    fn statuses_read_from_the_servers_words() {
        assert_eq!(status_from("needs_input"), Status::NeedsInput);
        assert_eq!(status_from("something new"), Status::Idle);
    }

    #[test]
    fn only_plain_http_links_wait_for_the_servers_tab() {
        let saved = |origin: &str, way: servers::Way| Saved {
            id: "srv".into(),
            name: String::new(),
            origin: origin.into(),
            way,
            device: "d".into(),
            key: String::new(),
            token: "t".into(),
            removed: false,
            sign_ins: false,
            holds_sign_ins: false,
        };
        assert!(in_the_clear(&saved(
            "http://10.0.0.2:7474",
            servers::Way::Direct
        )));
        assert!(!in_the_clear(&saved(
            "https://box.example",
            servers::Way::Direct
        )));
        let relay = servers::Way::Relay {
            label: "l".into(),
            ticket: "t".into(),
        };
        assert!(!in_the_clear(&saved("http://10.0.0.2:7474", relay)));
        // Its key pinned, a plain http server is sealed end to end.
        let pinned = Saved {
            key: "abcdefghij234567".into(),
            ..saved("http://10.0.0.2:7474", servers::Way::Direct)
        };
        assert!(!in_the_clear(&pinned));
    }

    #[test]
    fn the_servers_tab_shows_what_a_plain_http_server_listed_last() {
        let saved = |id: &str, origin: &str| Saved {
            id: id.into(),
            name: String::new(),
            origin: origin.into(),
            way: servers::Way::Direct,
            device: "d".into(),
            key: String::new(),
            token: "t".into(),
            removed: false,
            sign_ins: false,
            holds_sign_ins: false,
        };
        let servers = [
            saved("clear", "http://10.0.0.2:7474"),
            saved("safe", "https://box.example"),
            saved("new", "https://other.example"),
        ];
        let inbox = json!({ "places": [{ "path": "/srv/app" }], "sessions": [] });
        let inboxes = HashMap::from([
            ("clear".to_string(), inbox.clone()),
            ("safe".to_string(), inbox.clone()),
        ]);
        let ids = |seeded: Vec<(String, Value)>| -> Vec<String> {
            seeded.into_iter().map(|(id, _)| id).collect()
        };
        // At start the plain-http one is not connected, so nothing of it shows.
        assert_eq!(ids(seeded_inboxes(&servers, &inboxes, true)), ["safe"]);
        // Built from the Computers tab, it is, and its kept list shows.
        assert_eq!(
            ids(seeded_inboxes(&servers, &inboxes, false)),
            ["clear", "safe"]
        );
        // Nothing kept: nothing to show, so local origins are not read.
        assert!(seeded_inboxes(&servers, &HashMap::new(), false).is_empty());
    }

    #[test]
    fn a_folder_request_that_never_left_frees_the_form() {
        let mut adding = HashSet::from(["a".to_string(), "b".to_string()]);
        let why = folder_request_lost(&mut adding, "a", "clone").unwrap();
        assert!(why.contains("did not reach"), "{why}");
        assert_eq!(adding, HashSet::from(["b".to_string()]));
        // The other server's request is still on its way.
        assert!(folder_request_lost(&mut adding, "a", "add_place").is_none());
        assert!(folder_request_lost(&mut adding, "b", "start").is_none());
        assert!(adding.contains("b"));
        assert!(folder_request_lost(&mut adding, "b", "add_place").is_some());
        assert!(adding.is_empty());
    }

    #[test]
    fn an_older_server_is_told_from_its_plain_errors() {
        let old = "bad message: unknown variant `add_place`, expected one of `open`, `send` at line 1 column 19";
        assert!(older_server_refused(old, &["add_place", "clone"]));
        assert!(!older_server_refused(old, &["clone"]));
        assert!(!older_server_refused("no such session", &["add_place"]));
        let other = "bad message: unknown variant `rename`, expected one of `add_place`, `clone`";
        assert!(!older_server_refused(other, &["add_place", "clone"]));
        // A server with folders but no new environments lists them as expected.
        let no_new_env =
            "bad message: unknown variant `new_env`, expected one of `add_place`, `clone`";
        assert!(!older_server_refused(no_new_env, &["add_place", "clone"]));
        assert!(older_server_refused(no_new_env, &["new_env"]));
    }

    #[test]
    fn sessions_of_a_removed_folder_keep_one_of_their_own() {
        let session = |id: &str, place: &str| SessionRow {
            id: id.into(),
            title: String::new(),
            agent: "codex".into(),
            status: Status::Idle,
            updated: 0,
            place: place.into(),
            env: String::new(),
        };
        let sessions = [
            session("s1", "/srv/app"),
            session("s2", "/srv/old"),
            session("s3", "/srv/old"),
            session("s4", ""),
        ];
        let gone = gone_places(&sessions, &[place("/srv/app", "/srv/app", "")]);
        assert_eq!(gone.len(), 1, "one folder, once, and none for s4");
        assert_eq!(
            (gone[0].path.as_str(), gone[0].project.as_str()),
            ("/srv/old", "old")
        );
        assert_eq!(gone[0].agent, "codex");

        // An older server says the folder's name only.
        let named = |id: &str, env: &str| SessionRow {
            env: env.into(),
            ..session(id, "")
        };
        let sessions = [named("s5", "old"), named("s6", "old"), named("s7", "app")];
        let offered = [place("/srv/app", "/srv/app", "")];
        let gone = gone_places(&sessions, &offered);
        assert_eq!(gone.len(), 1, "s7 is in a folder still offered");
        assert_eq!(
            (gone[0].path.as_str(), gone[0].project.as_str()),
            ("gone:old", "old")
        );
        assert!(sessions[..2].iter().all(|s| s.in_place(&gone[0])));
    }

    #[test]
    fn a_remote_session_that_finishes_off_screen_waits_on_the_user() {
        let row = |id: &str, status| SessionRow {
            id: id.into(),
            title: String::new(),
            agent: "codex".into(),
            status,
            updated: 0,
            place: String::new(),
            env: String::new(),
        };
        let strip = model::TabStrip::default();
        let mut env = Environment::new(PathBuf::from("/srv/app"), None, true);
        let rows = || vec![row("a", Status::Working), row("b", Status::Working)];
        assert!(!sync_sessions(&strip, &[], &mut env, rows()));
        let on_screen = [(env.path.clone(), SharedString::from("b"))];
        let done = || vec![row("a", Status::Idle), row("b", Status::Idle)];
        assert!(sync_sessions(&strip, &on_screen, &mut env, done()));
        let unread = |id: &str| env.sessions[env.session_index(id).unwrap()].unread;
        assert!(unread("a"));
        assert!(!unread("b"), "it finished on screen");
        // One that stays idle is not marked again once read.
        env.sessions.iter_mut().for_each(|s| s.unread = false);
        assert!(!sync_sessions(&strip, &[], &mut env, done()));
    }

    #[test]
    fn a_session_state_carries_the_queue_there() {
        let msg = json!({
            "type": "state", "id": "s1", "status": "working",
            "queue": ["now", "later"], "queue_paused": true, "sending_now": 1,
        });
        assert_eq!(
            RemoteQueue::of(&msg),
            RemoteQueue {
                prompts: vec!["now".into(), "later".into()],
                paused: true,
                sending_now: 1,
            }
        );
        // An older server says nothing of it.
        let old = json!({ "type": "state", "id": "s1", "status": "working" });
        assert_eq!(RemoteQueue::of(&old), RemoteQueue::default());
    }
}
