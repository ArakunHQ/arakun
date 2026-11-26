//! A secret held in memory: a token, a key, a password. It prints as
//! `[redacted]` in `{:?}`, so a log line or a failed assertion never shows it,
//! and its memory is wiped when it is dropped. The value comes out only
//! through [`Secret::expose`], so every place that reads it says so.
//!
//! It is saved as the plain string it holds: the files and the keyring it goes
//! to are where secrets are meant to be. [`crate::secrets`] is something else:
//! it finds secrets in text.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// The secret itself. Call it where the value is used, such as in a
    /// header or a file, never to print it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(crate::secrets::REDACTED)
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.expose())
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_value() {
        let s = Secret::new("ghp_abc123");
        assert_eq!(format!("{s:?}"), "[redacted]");
        assert_eq!(format!("{:?}", Some(s.clone())), "Some([redacted])");
        assert_eq!(s.expose(), "ghp_abc123");
    }

    #[test]
    fn it_is_saved_as_the_plain_string() {
        let s = Secret::new("tok");
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"tok\"");
        let back: Secret = serde_json::from_str("\"tok\"").unwrap();
        assert_eq!(back, s);
    }
}
