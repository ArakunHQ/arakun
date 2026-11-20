//! The event and property names of a project, for the Events tab's pickers:
//! PostHog's definitions, with each event's volume over the last 24 hours.
//! They change slowly, so one read is kept on disk for `STALE`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::query::{self, Hogql};
use super::{ApiError, Project, Session, num, parse_iso};

/// Definitions are read again after this long.
pub const STALE: u64 = 6 * 3600;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventDef {
    pub name: String,
    /// Events in the last 24 hours, when the volume query answered.
    pub volume: Option<u64>,
    /// Unix seconds.
    pub last_seen: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Defs {
    pub project: u64,
    /// Busiest first.
    pub events: Vec<EventDef>,
    pub props: Vec<String>,
    pub fetched_at: u64,
    /// Scopes a part of the read was refused for.
    pub missing: Vec<String>,
}

fn file(p: &Project) -> PathBuf {
    let host: String = p
        .host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    super::cache_dir()
        .join("posthog")
        .join(format!("{host}-{}-defs.json", p.id))
}

pub fn cached(p: &Project) -> Option<Defs> {
    serde_json::from_slice(&std::fs::read(file(p)).ok()?).ok()
}

/// `results` of `/event_definitions/`.
pub fn parse_events(v: &Value) -> Vec<EventDef> {
    v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some(EventDef {
                name: d["name"].as_str().filter(|n| !n.is_empty())?.to_string(),
                volume: None,
                last_seen: d["last_seen_at"].as_str().and_then(parse_iso),
            })
        })
        .collect()
}

/// `results` of `/property_definitions/`, as names.
pub fn parse_props(v: &Value) -> Vec<String> {
    v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d["name"].as_str().filter(|n| !n.is_empty()))
        .map(str::to_string)
        .collect()
}

const VOLUME: &str = "SELECT event, count(), max(timestamp) FROM events \
    WHERE timestamp > now() - INTERVAL 24 HOUR GROUP BY event ORDER BY count() DESC LIMIT 500";

/// Adds the 24-hour volume rows to `defs` (events the definitions missed
/// too), busiest first, then by name.
pub fn merge_volume(mut defs: Vec<EventDef>, rows: &[Value]) -> Vec<EventDef> {
    for r in rows {
        let Some(name) = r[0].as_str() else { continue };
        let seen = r[2].as_str().and_then(parse_iso);
        match defs.iter_mut().find(|d| d.name == name) {
            Some(d) => {
                d.volume = Some(num(&r[1]));
                d.last_seen = d.last_seen.max(seen);
            }
            None => defs.push(EventDef {
                name: name.to_string(),
                volume: Some(num(&r[1])),
                last_seen: seen,
            }),
        }
    }
    defs.sort_by(|a, b| {
        b.volume
            .unwrap_or(0)
            .cmp(&a.volume.unwrap_or(0))
            .then_with(|| a.name.cmp(&b.name))
    });
    defs
}

/// Reads the definitions and the volumes in parallel and keeps them. A part
/// that is refused leaves its scope in `missing`; the rest still shows.
pub fn load(s: &Session, p: &Project) -> Result<Defs, ApiError> {
    let id = p.id;
    let (events, props, volume) = std::thread::scope(|scope| {
        let e = scope.spawn(|| {
            super::call(
                s,
                "GET",
                &format!("/api/projects/{id}/event_definitions/?limit=500"),
                None,
            )
        });
        let pr = scope.spawn(|| {
            super::call(
                s,
                "GET",
                &format!("/api/projects/{id}/property_definitions/?type=event&limit=500"),
                None,
            )
        });
        let v = scope.spawn(|| query::run(s, id, &Hogql::new(VOLUME), STALE));
        (super::joined(e), super::joined(pr), super::joined(v))
    });
    let mut missing = Vec::new();
    let mut first_error = None;
    let mut keep = |e: ApiError| {
        if let ApiError::MissingScope(scope) = &e {
            missing.push(scope.clone());
        }
        first_error.get_or_insert(e);
    };
    let defs = events.map(|v| parse_events(&v)).unwrap_or_else(|e| {
        keep(e);
        Vec::new()
    });
    let props = props.map(|v| parse_props(&v)).unwrap_or_else(|e| {
        keep(e);
        Vec::new()
    });
    let rows = volume.map(|a| a.rows).unwrap_or_else(|e| {
        keep(e);
        Vec::new()
    });
    if defs.is_empty()
        && rows.is_empty()
        && let Some(e) = first_error
    {
        return Err(e);
    }
    let d = Defs {
        project: id,
        events: merge_volume(defs, &rows),
        props,
        fetched_at: crate::model::unix_now(),
        missing,
    };
    super::write_json(&file(p), &d);
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_definitions_and_volumes() {
        let v: Value = serde_json::from_str(
            r#"{"count":3,"next":null,"results":[
                {"id":"a","name":"session started","volume_30_day":null,"last_seen_at":"2026-11-18T13:59:00Z"},
                {"id":"b","name":"phone paired","last_seen_at":null},
                {"id":"c","name":""}
            ]}"#,
        )
        .unwrap();
        let defs = parse_events(&v);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].last_seen, parse_iso("2026-11-18T13:59:00Z"));
        assert_eq!(defs[1].last_seen, None);

        let rows: Vec<Value> = serde_json::from_str(
            r#"[["agent turn finished", 8412, "2026-11-18T14:02:31Z"],
                ["session started", "1906", "2026-11-18T14:01:00Z"]]"#,
        )
        .unwrap();
        let merged = merge_volume(defs, &rows);
        let names: Vec<(&str, Option<u64>)> =
            merged.iter().map(|d| (d.name.as_str(), d.volume)).collect();
        assert_eq!(
            names,
            vec![
                ("agent turn finished", Some(8412)),
                ("session started", Some(1906)),
                ("phone paired", None),
            ]
        );
        // The newer of the two last-seen times wins.
        assert_eq!(merged[1].last_seen, parse_iso("2026-11-18T14:01:00Z"));

        let v: Value = serde_json::from_str(
            r#"{"results":[{"name":"$os","is_numerical":false},{"name":"agent"},{"nope":1}]}"#,
        )
        .unwrap();
        assert_eq!(parse_props(&v), vec!["$os", "agent"]);
    }
}
