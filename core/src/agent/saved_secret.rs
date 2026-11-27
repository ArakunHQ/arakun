//! Secrets the user saved for a project, so its agents can use them again in
//! later sessions without the user pasting them each time.
//!
//! The project's entry in the app's state keeps a [`Saved`] per secret: its
//! name, a random id and, if set, the variable its agents get it in. The
//! value is in the keyring (`crate::secret_store`) under the id, so a rename
//! never touches it. Agents see the names only: `use_saved_secret` gives
//! them a fresh one-use handle (`super::secret::keep`) or a private file.
//! One with a variable set is in the environment of the project's agents
//! from their start ([`env_for`]).

use serde::{Deserialize, Serialize};

use crate::secret::Secret;

/// The longest name a saved secret takes.
pub const MAX_NAME: usize = 60;

/// One saved secret, as the project's state keeps it. Holds no value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// Names its value in the keyring. Random, so a rename touches only this.
    pub id: String,
    pub name: String,
    /// The environment variable the project's agents start with it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
}

/// The keyring account of the saved secret `id`.
fn account(id: &str) -> String {
    crate::secret_store::account(&format!("project-secret:{id}"))
}

/// `name` trimmed, if it is a name no other saved secret in `taken` has.
/// `except` is the id of the one being renamed, which may keep its name.
pub fn check_name(name: &str, taken: &[Saved], except: Option<&str>) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give it a name.".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("A name has at most {MAX_NAME} characters."));
    }
    if name.chars().any(char::is_control) {
        return Err("A name is one line of text.".into());
    }
    if taken
        .iter()
        .any(|s| Some(s.id.as_str()) != except && s.name.eq_ignore_ascii_case(name))
    {
        return Err(format!(
            "This project has a secret named \"{name}\" already."
        ));
    }
    Ok(name.to_string())
}

/// The names the fields of one card are saved under: `name` for one field,
/// and `name (field)` for each of several.
pub fn names_for(name: &str, fields: &[String]) -> Vec<String> {
    match fields {
        [_] => vec![name.trim().to_string()],
        _ => fields
            .iter()
            .map(|f| format!("{} ({f})", name.trim()))
            .collect(),
    }
}

/// The saved secret in `list` named `name`, in any case.
pub fn find<'a>(list: &'a [Saved], name: &str) -> Option<&'a Saved> {
    let name = name.trim();
    list.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

/// Keeps `value` in the keyring under a new id, and returns the entry for
/// the project's state. Check the name with [`check_name`] first.
pub fn store(name: &str, value: &str) -> Result<Saved, String> {
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|_| "the system gave no random bytes")?;
    let saved = Saved {
        id: id.iter().map(|b| format!("{b:02x}")).collect(),
        name: name.to_string(),
        env: None,
    };
    replace(&saved, value)?;
    Ok(saved)
}

/// Puts `value` in place of the saved secret's value.
pub fn replace(saved: &Saved, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("The value is empty.".into());
    }
    vault().set(
        &account(&saved.id),
        &format!("arakun: {} for a project", saved.name),
        value,
    )
}

/// Deletes the saved secret's value from the keyring.
pub fn remove(saved: &Saved) {
    vault().forget(&account(&saved.id));
}

/// The saved secret's value.
pub fn value(saved: &Saved) -> Result<Secret, String> {
    match vault().get(&account(&saved.id))? {
        Some(v) => Ok(Secret::new(v)),
        None => Err(format!(
            "the value of \"{}\" is gone from the keyring; ask the user to save it again",
            saved.name
        )),
    }
}

/// `name` as an environment variable name, upper-cased: letters, digits and
/// `_`, not starting with a digit. Empty means none.
pub fn check_env(name: &str) -> Result<Option<String>, String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let ok = name.len() <= 64
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        return Err("A variable name is letters, digits and _, and starts with a letter.".into());
    }
    Ok(Some(name.to_ascii_uppercase()))
}

/// The variables the agents of the project at `cwd` start with: each saved
/// secret that has one, with its value from the keyring. One whose value
/// can't be read is left out, and the log says which, never the value.
pub fn env_for(cwd: &std::path::Path) -> Vec<(String, std::ffi::OsString)> {
    env_from(&super::sandbox::saved(cwd))
}

fn env_from(list: &[Saved]) -> Vec<(String, std::ffi::OsString)> {
    list.iter()
        .filter_map(|s| {
            let var = s.env.clone()?;
            match value(s) {
                Ok(v) => Some((var, v.expose().into())),
                Err(e) => {
                    eprintln!("arakun: no ${var} for the agent: {e}");
                    None
                }
            }
        })
        .collect()
}

/// Where values are kept: the keyring, or memory in the tests.
trait Vault: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, label: &str, value: &str) -> Result<(), String>;
    fn forget(&self, account: &str);
}

#[cfg_attr(test, allow(dead_code))]
struct Keyring;

impl Vault for Keyring {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        crate::secret_store::get(account)
    }

    fn set(&self, account: &str, label: &str, value: &str) -> Result<(), String> {
        crate::secret_store::set(account, label, value)
    }

    fn forget(&self, account: &str) {
        crate::secret_store::forget(account)
    }
}

#[cfg(test)]
#[derive(Default)]
struct Memory(std::sync::Mutex<std::collections::HashMap<String, String>>);

#[cfg(test)]
impl Vault for Memory {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, _label: &str, value: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }

    fn forget(&self, account: &str) {
        self.0.lock().unwrap().remove(account);
    }
}

fn vault() -> &'static dyn Vault {
    #[cfg(test)]
    {
        static MEMORY: std::sync::OnceLock<Memory> = std::sync::OnceLock::new();
        MEMORY.get_or_init(Memory::default)
    }
    #[cfg(not(test))]
    {
        &Keyring
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(id: &str, name: &str) -> Saved {
        Saved {
            id: id.into(),
            name: name.into(),
            env: None,
        }
    }

    #[test]
    fn names_are_checked_and_unique_in_any_case() {
        let taken = [saved("a", "Staging key")];
        assert_eq!(check_name("  Prod key ", &taken, None).unwrap(), "Prod key");
        assert!(check_name("staging KEY", &taken, None).is_err());
        // A rename may keep its own name.
        assert!(check_name("staging key", &taken, Some("a")).is_ok());
        assert!(check_name("  ", &taken, None).is_err());
        assert!(check_name("a\nb", &taken, None).is_err());
        assert!(check_name(&"x".repeat(MAX_NAME + 1), &taken, None).is_err());
    }

    #[test]
    fn several_fields_get_a_name_each() {
        assert_eq!(names_for(" DB ", &["Password".into()]), ["DB"]);
        assert_eq!(
            names_for("DB", &["User".into(), "Password".into()]),
            ["DB (User)", "DB (Password)"]
        );
    }

    #[test]
    fn a_value_is_stored_replaced_and_removed_under_its_id() {
        let mut s = store("Staging key", "sk_one").unwrap();
        assert_eq!(s.id.len(), 32);
        assert_eq!(value(&s).unwrap().expose(), "sk_one");
        // A rename keeps the value: it is filed under the id.
        s.name = "Renamed".into();
        assert_eq!(value(&s).unwrap().expose(), "sk_one");
        replace(&s, "sk_two").unwrap();
        assert_eq!(value(&s).unwrap().expose(), "sk_two");
        assert!(replace(&s, "").is_err());
        remove(&s);
        assert!(value(&s).unwrap_err().contains("gone"));
    }

    #[test]
    fn a_saved_secret_with_a_variable_goes_into_the_agents_environment() {
        let mut with = store("Stripe", "sk_env").unwrap();
        with.env = Some("STRIPE_KEY".into());
        let without = store("Other", "sk_none").unwrap();
        let mut gone = saved("missing", "Gone");
        gone.env = Some("GONE".into());
        let env = env_from(&[with.clone(), without.clone(), gone]);
        assert_eq!(env, [("STRIPE_KEY".to_string(), "sk_env".into())]);
        remove(&with);
        remove(&without);

        assert_eq!(
            check_env(" stripe_key ").unwrap().as_deref(),
            Some("STRIPE_KEY")
        );
        assert_eq!(check_env("").unwrap(), None);
        assert!(check_env("1KEY").is_err());
        assert!(check_env("MY-KEY").is_err());
    }

    #[test]
    fn find_ignores_case_and_spaces() {
        let list = [saved("a", "Stripe key")];
        assert_eq!(find(&list, " stripe KEY").map(|s| s.id.as_str()), Some("a"));
        assert!(find(&list, "stripe").is_none());
    }

    #[test]
    fn the_state_entry_holds_no_value() {
        let s = store("Key", "sk_live_never_in_state").unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("sk_live"));
        assert!(!json.contains("env"));
        let back: Saved = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
