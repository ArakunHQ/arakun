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
    collect(|after| {
        let data = graphql(s, &query, json!({ "after": after }))?;
        Ok(data["viewer"]["assignedIssues"].clone())
    })
}

/// Pages through `fetch` (cursor to page) until `LIMIT` issues or the last page.
fn collect(mut fetch: impl FnMut(&Value) -> Result<Value, ApiError>) -> Result<Assigned, ApiError> {
    let mut issues = Vec::new();
    let mut after = Value::Null;
    let mut next = false;
    while issues.len() < LIMIT {
        let page = fetch(&after)?;
        let page = &page;
        issues.extend(parse_issues(page));
        next = page["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
        if !next {
            break;
        }
        // A next page we can't ask for still means the list is not complete.
        match page["pageInfo"]["endCursor"].as_str() {
            Some(c) => after = json!(c),
            None => break,
        }
    }
    // Whole pages overshoot the limit, so dropped issues count as more too.
    let more = next || issues.len() > LIMIT;
    // Sort before the cut, so the issues in progress are the ones kept.
    sort(&mut issues);
    issues.truncate(LIMIT);
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

/// The text of a follow-up from its agent activity, when only the prompt
/// content has a body. Linear's schema: `content` is a union, and the
/// person's message is `AgentActivityPromptContent.body`. The activity must
/// belong to `agent_session`: an id from the relay must not make a session
/// read a message written for another one.
fn parse_prompt_text(data: &Value, agent_session: &str) -> Option<String> {
    let activity = &data["agentActivity"];
    if activity["agentSession"]["id"].as_str() != Some(agent_session) {
        return None;
    }
    let content = &activity["content"];
    if content["__typename"] != "AgentActivityPromptContent" {
        return None;
    }
    let body = content["body"].as_str()?;
    // Longer than the relay passes on: unreadable, never cut silently.
    (body.chars().count() <= crate::phone::linear::MAX_BODY).then(|| body.to_string())
}

/// A follow-up's text, read with the person's own key. The relay only names
/// the activity: its text always comes from Linear, for the agent session
/// the run was started for.
pub fn prompt_text(
    s: &Session,
    activity_id: &str,
    agent_session: &str,
) -> Result<String, ApiError> {
    let query = "query($id: String!) { agentActivity(id: $id) { agentSession { id } \
                 content { __typename ... on AgentActivityPromptContent { body } } } }";
    let data = graphql(s, query, json!({ "id": activity_id }))?;
    parse_prompt_text(&data, agent_session)
        .ok_or_else(|| ApiError::Failed("Linear didn't send that message.".into()))
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

    fn page(count: usize, next: bool) -> Value {
        let nodes: Vec<Value> = (0..count)
            .map(|i| node(&i.to_string(), "started", "2026-11-25T10:00:00.000Z"))
            .collect();
        json!({"nodes": nodes, "pageInfo": {"hasNextPage": next, "endCursor": "c"}})
    }
    #[test]
    fn three_full_pages_stop_at_the_cap_with_more() {
        let mut calls = 0;
        let got = collect(|_| {
            calls += 1;
            Ok(page(100, true))
        })
        .unwrap();
        assert_eq!((got.issues.len(), got.more, calls), (250, true, 3));
    }
    #[test]
    fn one_page_has_no_more() {
        let got = collect(|_| Ok(page(40, false))).unwrap();
        assert_eq!((got.issues.len(), got.more), (40, false));
    }
    #[test]
    fn exactly_the_cap_with_no_next_page_has_no_more() {
        let mut calls = 0;
        let got = collect(|_| {
            calls += 1;
            Ok(if calls < 3 {
                page(100, true)
            } else {
                page(50, false)
            })
        })
        .unwrap();
        assert_eq!((got.issues.len(), got.more), (250, false));
    }
    #[test]
    fn a_next_page_without_a_cursor_has_more() {
        let mut calls = 0;
        let got = collect(|_| {
            calls += 1;
            let mut p = page(40, true);
            p["pageInfo"]["endCursor"] = Value::Null;
            Ok(p)
        })
        .unwrap();
        assert_eq!((got.issues.len(), got.more, calls), (40, true, 1));
    }
    #[test]
    fn issues_in_progress_survive_the_cut() {
        let mut calls = 0;
        let got = collect(|_| {
            calls += 1;
            let state = if calls < 3 { "backlog" } else { "started" };
            let nodes: Vec<Value> = (0..100)
                .map(|i| node(&format!("{calls}-{i}"), state, "2026-11-25T10:00:00.000Z"))
                .collect();
            Ok(json!({"nodes": nodes, "pageInfo": {"hasNextPage": calls < 3, "endCursor": "c"}}))
        })
        .unwrap();
        assert_eq!((got.issues.len(), got.more, calls), (250, true, 3));
        let started = got
            .issues
            .iter()
            .filter(|i| i.group == Group::Started)
            .count();
        assert_eq!(started, 100);
        assert_eq!(got.issues[0].group, Group::Started);
    }

    #[test]
    fn a_prompt_activity_gives_its_text() {
        let data = json!({"agentActivity": {"agentSession": {"id": "as-1"}, "content": {
            "__typename": "AgentActivityPromptContent", "body": "Use the other API."}}});
        assert_eq!(
            parse_prompt_text(&data, "as-1").as_deref(),
            Some("Use the other API.")
        );
        let longest = "é".repeat(crate::phone::linear::MAX_BODY);
        let data = json!({"agentActivity": {"agentSession": {"id": "as-1"}, "content": {
            "__typename": "AgentActivityPromptContent", "body": longest}}});
        assert_eq!(parse_prompt_text(&data, "as-1"), Some(longest));
    }

    #[test]
    fn any_other_activity_or_shape_gives_none() {
        for data in [
            // Another content type: the fragment asks no body of it.
            json!({"agentActivity": {"agentSession": {"id": "as-1"}, "content": {"__typename": "AgentActivityThoughtContent"}}}),
            // Even with a body, only a prompt is the person's message.
            json!({"agentActivity": {"agentSession": {"id": "as-1"}, "content": {
                "__typename": "AgentActivityResponseContent", "body": "Done."}}}),
            json!({"agentActivity": {"agentSession": {"id": "as-1"}, "content": {
                "__typename": "AgentActivityPromptContent", "body": null}}}),
            json!({"agentActivity": {"agentSession": {"id": "as-1"}, "content": {
                "__typename": "AgentActivityPromptContent", "body": 7}}}),
            json!({"agentActivity": {"agentSession": {"id": "as-1"}, "content": {"body": "no type"}}}),
            json!({"agentActivity": {"agentSession": {"id": "as-1"}, "content": {
                "__typename": "AgentActivityPromptContent", "body": "é".repeat(16_001)}}}),
            // The activity of another agent session, or of none Linear names.
            json!({"agentActivity": {"agentSession": {"id": "as-2"}, "content": {
                "__typename": "AgentActivityPromptContent", "body": "Use the other API."}}}),
            json!({"agentActivity": {"agentSession": null, "content": {
                "__typename": "AgentActivityPromptContent", "body": "Use the other API."}}}),
            json!({"agentActivity": {"content": {
                "__typename": "AgentActivityPromptContent", "body": "Use the other API."}}}),
            json!({"agentActivity": null}),
            json!({}),
        ] {
            assert_eq!(parse_prompt_text(&data, "as-1"), None, "{data}");
        }
    }
}
