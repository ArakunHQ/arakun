//! Reading issues for the Linear tab: the ones assigned to you, and one in full.

use serde_json::{Value, json};

use super::{ApiError, Session, graphql};

/// Where an issue sits in the list, from its workflow state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Started,
    Todo,
    Backlog,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::Started, Group::Todo, Group::Backlog];

    pub fn label(self) -> &'static str {
        match self {
            Group::Started => "In progress",
            Group::Todo => "Todo",
            Group::Backlog => "Backlog",
        }
    }

    /// Triage and any state type we don't know go with the backlog.
    fn from_state_type(t: &str) -> Group {
        match t {
            "started" => Group::Started,
            "unstarted" => Group::Todo,
            _ => Group::Backlog,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub url: String,
    pub priority: u8,
    pub state: String,
    pub group: Group,
    pub team: String,
    pub team_id: String,
    pub project: Option<String>,
    pub project_id: Option<String>,
    pub branch: String,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Assigned {
    pub issues: Vec<Issue>,
    /// There are more assigned issues than we fetched.
    pub more: bool,
}

/// A read that panicked found no issues.
impl crate::guard::Fallback for Assigned {
    fn fallback(_: crate::guard::Panicked) -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    pub issue: Issue,
    pub description: String,
    pub comments: Vec<Comment>,
}

const FIELDS: &str = "id identifier title url priority branchName updatedAt \
    state { name type } team { id key name } project { id name }";

/// Most issues we load; a person works from a short list.
const LIMIT: usize = 250;

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn time(v: &Value) -> u64 {
    v.as_str().and_then(crate::posthog::parse_iso).unwrap_or(0)
}

fn parse_issue(n: &Value) -> Option<Issue> {
    let id = n["id"].as_str().filter(|s| !s.is_empty())?;
    let project = n["project"]["name"].as_str().map(str::to_string);
    Some(Issue {
        id: id.to_string(),
        identifier: text(&n["identifier"]),
        title: text(&n["title"]),
        url: text(&n["url"]),
        priority: n["priority"].as_u64().unwrap_or(0).min(4) as u8,
        state: text(&n["state"]["name"]),
        group: Group::from_state_type(n["state"]["type"].as_str().unwrap_or_default()),
        team: text(&n["team"]["name"]),
        team_id: text(&n["team"]["id"]),
        project_id: project
            .as_ref()
            .and_then(|_| n["project"]["id"].as_str().map(str::to_string)),
        project,
        branch: text(&n["branchName"]),
        updated_at: time(&n["updatedAt"]),
    })
}

fn parse_issues(page: &Value) -> Vec<Issue> {
    page["nodes"]
        .as_array()
        .map(|nodes| nodes.iter().filter_map(parse_issue).collect())
        .unwrap_or_default()
}

/// Group order, then the most recently updated first.
fn sort(issues: &mut [Issue]) {
    issues.sort_by(|a, b| {
        let rank = |g: Group| Group::ALL.iter().position(|x| *x == g);
        rank(a.group)
            .cmp(&rank(b.group))
            .then(b.updated_at.cmp(&a.updated_at))
    });
}

fn parse_detail(n: &Value) -> Option<Detail> {
    let mut comments: Vec<Comment> = n["comments"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|c| Comment {
                    author: c["user"]["name"].as_str().unwrap_or("Someone").to_string(),
                    body: text(&c["body"]),
                    at: time(&c["createdAt"]),
                })
                .collect()
        })
        .unwrap_or_default();
    comments.sort_by_key(|c| c.at);
    Some(Detail {
        issue: parse_issue(n)?,
        description: text(&n["description"]),
        comments,
    })
}

/// The issues assigned to the signed-in user that are not done or canceled.
pub fn assigned(s: &Session) -> Result<Assigned, ApiError> {
    let query = format!(
        "query($after: String) {{ viewer {{ assignedIssues(first: 100, after: $after, \
         filter: {{ state: {{ type: {{ in: [\"started\", \"unstarted\", \"backlog\", \"triage\"] }} }} }}) \
         {{ nodes {{ {FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
    );
    let mut issues = Vec::new();
    let mut after = Value::Null;
    let mut next = false;
    while issues.len() < LIMIT {
        let data = graphql(s, &query, json!({ "after": after }))?;
        let page = &data["viewer"]["assignedIssues"];
        issues.extend(parse_issues(page));
        next = page["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
        match page["pageInfo"]["endCursor"].as_str() {
            Some(c) if next => after = json!(c),
            _ => {
                next = false;
                break;
            }
        }
    }
    // Whole pages overshoot the limit, so dropped issues count as more too.
    let more = next || issues.len() > LIMIT;
    issues.truncate(LIMIT);
    sort(&mut issues);
    Ok(Assigned { issues, more })
}

/// One issue with its description and its comments, oldest first.
pub fn detail(s: &Session, id: &str) -> Result<Detail, ApiError> {
    let query = format!(
        "query($id: String!) {{ issue(id: $id) {{ {FIELDS} description \
         comments(first: 50) {{ nodes {{ body createdAt user {{ name }} }} }} }} }}"
    );
    let data = graphql(s, &query, json!({ "id": id }))?;
    parse_detail(&data["issue"])
        .ok_or_else(|| ApiError::Failed("Linear didn't send that issue.".into()))
}

/// Search reads the identifier, the title and the team, ignoring case.
pub fn matches(issue: &Issue, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    [&issue.identifier, &issue.title, &issue.team]
        .iter()
        .any(|f| f.to_lowercase().contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn node(id: &str, state_type: &str, updated: &str) -> Value {
        json!({"id": id, "identifier": format!("ENG-{id}"), "title": "T", "url": "https://linear.app/a/issue/ENG-1",
            "priority": 2, "branchName": "rik/eng-1-t", "updatedAt": updated,
            "state": {"name": "S", "type": state_type}, "team": {"id": "t1", "key": "ENG", "name": "Eng"},
            "project": null})
    }
    #[test]
    fn issues_are_parsed_and_grouped() {
        let page = json!({"nodes": [node("1", "started", "2026-11-25T10:00:00.000Z"),
            node("2", "unstarted", "2026-11-25T10:00:00.000Z"), node("3", "backlog", "2026-11-25T10:00:00.000Z"),
            node("4", "triage", "2026-11-25T10:00:00.000Z")]});
        let got = parse_issues(&page);
        assert_eq!(
            got.iter().map(|i| i.group).collect::<Vec<_>>(),
            [Group::Started, Group::Todo, Group::Backlog, Group::Backlog]
        );
        assert_eq!(got[0].project, None);
        assert_eq!(got[0].team, "Eng");
    }
    #[test]
    fn a_node_without_an_id_is_skipped() {
        assert!(parse_issues(&json!({"nodes": [{"title": "x"}]})).is_empty());
    }
    #[test]
    fn newest_first_within_a_group() {
        let mut v = parse_issues(
            &json!({"nodes": [node("1", "started", "2026-11-24T10:00:00.000Z"),
            node("2", "started", "2026-11-25T10:00:00.000Z")]}),
        );
        sort(&mut v);
        assert_eq!(v[0].id, "2");
    }
    #[test]
    fn a_detail_without_description_or_comments_is_empty_not_null() {
        let mut n = node("1", "started", "2026-11-25T10:00:00.000Z");
        n["description"] = Value::Null;
        n["comments"] = json!({"nodes": []});
        let d = parse_detail(&n).unwrap();
        assert_eq!(d.description, "");
        assert!(d.comments.is_empty());
    }
    #[test]
    fn a_comment_without_a_user_has_a_name_anyway() {
        let mut n = node("1", "started", "2026-11-25T10:00:00.000Z");
        n["comments"] = json!({"nodes": [{"body": "hi", "createdAt": "2026-11-25T10:00:00.000Z", "user": null}]});
        assert_eq!(parse_detail(&n).unwrap().comments[0].author, "Someone");
    }
    #[test]
    fn search_reads_identifier_title_and_team() {
        let i =
            &parse_issues(&json!({"nodes": [node("1", "started", "2026-11-25T10:00:00.000Z")]}))[0];
        assert!(matches(i, "eng-1") && matches(i, "ENG") && matches(i, "") && !matches(i, "zzz"));
    }
}
