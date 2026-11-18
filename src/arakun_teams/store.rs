//! What this device keeps for Teams between runs.
//!
//! The keyring holds the small secrets: the session (`mod.rs`), and this
//! device's two key pairs with the key of the file below, in one entry.
//!
//! The team keys go in a file, `teams.keys` next to the state, sealed with
//! AES-256-GCM under that key. They grow with every team and every epoch, and
//! a keyring entry has little room (2.5 KB in the Windows Credential Manager);
//! the file can grow and still opens only with what the keyring holds. Losing
//! it loses nothing for good: the server keeps every envelope for this device,
//! and the app opens them again on start. The file also remembers which
//! devices of each member this device has seen (for "added a new device"
//! notices), which ones it gave the team keys to, and the teams it asked to join.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::crypto::{self, Device, KEY_LEN, TeamKey, b64, unb64};
use crate::platform::keyring;

const DEVICE_ACCOUNT: &str = "teams-device";

/// The keyring account for `name`. A test Arakun with a state file of its own
/// (`ARAKUN_STATE`, as `arakun dev` sets) keeps its own Teams session and
/// device, so it never signs the person's own Arakun out or takes its keys.
pub fn account(name: &str) -> String {
    account_for(name, std::env::var("ARAKUN_STATE").ok().as_deref())
}

fn account_for(name: &str, state: Option<&str>) -> String {
    match state.filter(|s| !s.is_empty()) {
        Some(state) => format!("{name}@{state}"),
        None => name.to_string(),
    }
}

fn file() -> PathBuf {
    if let Some(state) = std::env::var_os("ARAKUN_STATE") {
        return PathBuf::from(state).with_file_name("teams.keys");
    }
    crate::state::data_dir().join("teams.keys")
}

/// This device, and the key its file is sealed with.
pub struct Local {
    pub device: Device,
    file_key: TeamKey,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    device: String,
    file: String,
}

/// This device from the keyring, or a new one kept there. A keyring that can't
/// keep it is an error: a device that changes on every start could never be
/// trusted by the others.
pub fn load_device() -> Result<Local, String> {
    let account = account(DEVICE_ACCOUNT);
    if let Some(text) = keyring::lookup(&account) {
        let entry: Entry = serde_json::from_str(&text)
            .map_err(|_| "The keyring's Teams device entry could not be read.".to_string())?;
        let device = Device::from_secret(&entry.device)
            .ok_or("The keyring's Teams device keys could not be read.")?;
        let file_key = unb64(&entry.file)
            .and_then(|k| TeamKey::try_from(k.as_slice()).ok())
            .ok_or("The keyring's Teams device entry could not be read.")?;
        return Ok(Local { device, file_key });
    }
    let device = Device::generate();
    let mut file_key = [0u8; KEY_LEN];
    getrandom::fill(&mut file_key).map_err(|e| e.to_string())?;
    let entry = Entry {
        device: device.to_secret(),
        file: b64(file_key),
    };
    let text = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
    keyring::store(&account, "arakun: Teams device keys", &text)
        .map_err(|e| format!("The keyring could not keep this device's Teams keys: {e}"))?;
    Ok(Local { device, file_key })
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
    /// Team → devices this device sealed every key it held for.
    #[serde(default)]
    sealed: BTreeMap<String, BTreeSet<String>>,
    /// Teams this user asked to join (an invite accepted), by ID, with their
    /// names, until an admin approves.
    #[serde(default)]
    asked: BTreeMap<String, String>,
    /// Devices of other people whose compare numbers the user checked with
    /// them and found the same. A device ID derives from its signing key, so a
    /// new key is a new, unchecked device.
    #[serde(default)]
    verified: BTreeSet<String>,
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

    pub fn sealed_for(&self, team_id: &str, device_id: &str) -> bool {
        self.sealed
            .get(team_id)
            .is_some_and(|s| s.contains(device_id))
    }

    pub fn mark_sealed(&mut self, team_id: &str, devices: impl IntoIterator<Item = String>) {
        self.sealed
            .entry(team_id.to_string())
            .or_default()
            .extend(devices);
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

    /// Marks a device checked, or not; whether that changed anything.
    pub fn set_verified(&mut self, device_id: &str, yes: bool) -> bool {
        if yes {
            self.verified.insert(device_id.to_string())
        } else {
            self.verified.remove(device_id)
        }
    }

    /// Forgets a team this user is no longer in.
    pub fn forget_team(&mut self, team_id: &str) {
        self.keys.remove(team_id);
        self.sealed.remove(team_id);
    }
}

impl Local {
    /// A device kept nowhere, for tests that play one against a server.
    #[cfg(test)]
    pub fn for_test(device: Device) -> Local {
        Local {
            device,
            file_key: [0; KEY_LEN],
        }
    }

    /// The file, opened. Missing, of another device, or unreadable: empty, and
    /// the keys come back from the server's envelopes.
    pub fn read(&self) -> Kept {
        std::fs::read(file())
            .ok()
            .and_then(|bytes| crypto::open_local(&self.file_key, &bytes).ok())
            .and_then(|plain| serde_json::from_slice::<Kept>(&plain).ok())
            .filter(|k| k.device == self.device.id())
            .unwrap_or_else(|| Kept {
                device: self.device.id().to_string(),
                ..Kept::default()
            })
    }

    pub fn sealer(&self) -> Sealer {
        Sealer {
            file_key: self.file_key,
            device: self.device.id().to_string(),
        }
    }
}

/// What writing the file needs, apart from the device, to send to a thread.
#[derive(Clone)]
pub struct Sealer {
    file_key: TeamKey,
    device: String,
}

impl Sealer {
    pub fn write(&self, kept: &Kept) {
        let mut kept = kept.clone();
        kept.device = self.device.clone();
        let saved = serde_json::to_vec(&kept)
            .map_err(std::io::Error::other)
            .and_then(|plain| {
                crate::durable::write_private(&file(), &crypto::seal_local(&self.file_key, &plain))
            });
        if let Err(e) = saved {
            eprintln!("arakun: could not save {}: {e}", file().display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_arakun_keeps_its_own_keyring_entries() {
        assert_eq!(account_for("teams-session", None), "teams-session");
        assert_eq!(account_for("teams-session", Some("")), "teams-session");
        assert_eq!(
            account_for("teams-device", Some("/tmp/dev/state.json")),
            "teams-device@/tmp/dev/state.json"
        );
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
        k.forget_team("T");
        assert_eq!(k.newest("T"), None);
    }

    #[test]
    fn remembers_devices_seen_and_sealed_for() {
        let mut k = Kept::default();
        assert!(k.seen(7).is_none());
        assert!(k.see(7, ["a".to_string()]));
        assert!(!k.see(7, ["a".to_string()]));
        assert!(k.seen(7).unwrap().contains("a"));
        k.mark_sealed("T", ["a".to_string()]);
        assert!(k.sealed_for("T", "a"));
        assert!(!k.sealed_for("T", "b"));
        assert!(!k.sealed_for("U", "a"));
        assert!(!k.verified("a"));
        assert!(k.set_verified("a", true));
        assert!(!k.set_verified("a", true));
        assert!(k.verified("a"));
        assert!(k.set_verified("a", false));
        assert!(!k.verified("a"));
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
