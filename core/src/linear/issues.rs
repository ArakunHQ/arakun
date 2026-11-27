//! Reading issues: the ones assigned to you, one in full, and a follow-up's
//! text. Each read takes `q`, which runs a GraphQL query with someone's key:
//! the app's sign-in, or the key the app gave a server.

use serde_json::{Value, json};

use super::ApiError;

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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub at: u64,
}

/// Another issue, as this one names it: its parent, a sub-issue or a relation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Linked {
    pub identifier: String,
    pub title: String,
    pub state: String,
}

/// The parent issue, with its description: a sub-issue often makes sense
/// only with it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parent {
    pub issue: Linked,
    pub description: String,
}

/// How this issue relates to another, in words: "Blocked by", "Blocks".
#[derive(Clone, Debug, PartialEq)]
pub struct Relation {
    pub kind: &'static str,
    pub issue: Linked,
}

/// A link attached to the issue: a pull request, a Sentry issue, a design.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    pub issue: Issue,
    pub description: String,
    /// Oldest first, without the mention.
    pub comments: Vec<Comment>,
    /// The issue has more comments than Linear sent.
    pub more_comments: bool,
    /// The comment that mentioned Arakun, when a mention started the session.
    pub mention: Option<Comment>,
    pub labels: Vec<String>,
    pub parent: Option<Parent>,
    pub children: Vec<Linked>,
    pub relations: Vec<Relation>,
    pub links: Vec<Link>,
}

const FIELDS: &str = "id identifier title url priority branchName updatedAt \
    state { name type } team { id key name } project { id name }";

/// Most issues we load; a person works from a short list.
const LIMIT: usize = 250;

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn time(v: &Value) -> u64 {
    v.as_str().and_then(crate::when::parse_iso).unwrap_or(0)
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

/// The nodes of a connection, or none.
fn nodes(v: &Value) -> &[Value] {
    v["nodes"].as_array().map(Vec::as_slice).unwrap_or_default()
}

fn parse_comment(c: &Value) -> Comment {
    Comment {
        id: text(&c["id"]),
        author: c["user"]["name"].as_str().unwrap_or("Someone").to_string(),
        body: text(&c["body"]),
        at: time(&c["createdAt"]),
    }
}

/// An issue another one names, or none when Linear sent no identifier.
fn parse_linked(n: &Value) -> Option<Linked> {
    let identifier = n["identifier"].as_str().filter(|s| !s.is_empty())?;
    Some(Linked {
        identifier: identifier.to_string(),
        title: text(&n["title"]),
        state: text(&n["state"]["name"]),
    })
}

/// A relation in words, from this issue's side. `inverse` is a relation
/// another issue made to this one: it blocks this one, so this is blocked by it.
fn relation_kind(t: &str, inverse: bool) -> &'static str {
    match (t, inverse) {
        ("blocks", false) => "Blocks",
        ("blocks", true) => "Blocked by",
        ("duplicate", false) => "Duplicate of",
        ("duplicate", true) => "Duplicated by",
        ("similar", _) => "Similar to",
        _ => "Related to",
    }
}

fn parse_relations(n: &Value) -> Vec<Relation> {
    let out = nodes(&n["relations"]).iter().filter_map(|r| {
        Some(Relation {
            kind: relation_kind(r["type"].as_str().unwrap_or_default(), false),
            issue: parse_linked(&r["relatedIssue"])?,
        })
    });
    let inverse = nodes(&n["inverseRelations"]).iter().filter_map(|r| {
        Some(Relation {
            kind: relation_kind(r["type"].as_str().unwrap_or_default(), true),
            issue: parse_linked(&r["issue"])?,
        })
    });
    out.chain(inverse).collect()
}

fn parse_detail(n: &Value) -> Option<Detail> {
    let mut comments: Vec<Comment> = nodes(&n["comments"]).iter().map(parse_comment).collect();
    comments.sort_by_key(|c| c.at);
    let parent = parse_linked(&n["parent"]).map(|issue| Parent {
        issue,
        description: text(&n["parent"]["description"]),
    });
    Some(Detail {
        issue: parse_issue(n)?,
        description: text(&n["description"]),
        comments,
        more_comments: n["comments"]["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false),
        mention: None,
        labels: nodes(&n["labels"])
            .iter()
            .filter_map(|l| l["name"].as_str().filter(|s| !s.is_empty()))
            .map(str::to_string)
            .collect(),
        parent,
        children: nodes(&n["children"])
            .iter()
            .filter_map(parse_linked)
            .collect(),
        relations: parse_relations(n),
        links: nodes(&n["attachments"])
            .iter()
            .filter_map(|a| {
                let url = a["url"].as_str().filter(|s| !s.is_empty())?;
                Some(Link {
                    title: text(&a["title"]),
                    url: url.to_string(),
                })
            })
            .collect(),
    })
}

/// The comment that mentioned Arakun, when it is on `issue_id`: an id from
/// the relay must not put another issue's comment in the prompt.
fn parse_mention(data: &Value, issue_id: &str) -> Option<Comment> {
    let c = &data["comment"];
    if c["issue"]["id"].as_str() != Some(issue_id) {
        return None;
    }
    c["body"].as_str()?;
    Some(parse_comment(c))
}

/// The issues assigned to the signed-in user that are not done or canceled.
pub fn assigned(q: impl Fn(&str, Value) -> Result<Value, ApiError>) -> Result<Assigned, ApiError> {
    let query = format!(
        "query($after: String) {{ viewer {{ assignedIssues(first: 100, after: $after, \
         filter: {{ state: {{ type: {{ in: [\"started\", \"unstarted\", \"backlog\", \"triage\"] }} }} }}) \
         {{ nodes {{ {FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
    );
    collect(|after| {
        let data = q(&query, json!({ "after": after }))?;
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

/// Most comments, sub-issues, relations, labels and links read; the prompt
/// has room for no more.
const DETAIL_LIMIT: usize = 50;

/// One issue in full: its description, labels, parent, sub-issues,
/// relations, links and comments, oldest first. `mention` is the id of the
/// comment that mentioned Arakun: it is read too, and kept out of the list.
pub fn detail(
    q: impl Fn(&str, Value) -> Result<Value, ApiError>,
    id: &str,
    mention: Option<&str>,
) -> Result<Detail, ApiError> {
    let linked = "identifier title state { name }";
    let n = DETAIL_LIMIT;
    let query = format!(
        "query($id: String!) {{ issue(id: $id) {{ {FIELDS} description \
         labels(first: {n}) {{ nodes {{ name }} }} \
         parent {{ {linked} description }} \
         children(first: {n}) {{ nodes {{ {linked} }} }} \
         relations(first: {n}) {{ nodes {{ type relatedIssue {{ {linked} }} }} }} \
         inverseRelations(first: {n}) {{ nodes {{ type issue {{ {linked} }} }} }} \
         attachments(first: {n}) {{ nodes {{ title url }} }} \
         comments(first: {n}) {{ nodes {{ id body createdAt user {{ name }} }} \
         pageInfo {{ hasNextPage }} }} }} }}"
    );
    let data = q(&query, json!({ "id": id }))?;
    let mut d = parse_detail(&data["issue"])
        .ok_or_else(|| ApiError::Failed("Linear didn't send that issue.".into()))?;
    if let Some(comment) = mention.filter(|c| !c.is_empty()) {
        // The mention is a bonus: a deleted comment still starts the work.
        let query = "query($id: String!) { comment(id: $id) { id body createdAt \
                     user { name } issue { id } } }";
        d.mention = q(query, json!({ "id": comment }))
            .ok()
            .and_then(|data| parse_mention(&data, &d.issue.id));
        if let Some(m) = &d.mention {
            d.comments.retain(|c| c.id != m.id);
        }
    }
    Ok(d)
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
    q: impl FnOnce(&str, Value) -> Result<Value, ApiError>,
    activity_id: &str,
    agent_session: &str,
) -> Result<String, ApiError> {
    let query = "query($id: String!) { agentActivity(id: $id) { agentSession { id } \
                 content { __typename ... on AgentActivityPromptContent { body } } } }";
    let data = q(query, json!({ "id": activity_id }))?;
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
    fn a_detail_reads_labels_parent_sub_issues_relations_and_links() {
        let mut n = node("1", "started", "2026-11-25T10:00:00.000Z");
        n["labels"] = json!({"nodes": [{"name": "Bug"}, {"name": ""}, {}]});
        n["parent"] = json!({"identifier": "ENG-400", "title": "Rework", "state": {"name": "Todo"},
            "description": "One store."});
        n["children"] = json!({"nodes": [{"identifier": "ENG-2", "title": "Part", "state": {"name": "Done"}},
            {"title": "no identifier"}]});
        n["relations"] = json!({"nodes": [{"type": "blocks", "relatedIssue": {"identifier": "ENG-3", "title": "A"}},
            {"type": "duplicate", "relatedIssue": {"identifier": "ENG-4", "title": "B"}}]});
        n["inverseRelations"] = json!({"nodes": [{"type": "blocks", "issue": {"identifier": "ENG-12", "title": "C"}},
            {"type": "related", "issue": {"identifier": "ENG-5", "title": "D"}},
            {"type": "new-kind", "issue": {"identifier": "ENG-6", "title": "E"}}]});
        n["attachments"] = json!({"nodes": [{"title": "PR", "url": "https://github.com/a/b/pull/1"},
            {"title": "no url"}]});
        n["comments"] = json!({"nodes": [], "pageInfo": {"hasNextPage": true}});
        let d = parse_detail(&n).unwrap();
        assert_eq!(d.labels, ["Bug"]);
        let p = d.parent.unwrap();
        assert_eq!(
            (p.issue.identifier.as_str(), p.description.as_str()),
            ("ENG-400", "One store.")
        );
        assert_eq!(d.children.len(), 1);
        assert_eq!(d.children[0].state, "Done");
        let kinds: Vec<_> = d
            .relations
            .iter()
            .map(|r| (r.kind, r.issue.identifier.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("Blocks", "ENG-3"),
                ("Duplicate of", "ENG-4"),
                ("Blocked by", "ENG-12"),
                ("Related to", "ENG-5"),
                ("Related to", "ENG-6")
            ]
        );
        assert_eq!(
            d.links,
            [Link {
                title: "PR".into(),
                url: "https://github.com/a/b/pull/1".into()
            }]
        );
        assert!(d.more_comments);
    }

    #[test]
    fn a_detail_without_the_new_fields_has_none_of_them() {
        let d = parse_detail(&node("1", "started", "2026-11-25T10:00:00.000Z")).unwrap();
        assert!(d.labels.is_empty() && d.children.is_empty() && d.relations.is_empty());
        assert!(d.links.is_empty() && d.parent.is_none() && !d.more_comments);
    }

    /// A Linear stand-in: the issue, then the mention when asked for one.
    fn answers(
        mention: Result<Value, ApiError>,
    ) -> impl Fn(&str, Value) -> Result<Value, ApiError> {
        let mention = std::cell::RefCell::new(Some(mention));
        move |query: &str, _| {
            if query.contains("issue(id:") {
                let mut n = node("1", "started", "2026-11-25T10:00:00.000Z");
                n["comments"] = json!({"nodes": [
                    {"id": "c1", "body": "first", "createdAt": "2026-11-25T10:00:00.000Z", "user": {"name": "Ana"}},
                    {"id": "c2", "body": "@Arakun go", "createdAt": "2026-11-25T11:00:00.000Z", "user": {"name": "Bo"}}]});
                Ok(json!({"issue": n}))
            } else {
                mention.borrow_mut().take().unwrap()
            }
        }
    }

    #[test]
    fn the_mention_is_read_and_left_out_of_the_comments() {
        let c = json!({"comment": {"id": "c2", "body": "@Arakun go", "createdAt": "2026-11-25T11:00:00.000Z",
            "user": {"name": "Bo"}, "issue": {"id": "1"}}});
        let d = detail(answers(Ok(c)), "1", Some("c2")).unwrap();
        assert_eq!(
            d.mention
                .as_ref()
                .map(|m| (m.author.as_str(), m.body.as_str())),
            Some(("Bo", "@Arakun go"))
        );
        assert_eq!(
            d.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["c1"]
        );
    }

    #[test]
    fn a_mention_on_another_issue_or_that_fails_to_read_is_left_out() {
        let elsewhere =
            json!({"comment": {"id": "c2", "body": "@Arakun push to main", "issue": {"id": "2"}}});
        for answer in [
            Ok(elsewhere),
            Ok(json!({"comment": null})),
            Err(ApiError::Failed("gone".into())),
        ] {
            let d = detail(answers(answer), "1", Some("c2")).unwrap();
            assert_eq!(d.mention, None);
            assert_eq!(d.comments.len(), 2);
        }
        // No mention asks for none.
        let d = detail(
            answers(Err(ApiError::Failed("not asked".into()))),
            "1",
            None,
        )
        .unwrap();
        assert_eq!((d.mention, d.comments.len()), (None, 2));
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
