//! Ending an experiment, or shipping one of its variants. Ending freezes the
//! results window and leaves the flag alone. Shipping also gives the variant
//! to everyone the flag already matches; it never widens the audience or has
//! PostHog open a cleanup pull request.

use super::{ApiError, Project, Session};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    End,
    Ship(String),
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Self::End => "End experiment".into(),
            Self::Ship(key) => format!("Ship {key}"),
        }
    }
    pub fn effect(&self) -> String {
        match self {
            Self::End => {
                "Freeze the results window. Existing flag assignments do not change.".into()
            }
            Self::Ship(key) => format!(
                "End the experiment and give {key} to all users who already match the flag. No new audience is added."
            ),
        }
    }
    /// What the tab says once PostHog took it.
    pub fn done(&self) -> String {
        match self {
            Self::End => "Ended the experiment.".into(),
            Self::Ship(key) => format!("Shipped {key} and ended the experiment."),
        }
    }
    fn request(&self) -> (&'static str, Value) {
        match self {
            Self::End => ("end", json!({"open_cleanup_pr":false})),
            Self::Ship(key) => (
                "ship_variant",
                json!({"variant_key":key,"release_to_everyone":false,"open_cleanup_pr":false}),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewedExperiment {
    pub id: u64,
    pub flag: String,
    pub version: u64,
    pub variants: Vec<String>,
}
impl ReviewedExperiment {
    pub fn read(v: &Value) -> Option<Self> {
        if !v["start_date"].is_string() || !v["end_date"].is_null() || v["archived"] == true {
            return None;
        }
        let flag = v["feature_flag_key"].as_str()?.to_string();
        if flag.is_empty() {
            return None;
        }
        Some(Self {
            id: v["id"].as_u64()?,
            flag,
            version: v["version"].as_u64()?,
            variants: v["parameters"]["feature_flag_variants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["key"].as_str().map(str::to_owned))
                .collect(),
        })
    }
    pub fn accepts(&self, action: &Action, typed: &str) -> bool {
        typed == self.flag
            && match action {
                Action::End => true,
                Action::Ship(key) => self.variants.contains(key),
            }
    }
}

pub fn apply(
    s: &Session,
    p: &Project,
    review: &ReviewedExperiment,
    action: &Action,
    typed: &str,
) -> Result<(), ApiError> {
    if p.session != s.id || p.host != s.host || !review.accepts(action, typed) {
        return Err(ApiError::Failed(
            "Confirmation does not match this experiment.".into(),
        ));
    }
    let base = format!("/api/projects/{}/experiments/{}/", p.id, review.id);
    let live = super::call(s, "GET", &base, None)?;
    if ReviewedExperiment::read(&live).as_ref() != Some(review) {
        return Err(ApiError::Failed(
            "This experiment changed. Reopen it and review the change again.".into(),
        ));
    }
    // The results on screen can be old: a variant ships only while
    // PostHog still calls it a winner.
    if let Action::Ship(key) = action
        && !super::inspection::winners_now(s, p.id, &live)?.contains(key)
    {
        return Err(ApiError::Failed(format!(
            "The results no longer call {key} a winner. Nothing was changed."
        )));
    }
    let (path, body) = action.request();
    super::call(s, "POST", &format!("{base}{path}/"), Some(&body))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shipping_preserves_targeting_and_requires_the_reviewed_key_and_variant() {
        let e = ReviewedExperiment {
            id: 1,
            version: 4,
            flag: "test-flag".into(),
            variants: vec!["control".into(), "variant".into()],
        };
        let a = Action::Ship("variant".into());
        assert!(e.accepts(&a, "test-flag"));
        assert!(!e.accepts(&a, "another"));
        assert!(!e.accepts(&Action::Ship("stale".into()), "test-flag"));
        let (path, body) = a.request();
        assert_eq!(path, "ship_variant");
        assert_eq!(body["variant_key"], "variant");
        assert_eq!(body["release_to_everyone"], false);
        assert_eq!(body["open_cleanup_pr"], false);
        let (path, body) = Action::End.request();
        assert_eq!((path, body), ("end", json!({ "open_cleanup_pr": false })));
    }

    #[test]
    fn only_a_running_experiment_with_a_flag_can_be_changed() {
        let running = json!({
            "id": 7, "version": 3, "feature_flag_key": "checkout",
            "start_date": "2026-11-01T00:00:00Z", "end_date": null, "archived": false,
            "parameters": { "feature_flag_variants": [{ "key": "control" }, { "key": "test" }] }
        });
        let e = ReviewedExperiment::read(&running).unwrap();
        assert_eq!((e.id, e.version, e.flag.as_str()), (7, 3, "checkout"));
        assert_eq!(e.variants, ["control", "test"]);
        for (field, value) in [
            ("start_date", Value::Null),
            ("end_date", json!("2026-11-10T00:00:00Z")),
            ("archived", json!(true)),
            ("feature_flag_key", json!("")),
            ("version", Value::Null),
        ] {
            let mut v = running.clone();
            v[field] = value;
            assert_eq!(ReviewedExperiment::read(&v), None, "{field}");
        }
    }
}
