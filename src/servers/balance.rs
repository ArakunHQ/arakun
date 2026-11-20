//! Where a new environment goes when they are spread across machines: this
//! computer and the paired servers that have the project. Each machine has a
//! preference. A new environment goes to the most preferred group and, within
//! it, to the machine with the fewest sessions at work. An avoided machine
//! gets one only when no other machine can take it.
//!
//! Kept in `servers-balance.json` beside `servers.json`, so that file keeps
//! its format and an older app still reads it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How much a machine is wanted for new environments.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Preference {
    Preferred,
    #[default]
    Normal,
    Avoided,
}

impl Preference {
    pub const ALL: [Preference; 3] = [
        Preference::Preferred,
        Preference::Normal,
        Preference::Avoided,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preference::Preferred => "Prefer",
            Preference::Normal => "Normal",
            Preference::Avoided => "Avoid",
        }
    }

    /// Lower goes first.
    fn rank(self) -> u8 {
        match self {
            Preference::Preferred => 0,
            Preference::Normal => 1,
            Preference::Avoided => 2,
        }
    }
}

/// Whether new environments are spread, and each machine's preference.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Balance {
    /// New environments go to the machine [`pick`] picks, not the open
    /// environment's.
    #[serde(default)]
    pub spread: bool,
    /// This computer's preference.
    #[serde(default)]
    pub here: Preference,
    /// Each server's, by its id. A server at `Normal` is left out.
    #[serde(default)]
    pub servers: BTreeMap<String, Preference>,
}

impl Balance {
    /// The preference of `server`, or of this computer for `None`.
    pub fn of(&self, server: Option<&str>) -> Preference {
        match server {
            None => self.here,
            Some(id) => self.servers.get(id).copied().unwrap_or_default(),
        }
    }

    pub fn set(&mut self, server: Option<&str>, preference: Preference) {
        match server {
            None => self.here = preference,
            Some(id) if preference == Preference::Normal => {
                self.servers.remove(id);
            }
            Some(id) => {
                self.servers.insert(id.to_string(), preference);
            }
        }
    }

    /// A forgotten server's preference goes with it.
    pub fn forget(&mut self, server: &str) {
        self.servers.remove(server);
    }
}

/// A machine that could take a new environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub preference: Preference,
    /// Its sessions at work right now.
    pub busy: usize,
}

/// The index of the candidate a new environment goes to: the most preferred
/// group first, then the fewest sessions at work. On a tie the earlier one
/// wins, so callers list the open environment's machine first and nothing
/// moves while the machines are equal.
pub fn pick(candidates: &[Candidate]) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .min_by_key(|(i, c)| (c.preference.rank(), c.busy, *i))
        .map(|(i, _)| i)
}

fn file() -> PathBuf {
    super::file().with_file_name("servers-balance.json")
}

pub fn load() -> Balance {
    crate::durable::load_json(file()).unwrap_or_default()
}

pub fn save(balance: &Balance) {
    let saved = serde_json::to_vec_pretty(balance)
        .map_err(std::io::Error::other)
        .and_then(|bytes| crate::durable::write_private(&file(), &bytes));
    if let Err(e) = saved {
        eprintln!("arakun: could not save {}: {e}", file().display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(preference: Preference, busy: usize) -> Candidate {
        Candidate { preference, busy }
    }

    #[test]
    fn the_least_busy_machine_takes_it() {
        use Preference::Normal;
        assert_eq!(pick(&[c(Normal, 3), c(Normal, 1), c(Normal, 2)]), Some(1));
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn on_a_tie_the_first_machine_keeps_it() {
        use Preference::Normal;
        assert_eq!(pick(&[c(Normal, 2), c(Normal, 2)]), Some(0));
    }

    #[test]
    fn a_preferred_machine_goes_first_however_busy() {
        use Preference::{Normal, Preferred};
        assert_eq!(pick(&[c(Normal, 0), c(Preferred, 5)]), Some(1));
        // Among preferred ones the load still counts.
        assert_eq!(
            pick(&[c(Preferred, 4), c(Normal, 0), c(Preferred, 1)]),
            Some(2)
        );
    }

    #[test]
    fn an_avoided_machine_is_used_only_when_it_is_all_there_is() {
        use Preference::{Avoided, Normal};
        assert_eq!(pick(&[c(Avoided, 0), c(Normal, 9)]), Some(1));
        assert_eq!(pick(&[c(Avoided, 3), c(Avoided, 1)]), Some(1));
    }

    #[test]
    fn a_normal_server_is_not_written_down() {
        let mut b = Balance::default();
        b.set(Some("a1"), Preference::Avoided);
        b.set(None, Preference::Preferred);
        assert_eq!(b.of(Some("a1")), Preference::Avoided);
        assert_eq!(b.of(None), Preference::Preferred);
        assert_eq!(b.of(Some("b2")), Preference::Normal);
        b.set(Some("a1"), Preference::Normal);
        assert!(b.servers.is_empty());
        b.set(Some("a1"), Preference::Preferred);
        b.forget("a1");
        assert!(b.servers.is_empty());
    }

    #[test]
    fn it_reads_back_and_a_missing_field_is_the_default() {
        let mut b = Balance {
            spread: true,
            ..Balance::default()
        };
        b.set(Some("a1"), Preference::Preferred);
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains(r#""a1":"preferred""#));
        assert_eq!(serde_json::from_str::<Balance>(&json).unwrap(), b);
        assert_eq!(
            serde_json::from_str::<Balance>("{}").unwrap(),
            Balance::default()
        );
    }
}
