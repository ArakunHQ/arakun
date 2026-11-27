//! Turns a survey and its answers into the PostHog `survey sent` event, and
//! sends it under the feedback consent rules (`telemetry::feedback_allowed`).
//!
//! Free-text answers never leave as typed. They go twice: under PostHog's own
//! name with the secrets the shared detector finds taken out, for the team,
//! and scrubbed harder (`_scrubbed`, no paths, links or emails either) for
//! anything that may be shown in public. The optional email of the user survey
//! has no `_scrubbed` copy, since a scrubbed email is nothing.

use serde_json::{Map, Value, json};

use super::ids;
use super::linear;
use super::model::{Answer, Body, Question, Survey, kind_of};
use crate::telemetry::{self, Refused, Source};

/// Whether the answer to `q` must not get a scrubbed copy.
fn is_email(s: &Survey, q: &Question) -> bool {
    s.id == ids::USER.survey_id && q.id == ids::USER.questions[2].id
}

/// A free-text answer: open text, or the text typed into an open choice.
fn free_text<'a>(q: &Question, a: &'a Answer) -> Option<&'a str> {
    match (&q.body, a) {
        (Body::Open, Answer::Text(t)) => Some(t),
        (
            Body::Single {
                choices,
                open_choice: true,
            },
            Answer::Choice(c),
        ) if !choices.contains(c) => Some(c),
        _ => None,
    }
}

pub fn response_props(
    s: &Survey,
    answers: &[(usize, Answer)],
    src: Source,
    extra: &[(&str, Value)],
) -> Map<String, Value> {
    let home = telemetry::home();
    let mut m = Map::new();
    m.insert("$survey_id".into(), s.id.clone().into());
    m.insert("$survey_name".into(), s.name.clone().into());
    m.insert(
        "$survey_questions".into(),
        s.questions
            .iter()
            .map(|q| json!({"id": q.id, "question": q.text}))
            .collect(),
    );
    m.insert("$survey_completed".into(), true.into());
    m.insert("source".into(), src.as_str().into());

    let mut kind = None;
    let kind_q = ids::builtin(&s.id).and_then(|b| b.kind_question);
    for (i, a) in answers {
        let Some(q) = s.questions.get(*i) else {
            continue;
        };
        let key = format!("$survey_response_{}", q.id);
        // A pasted chat may hold a key; PostHog's results read this name.
        let value = match a {
            Answer::Text(t) => Value::from(crate::secrets::scrub(t)),
            Answer::Rating(n) => Value::from(*n),
            Answer::Choice(c) => Value::from(crate::secrets::scrub(c)),
            Answer::Choices(c) => Value::from(c.clone()),
            Answer::Skipped => continue,
        };
        if let Some(text) = free_text(q, a)
            && !is_email(s, q)
        {
            m.insert(
                format!("{key}_scrubbed"),
                telemetry::scrub(text, &home).into(),
            );
        }
        if Some(q.id.as_str()) == kind_q {
            kind = kind_of(q, a);
        }
        m.insert(key, value);
    }
    if src == Source::Error {
        m.insert("kind".into(), "bug".into());
    } else if let Some(k) = kind {
        m.insert("kind".into(), k.as_str().into());
    }
    // The survey decides first: extra only fills what is still missing.
    for (k, v) in extra {
        m.entry(k.to_string()).or_insert_with(|| v.clone());
    }
    m
}

pub fn submit(
    s: &Survey,
    answers: &[(usize, Answer)],
    src: Source,
    extra: &[(&str, Value)],
) -> Result<(), Refused> {
    let props = response_props(s, answers, src, extra);
    telemetry::send_feedback(telemetry::events::SURVEY_SENT, src, props.clone())?;
    // Only once consent passed, so it follows the event's rules. Product
    // feedback goes to Linear too; a support question, or a survey with no
    // kind, stays in the event alone.
    linear::open(s, &props);
    Ok(())
}

/// A popover survey was shown. Counted as usage, so it needs that consent.
pub fn shown(s: &Survey) {
    telemetry::capture(
        "survey shown",
        &[("$survey_id", &s.id), ("$survey_name", &s.name)],
    );
}

pub fn dismissed(s: &Survey) {
    telemetry::capture(
        "survey dismissed",
        &[("$survey_id", &s.id), ("$survey_name", &s.name)],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surveys::ids::Builtin;
    use crate::surveys::model::{Branching, Conditions, SurveyType};

    fn survey(b: &'static Builtin) -> Survey {
        let questions = b
            .questions
            .iter()
            .map(|f| Question {
                id: f.id.into(),
                text: f.text.into(),
                description: String::new(),
                optional: f.optional,
                button_text: None,
                body: match &f.body {
                    ids::FallbackBody::Open => Body::Open,
                    ids::FallbackBody::SingleChoice { choices } => Body::Single {
                        choices: choices.iter().map(|c| c.to_string()).collect(),
                        open_choice: false,
                    },
                    ids::FallbackBody::Rating { scale } => Body::Rating {
                        scale: *scale,
                        emoji: false,
                        lower: String::new(),
                        upper: String::new(),
                    },
                },
                branching: Branching::Next,
            })
            .collect();
        Survey {
            id: b.survey_id.into(),
            name: "Feedback".into(),
            kind: SurveyType::Api,
            questions,
            start_date: None,
            end_date: None,
            archived: false,
            conditions: Conditions::default(),
        }
    }

    const SECRET: &str = "see https://x.example/a and mail ann@example.com";

    fn user_answers() -> Vec<(usize, Answer)> {
        vec![
            (0, Answer::Choice("Feature request".into())),
            (1, Answer::Text(SECRET.into())),
            (2, Answer::Text("ann@example.com".into())),
        ]
    }

    #[test]
    fn props_have_posthog_keys() {
        let s = survey(&ids::USER);
        let p = response_props(&s, &user_answers(), Source::User, &[]);
        assert_eq!(p["$survey_id"], ids::USER.survey_id);
        assert_eq!(p["$survey_name"], "Feedback");
        assert_eq!(p["$survey_completed"], true);
        assert_eq!(p["source"], "user");
        assert_eq!(p["kind"], "feature");
        assert_eq!(p["$survey_questions"].as_array().unwrap().len(), 3);
        assert_eq!(p["$survey_questions"][1]["id"], ids::USER.questions[1].id);
        assert_eq!(
            p["$survey_questions"][1]["question"],
            ids::USER.questions[1].text
        );
        let key = format!("$survey_response_{}", ids::USER.questions[1].id);
        assert_eq!(p[&key], SECRET);
    }

    #[test]
    fn types_and_skips() {
        let s = survey(&ids::ERROR);
        let answers = vec![(0, Answer::Text("x".into())), (1, Answer::Skipped)];
        let p = response_props(&s, &answers, Source::Popover, &[]);
        assert!(!p.contains_key(&format!("$survey_response_{}", ids::ERROR.questions[1].id)));
        let answers = vec![(1, Answer::Rating(4))];
        let p = response_props(&s, &answers, Source::Error, &[]);
        assert_eq!(
            p[&format!("$survey_response_{}", ids::ERROR.questions[1].id)],
            4
        );
    }

    #[test]
    fn sent_event_carries_scrubbed_copy() {
        let s = survey(&ids::USER);
        let p = response_props(&s, &user_answers(), Source::User, &[]);
        let key = format!("$survey_response_{}_scrubbed", ids::USER.questions[1].id);
        let want = telemetry::scrub(SECRET, &telemetry::home());
        assert_eq!(p[&key], want.as_str());
        assert_ne!(want, SECRET);
    }

    #[test]
    fn a_pasted_key_is_sent_nowhere() {
        // Built here so no scanner reads it as a real key.
        let key = format!("sk-{}", "a1B2".repeat(8));
        let s = survey(&ids::USER);
        let mut s2 = s.clone();
        s2.questions[0].body = Body::Single {
            choices: vec!["Bug".into()],
            open_choice: true,
        };
        let text = format!("It broke, my log says\nAuthorization: {key}\nsee ~/x");
        for (s, a) in [
            (&s, vec![(1, Answer::Text(text.clone()))]),
            (&s2, vec![(0, Answer::Choice(text.clone()))]),
        ] {
            let p = response_props(s, &a, Source::User, &[]);
            let sent = serde_json::to_string(&p).unwrap();
            assert!(!sent.contains(&key), "{sent}");
            let q = &s.questions[a[0].0].id;
            let typed = p[&format!("$survey_response_{q}")].as_str().unwrap();
            // Only the secret goes: the team still reads the rest as typed.
            assert!(typed.starts_with("It broke, my log says\n"), "{typed}");
            assert!(typed.ends_with("see ~/x"), "{typed}");
            assert!(typed.contains(crate::secrets::SECRET), "{typed}");
        }
    }

    #[test]
    fn open_choice_other_is_scrubbed() {
        let mut s = survey(&ids::USER);
        s.questions[0].body = Body::Single {
            choices: vec!["Bug".into()],
            open_choice: true,
        };
        let a = vec![(0, Answer::Choice(SECRET.into()))];
        let p = response_props(&s, &a, Source::User, &[]);
        let key = format!("$survey_response_{}_scrubbed", ids::USER.questions[0].id);
        assert_eq!(
            p[&key],
            telemetry::scrub(SECRET, &telemetry::home()).as_str()
        );
    }

    #[test]
    fn email_question_has_no_scrubbed_copy() {
        let s = survey(&ids::USER);
        let p = response_props(&s, &user_answers(), Source::User, &[]);
        let email = ids::USER.questions[2].id;
        assert_eq!(p[&format!("$survey_response_{email}")], "ann@example.com");
        assert!(!p.contains_key(&format!("$survey_response_{email}_scrubbed")));
    }

    #[test]
    fn error_kind_is_bug() {
        let s = survey(&ids::ERROR);
        let a = vec![(0, Answer::Text("clicking".into()))];
        let p = response_props(&s, &a, Source::Error, &[("error_event", "abc".into())]);
        assert_eq!(p["kind"], "bug");
        assert_eq!(p["source"], "error");
        assert_eq!(p["error_event"], "abc");
    }

    #[test]
    fn extra_overrides_nothing() {
        let s = survey(&ids::USER);
        let extra = [
            ("$survey_id", Value::from("evil")),
            ("source", "agent".into()),
            ("agent", "claude".into()),
        ];
        let p = response_props(&s, &user_answers(), Source::User, &extra);
        assert_eq!(p["$survey_id"], ids::USER.survey_id);
        assert_eq!(p["source"], "user");
        assert_eq!(p["agent"], "claude");
    }

    #[test]
    fn submit_is_refused_in_tests() {
        let s = survey(&ids::USER);
        assert_eq!(
            submit(&s, &user_answers(), Source::User, &[]),
            Err(Refused::DevBuild)
        );
    }
}
