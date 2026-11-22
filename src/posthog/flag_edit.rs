//! Changing a feature flag: a versioned patch of the one field the person
//! reviewed. Nothing here runs without the flag key typed to confirm it.

use super::{ApiError, Flag, Project, Session};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
pub enum Change {
    Active(bool),
    Rollout(usize, u64),
}

/// How many release conditions can be changed here: none for a flag in
/// PostHog's newer rule format, which this does not write.
pub fn conditions(flag: &Flag) -> usize {
    if flag.filters.get("version").is_some() {
        return 0;
    }
    flag.filters["groups"].as_array().map_or(0, Vec::len)
}

/// The rollout of condition `i`. A condition without one gets everyone it matches.
pub fn rollout(flag: &Flag, i: usize) -> Option<f64> {
    let group = flag.filters["groups"].get(i)?;
    Some(group["rollout_percentage"].as_f64().unwrap_or(100.))
}

/// The body that makes `change` to `flag`, or None when `typed` is not the
/// flag's key or the change is not one this can make. The reviewed version
/// and the fields as they were go along, so PostHog refuses the change when
/// someone else changed those fields since.
pub fn patch(flag: &Flag, change: Change, typed: &str) -> Option<Value> {
    if flag.key.is_empty() || typed != flag.key {
        return None;
    }
    let mut body = json!({"version":flag.version?});
    match change {
        Change::Active(active) if flag.active != active => {
            body["active"] = json!(active);
            body["original_flag"] = json!({"active":flag.active});
        }
        Change::Rollout(i, pct) if pct <= 100 && i < conditions(flag) => {
            let mut filters = flag.filters.clone();
            let group = filters
                .get_mut("groups")?
                .as_array_mut()?
                .get_mut(i)?
                .as_object_mut()?;
            let before = group
                .get("rollout_percentage")
                .and_then(Value::as_f64)
                .unwrap_or(100.);
            if before == pct as f64 {
                return None;
            }
            group.insert("rollout_percentage".into(), json!(pct));
            body["filters"] = filters;
            body["original_flag"] = json!({"filters":flag.filters});
        }
        _ => return None,
    }
    Some(body)
}

pub fn apply(
    s: &Session,
    p: &Project,
    flag: &Flag,
    change: Change,
    typed: &str,
) -> Result<(), ApiError> {
    if s.id != p.session || s.host != p.host {
        return Err(ApiError::Failed(
            "Account changed. Review the flag again.".into(),
        ));
    }
    let body = patch(flag, change, typed)
        .ok_or_else(|| ApiError::Failed("Flag confirmation is no longer valid.".into()))?;
    super::call(
        s,
        "PATCH",
        &format!("/api/projects/{}/feature_flags/{}/", p.id, flag.id),
        Some(&body),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rollout_preserves_other_conditions_and_binds_the_reviewed_version() {
        let f = Flag {
            id: 1,
            key: "editor".into(),
            name: String::new(),
            active: true,
            rollout: String::new(),
            version: Some(9),
            filters: json!({"groups":[{"properties":[{"key":"plan"}],"rollout_percentage":30},{"rollout_percentage":70}],"multivariate":{"variants":[{"key":"a"}]}}),
        };
        assert!(patch(&f, Change::Rollout(0, 50), "other").is_none());
        assert!(patch(&f, Change::Rollout(1, 101), "editor").is_none());
        let got = patch(&f, Change::Rollout(0, 50), "editor").unwrap();
        assert_eq!(got["original_flag"]["filters"], f.filters);
        let mut expected = f.filters.clone();
        expected["groups"][0]["rollout_percentage"] = json!(50);
        assert_eq!(got["filters"], expected);
        assert_eq!(got["version"], 9);
        assert!(
            patch(
                &Flag { version: None, ..f },
                Change::Active(false),
                "editor"
            )
            .is_none()
        );
    }

    #[test]
    fn a_toggle_sends_only_active_and_a_newer_rule_format_is_left_alone() {
        let f = Flag {
            id: 1,
            key: "editor".into(),
            name: String::new(),
            active: true,
            rollout: String::new(),
            version: Some(2),
            filters: json!({ "groups": [{ "rollout_percentage": null }] }),
        };
        let got = patch(&f, Change::Active(false), "editor").unwrap();
        assert_eq!(
            got,
            json!({ "version": 2, "active": false, "original_flag": { "active": true } })
        );
        // Already in that state: nothing to send.
        assert!(patch(&f, Change::Active(true), "editor").is_none());
        assert_eq!(rollout(&f, 0), Some(100.));
        assert!(patch(&f, Change::Rollout(0, 100), "editor").is_none());
        assert!(patch(&f, Change::Rollout(1, 50), "editor").is_none());
        let v2 = Flag {
            filters: json!({ "version": 2, "groups": [{ "rollout_percentage": 10 }] }),
            ..f
        };
        assert_eq!(conditions(&v2), 0);
        assert!(patch(&v2, Change::Rollout(0, 20), "editor").is_none());
    }
}
