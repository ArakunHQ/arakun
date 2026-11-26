//! The teams and projects of a Linear workspace, for the picker in the opt-in.
// Task 3 (the opt-in UI) uses these and removes the allow.
#![allow(dead_code)]

use serde_json::Value;

use super::{ApiError, Session, graphql};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Place {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Places {
    pub teams: Vec<Place>,
    pub projects: Vec<Place>,
}

/// Reads at most 250 of each and does not page: a workspace with more shows the first 250.
pub fn places(s: &Session) -> Result<Places, ApiError> {
    let data = graphql(
        s,
        "query { teams(first: 250) { nodes { id name } } projects(first: 250) { nodes { id name } } }",
        serde_json::json!({}),
    )?;
    Ok(parse_places(&data))
}

fn parse_list(list: &Value) -> Vec<Place> {
    let mut out: Vec<Place> = list["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    Some(Place {
                        id: n["id"].as_str().filter(|s| !s.is_empty())?.to_string(),
                        name: n["name"].as_str().unwrap_or_default().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

pub fn parse_places(data: &Value) -> Places {
    Places {
        teams: parse_list(&data["teams"]),
        projects: parse_list(&data["projects"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn places_are_parsed_and_sorted() {
        let data = json!({
            "teams": { "nodes": [
                { "id": "t2", "name": "Web" },
                { "name": "No id" },
                { "id": "t1", "name": "App" },
            ] },
            "projects": { "nodes": [{ "id": "x1", "name": "Launch" }] },
        });
        let p = parse_places(&data);
        let names: Vec<_> = p.teams.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["App", "Web"]);
        assert_eq!(p.teams[0].id, "t1");
        assert_eq!(p.projects.len(), 1);
        assert_eq!(parse_places(&json!({})), Places::default());
    }
}
