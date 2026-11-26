//! What this device keeps for Teams between runs.
//!
//! The keyring holds the small secrets: the session (`mod.rs`), and this
//! device's two key pairs with the key of the file below, in one entry.
//!
//! Each account's team keys go in a separate file next to the state, sealed with
//! AES-256-GCM under that key. They grow with every team and every epoch, and
//! a keyring entry has little room (2.5 KB in the Windows Credential Manager);
//! the file can grow and still opens only with what the keyring holds. The
//! server can return key envelopes, but not this device's local trust records,
//! the devices it has seen or given keys to, or the teams it asked to join.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use super::api::{DEFAULT_URL, DeviceRow, TeamDetail};
use super::crypto::{self, Device, Envelope, TeamKey, b64, unb64};

const DEVICE_ACCOUNT: &str = "teams-device";
/// The default server's address before it moved to `DEFAULT_URL`. Its scope
/// still holds the device of everyone who used the default server then.
const FORMER_DEFAULT_URL: &str = "https://teams.arakun.dev";

pub use crate::secret_store::account;
use crate::secret_store::{account_for, profile};

/// A compact, filesystem- and keyring-safe name. Neither a change of GitHub
/// login nor a new session changes it. Two states in one folder stay separate.
struct Scope {
    entry: String,
    file: PathBuf,
    legacy: Option<String>,
    /// The entry and file this account had at [`FORMER_DEFAULT_URL`], for the
    /// default server only.
    former: Option<(String, PathBuf)>,
}

impl Scope {
    fn new(base: &str, github_id: u64, state: Option<&str>, dir: &Path) -> Self {
        let (entry, file) = scope_names(base, github_id, state, dir);
        let default = base == DEFAULT_URL;
        Self {
            entry,
            file,
            // An override could echo a production device's public keys. The
            // old entry did not record an origin; don't entrust it to one.
            legacy: default.then(|| account_for(DEVICE_ACCOUNT, state)),
            former: default.then(|| scope_names(FORMER_DEFAULT_URL, github_id, state, dir)),
        }
    }
}

/// The keyring entry and key file of an account at `base`.
fn scope_names(base: &str, github_id: u64, state: Option<&str>, dir: &Path) -> (String, PathBuf) {
    let profile = state.unwrap_or_default();
    let hash = sha2::Sha256::digest(format!("{base}\0{profile}"));
    // Windows filenames and credential names are case-insensitive.
    let hash: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    let scope = format!("{github_id}-{hash}");
    (
        format!("{DEVICE_ACCOUNT}-{scope}"),
        dir.join(format!("teams-{scope}.keys")),
    )
}

/// This device, and the key its file is sealed with.
pub struct Local {
    pub device: Device,
    file_key: TeamKey,
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    device: String,
    file: String,
}

/// This device from the keyring, or a new one kept there. A keyring that can't
/// keep it is an error: a device that changes on every start could never be
/// trusted by the others.
pub fn load_device(
    base: &str,
    github_id: u64,
    trusted_devices: impl FnOnce() -> Result<Vec<DeviceRow>, String>,
) -> Result<Local, String> {
    let state = std::env::var_os("ARAKUN_STATE");
    let state = state
        .as_deref()
        .map(|s| s.to_str().ok_or("The Teams state path is not UTF-8."))
        .transpose()?
        .filter(|s| !s.is_empty())
        .map(profile);
    let dir = state
        .as_deref()
        .and_then(|s| Path::new(s).parent())
        .map_or_else(crate::state::data_dir, Path::to_owned);
    let scope = Scope::new(base, github_id, state.as_deref(), &dir);
    // Two tabs starting together must not create different identities for
    // one account. The application already holds the state lock across processes.
    static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = INIT.lock().map_err(|_| "Teams device setup failed.")?;
    load_in(&scope, read_entry, write_entry, trusted_devices)
}

fn load_in(
    scope: &Scope,
    read: impl Fn(&str) -> Result<Option<String>, String>,
    write: impl FnOnce(&str, &str) -> Result<(), String>,
    trusted_devices: impl FnOnce() -> Result<Vec<DeviceRow>, String>,
) -> Result<Local, String> {
    if let Some(text) = read(&scope.entry)? {
        return Local::from_entry(&text, &scope.file);
    }
    let cached = scope.file.try_exists().map_err(|e| e.to_string())?;
    // The same account at the former default address: its own entry, so no
    // proof of ownership is needed. Copied like the legacy entry below.
    if let Some((entry, file)) = &scope.former
        && let Some(text) = read(entry)?
    {
        let local = Local::from_entry(&text, &scope.file)?;
        if cached {
            local.read()?;
        } else if let Some(bytes) = read_file(file)? {
            local.open(&bytes)?;
            crate::durable::write_private(&scope.file, &bytes).map_err(|e| e.to_string())?;
        }
        write(&scope.entry, &text)?;
        return Ok(local);
    }
    let legacy_file = scope.file.with_file_name("teams.keys");
    let legacy = match &scope.legacy {
        Some(name) => match read(name)? {
            None if legacy_file.try_exists().map_err(|e| e.to_string())? => {
                // Its owner registered its device, and v1 never removes one.
                // An account with no devices is not the owner, so it sets
                // up beside the file and leaves it alone.
                if !trusted_devices()?.is_empty() {
                    return Err("The old Teams cache exists, but its device keys could not be found. Restore the keyring entry and try again.".into());
                }
                return new_device(scope, cached, write);
            }
            result => result,
        },
        None => None,
    };
    if let Some(text) = legacy {
        let local = Local::from_entry(&text, &scope.file)?;
        let device = &local.device;
        if let Some(row) = trusted_devices()?
            .iter()
            .find(|d| d.device_id == device.id())
        {
            if row.sig_pub != device.sig_pub() || row.enc_pub != device.enc_pub() {
                return Err("This account lists the old Teams device with different keys. Its local keys have not been changed.".into());
            }
            // Copy, never move. Write the credential last: an interrupted
            // migration can validate the copy and retry without losing either original.
            if cached {
                local.read()?;
            } else if let Some(bytes) = read_file(&legacy_file)? {
                local.open(&bytes)?;
                crate::durable::write_private(&scope.file, &bytes).map_err(|e| e.to_string())?;
            }
            write(&scope.entry, &text)?;
            return Ok(local);
        }
    }
    new_device(scope, cached, write)
}

/// A new device for `scope`, unless its cache exists: that one's keys are
/// missing, and a new device could never open it.
fn new_device(
    scope: &Scope,
    cached: bool,
    write: impl FnOnce(&str, &str) -> Result<(), String>,
) -> Result<Local, String> {
    if cached {
        return Err("The Teams cache exists, but its device keys are missing. Restore the keyring entry to open it.".into());
    }
    let device = Device::generate();
    let file_key = crypto::new_team_key();
    let entry = Entry {
        device: device.to_secret(),
        file: b64(file_key),
    };
    let text = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
    write(&scope.entry, &text)?;
    Ok(Local {
        device,
        file_key,
        path: scope.file.clone(),
    })
}

const BAD_ENTRY: &str = "The keyring's Teams device entry could not be read.";

// A locked keyring, a missing one and malformed bytes are errors, not an absent
// entry: only an absent entry permits creation of a new device.
fn read_entry(account: &str) -> Result<Option<String>, String> {
    match crate::platform::keyring::read(account) {
        Ok(Some(text)) if text.is_empty() => Err(BAD_ENTRY.to_string()),
        Ok(entry) => Ok(entry),
        Err(e) => Err(format!(
            "The keyring could not read this device's Teams keys: {e}. Unlock the keyring and try again."
        )),
    }
}

// Only called after the entry read as absent, so replacing never loses a device.
fn write_entry(account: &str, text: &str) -> Result<(), String> {
    crate::platform::keyring::store(account, "arakun: Teams device keys", text)
        .map_err(|e| format!("The keyring could not keep this device's Teams keys: {e}"))
}

fn read_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Could not read the Teams cache: {e}")),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Membership {
    /// Creation and approval don't return joined_at. A removal bumps the team epoch.
    AdmittedIn(u64),
    JoinedAt(u64),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct MemberGrant {
    membership: Membership,
    first_epoch: u64,
    /// One entry only after the Worker accepted that device's envelopes.
    delivered: BTreeMap<String, BTreeSet<u64>>,
}

/// What the file holds, open.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    /// The device these belong to: a file of another device is not read.
    #[serde(default)]
    device: String,
    /// Team → epoch → team key (base64url).
    #[serde(default)]
    keys: BTreeMap<String, BTreeMap<u64, String>>,
    /// GitHub ID → the IDs of that user's devices seen so far.
    #[serde(default)]
    seen: BTreeMap<u64, BTreeSet<String>>,
    /// First trusted device, including its encryption key (not part of its ID).
    #[serde(default)]
    pub(super) roots: BTreeMap<u64, (String, String)>,
    /// The old `sealed` field had no epochs or approval scope. Ignore it.
    #[serde(default)]
    grants: BTreeMap<String, BTreeMap<u64, MemberGrant>>,
    /// Teams this user asked to join (an invite accepted), by ID, with their
    /// names, until an admin approves.
    #[serde(default)]
    asked: BTreeMap<String, String>,
    /// Devices of other people whose compare numbers the user checked with
    /// them and found the same. A device ID derives from its signing key, so a
    /// new key is a new, unchecked device.
    #[serde(default)]
    verified: BTreeSet<String>,
    /// Devices whose compare numbers the user found different. Kept so that,
    /// as PROTOCOL.md asks, no team key goes to their owner after a restart.
    #[serde(default)]
    mismatched: BTreeSet<String>,
    /// Channel (`team/channel`) → the newest `seq` this device showed, for
    /// the unread counts.
    #[serde(default)]
    read: BTreeMap<String, u64>,
    /// Team → a keyed epoch → the newest epoch seen skipped right after it.
    #[serde(default)]
    skipped: BTreeMap<String, BTreeMap<u64, u64>>,
}

impl Kept {
    pub fn key(&self, team_id: &str, epoch: u64) -> Option<TeamKey> {
        let text = self.keys.get(team_id)?.get(&epoch)?;
        unb64(text)?.try_into().ok()
    }

    /// Every key held for the team, oldest epoch first.
    pub fn keys(&self, team_id: &str) -> Vec<(u64, TeamKey)> {
        self.keys
            .get(team_id)
            .map(|m| {
                m.iter()
                    .filter_map(|(e, k)| Some((*e, unb64(k)?.try_into().ok()?)))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn newest(&self, team_id: &str) -> Option<(u64, TeamKey)> {
        self.keys(team_id).pop()
    }

    /// Adds a key; whether it was new.
    pub fn put_key(&mut self, team_id: &str, epoch: u64, key: &TeamKey) -> bool {
        let text = b64(key);
        let slot = self.keys.entry(team_id.to_string()).or_default();
        slot.insert(epoch, text.clone()) != Some(text)
    }

    /// The devices of a user seen before, or `None` the first time.
    pub fn seen(&self, github_id: u64) -> Option<&BTreeSet<String>> {
        self.seen.get(&github_id)
    }

    pub fn see(&mut self, github_id: u64, devices: impl IntoIterator<Item = String>) -> bool {
        let set = self.seen.entry(github_id).or_default();
        let before = set.len();
        set.extend(devices);
        set.len() != before
    }

    /// Creation and approval grant exactly the history in the accepted request.
    pub(super) fn admitted(
        &mut self,
        team_id: &str,
        gid: u64,
        team_epoch: u64,
        batch: &[Envelope],
    ) {
        let Some(first_epoch) = batch.iter().map(|e| e.epoch).min() else {
            return;
        };
        let mut delivered = BTreeMap::<_, BTreeSet<_>>::new();
        for e in batch {
            delivered
                .entry(e.device_id.clone())
                .or_default()
                .insert(e.epoch);
        }
        self.grants.entry(team_id.into()).or_default().insert(
            gid,
            MemberGrant {
                membership: Membership::AdmittedIn(team_epoch),
                first_epoch,
                delivered,
            },
        );
    }

    /// Without a local approval, only the newest keyed epoch is known to have
    /// been granted to every current member. Later rotations include them too.
    pub(super) fn observe_grants(&mut self, team: &TeamDetail) -> bool {
        let members = self.grants.entry(team.team_id.clone()).or_default();
        let before = members.len();
        members.retain(|gid, grant| {
            // An in-flight directory read can predate a successful approval.
            grant.membership == Membership::AdmittedIn(team.epoch)
                || team.members.iter().any(|m| m.github_id == *gid)
        });
        let mut changed = before != members.len();
        for m in &team.members {
            let joined = Membership::JoinedAt(m.joined_at);
            if let Some(grant) = members.get_mut(&m.github_id) {
                if grant.membership == joined {
                    // Removal clears the member's envelopes, even if a rejoin
                    // has the same seconds-resolution timestamp. Never use a
                    // stale local grant to widen the current roster's history.
                    if let Some(first) = m.first_key_epoch
                        && grant.first_epoch != first
                    {
                        grant.first_epoch = first;
                        grant.delivered.clear();
                        changed = true;
                    }
                    continue;
                }
                if grant.membership == Membership::AdmittedIn(team.epoch) {
                    grant.membership = joined;
                    changed = true;
                    continue;
                }
            }
            members.insert(
                m.github_id,
                MemberGrant {
                    membership: joined,
                    first_epoch: m
                        .first_key_epoch
                        .or(team.keyed_epoch.filter(|e| *e > 0))
                        .unwrap_or(team.epoch),
                    delivered: BTreeMap::new(),
                },
            );
            changed = true;
        }
        changed
    }

    /// Notes the epochs a pending rotation skips: those between the newest
    /// keyed one and the current one. The Worker gives a key only to the
    /// current epoch or one that has envelopes already, so these never get one.
    pub(super) fn observe_skips(&mut self, team: &TeamDetail) -> bool {
        let Some(keyed) = team.keyed_epoch else {
            return false;
        };
        if !team.rotation_pending || team.epoch <= keyed.saturating_add(1) {
            return false;
        }
        let last = team.epoch - 1;
        let seen = self
            .skipped
            .entry(team.team_id.clone())
            .or_default()
            .entry(keyed)
            .or_default();
        if *seen >= last {
            return false;
        }
        *seen = last;
        true
    }

    /// Whether every epoch between `older` and `newer` was seen skipped, so
    /// none of them has a key.
    pub(super) fn skipped_between(&self, team_id: &str, older: u64, newer: u64) -> bool {
        self.skipped
            .get(team_id)
            .and_then(|m| m.get(&older))
            .is_some_and(|last| last.saturating_add(1) >= newer)
    }

    pub(super) fn needs_delivery(
        &self,
        team: &str,
        gid: u64,
        joined: u64,
        device: &str,
        epoch: u64,
    ) -> bool {
        self.grants
            .get(team)
            .and_then(|m| m.get(&gid))
            .is_some_and(|g| {
                g.membership == Membership::JoinedAt(joined)
                    && epoch >= g.first_epoch
                    && !g
                        .delivered
                        .get(device)
                        .is_some_and(|sent| sent.contains(&epoch))
            })
    }

    pub(super) fn delivered(
        &mut self,
        team: &str,
        gid: u64,
        joined: u64,
        device: &str,
        epoch: u64,
    ) {
        if let Some(g) = self.grants.get_mut(team).and_then(|m| m.get_mut(&gid))
            && g.membership == Membership::JoinedAt(joined)
            && epoch >= g.first_epoch
        {
            g.delivered.entry(device.into()).or_default().insert(epoch);
        }
    }

    pub fn asked(&self) -> &BTreeMap<String, String> {
        &self.asked
    }

    pub fn ask(&mut self, team_id: &str, name: &str) {
        self.asked.insert(team_id.to_string(), name.to_string());
    }

    pub fn forget_ask(&mut self, team_id: &str) -> bool {
        self.asked.remove(team_id).is_some()
    }

    pub fn verified(&self, device_id: &str) -> bool {
        self.verified.contains(device_id)
    }

    /// The user found this device's compare numbers different.
    pub fn mismatched(&self, device_id: &str) -> bool {
        self.mismatched.contains(device_id)
    }

    pub fn mismatched_ids(&self) -> impl Iterator<Item = &String> {
        self.mismatched.iter()
    }

    /// Marks a device as matching ("They match") or not ("They don't");
    /// whether that changed anything.
    pub fn set_verified(&mut self, device_id: &str, yes: bool) -> bool {
        let (add, drop) = if yes {
            (&mut self.verified, &mut self.mismatched)
        } else {
            (&mut self.mismatched, &mut self.verified)
        };
        let dropped = drop.remove(device_id);
        add.insert(device_id.to_string()) || dropped
    }

    /// The newest `seq` this device showed in a channel, if it ever showed it.
    pub fn last_read(&self, channel: &str) -> Option<u64> {
        self.read.get(channel).copied()
    }

    /// Moves a channel's last-read mark forward; whether it moved.
    pub fn mark_read(&mut self, channel: &str, seq: u64) -> bool {
        let slot = self.read.entry(channel.to_string()).or_default();
        if seq > *slot {
            *slot = seq;
            true
        } else {
            false
        }
    }

    /// Forgets a team this user is no longer in. True when it held anything.
    pub fn forget_team(&mut self, team_id: &str) -> bool {
        let keys = self.keys.remove(team_id).is_some();
        let grants = self.grants.remove(team_id).is_some();
        let prefix = format!("{team_id}/");
        let before = self.read.len();
        self.read.retain(|k, _| !k.starts_with(&prefix));
        let read = self.read.len() != before;
        let skipped = self.skipped.remove(team_id).is_some();
        keys || grants || read || skipped
    }

    /// Keeps only these teams. True when that dropped anything.
    pub(super) fn retain_teams(&mut self, ids: &std::collections::HashSet<&str>) -> bool {
        let before = self.keys.len() + self.grants.len() + self.read.len() + self.skipped.len();
        self.keys.retain(|id, _| ids.contains(id.as_str()));
        self.grants.retain(|id, _| ids.contains(id.as_str()));
        self.read
            .retain(|k, _| k.split_once('/').is_some_and(|(t, _)| ids.contains(t)));
        self.skipped.retain(|id, _| ids.contains(id.as_str()));
        before != self.keys.len() + self.grants.len() + self.read.len() + self.skipped.len()
    }
}

impl Local {
    fn from_entry(text: &str, path: &Path) -> Result<Self, String> {
        let entry: Entry = serde_json::from_str(text).map_err(|_| BAD_ENTRY)?;
        let device = Device::from_secret(&entry.device).ok_or(BAD_ENTRY)?;
        let file_key = unb64(&entry.file)
            .and_then(|k| TeamKey::try_from(k.as_slice()).ok())
            .ok_or(BAD_ENTRY)?;
        Ok(Self {
            device,
            file_key,
            path: path.to_owned(),
        })
    }

    /// A device kept nowhere, for tests that play one against a server.
    #[cfg(test)]
    pub fn for_test(device: Device) -> Local {
        Local {
            device,
            file_key: [0; crypto::KEY_LEN],
            path: PathBuf::new(),
        }
    }

    fn open(&self, bytes: &[u8]) -> Result<Kept, String> {
        crypto::open_local(&self.file_key, bytes)
            .ok()
            .and_then(|plain| serde_json::from_slice::<Kept>(&plain).ok())
            .filter(|k| k.device == self.device.id())
            .ok_or_else(|| {
                "Could not open this account's Teams cache. Its contents have not been replaced."
                    .into()
            })
    }

    pub fn read(&self) -> Result<Kept, String> {
        match read_file(&self.path)? {
            Some(bytes) => self.open(&bytes),
            None => Ok(Kept {
                device: self.device.id().to_string(),
                ..Kept::default()
            }),
        }
    }

    pub fn sealer(&self) -> Sealer {
        Sealer {
            file_key: self.file_key,
            device: self.device.id().to_string(),
            path: self.path.clone(),
        }
    }
}

/// What writing the file needs, apart from the device, to send to a thread.
#[derive(Clone)]
pub struct Sealer {
    file_key: TeamKey,
    device: String,
    path: PathBuf,
}

pub(super) async fn after_save<T>(pending: Option<impl Future>, next: impl FnOnce() -> T) -> T {
    if let Some(task) = pending {
        task.await;
    }
    next()
}

impl Sealer {
    /// Writes the whole cache, so a later write also retries a failed one.
    pub fn write(&self, kept: &Kept) -> Result<(), String> {
        if kept.device != self.device {
            return Err("it belongs to another device".into());
        }
        serde_json::to_vec(kept)
            .map_err(std::io::Error::other)
            .and_then(|plain| {
                crate::durable::write_private(
                    &self.path,
                    &crypto::seal_local(&self.file_key, &plain),
                )
            })
            .map_err(|e| {
                eprintln!("arakun: could not save {}: {e}", self.path.display());
                e.to_string()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fixture {
        dir: PathBuf,
        ring: RefCell<BTreeMap<String, String>>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("arakun-teams-{}", crypto::ulid()));
            std::fs::create_dir(&dir).unwrap();
            Self {
                dir,
                ring: RefCell::default(),
            }
        }

        fn scope(&self, id: u64) -> Scope {
            Scope::new(DEFAULT_URL, id, None, &self.dir)
        }

        fn load(
            &self,
            scope: &Scope,
            rows: impl FnOnce() -> Result<Vec<DeviceRow>, String>,
        ) -> Result<Local, String> {
            load_in(
                scope,
                |name| Ok(self.ring.borrow().get(name).cloned()),
                |name, text| {
                    assert!(
                        self.ring
                            .borrow_mut()
                            .insert(name.into(), text.into())
                            .is_none()
                    );
                    Ok(())
                },
                rows,
            )
        }

        fn legacy(&self) -> Local {
            let device = Device::generate();
            let file_key = crypto::new_team_key();
            self.ring.borrow_mut().insert(
                DEVICE_ACCOUNT.into(),
                serde_json::to_string(&Entry {
                    device: device.to_secret(),
                    file: b64(file_key),
                })
                .unwrap(),
            );
            let local = Local {
                device,
                file_key,
                path: self.dir.join("teams.keys"),
            };
            let mut kept = local.read().unwrap();
            kept.put_key("shared", 3, &[42; 32]);
            kept.ask("private", "unannounced team");
            local.sealer().write(&kept).unwrap();
            local
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).unwrap();
        }
    }

    fn row(d: &Device) -> DeviceRow {
        DeviceRow {
            device_id: d.id().into(),
            sig_pub: d.sig_pub().into(),
            enc_pub: d.enc_pub().into(),
            created_at: 1,
            name: "test".into(),
            endorsed_by: None,
            endorsement_sig: None,
        }
    }

    fn no_lookup() -> Result<Vec<DeviceRow>, String> {
        panic!("no device lookup is needed")
    }

    #[test]
    fn signing_back_in_waits_for_the_pending_invitation_save() {
        use futures::FutureExt as _;

        let f = Fixture::new();
        let local = f.load(&f.scope(7), no_lookup).unwrap();
        let mut accepted = local.read().unwrap();
        accepted.ask("T", "waiting for approval");
        let (release, gate) = futures::channel::oneshot::channel::<()>();
        let saving = async {
            gate.await.unwrap();
            local.sealer().write(&accepted).unwrap();
        }
        .shared();
        let reload = || {
            after_save(Some(saving.clone()), || {
                f.load(&f.scope(7), no_lookup).unwrap().read().unwrap()
            })
        };
        // Two sign-ins can overlap before the first one finishes.
        assert!(reload().now_or_never().is_none());
        let again = reload();
        release.send(()).unwrap();
        assert_eq!(futures::executor::block_on(again), accepted);
    }

    #[test]
    fn a_failed_save_is_returned_not_only_logged() {
        let f = Fixture::new();
        let local = f.load(&f.scope(7), no_lookup).unwrap();
        let blocked = f.dir.join("not-a-folder");
        std::fs::write(&blocked, b"").unwrap();
        let sealer = Sealer {
            path: blocked.join("teams.keys"),
            ..local.sealer()
        };
        assert!(sealer.write(&local.read().unwrap()).is_err());
        assert!(local.sealer().write(&local.read().unwrap()).is_ok());
    }

    #[test]
    fn device_and_encrypted_cache_survive_a_round_trip_between_accounts() {
        let f = Fixture::new();
        let (alice, bob) = (f.scope(7), f.scope(8));
        let a = f.load(&alice, no_lookup).unwrap();
        let b = f.load(&bob, no_lookup).unwrap();
        assert_ne!(a.device.id(), b.device.id());
        assert_ne!(a.file_key, b.file_key);
        let mut ak = a.read().unwrap();
        let mut bk = b.read().unwrap();
        ak.put_key("shared", 1, &[1; 32]);
        bk.put_key("shared", 1, &[2; 32]);
        ak.ask("alone", "private team");
        b.sealer().write(&bk).unwrap();
        // A delayed save captured before the switch cannot target Bob's cache.
        a.sealer().write(&ak).unwrap();
        assert!(b.sealer().write(&ak).is_err());
        let back = f.load(&alice, no_lookup).unwrap();
        assert_eq!(back.device.id(), a.device.id());
        assert_eq!(back.read().unwrap(), ak);
        assert_eq!(f.load(&bob, no_lookup).unwrap().read().unwrap(), bk);
        let bytes = std::fs::read(&alice.file).unwrap();
        assert!(!bytes.windows(12).any(|w| w == b"private team"));
        assert!(b.open(&bytes).is_err());
        assert!(!alice.file.with_file_name("teams.keys").exists());
    }

    #[test]
    fn only_the_verified_owner_can_copy_the_legacy_device_and_cache() {
        let f = Fixture::new();
        let old = f.legacy();
        let bytes = std::fs::read(&old.path).unwrap();
        let (alice, bob) = (f.scope(7), f.scope(8));
        // Bob signs in first, with no registered devices. Don't register
        // Alice's identity as Bob's first device, or open her cache for Bob.
        let b = f.load(&bob, || Ok(vec![])).unwrap();
        assert_ne!(b.device.id(), old.device.id());
        assert!(b.read().unwrap().asked().is_empty());
        b.sealer().write(&b.read().unwrap()).unwrap();
        let a = f.load(&alice, || Ok(vec![row(&old.device)])).unwrap();
        assert_eq!(a.device.id(), old.device.id());
        assert_eq!(a.file_key, old.file_key);
        assert_eq!(a.read().unwrap(), old.read().unwrap());
        assert_eq!(std::fs::read(&old.path).unwrap(), bytes);
        assert!(f.ring.borrow()[DEVICE_ACCOUNT] == f.ring.borrow()[&alice.entry]);
        assert_eq!(f.load(&bob, no_lookup).unwrap().device.id(), b.device.id());
    }

    #[test]
    fn another_server_or_a_row_with_different_keys_cannot_claim_the_legacy_cache() {
        let f = Fixture::new();
        let old = f.legacy();
        let staging = Scope::new("https://staging.example", 7, None, &f.dir);
        let other = f.load(&staging, no_lookup).unwrap();
        assert_ne!(other.device.id(), old.device.id());
        assert!(other.read().unwrap().asked().is_empty());
        let mut swapped = row(&old.device);
        swapped.enc_pub = Device::generate().enc_pub().into();
        assert!(f.load(&f.scope(7), || Ok(vec![swapped])).is_err());
        assert!(!f.ring.borrow().contains_key(&f.scope(7).entry));
        let state = Scope::new(DEFAULT_URL, 7, Some("/tmp/dev/state-2.json"), &f.dir);
        assert_ne!(state.entry, f.scope(7).entry);
        assert_ne!(state.file, f.scope(7).file);
    }

    #[test]
    fn unavailable_proof_or_unreadable_keys_never_trigger_replacement() {
        let f = Fixture::new();
        let old = f.legacy();
        let scope = f.scope(7);
        assert!(f.load(&scope, || Err("offline".into())).is_err());
        assert!(load_in(&scope, |_| Err("locked".into()), |_, _| panic!(), no_lookup).is_err());
        assert_eq!(f.ring.borrow().len(), 1);
        assert!(!scope.file.exists());
        for name in [DEVICE_ACCOUNT, &scope.entry] {
            for bad in ["", "invalid", r#"{"device":"invalid","file":"invalid"}"#] {
                f.ring.borrow_mut().insert(name.into(), bad.into());
                assert!(f.load(&scope, no_lookup).is_err());
                assert_eq!(f.ring.borrow()[name], bad);
            }
        }
        assert_eq!(old.read().unwrap().key("shared", 3), Some([42; 32]));
    }

    #[test]
    fn an_interrupted_migration_can_retry_without_replacing_either_file() {
        let f = Fixture::new();
        let old = f.legacy();
        let scope = f.scope(7);
        let proof = || Ok(vec![row(&old.device)]);
        assert!(
            load_in(
                &scope,
                |n| Ok(f.ring.borrow().get(n).cloned()),
                |_, _| Err("could not save".into()),
                proof
            )
            .is_err()
        );
        assert!(scope.file.exists());
        assert_eq!(f.ring.borrow().len(), 1);
        assert_eq!(
            f.load(&scope, proof).unwrap().read().unwrap(),
            old.read().unwrap()
        );
    }

    #[test]
    fn an_orphaned_legacy_cache_does_not_block_an_account_without_devices() {
        let f = Fixture::new();
        let old = f.legacy();
        let bytes = std::fs::read(&old.path).unwrap();
        f.ring.borrow_mut().clear();
        // The owner, or an account the server can't vouch for, still has to
        // restore the entry.
        assert!(f.load(&f.scope(7), || Ok(vec![row(&old.device)])).is_err());
        assert!(f.load(&f.scope(7), || Err("offline".into())).is_err());
        assert!(f.ring.borrow().is_empty());
        let bob = f.scope(8);
        let b = f.load(&bob, || Ok(vec![])).unwrap();
        assert_ne!(b.device.id(), old.device.id());
        assert!(f.ring.borrow().contains_key(&bob.entry));
        assert_eq!(std::fs::read(&old.path).unwrap(), bytes);
    }

    /// A device kept under the scope of the old default address.
    fn former(f: &Fixture, id: u64) -> Local {
        let (entry, file) = scope_names(FORMER_DEFAULT_URL, id, None, &f.dir);
        let device = Device::generate();
        let file_key = crypto::new_team_key();
        f.ring.borrow_mut().insert(
            entry,
            serde_json::to_string(&Entry {
                device: device.to_secret(),
                file: b64(file_key),
            })
            .unwrap(),
        );
        let local = Local {
            device,
            file_key,
            path: file,
        };
        let mut kept = local.read().unwrap();
        kept.put_key("shared", 3, &[42; 32]);
        local.sealer().write(&kept).unwrap();
        local
    }

    #[test]
    fn the_former_default_address_keeps_its_device_once() {
        let f = Fixture::new();
        let old = former(&f, 7);
        let scope = f.scope(7);
        let moved = f.load(&scope, no_lookup).unwrap();
        assert_eq!(moved.device.id(), old.device.id());
        assert_eq!(moved.file_key, old.file_key);
        assert_eq!(moved.read().unwrap(), old.read().unwrap());
        assert!(scope.file.exists());
        assert_eq!(
            f.ring.borrow()[&scope.entry],
            f.ring.borrow()[&scope.former.as_ref().unwrap().0]
        );
        // Found under the new scope from now on: the old one is not read again.
        let former_entry = scope.former.clone().unwrap().0;
        let again = load_in(
            &scope,
            |name| {
                assert_ne!(name, former_entry, "the old scope was read again");
                Ok(f.ring.borrow().get(name).cloned())
            },
            |_, _| panic!("nothing is written the second time"),
            no_lookup,
        )
        .unwrap();
        assert_eq!(again.device.id(), old.device.id());
    }

    #[test]
    fn the_new_scope_wins_over_the_former_default_address() {
        let f = Fixture::new();
        let scope = f.scope(7);
        let new = f.load(&scope, no_lookup).unwrap();
        let old = former(&f, 7);
        let loaded = f.load(&scope, no_lookup).unwrap();
        assert_eq!(loaded.device.id(), new.device.id());
        assert_ne!(loaded.device.id(), old.device.id());
    }

    #[test]
    fn another_server_never_looks_at_the_former_default_address() {
        let f = Fixture::new();
        let old = former(&f, 7);
        let staging = Scope::new("https://staging.example", 7, None, &f.dir);
        assert!(staging.former.is_none());
        let other = f.load(&staging, no_lookup).unwrap();
        assert_ne!(other.device.id(), old.device.id());
        // A test Arakun's own state is its own scope at the new address only.
        let state = Scope::new(DEFAULT_URL, 7, Some("/tmp/dev/state-2.json"), &f.dir);
        let (entry, _) = state.former.unwrap();
        assert!(!f.ring.borrow().contains_key(&entry));
    }

    #[test]
    fn an_unreadable_or_orphaned_cache_stays_in_place() {
        let f = Fixture::new();
        let old = f.legacy();
        let scope = f.scope(7);
        std::fs::write(&old.path, b"unreadable").unwrap();
        assert!(f.load(&scope, || Ok(vec![row(&old.device)])).is_err());
        assert_eq!(std::fs::read(&old.path).unwrap(), b"unreadable");
        f.ring.borrow_mut().clear();
        assert!(f.load(&scope, || Ok(vec![row(&old.device)])).is_err());
        assert!(f.ring.borrow().is_empty());
        std::fs::rename(&old.path, &scope.file).unwrap();
        assert!(f.load(&scope, no_lookup).is_err());
        assert_eq!(std::fs::read(&scope.file).unwrap(), b"unreadable");
    }

    #[test]
    fn keeps_keys_by_team_and_epoch() {
        let mut k = Kept::default();
        assert!(k.put_key("T", 2, &[2; 32]));
        assert!(k.put_key("T", 1, &[1; 32]));
        assert!(!k.put_key("T", 1, &[1; 32]));
        assert_eq!(k.key("T", 2), Some([2; 32]));
        assert_eq!(k.key("U", 2), None);
        assert_eq!(k.keys("T"), vec![(1, [1; 32]), (2, [2; 32])]);
        assert_eq!(k.newest("T"), Some((2, [2; 32])));
        assert!(k.forget_team("T"));
        assert_eq!(k.newest("T"), None);
        // Nothing left to forget: the caller need not write the file.
        assert!(!k.forget_team("T"));
    }

    #[test]
    fn the_last_read_mark_only_moves_forward() {
        let mut k = Kept::default();
        assert_eq!(k.last_read("T/c"), None);
        assert!(k.mark_read("T/c", 5));
        assert!(!k.mark_read("T/c", 3));
        assert_eq!(k.last_read("T/c"), Some(5));
        k.forget_team("T");
        assert_eq!(k.last_read("T/c"), None);
    }

    #[test]
    fn old_device_markers_are_not_epoch_receipts_or_history_grants() {
        let f = Fixture::new();
        let local = f.load(&f.scope(7), no_lookup).unwrap();
        let legacy = serde_json::json!({"device": local.device.id(), "sealed": {"T": ["a"]}});
        let mut k = local
            .open(&crypto::seal_local(
                &local.file_key,
                &serde_json::to_vec(&legacy).unwrap(),
            ))
            .unwrap();
        assert!(k.seen(7).is_none());
        assert!(k.see(7, ["a".to_string()]));
        assert!(!k.see(7, ["a".to_string()]));
        assert!(k.seen(7).unwrap().contains("a"));
        assert!(!k.verified("a"));
        assert!(k.set_verified("a", true));
        assert!(!k.set_verified("a", true));
        assert!(k.verified("a"));
        assert!(k.set_verified("a", false));
        assert!(!k.verified("a"));
        assert!(k.mismatched("a"));
        assert!(!k.set_verified("a", false));
        assert!(k.set_verified("a", true));
        assert!(!k.mismatched("a"));
        assert!(k.set_verified("a", false));
        assert!(k.grants.is_empty());
        assert!(!k.needs_delivery("T", 7, 10, "a", 1));
        let mut team = TeamDetail {
            team_id: "T".into(),
            name: "T".into(),
            members: vec![super::super::api::MemberRow {
                github_id: 7,
                login: "seven".into(),
                role: super::super::api::Role::Member,
                joined_at: 10,
                first_key_epoch: Some(3),
            }],
            channels: vec![],
            epoch: 4,
            keyed_epoch: Some(3),
            roster_epoch: Some(4),
            rotation_pending: true,
        };
        assert!(k.observe_grants(&team));
        assert!(!k.needs_delivery("T", 7, 10, "a", 2));
        assert!(k.needs_delivery("T", 7, 10, "a", 3));
        k.delivered("T", 7, 10, "a", 3);
        local.sealer().write(&k).unwrap();
        k = local.read().unwrap();
        assert!(!k.needs_delivery("T", 7, 10, "a", 3));
        assert!(k.needs_delivery("T", 7, 10, "b", 3));
        team.team_id = "U".into();
        k.observe_grants(&team);
        assert!(k.needs_delivery("U", 7, 10, "a", 3));
        assert!(k.retain_teams(&["T"].into_iter().collect()));
        assert!(!k.grants.contains_key("U"));
        assert!(!k.retain_teams(&["T"].into_iter().collect()));
        assert!(k.forget_team("T"));
        assert!(k.grants.is_empty());
    }

    #[test]
    fn the_file_opens_only_with_its_key() {
        let mut k = Kept::default();
        k.put_key("T", 1, &[9; 32]);
        k.ask("U", "Design");
        let plain = serde_json::to_vec(&k).unwrap();
        let key = [5u8; 32];
        let sealed = crypto::seal_local(&key, &plain);
        assert!(!sealed.windows(8).any(|w| w == b"Design\"}"));
        let back: Kept =
            serde_json::from_slice(&crypto::open_local(&key, &sealed).unwrap()).unwrap();
        assert_eq!(back, k);
        assert!(crypto::open_local(&[6u8; 32], &sealed).is_err());
    }
}
