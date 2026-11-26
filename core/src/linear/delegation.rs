//! What the Linear opt-in saves, and the rules over it.
//!
//! `app_dir/linear/delegation.json` holds ids, names and paths, never a token.
//! The switch is off until the person turns it on.

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
    /// The root of the Arakun project: here, or on the server.
    pub project: PathBuf,
    /// The paired server (`servers::Saved::id`) the project is on, or None
    /// for this computer. A server is sent its own mappings without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// Each field defaults, so a file from an older or newer Arakun still loads
/// what it can.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Delegation {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mappings: Vec<Mapping>,
    /// The Linear organization ids whose key the person agreed to send.
    /// Turning the switch off keeps them: the person agreed, and the link
    /// on the server stays.
    #[serde(default)]
    pub consented: Vec<String>,
}

fn file() -> PathBuf {
    crate::paths::shared("linear").join("delegation.json")
}

impl Delegation {
    pub fn load() -> Delegation {
        Delegation::load_from(&file())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&file())
    }

    /// A missing or unreadable file reads as off, so a bad file never sends
    /// work. An unreadable one is set aside, not saved over: it holds the
    /// person's mappings and consents.
    pub fn load_from(file: &Path) -> Delegation {
        crate::stored::load(file).unwrap_or_default()
    }

    pub fn save_to(&self, file: &Path) -> Result<(), String> {
        serde_json::to_vec(self)
            .map_err(std::io::Error::other)
            .and_then(|bytes| crate::stored::write(file, &bytes))
            .map_err(|e| format!("Couldn't save the Linear settings: {e}"))
    }

    /// Records that the person agreed to send `workspace`'s Linear key.
    /// False when they had already.
    pub fn consent(&mut self, workspace: &str) -> bool {
        if self.consents(workspace) {
            return false;
        }
        self.consented.push(workspace.to_string());
        true
    }

    pub fn consents(&self, workspace: &str) -> bool {
        self.consented.iter().any(|w| w == workspace)
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

    /// The settings as one machine sees them: only its own mappings, with no
    /// server named. `server` None is this computer.
    pub fn on(&self, server: Option<&str>) -> Delegation {
        Delegation {
            enabled: self.enabled,
            mappings: self
                .mappings
                .iter()
                .filter(|m| m.server.as_deref() == server)
                .map(|m| Mapping {
                    server: None,
                    ..m.clone()
                })
                .collect(),
            consented: self.consented.clone(),
        }
    }

    /// The servers that have a mapping.
    pub fn servers(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .mappings
            .iter()
            .filter_map(|m| m.server.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn find(&self, workspace: &str, kind: Kind, target: &str) -> Option<&Mapping> {
        self.mappings
            .iter()
            .find(|m| m.workspace == workspace && m.kind == kind && m.target == target)
    }
}

/// The Linear targets this computer takes issues for, to tell the server.
#[derive(Default, Debug, PartialEq, Clone)]
pub struct Routes {
    pub teams: Vec<String>,
    pub projects: Vec<String>,
}

/// Empty when off. Otherwise the targets of `workspace` whose project is
/// still open here. A mapping to a server's project is that server's to send.
pub fn routes(d: &Delegation, workspace: &str, local_projects: &[PathBuf]) -> Routes {
    let mut r = Routes::default();
    if !d.enabled {
        return r;
    }
    for m in d
        .mappings
        .iter()
        .filter(|m| m.workspace == workspace && m.server.is_none() && !broken(m, local_projects))
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
pub fn broken(m: &Mapping, local_projects: &[PathBuf]) -> bool {
    !local_projects.contains(&m.project)
}

/// Where an issue goes. The project mapping wins: it is the narrower choice.
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

/// Why a delegated issue does not start on this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The person has not let Linear start sessions here.
    Off,
    /// Neither the issue's team nor its project is mapped here.
    Unmapped,
    /// The mapped Arakun project is no longer open here.
    ProjectGone,
}

/// The guard before a delegated issue starts: the switch is on, the issue's
/// project or team is mapped, and that mapping's project is still here.
pub fn target_for<'a>(
    d: &'a Delegation,
    workspace: &str,
    team: &str,
    project: Option<&str>,
    local: &[PathBuf],
) -> Result<&'a Mapping, Refusal> {
    if !d.enabled {
        return Err(Refusal::Off);
    }
    let m = project_for(d, workspace, team, project).ok_or(Refusal::Unmapped)?;
    // No fallback to the team's mapping: the person chose this project for it.
    if broken(m, local) {
        return Err(Refusal::ProjectGone);
    }
    Ok(m)
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
            server: None,
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
            consented: vec![],
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
            consented: vec![],
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
    fn consent_is_kept_once_per_workspace() {
        let mut d = Delegation::default();
        assert!(!d.consents("w1"));
        assert!(d.consent("w1"));
        assert!(!d.consent("w1"));
        assert!(d.consents("w1") && !d.consents("w2"));
        assert_eq!(d.consented, ["w1"]);
    }
    #[test]
    fn it_survives_the_disk_and_a_missing_or_broken_file() {
        let dir = crate::stored::tests::scratch("delegation");
        let file = dir.join("linear").join("delegation.json");
        assert_eq!(Delegation::load_from(&file), Delegation::default());

        let mut d = on(vec![team("w1", "t1", "/p/a"), project("w1", "x1", "/p/b")]);
        d.consent("w1");
        d.save_to(&file).unwrap();
        assert_eq!(Delegation::load_from(&file), d);

        // A file it can't read reads as off, and is kept aside, not lost.
        std::fs::write(&file, b"not json {").unwrap();
        assert_eq!(Delegation::load_from(&file), Delegation::default());
        assert_eq!(
            crate::stored::tests::kept(&file).as_deref(),
            Some("not json {")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_file_from_another_version_loads_what_it_can() {
        let dir = crate::stored::tests::scratch("delegation-versions");
        let file = dir.join("delegation.json");
        // Newer: a field this version doesn't know.
        std::fs::write(
            &file,
            br#"{"enabled": true, "mappings": [], "consented": ["w1"], "later": 1}"#,
        )
        .unwrap();
        let d = Delegation::load_from(&file);
        assert!(d.enabled && d.consents("w1"));
        // Older: written before consent was kept.
        std::fs::write(&file, br#"{"enabled": true, "mappings": []}"#).unwrap();
        let d = Delegation::load_from(&file);
        assert!(d.enabled && d.consented.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_guard_refuses_when_off() {
        let mut d = on(vec![team("w1", "t1", "/p/a")]);
        d.enabled = false;
        assert_eq!(
            target_for(&d, "w1", "t1", None, &[p("/p/a")]),
            Err(Refusal::Off)
        );
    }
    #[test]
    fn the_guard_refuses_an_unmapped_issue() {
        let d = on(vec![team("w1", "t1", "/p/a")]);
        assert_eq!(
            target_for(&d, "w1", "t2", Some("x1"), &[p("/p/a")]),
            Err(Refusal::Unmapped)
        );
    }
    #[test]
    fn the_guard_takes_the_project_mapping_over_the_team() {
        let d = on(vec![team("w1", "t1", "/p/a"), project("w1", "x1", "/p/b")]);
        let local = [p("/p/a"), p("/p/b")];
        assert_eq!(
            target_for(&d, "w1", "t1", Some("x1"), &local).map(|m| &m.project),
            Ok(&p("/p/b"))
        );
        assert_eq!(
            target_for(&d, "w1", "t1", Some("x2"), &local).map(|m| &m.project),
            Ok(&p("/p/a"))
        );
    }
    #[test]
    fn the_guard_never_matches_another_workspace() {
        let d = on(vec![team("w2", "t1", "/p/a"), project("w2", "x1", "/p/a")]);
        assert_eq!(
            target_for(&d, "w1", "t1", Some("x1"), &[p("/p/a")]),
            Err(Refusal::Unmapped)
        );
    }
    #[test]
    fn the_guard_refuses_when_the_project_is_gone() {
        // The project mapping wins even when gone: its team's is not a fallback.
        let d = on(vec![
            team("w1", "t1", "/p/a"),
            project("w1", "x1", "/p/gone"),
        ]);
        assert_eq!(
            target_for(&d, "w1", "t1", Some("x1"), &[p("/p/a")]),
            Err(Refusal::ProjectGone)
        );
    }
}
