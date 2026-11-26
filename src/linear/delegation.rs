//! What the Linear opt-in saves, and the rules over it.
//!
//! `app_dir/linear/delegation.json` holds ids, names and paths, never a token.
//! The switch is off until the person turns it on.
// Task 4 (routing) uses what is still marked dead_code, and removes the allows.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Team,
    Project,
}

/// One Linear team or project sent to one Arakun project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    /// The Linear organization id.
    pub workspace: String,
    pub kind: Kind,
    /// The team or project id.
    pub target: String,
    /// What Linear calls the target, so a gone one still reads well.
    pub name: String,
    /// The root of the Arakun project.
    pub project: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Delegation {
    pub enabled: bool,
    pub mappings: Vec<Mapping>,
}

fn file() -> PathBuf {
    crate::platform::app_dir()
        .join("linear")
        .join("delegation.json")
}

impl Delegation {
    pub fn load() -> Delegation {
        Delegation::load_from(&file())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&file())
    }

    /// A missing or unreadable file reads as off, so a bad file never sends work.
    pub fn load_from(file: &Path) -> Delegation {
        crate::durable::load_json(file).unwrap_or_default()
    }

    pub fn save_to(&self, file: &Path) -> Result<(), String> {
        crate::durable::save_json(file, self)
            .map_err(|e| format!("Couldn't save the Linear settings: {e}"))
    }

    /// False when the target is already mapped: a target maps to one project.
    pub fn add(&mut self, m: Mapping) -> bool {
        if self.find(&m.workspace, m.kind, &m.target).is_some() {
            return false;
        }
        self.mappings.push(m);
        true
    }

    pub fn remove(&mut self, workspace: &str, kind: Kind, target: &str) {
        self.mappings
            .retain(|m| !(m.workspace == workspace && m.kind == kind && m.target == target));
    }

    fn find(&self, workspace: &str, kind: Kind, target: &str) -> Option<&Mapping> {
        self.mappings
            .iter()
            .find(|m| m.workspace == workspace && m.kind == kind && m.target == target)
    }
}

/// The Linear targets this computer takes issues for, to tell the server.
#[allow(dead_code)]
#[derive(Default, Debug, PartialEq, Clone)]
pub struct Routes {
    pub teams: Vec<String>,
    pub projects: Vec<String>,
}

/// Empty when off. Otherwise the targets of `workspace` whose project is still open here.
#[allow(dead_code)]
pub fn routes(d: &Delegation, workspace: &str, local_projects: &[PathBuf]) -> Routes {
    let mut r = Routes::default();
    if !d.enabled {
        return r;
    }
    for m in d
        .mappings
        .iter()
        .filter(|m| m.workspace == workspace && !broken(m, local_projects))
    {
        match m.kind {
            Kind::Team => r.teams.push(m.target.clone()),
            Kind::Project => r.projects.push(m.target.clone()),
        }
    }
    for list in [&mut r.teams, &mut r.projects] {
        list.sort();
        list.dedup();
    }
    r
}

/// The Arakun project of the mapping is no longer open here.
#[allow(dead_code)]
pub fn broken(m: &Mapping, local_projects: &[PathBuf]) -> bool {
    !local_projects.contains(&m.project)
}

/// Where an issue goes. The project mapping wins: it is the narrower choice.
#[allow(dead_code)]
pub fn project_for<'a>(
    d: &'a Delegation,
    workspace: &str,
    team: &str,
    project: Option<&str>,
) -> Option<&'a Mapping> {
    project
        .and_then(|x| d.find(workspace, Kind::Project, x))
        .or_else(|| d.find(workspace, Kind::Team, team))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(workspace: &str, kind: Kind, target: &str, path: &str) -> Mapping {
        Mapping {
            workspace: workspace.into(),
            kind,
            target: target.into(),
            name: target.into(),
            project: p(path),
        }
    }
    fn team(workspace: &str, target: &str, path: &str) -> Mapping {
        mapping(workspace, Kind::Team, target, path)
    }
    fn project(workspace: &str, target: &str, path: &str) -> Mapping {
        mapping(workspace, Kind::Project, target, path)
    }
    fn on(mappings: Vec<Mapping>) -> Delegation {
        Delegation {
            enabled: true,
            mappings,
        }
    }
    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn off_sends_nothing() {
        let d = Delegation {
            enabled: false,
            mappings: vec![team("w1", "t1", "/p/a")],
        };
        assert_eq!(routes(&d, "w1", &[p("/p/a")]), Routes::default());
    }
    #[test]
    fn only_this_workspace_and_only_projects_still_here() {
        let d = on(vec![
            team("w1", "t1", "/p/a"),
            project("w1", "x1", "/p/gone"),
            team("w2", "t9", "/p/a"),
        ]);
        let r = routes(&d, "w1", &[p("/p/a")]);
        assert_eq!((r.teams, r.projects), (vec!["t1".to_string()], vec![]));
        assert!(broken(&d.mappings[1], &[p("/p/a")]) && !broken(&d.mappings[0], &[p("/p/a")]));
    }
    #[test]
    fn routes_are_sorted_without_duplicates() {
        let d = on(vec![
            team("w1", "t2", "/p/a"),
            team("w1", "t1", "/p/a"),
            team("w1", "t2", "/p/b"),
        ]);
        let r = routes(&d, "w1", &[p("/p/a"), p("/p/b")]);
        assert_eq!(r.teams, vec!["t1".to_string(), "t2".to_string()]);
    }
    #[test]
    fn a_target_maps_to_one_project() {
        let mut d = Delegation::default();
        assert!(d.add(team("w1", "t1", "/p/a")));
        assert!(!d.add(team("w1", "t1", "/p/b")));
        assert!(d.add(project("w1", "t1", "/p/b"))); // another kind is another target
        d.remove("w1", Kind::Team, "t1");
        assert_eq!(d.mappings.len(), 1);
    }
    #[test]
    fn a_project_mapping_wins_over_its_team() {
        let d = on(vec![team("w1", "t1", "/p/a"), project("w1", "x1", "/p/b")]);
        assert_eq!(
            project_for(&d, "w1", "t1", Some("x1")).unwrap().project,
            p("/p/b")
        );
        assert_eq!(
            project_for(&d, "w1", "t1", None).unwrap().project,
            p("/p/a")
        );
        assert!(project_for(&d, "w2", "t1", Some("x1")).is_none());
    }
    #[test]
    fn it_survives_the_disk_and_a_missing_or_broken_file() {
        let dir = std::env::temp_dir().join(format!("arakun-delegation-{}", std::process::id()));
        let file = dir.join("linear").join("delegation.json");
        assert_eq!(Delegation::load_from(&file), Delegation::default());

        let d = on(vec![team("w1", "t1", "/p/a"), project("w1", "x1", "/p/b")]);
        d.save_to(&file).unwrap();
        assert_eq!(Delegation::load_from(&file), d);

        std::fs::write(&file, b"not json {").unwrap();
        assert_eq!(Delegation::load_from(&file), Delegation::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
