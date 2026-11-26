//! What the app sends a server so that the server takes Linear's work for
//! its own projects: the switch, the mappings to the server's projects, and
//! the Linear key of each mapped workspace. The server keeps them in a
//! private file and tells Arakun's server which teams and projects it takes.
//!
//! The key is what the server reads a delegated issue with, as the app does
//! with its own sign-in: the relay's word alone never starts anything.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::delegation::{Delegation, Mapping, Routes, routes};
use crate::secret::Secret;

/// The most mappings a server keeps. Each is one route row on the server.
pub const MAX_MAPPINGS: usize = 200;

/// A Linear workspace's key. Debug never shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceKey {
    /// The Linear organization id.
    pub workspace: String,
    pub key: Secret,
}

/// Each field defaults, so an older or newer app's message still reads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Their `project` is a place on the server.
    #[serde(default)]
    pub mappings: Vec<Mapping>,
    #[serde(default)]
    pub keys: Vec<WorkspaceKey>,
}

impl ServerSettings {
    /// The guard's view of these settings.
    pub fn delegation(&self) -> Delegation {
        Delegation {
            enabled: self.enabled,
            mappings: self.mappings.clone(),
            consented: Vec::new(),
        }
    }

    /// The key for `workspace`, when there is a usable one.
    pub fn key_for(&self, workspace: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|k| k.workspace == workspace)
            .map(|k| k.key.expose())
            .filter(|k| !k.is_empty())
    }

    /// What a server keeps of settings an app sent: one mapping per target,
    /// at most [`MAX_MAPPINGS`], and only the keys of mapped workspaces, so
    /// a key the server has no use for is not kept on its disk.
    pub fn tidy(self) -> ServerSettings {
        let mut mappings: Vec<Mapping> = Vec::new();
        for m in self.mappings {
            let taken = mappings
                .iter()
                .any(|o| o.workspace == m.workspace && o.kind == m.kind && o.target == m.target);
            if !taken && mappings.len() < MAX_MAPPINGS {
                mappings.push(m);
            }
        }
        let mut keys: Vec<WorkspaceKey> = Vec::new();
        for k in self.keys {
            let mapped = mappings.iter().any(|m| m.workspace == k.workspace);
            let seen = keys.iter().any(|o| o.workspace == k.workspace);
            if mapped && !seen && !k.key.is_empty() {
                keys.push(k);
            }
        }
        ServerSettings {
            enabled: self.enabled,
            mappings,
            keys,
        }
    }
}

/// The route calls that bring Arakun's server in line with `s`: each mapped
/// workspace with what it sends here (empty when off, or when none of its
/// projects is a place any more), and an empty one for each workspace routes
/// were sent for before and that is no longer mapped.
pub fn route_plan(
    s: &ServerSettings,
    announced: &[String],
    places: &[PathBuf],
) -> Vec<(String, Routes)> {
    let d = s.delegation();
    let mut workspaces: Vec<&str> = s.mappings.iter().map(|m| m.workspace.as_str()).collect();
    workspaces.extend(announced.iter().map(String::as_str));
    workspaces.sort();
    workspaces.dedup();
    workspaces
        .into_iter()
        .map(|w| (w.to_string(), routes(&d, w, places)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::delegation::Kind;

    fn mapping(workspace: &str, kind: Kind, target: &str, project: &str) -> Mapping {
        Mapping {
            workspace: workspace.into(),
            kind,
            target: target.into(),
            name: target.into(),
            project: PathBuf::from(project),
        }
    }

    fn key(workspace: &str, key: &str) -> WorkspaceKey {
        WorkspaceKey {
            workspace: workspace.into(),
            key: Secret::new(key),
        }
    }

    #[test]
    fn a_target_is_kept_once_and_unmapped_keys_are_dropped() {
        let s = ServerSettings {
            enabled: true,
            mappings: vec![
                mapping("w1", Kind::Team, "t1", "/srv/a"),
                mapping("w1", Kind::Team, "t1", "/srv/b"),
                mapping("w1", Kind::Project, "t1", "/srv/b"),
            ],
            keys: vec![key("w1", "lin_1"), key("w1", "lin_2"), key("w2", "lin_3")],
        }
        .tidy();
        assert_eq!(s.mappings.len(), 2);
        assert_eq!(s.mappings[0].project, PathBuf::from("/srv/a"));
        assert_eq!(s.keys, vec![key("w1", "lin_1")]);
        assert_eq!(s.key_for("w1"), Some("lin_1"));
        assert_eq!(s.key_for("w2"), None);
    }

    #[test]
    fn an_empty_key_is_no_key() {
        let s = ServerSettings {
            enabled: true,
            mappings: vec![mapping("w1", Kind::Team, "t1", "/srv/a")],
            keys: vec![key("w1", "")],
        }
        .tidy();
        assert!(s.keys.is_empty());
        assert_eq!(s.key_for("w1"), None);
    }

    #[test]
    fn mappings_are_capped() {
        let s = ServerSettings {
            enabled: true,
            mappings: (0..MAX_MAPPINGS + 5)
                .map(|i| mapping("w1", Kind::Team, &format!("t{i}"), "/srv/a"))
                .collect(),
            keys: Vec::new(),
        }
        .tidy();
        assert_eq!(s.mappings.len(), MAX_MAPPINGS);
    }

    #[test]
    fn routes_go_for_mapped_workspaces_and_clear_the_ones_announced_before() {
        let s = ServerSettings {
            enabled: true,
            mappings: vec![
                mapping("w1", Kind::Team, "t1", "/srv/a"),
                mapping("w1", Kind::Project, "p1", "/srv/gone"),
            ],
            keys: Vec::new(),
        };
        let places = [PathBuf::from("/srv/a")];
        let plan = route_plan(&s, &["w0".into(), "w1".into()], &places);
        assert_eq!(
            plan,
            vec![
                ("w0".to_string(), Routes::default()),
                (
                    "w1".to_string(),
                    Routes {
                        teams: vec!["t1".into()],
                        projects: Vec::new(),
                    }
                ),
            ]
        );
    }

    #[test]
    fn off_sends_empty_routes() {
        let s = ServerSettings {
            enabled: false,
            mappings: vec![mapping("w1", Kind::Team, "t1", "/srv/a")],
            keys: Vec::new(),
        };
        let plan = route_plan(&s, &[], &[PathBuf::from("/srv/a")]);
        assert_eq!(plan, vec![("w1".to_string(), Routes::default())]);
    }

    #[test]
    fn the_key_never_shows_in_debug() {
        let s = ServerSettings {
            enabled: true,
            mappings: Vec::new(),
            keys: vec![key("w1", "lin_api_secret")],
        };
        assert!(!format!("{s:?}").contains("lin_api_secret"));
    }

    #[test]
    fn the_wire_reads_as_the_app_sends_it() {
        let s: ServerSettings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "mappings": [{"workspace": "w1", "kind": "team", "target": "t1", "name": "Eng", "project": "/srv/a"}],
            "keys": [{"workspace": "w1", "key": "lin_1"}],
        }))
        .unwrap();
        assert_eq!(s.key_for("w1"), Some("lin_1"));
        assert_eq!(s.mappings[0].kind, Kind::Team);
        // An older app that sends nothing new still reads.
        let empty: ServerSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(empty, ServerSettings::default());
    }
}
