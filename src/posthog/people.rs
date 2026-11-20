//! The People tab's reads: who sent events in the last 30 days, searched by
//! email or distinct_id, and one person's profile: identity, properties,
//! cohorts, the flags their events carried, 14 days of events, their
//! sessions and a timeline.

use serde_json::{Map, Value};

use super::events::show;
use super::query::{self, Hogql, Paging};
use super::{ApiError, Project, Session, num, parse_iso};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Person {
    pub id: String,
    pub email: Option<String>,
    pub distinct_id: String,
    pub first_seen: u64,
    pub last_seen: u64,
    pub events: u64,
}

impl Person {
    pub fn name(&self) -> &str {
        self.email.as_deref().unwrap_or(&self.distinct_id)
    }
}

fn search(q: &mut Hogql, text: &str) {
    q.push(" WHERE timestamp > now() - INTERVAL 30 DAY");
    let text = text.trim();
    if !text.is_empty() {
        let p = q.bind(query::contains_pattern(text));
        q.push(&format!(
            " AND (person.properties.email ILIKE {p} OR distinct_id ILIKE {p})"
        ));
    }
}

/// People with events in the last 30 days, most recently seen first.
pub fn list_query(text: &str, page: Paging) -> Hogql {
    let mut q = Hogql::new(
        "SELECT person_id, argMax(person.properties.email, timestamp), argMax(distinct_id, timestamp), \
         min(person.created_at), max(timestamp), count() FROM events",
    );
    search(&mut q, text);
    // person_id breaks the tie between people last seen at the same moment,
    // so a page boundary cannot drop or repeat one.
    q.push(" GROUP BY person_id ORDER BY max(timestamp) DESC, person_id");
    q.push(&page.clause());
    q
}

/// How many people `list_query` would find in all.
pub fn total_query(text: &str) -> Hogql {
    let mut q = Hogql::new("SELECT uniq(person_id) FROM events");
    search(&mut q, text);
    q
}

pub fn parse_list(rows: &[Value]) -> Vec<Person> {
    rows.iter()
        .filter_map(|r| {
            Some(Person {
                id: r[0].as_str().filter(|s| !s.is_empty())?.to_string(),
                email: r[1].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                distinct_id: r[2].as_str().unwrap_or_default().to_string(),
                first_seen: r[3].as_str().and_then(parse_iso).unwrap_or(0),
                last_seen: r[4].as_str().and_then(parse_iso).unwrap_or(0),
                events: num(&r[5]),
            })
        })
        .collect()
}

/// A person as `/persons/` lists them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identity {
    pub uuid: String,
    pub distinct_ids: Vec<String>,
    pub properties: Map<String, Value>,
    pub created_at: u64,
    pub identified: bool,
}

pub fn parse_person(v: &Value) -> Option<Identity> {
    let p = v["results"].as_array().and_then(|r| r.first()).unwrap_or(v);
    Some(Identity {
        uuid: p["uuid"]
            .as_str()
            .or(p["id"].as_str())
            .filter(|s| !s.is_empty())?
            .to_string(),
        distinct_ids: p["distinct_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d.as_str().map(str::to_string))
            .collect(),
        properties: p["properties"].as_object().cloned().unwrap_or_default(),
        created_at: p["created_at"].as_str().and_then(parse_iso).unwrap_or(0),
        identified: p["is_identified"].as_bool().unwrap_or(false),
    })
}

pub fn parse_cohorts(v: &Value) -> Vec<String> {
    v["results"]
        .as_array()
        .or(v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| c["name"].as_str().filter(|n| !n.is_empty()))
        .map(str::to_string)
        .collect()
}

pub const DAYS: usize = 14;

fn of_person(select: &str, person: &str, rest: &str) -> Hogql {
    let mut q = Hogql::new(select);
    let p = q.bind(person);
    q.push(&format!(
        " FROM events WHERE person_id = {p} AND timestamp > now() - INTERVAL {DAYS} DAY{rest}"
    ));
    q
}

pub fn daily_query(person: &str) -> Hogql {
    of_person(
        "SELECT dateDiff('day', timestamp, now()) AS ago, count()",
        person,
        " GROUP BY ago",
    )
}

pub fn sessions_query(person: &str) -> Hogql {
    of_person(
        "SELECT properties.$session_id AS sid, min(timestamp), max(timestamp), count(), \
         any(properties.$app_version), any(properties.$os)",
        person,
        " AND sid IS NOT NULL AND sid != '' GROUP BY sid ORDER BY max(timestamp) DESC LIMIT 5",
    )
}

pub fn timeline_query(person: &str) -> Hogql {
    of_person(
        "SELECT event, timestamp, properties",
        person,
        " ORDER BY timestamp DESC LIMIT 100",
    )
}

/// Rows of `daily_query` → events per day, oldest first.
pub fn parse_daily(rows: &[Value]) -> Vec<u64> {
    let mut out = vec![0; DAYS];
    for r in rows {
        let ago = (num(&r[0]) as usize).min(DAYS - 1);
        out[DAYS - 1 - ago] += num(&r[1]);
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recording {
    pub id: String,
    pub start: u64,
    pub end: u64,
    pub events: u64,
    /// "0.6.0 · macOS".
    pub detail: String,
}

pub fn parse_sessions(rows: &[Value]) -> Vec<Recording> {
    rows.iter()
        .filter_map(|r| {
            let detail = [r[4].as_str(), r[5].as_str()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            Some(Recording {
                id: r[0].as_str().filter(|s| !s.is_empty())?.to_string(),
                start: r[1].as_str().and_then(parse_iso).unwrap_or(0),
                end: r[2].as_str().and_then(parse_iso).unwrap_or(0),
                events: num(&r[3]),
                detail,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Other,
    AgentTurn,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Moment {
    pub event: String,
    pub time: u64,
    pub detail: String,
    pub kind: Kind,
    pub props: Map<String, Value>,
}

fn kind_of(event: &str) -> Kind {
    let e = event.to_ascii_lowercase();
    if e == "$exception" || e.contains("error") {
        Kind::Error
    } else if e.contains("turn") {
        Kind::AgentTurn
    } else {
        Kind::Other
    }
}

/// What a timeline line says after the event: the exception's type, or the
/// first two of the event's own properties.
fn detail(event: &str, props: &Map<String, Value>) -> String {
    if event == "$exception" {
        let first = props
            .get("$exception_types")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .or(props.get("$exception_type").and_then(Value::as_str));
        if let Some(t) = first {
            return t.to_string();
        }
    }
    let mut own: Vec<(&String, &Value)> = props
        .iter()
        .filter(|(k, v)| !k.starts_with('$') && k.as_str() != "token" && !v.is_object())
        .collect();
    own.sort_by_key(|(k, _)| k.as_str());
    own.iter()
        .take(2)
        .map(|(_, v)| show(v))
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn parse_timeline(rows: &[Value]) -> Vec<Moment> {
    rows.iter()
        .filter_map(|r| {
            let event = r[0].as_str()?.to_string();
            let props = match &r[2] {
                Value::Object(m) => m.clone(),
                Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
                _ => Map::new(),
            };
            Some(Moment {
                time: r[1].as_str().and_then(parse_iso).unwrap_or(0),
                detail: detail(&event, &props),
                kind: kind_of(&event),
                event,
                props,
            })
        })
        .collect()
}

/// The flags their newest events carried (`$feature/<key>`), newest value
/// per flag, by key.
pub fn flags(timeline: &[Moment]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for m in timeline {
        for (k, v) in &m.props {
            if let Some(flag) = k.strip_prefix("$feature/")
                && !out.iter().any(|(f, _)| f == flag)
            {
                out.push((flag.to_string(), show(v)));
            }
        }
    }
    out.sort();
    out
}

/// Everything the profile shows of one person.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub person: String,
    pub identity: Option<Identity>,
    pub cohorts: Option<Vec<String>>,
    /// Each analytics read stands alone: one that failed shows its error in
    /// its own section, not the whole profile's.
    pub daily: Result<Vec<u64>, ApiError>,
    pub sessions: Result<Vec<Recording>, ApiError>,
    pub timeline: Result<Vec<Moment>, ApiError>,
    /// From the timeline's events; fails with it.
    pub flags: Result<Vec<(String, String)>, ApiError>,
    /// Queries this read cost.
    pub spent: u32,
}

/// Reads `who`'s profile (person_id, with a distinct_id to find them by)
/// as `s`. The person itself must be readable (person:read); the rest
/// shows what it can.
pub fn load_profile(
    s: &Session,
    p: &Project,
    person_id: &str,
    distinct_id: &str,
    fresh: u64,
) -> Result<Profile, ApiError> {
    let id = p.id;
    // The identity comes first: a distinct_id can have moved to another
    // person since the list was read, and the activity below must be theirs,
    // not the id the list cached.
    let who = super::call(
        s,
        "GET",
        &format!(
            "/api/projects/{id}/persons/?distinct_id={}",
            super::encode(distinct_id)
        ),
        None,
    )?;
    let identity = parse_person(&who);
    let person_id = identity
        .as_ref()
        .map(|i| i.uuid.as_str())
        .unwrap_or(person_id);
    let (daily, sessions, timeline) = std::thread::scope(|scope| {
        let d = scope.spawn(|| query::run(s, id, &daily_query(person_id), fresh));
        let se = scope.spawn(|| query::run(s, id, &sessions_query(person_id), fresh));
        let t = scope.spawn(|| query::run(s, id, &timeline_query(person_id), fresh));
        (super::joined(d), super::joined(se), super::joined(t))
    });
    let cohorts = identity.as_ref().and_then(|i| {
        super::call(
            s,
            "GET",
            &format!(
                "/api/projects/{id}/persons/cohorts/?person_id={}",
                super::encode(&i.uuid)
            ),
            None,
        )
        .ok()
        .map(|v| parse_cohorts(&v))
    });
    let spent = [&daily, &sessions, &timeline]
        .into_iter()
        .filter_map(|r| r.as_ref().ok().map(|a| a.spent))
        .sum::<u32>();
    let timeline = timeline.map(|a| parse_timeline(&a.rows));
    let flags = timeline.as_ref().map(|m| flags(m)).map_err(|e| e.clone());
    let daily = daily.map(|a| parse_daily(&a.rows));
    let sessions = sessions.map(|a| parse_sessions(&a.rows));
    Ok(Profile {
        person: person_id.to_string(),
        identity,
        cohorts,
        daily,
        sessions,
        flags,
        timeline,
        spent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_is_bound_and_literal() {
        let q = list_query("50%' OR 1=1", Paging::first(50));
        assert!(q.text.contains(
            "WHERE timestamp > now() - INTERVAL 30 DAY AND (person.properties.email ILIKE {v0} OR distinct_id ILIKE {v0})"
        ));
        assert!(
            q.text
                .ends_with("ORDER BY max(timestamp) DESC, person_id LIMIT 51 OFFSET 0")
        );
        assert!(!q.text.contains("OR 1=1"));
        assert_eq!(q.values["v0"], "%50\\%' OR 1=1%");
        let all = total_query("  ");
        assert_eq!(
            all.text,
            "SELECT uniq(person_id) FROM events WHERE timestamp > now() - INTERVAL 30 DAY"
        );
        assert!(all.values.is_empty());
        let t = timeline_query("p'1");
        assert!(t.text.contains("WHERE person_id = {v0} AND"));
        assert_eq!(t.values["v0"], "p'1");
    }

    #[test]
    fn parses_people_and_persons() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[["p1", "rik@tallerlabs.mx", "01936e7a", "2026-10-04T10:00:00Z", "2026-11-18T14:01:00Z", 3218],
                ["p2", "", "4f2c91e0", null, "2026-11-18T13:50:00Z", "214"],
                [null, "x", "y", null, null, 1]]"#,
        )
        .unwrap();
        let people = parse_list(&rows);
        assert_eq!(people.len(), 2);
        assert_eq!(people[0].name(), "rik@tallerlabs.mx");
        assert_eq!(people[1].name(), "4f2c91e0");
        assert_eq!((people[1].events, people[1].first_seen), (214, 0));

        let v: Value = serde_json::from_str(
            r#"{"next":null,"results":[{"id":12,"uuid":"01936e7a-b2c4","distinct_ids":["01936e7a","phone-rik-ios"],
                "properties":{"email":"rik@tallerlabs.mx","plan":"pro"},"created_at":"2026-10-04T10:00:00Z","is_identified":true}]}"#,
        )
        .unwrap();
        let i = parse_person(&v).unwrap();
        assert_eq!(i.uuid, "01936e7a-b2c4");
        assert_eq!(i.distinct_ids, vec!["01936e7a", "phone-rik-ios"]);
        assert_eq!(i.properties["plan"], "pro");
        assert!(i.identified);
        assert_eq!(parse_person(&serde_json::json!({"results": []})), None);

        let c: Value =
            serde_json::from_str(r#"{"results":[{"id":1,"name":"Teams beta"},{"id":2}]}"#).unwrap();
        assert_eq!(parse_cohorts(&c), vec!["Teams beta"]);
    }

    #[test]
    fn profile_parts() {
        let rows: Vec<Value> = serde_json::from_str("[[0, 11], [13, 2], [40, 1]]").unwrap();
        let d = parse_daily(&rows);
        assert_eq!((d.len(), d[13], d[0]), (DAYS, 11, 3));

        let rows: Vec<Value> = serde_json::from_str(
            r#"[["s1", "2026-11-18T13:44:00Z", "2026-11-18T14:02:00Z", 46, "0.6.0", "macOS"], ["", null, null, 1, null, null]]"#,
        )
        .unwrap();
        let s = parse_sessions(&rows);
        assert_eq!(s.len(), 1);
        assert_eq!(
            (s[0].end - s[0].start, s[0].detail.as_str()),
            (18 * 60, "0.6.0 · macOS")
        );

        let rows: Vec<Value> = serde_json::from_str(
            r#"[["agent turn finished", "2026-11-18T14:01:00Z", "{\"model\":\"opus-5.5\",\"agent\":\"claude-code\",\"$feature/new-composer\":\"inline-tools\"}"],
                ["$exception", "2026-11-18T13:50:00Z", {"$exception_types":["TypeError"],"$feature/new-composer":"control","$feature/team-chat":true}],
                ["view opened", "2026-11-18T13:59:00Z", {"view":"PostHog"}]]"#,
        )
        .unwrap();
        let t = parse_timeline(&rows);
        assert_eq!(t[0].kind, Kind::AgentTurn);
        assert_eq!(t[0].detail, "claude-code · opus-5.5");
        assert_eq!(
            (t[1].kind, t[1].detail.as_str()),
            (Kind::Error, "TypeError")
        );
        assert_eq!((t[2].kind, t[2].detail.as_str()), (Kind::Other, "PostHog"));
        // The newest value of a flag wins.
        assert_eq!(
            flags(&t),
            vec![
                ("new-composer".to_string(), "inline-tools".to_string()),
                ("team-chat".into(), "true".into())
            ]
        );
    }
}
