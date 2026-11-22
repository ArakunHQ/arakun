//! What an error or an experiment shows once opened: read then, not on every
//! overview refresh. An error gets its latest stack trace and replay; an
//! experiment, each metric's result per variant (at most four per kind), and
//! what may be done to it.

use super::{
    ApiError, Session,
    query::{self, Hogql},
};
use serde_json::{Value, json};

#[derive(Default)]
pub struct Inspection {
    pub sections: Vec<(String, Vec<String>)>,
    pub replay: Option<String>,
    pub experiment: Option<super::lifecycle::ReviewedExperiment>,
    pub winners: Vec<String>,
}

pub fn issue(s: &Session, p: u64, id: &str) -> Result<Inspection, ApiError> {
    let mut q = Hogql::new(
        "SELECT properties.$exception_list, properties.$session_id, properties.$browser, properties.$os, properties.$app_version FROM events WHERE event = '$exception' AND timestamp > now() - INTERVAL 7 DAY AND properties.$exception_issue_id = ",
    );
    let bound = q.bind(id);
    q.push(&bound);
    q.push(" ORDER BY timestamp DESC LIMIT 1");
    let rows = query::run(s, p, &q, query::FRESH)?.rows;
    let mut out = Inspection::default();
    let Some(row) = rows.first() else {
        return Ok(out);
    };
    let list = match &row[0] {
        Value::String(text) => serde_json::from_str(text).unwrap_or_default(),
        other => other.clone(),
    };
    let frames = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|exc| exc["stacktrace"]["frames"].as_array())
        .flatten()
        .take(24)
        .map(|f| {
            let name = f["function"].as_str().unwrap_or("(anonymous)");
            let file = f["filename"].as_str().unwrap_or("(unknown file)");
            match f["lineno"].as_u64() {
                Some(n) => format!("{name}   {file}:{n}"),
                None => format!("{name}   {file}"),
            }
        })
        .collect::<Vec<_>>();
    if !frames.is_empty() {
        out.sections.push(("Stack trace".into(), frames));
    }
    let props = [("Browser", 2), ("OS", 3), ("Release", 4)]
        .into_iter()
        .filter_map(|(k, i)| row[i].as_str().map(|v| format!("{k}: {v}")))
        .collect::<Vec<_>>();
    if !props.is_empty() {
        out.sections.push(("Latest occurrence".into(), props));
    }
    out.replay = row[1]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| format!("/replay/{}", super::encode(s)));
    Ok(out)
}

fn valid(stats: &Value) -> bool {
    stats["number_of_samples"].as_u64().is_some_and(|n| n > 0)
        && stats["validation_failures"]
            .as_array()
            .is_none_or(Vec::is_empty)
}

fn variants(v: &Value) -> Vec<String> {
    let baseline = &v["baseline"];
    let mut lines = vec![];
    if let Some(n) = baseline["number_of_samples"].as_u64() {
        lines.push(format!(
            "{} · {n} samples",
            baseline["key"].as_str().unwrap_or("Control")
        ));
    }
    for variant in v["variant_results"].as_array().into_iter().flatten() {
        let key = variant["key"].as_str().unwrap_or("Variant");
        let samples = variant["number_of_samples"].as_u64().unwrap_or(0);
        let status = if !valid(baseline) || !valid(variant) {
            "Not enough data".into()
        } else if let Some(chance) = variant["chance_to_win"]
            .as_f64()
            .filter(|c| (0.0..=1.).contains(c))
        {
            let lead = if variant["significant"] == true && chance > 0.5 {
                "Winning"
            } else {
                "Chance to win"
            };
            format!("{lead}: {:.1}%", chance * 100.)
        } else if let Some(p) = variant["p_value"].as_f64() {
            format!(
                "p = {p:.4} · {}",
                if variant["significant"] == true {
                    "significant difference"
                } else {
                    "no significant difference"
                }
            )
        } else {
            "No statistical result yet".into()
        };
        lines.push(format!("{key} · {samples} samples · {status}"));
    }
    if v["variant_results"].as_array().is_none_or(Vec::is_empty) {
        lines.push("Not enough data for a variant result.".into());
    }
    lines
}

/// Ship is only offered when one primary metric decides: never on a
/// secondary metric, nor while another primary one is left to weigh.
fn sole_primary(e: &Value) -> bool {
    e["metrics"].as_array().is_some_and(|m| m.len() == 1)
        && e["saved_metrics"].as_array().is_none_or(Vec::is_empty)
}

/// The variants a metric's result calls significant winners, each with
/// enough valid data on both sides.
fn winners(result: &Value) -> Vec<String> {
    if !valid(&result["baseline"]) {
        return vec![];
    }
    result["variant_results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| {
            valid(v)
                && v["significant"] == true
                && v["chance_to_win"]
                    .as_f64()
                    .is_some_and(|p| p > 0.5 && p <= 1.)
        })
        .filter_map(|v| v["key"].as_str().map(str::to_owned))
        .collect()
}

pub fn experiment(s: &Session, p: u64, id: u64) -> Result<Inspection, ApiError> {
    let e = super::call(
        s,
        "GET",
        &format!("/api/projects/{p}/experiments/{id}/"),
        None,
    )?;
    let mut out = Inspection {
        experiment: super::lifecycle::ReviewedExperiment::read(&e),
        ..Default::default()
    };
    if !e["start_date"].is_string() {
        out.sections.push((
            "Results".into(),
            vec!["The experiment has not started.".into()],
        ));
        return Ok(out);
    }
    for (slot, field) in [("Primary", "metrics"), ("Secondary", "metrics_secondary")] {
        let metrics = e[field].as_array().into_iter().flatten().chain(
            e["saved_metrics"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| {
                    m["metadata"]["type"]
                        .as_str()
                        .is_some_and(|v| v.eq_ignore_ascii_case(slot))
                })
                .map(|m| &m["query"]),
        );
        for (i, m) in metrics.enumerate() {
            if i >= 4 {
                out.sections.push((
                    format!("More {slot} metrics"),
                    vec!["Open in PostHog to see the remaining metrics.".into()],
                ));
                break;
            }
            let label = m["name"]
                .as_str()
                .or(m["metric_type"].as_str())
                .unwrap_or("metric");
            let body = json!({"query":{"kind":"ExperimentQuery","experiment_id":id,"metric":m}});
            let lines = super::call(
                s,
                "POST",
                &format!("/api/environments/{p}/query/"),
                Some(&body),
            )
            .map(|v| {
                if slot == "Primary" && sole_primary(&e) {
                    out.winners = winners(&v);
                }
                variants(&v)
            })
            .unwrap_or_else(|e| vec![e.message()]);
            out.sections.push((format!("{slot} · {label}"), lines));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_failed_validation_or_an_insignificant_variant_is_never_a_winner() {
        let mut result = json!({"baseline":{"number_of_samples":100},"variant_results":[{"key":"treatment","number_of_samples":110,"chance_to_win":0.96,"significant":true}]});
        assert!(variants(&result)[1].contains("Winning: 96.0%"));
        result["variant_results"][0]["significant"] = json!(false);
        assert!(!variants(&result)[1].contains("Winning"));
        result["baseline"]["validation_failures"] = json!(["small_sample_size"]);
        assert!(variants(&result)[1].contains("Not enough data"));
    }

    #[test]
    fn ship_is_offered_only_for_a_significant_winner_of_the_sole_primary_metric() {
        let mut result = json!({
            "baseline": { "key": "control", "number_of_samples": 100 },
            "variant_results": [
                { "key": "a", "number_of_samples": 90, "chance_to_win": 0.97, "significant": true },
                { "key": "b", "number_of_samples": 90, "chance_to_win": 0.40, "significant": true },
                { "key": "c", "number_of_samples": 90, "chance_to_win": 0.99, "significant": false }
            ]
        });
        assert_eq!(winners(&result), ["a"]);
        result["variant_results"][0]["validation_failures"] = json!(["not_enough_exposures"]);
        assert!(winners(&result).is_empty());
        result["variant_results"][0]["validation_failures"] = json!([]);
        result["baseline"]["number_of_samples"] = json!(0);
        assert!(winners(&result).is_empty());

        let one = json!({ "metrics": [{}], "saved_metrics": [] });
        assert!(sole_primary(&one));
        assert!(!sole_primary(&json!({ "metrics": [{}, {}] })));
        assert!(!sole_primary(
            &json!({ "metrics": [{}], "saved_metrics": [{ "metadata": { "type": "primary" } }] })
        ));
    }
}
