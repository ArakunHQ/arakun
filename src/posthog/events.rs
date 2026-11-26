//! The Events tab's reads: the events that match a filter, newest first, a
//! page at a time, and their volume over the filter's range.

use serde_json::{Map, Value};

use super::query::{Hogql, Paging};
use super::{num, parse_iso};

/// How far back the tab looks, and how its chart cuts that into bars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    Hour,
    #[default]
    Day,
    Week,
    Month,
}

impl Range {
    pub const ALL: [Range; 4] = [Range::Hour, Range::Day, Range::Week, Range::Month];

    pub fn label(self) -> &'static str {
        match self {
            Range::Hour => "1 h",
            Range::Day => "24 h",
            Range::Week => "7 d",
            Range::Month => "30 d",
        }
    }

    /// "last 24 hours", under the chart's total.
    pub fn span(self) -> &'static str {
        match self {
            Range::Hour => "last hour",
            Range::Day => "last 24 hours",
            Range::Week => "last 7 days",
            Range::Month => "last 30 days",
        }
    }

    pub fn seconds(self) -> u64 {
        match self {
            Range::Hour => 3600,
            Range::Day => 86_400,
            Range::Week => 7 * 86_400,
            Range::Month => 30 * 86_400,
        }
    }

    /// Bars in the chart.
    pub fn buckets(self) -> u64 {
        match self {
            Range::Hour => 12,
            Range::Day => 24,
            Range::Week => 28,
            Range::Month => 30,
        }
    }

    pub fn bucket_seconds(self) -> u64 {
        self.seconds() / self.buckets()
    }
}

/// `key = value` on an event property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropFilter {
    // Not a secret: the property's name.
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// None: every event.
    pub event: Option<String>,
    pub props: Vec<PropFilter>,
    /// An email or a distinct_id; empty for everyone.
    pub person: String,
    pub range: Range,
    /// The read's right edge: the first page reads "until now", and the pages
    /// after it keep that edge so new events arriving between them cannot
    /// shift what a later page returns. `None` is "now".
    pub until: Option<u64>,
}

/// A property's value as text, the way HogQL prints `properties.x`: strings
/// without their quotes, numbers and booleans as written.
const PROP: &str = "replaceRegexpAll(JSONExtractRaw(properties, {key}), '^\"|\"$', '')";

impl Filter {
    /// `WHERE …` with every value from the user bound, never written in.
    fn conditions(&self, q: &mut Hogql) {
        let hi = self
            .until
            .map(|u| format!("fromUnixTimestamp({u})"))
            .unwrap_or_else(|| "now()".into());
        q.push(&format!(
            " WHERE timestamp > {hi} - INTERVAL {} SECOND AND timestamp <= {hi}",
            self.range.seconds()
        ));
        if let Some(e) = &self.event {
            let p = q.bind(e.as_str());
            q.push(&format!(" AND event = {p}"));
        }
        for f in &self.props {
            let key = q.bind(f.key.as_str());
            let value = q.bind(f.value.as_str());
            q.push(&format!(" AND {} = {value}", PROP.replace("{key}", &key)));
        }
        let person = self.person.trim();
        if !person.is_empty() {
            let p = q.bind(person);
            q.push(&format!(
                " AND (distinct_id = {p} OR person.properties.email = {p})"
            ));
        }
    }
}

/// The events of `f`, newest first. `after` is the last row of the page
/// before — its raw timestamp and uuid — so a page continues exactly where
/// the one before ended: a new event or a shared timestamp cannot repeat or
/// skip a row the way a live OFFSET can.
pub fn list_query(f: &Filter, page: Paging, after: Option<(&str, &str)>) -> Hogql {
    let mut q = Hogql::new(
        "SELECT uuid, event, timestamp, distinct_id, person_id, person.properties.email, properties, person.properties FROM events",
    );
    f.conditions(&mut q);
    if let Some((ts, uuid)) = after {
        let t = q.bind(ts);
        let u = q.bind(uuid);
        q.push(&format!(
            " AND (timestamp < {t} OR (timestamp = {t} AND uuid < {u}))"
        ));
    }
    q.push(" ORDER BY timestamp DESC, uuid DESC");
    q.push(&page.clause());
    q
}

/// How many events of `f` fell in each bar, by bars ago.
pub fn volume_query(f: &Filter) -> Hogql {
    let mut q = Hogql::new(format!(
        "SELECT intDiv(dateDiff('second', timestamp, now()), {}) AS ago, count() FROM events",
        f.range.bucket_seconds()
    ));
    f.conditions(&mut q);
    q.push(" GROUP BY ago ORDER BY ago");
    q
}

/// Rows of `volume_query` → one count per bar, oldest first.
pub fn parse_volume(rows: &[Value], range: Range) -> Vec<u64> {
    let n = range.buckets() as usize;
    let mut out = vec![0; n];
    for r in rows {
        let ago = (num(&r[0]) as usize).min(n - 1);
        out[n - 1 - ago] += num(&r[1]);
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    pub uuid: String,
    pub event: String,
    /// The timestamp as returned, for the next page's cursor.
    pub stamp: String,
    pub time: u64,
    pub distinct_id: String,
    pub person_id: String,
    pub email: Option<String>,
    pub props: Map<String, Value>,
    pub person_props: Map<String, Value>,
}

impl Event {
    /// Who did it, as the table shows them.
    pub fn who(&self) -> &str {
        self.email.as_deref().unwrap_or(&self.distinct_id)
    }

    pub fn session(&self) -> Option<&str> {
        self.props
            .get("$session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }
}

/// HogQL hands JSON columns back as text; either way, an object.
fn object(v: &Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m.clone(),
        Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
        _ => Map::new(),
    }
}

pub fn parse_rows(rows: &[Value]) -> Vec<Event> {
    rows.iter()
        .filter_map(|r| {
            Some(Event {
                uuid: r[0].as_str().unwrap_or_default().to_string(),
                event: r[1].as_str()?.to_string(),
                stamp: r[2].as_str().unwrap_or_default().to_string(),
                time: r[2].as_str().and_then(parse_iso).unwrap_or(0),
                distinct_id: r[3].as_str().unwrap_or_default().to_string(),
                person_id: r[4].as_str().unwrap_or_default().to_string(),
                email: r[5].as_str().filter(|e| !e.is_empty()).map(str::to_string),
                props: object(&r[6]),
                person_props: object(&r[7]),
            })
        })
        .collect()
}

/// Properties every SDK sets that say little about one event.
fn noise(key: &str) -> bool {
    const NOISE: &[&str] = &[
        "$session_id",
        "$window_id",
        "$insert_id",
        "$time",
        "$lib",
        "$lib_version",
        "$ip",
        "$device_id",
        "$groups",
        "$sent_at",
        "$active_feature_flags",
        "token",
        "distinct_id",
    ];
    NOISE.contains(&key)
        || key.starts_with("$set")
        || key.starts_with("$feature/")
        || key.starts_with("$geoip_")
        || key.starts_with("$initial_")
}

/// The `n` properties most of these events carry: the table's last columns.
/// Among those equally common, ones whose values differ from event to event
/// say more, but not ones unique to each event (ids, durations).
pub fn key_columns(events: &[Event], n: usize) -> Vec<String> {
    let mut seen: Vec<(String, usize, Vec<String>)> = Vec::new();
    for e in events {
        for (k, v) in e.props.iter().filter(|(k, _)| !noise(k)) {
            let v = show(v);
            match seen.iter_mut().find(|(s, _, _)| s == k) {
                Some(s) => {
                    s.1 += 1;
                    if !s.2.contains(&v) {
                        s.2.push(v);
                    }
                }
                None => seen.push((k.clone(), 1, vec![v])),
            }
        }
    }
    let unique = |count: usize, distinct: usize| distinct > (count * 3 / 4).max(3);
    seen.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| unique(a.1, a.2.len()).cmp(&unique(b.1, b.2.len())))
            .then_with(|| b.2.len().cmp(&a.2.len()))
            .then_with(|| a.0.starts_with('$').cmp(&b.0.starts_with('$')))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut keys: Vec<String> = seen.into_iter().take(n).map(|s| s.0).collect();
    // Own properties first, then PostHog's, so `agent` sits before `$app_version`.
    keys.sort_by(|a, b| a.starts_with('$').cmp(&b.starts_with('$')).then(a.cmp(b)));
    keys
}

/// A value as one line of text.
pub fn show(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_u64() {
            Some(i) if i >= 10_000 => group_digits(i),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

/// 41208 → "41,208".
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Property rows: (key, value as text).
pub type Rows = Vec<(String, String)>;

/// The drawer's groups: what the event says, and what PostHog added about
/// the session and device. Each sorted by key.
pub fn groups(e: &Event) -> (Rows, Rows) {
    let mut own = Vec::new();
    let mut session = Vec::new();
    for (k, v) in &e.props {
        if k.starts_with("$set") || k.starts_with("$feature/") || k == "$active_feature_flags" {
            continue;
        }
        let row = (k.clone(), show(v));
        if k.starts_with('$') || k == "token" {
            session.push(row);
        } else {
            own.push(row);
        }
    }
    own.sort();
    // The session id first: it is what Replay opens.
    session.sort_by(|a, b| {
        (a.0 != "$session_id")
            .cmp(&(b.0 != "$session_id"))
            .then_with(|| a.0.cmp(&b.0))
    });
    (own, session)
}

pub fn person_rows(e: &Event) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = e
        .person_props
        .iter()
        .filter(|(k, _)| !k.starts_with("$initial_") && !k.starts_with("$geoip_"))
        .map(|(k, v)| (k.clone(), show(v)))
        .collect();
    rows.sort_by(|a, b| (a.0 != "email").cmp(&(b.0 != "email")).then(a.0.cmp(&b.0)));
    rows
}

/// "Copy as JSON": the event as PostHog would list it.
pub fn to_json(e: &Event) -> String {
    let v = serde_json::json!({
        "uuid": e.uuid,
        "event": e.event,
        "timestamp": e.time,
        "distinct_id": e.distinct_id,
        "person_id": e.person_id,
        "properties": e.props,
        "person": { "properties": e.person_props },
    });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

/// The replay of a session in PostHog.
pub fn replay_path(session: &str) -> String {
    format!("/replay/{}", super::encode(session))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> Filter {
        Filter {
            event: Some("agent turn finished".into()),
            props: vec![PropFilter {
                key: "os') OR 1=1 --".into(),
                value: "macOS' OR '1'='1".into(),
            }],
            person: " reader@example.test ".into(),
            range: Range::Day,
            until: None,
        }
    }

    #[test]
    fn filters_are_bound_not_written() {
        let q = list_query(&filter(), Paging::first(50), None);
        assert_eq!(
            q.text,
            "SELECT uuid, event, timestamp, distinct_id, person_id, person.properties.email, properties, person.properties FROM events \
             WHERE timestamp > now() - INTERVAL 86400 SECOND AND timestamp <= now() AND event = {v0} \
             AND replaceRegexpAll(JSONExtractRaw(properties, {v1}), '^\"|\"$', '') = {v2} \
             AND (distinct_id = {v3} OR person.properties.email = {v3}) \
             ORDER BY timestamp DESC, uuid DESC LIMIT 51"
        );
        assert!(!q.text.contains("OR 1=1") && !q.text.contains("macOS"));
        assert_eq!(q.values["v0"], "agent turn finished");
        assert_eq!(q.values["v1"], "os') OR 1=1 --");
        assert_eq!(q.values["v2"], "macOS' OR '1'='1");
        assert_eq!(q.values["v3"], "reader@example.test");
        // The next page continues from the last row, not a live offset.
        let mut f = filter();
        f.until = Some(1_700_000_000);
        let next = list_query(
            &f,
            Paging::first(50),
            Some(("2026-01-01 12:00:00.123", "u-9")),
        );
        assert!(
            next.text
                .contains("AND (timestamp < {v4} OR (timestamp = {v4} AND uuid < {v5}))")
        );
        assert!(next.text.contains("fromUnixTimestamp(1700000000)"));
        assert!(next.text.ends_with("LIMIT 51"));
        assert_eq!(next.values["v4"], "2026-01-01 12:00:00.123");
        assert_eq!(next.values["v5"], "u-9");
    }

    #[test]
    fn everything_without_filters() {
        let f = Filter {
            range: Range::Hour,
            ..Default::default()
        };
        let q = volume_query(&f);
        assert_eq!(
            q.text,
            "SELECT intDiv(dateDiff('second', timestamp, now()), 300) AS ago, count() FROM events \
             WHERE timestamp > now() - INTERVAL 3600 SECOND AND timestamp <= now() GROUP BY ago ORDER BY ago"
        );
        assert!(q.values.is_empty());
        for r in Range::ALL {
            assert_eq!(r.bucket_seconds() * r.buckets(), r.seconds());
        }
    }

    #[test]
    fn volume_fills_every_bar() {
        let rows: Vec<Value> = serde_json::from_str("[[0, 5], [3, \"7\"], [99, 1]]").unwrap();
        let v = parse_volume(&rows, Range::Day);
        assert_eq!(v.len(), 24);
        assert_eq!((v[23], v[20], v[0]), (5, 7, 1));
        assert_eq!(v.iter().sum::<u64>(), 13);
    }

    fn fixture() -> Vec<Event> {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[
              ["u1", "agent turn finished", "2026-11-18T14:01:47Z", "rik-mac", "p1", "reader@example.test",
               "{\"agent\":\"claude-code\",\"model\":\"opus-5.5\",\"turn_ms\":41208,\"$app_version\":\"0.6.0\",\"$session_id\":\"s1\",\"$lib\":\"posthog-rs\",\"$set\":{\"plan\":\"pro\"},\"$feature/new-composer\":\"inline-tools\"}",
               {"email":"reader@example.test","plan":"pro","$initial_os":"macOS"}],
              ["u2", "agent turn finished", "2026-11-18T14:01:40Z", "4f2c91e0", "p2", null,
               {"agent":"claude-code","model":"haiku-4.5","$app_version":"0.5.9"}, "{}"],
              [null, null, null, null, null, null, null, null]
            ]"#,
        )
        .unwrap();
        parse_rows(&rows)
    }

    #[test]
    fn parses_events_with_json_text_or_objects() {
        let e = fixture();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].who(), "reader@example.test");
        assert_eq!(e[1].who(), "4f2c91e0");
        assert_eq!(e[0].session(), Some("s1"));
        assert_eq!(e[1].session(), None);
        assert_eq!(e[0].props["turn_ms"], 41208);
        assert_eq!(e[0].time, parse_iso("2026-11-18T14:01:47Z").unwrap());
        assert_eq!(key_columns(&e, 3), vec!["agent", "model", "$app_version"]);
        // Values unique to each event (durations, ids) lose to ones that repeat.
        let many: Vec<Event> = (0..8)
            .map(|i| Event {
                props: serde_json::from_value(serde_json::json!({
                    "turn_ms": i * 100, "model": if i % 2 == 0 { "a" } else { "b" }, "agent": "x", "mode": "auto"
                }))
                .unwrap(),
                ..Default::default()
            })
            .collect();
        assert_eq!(key_columns(&many, 2), vec!["agent", "model"]);
    }

    #[test]
    fn drawer_groups_and_json() {
        let e = &fixture()[0];
        let (own, session) = groups(e);
        assert_eq!(
            own,
            vec![
                ("agent".to_string(), "claude-code".to_string()),
                ("model".into(), "opus-5.5".into()),
                ("turn_ms".into(), "41,208".into()),
            ]
        );
        let keys: Vec<&str> = session.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(keys, vec!["$session_id", "$app_version", "$lib"]);
        let person = person_rows(e);
        assert_eq!(person[0], ("email".into(), "reader@example.test".into()));
        assert_eq!(person.len(), 2);
        let json: Value = serde_json::from_str(&to_json(e)).unwrap();
        assert_eq!(json["properties"]["model"], "opus-5.5");
        assert_eq!(json["person"]["properties"]["plan"], "pro");
        assert_eq!(replay_path("01936f1c-04aa"), "/replay/01936f1c-04aa");
        assert_eq!(replay_path("a/b"), "/replay/a%2Fb");
        assert_eq!(group_digits(1_234_567), "1,234,567");
        assert_eq!(group_digits(999), "999");
    }
}
