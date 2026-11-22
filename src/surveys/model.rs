//! Parses PostHog's public survey reply and walks a survey's questions.
//!
//! Parsing is forgiving: PostHog adds fields and question types over time,
//! and one odd survey must not hide the others.

use serde::Deserialize;
use serde_json::Value;

use super::ids;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurveyType {
    Popover,
    Api,
    Other,
}

/// When a popover survey may appear. Empty means no restriction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conditions {
    pub url: Option<String>,
    pub url_match_type: Option<String>,
    /// Days to wait after any survey was shown.
    pub wait_days: Option<u32>,
    /// Names of the events that trigger the survey.
    pub events: Vec<String>,
    /// PostHog sets a rule this app cannot check (a selector, device types,
    /// a feature flag, actions). A popover must not show then.
    pub unsupported: bool,
}

#[derive(Debug, Clone)]
pub struct Survey {
    pub id: String,
    pub name: String,
    pub kind: SurveyType,
    pub questions: Vec<Question>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub archived: bool,
    pub conditions: Conditions,
}

#[derive(Debug, Clone)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub description: String,
    pub optional: bool,
    pub button_text: Option<String>,
    pub body: Body,
    pub branching: Branching,
}

#[derive(Debug, Clone)]
pub enum Body {
    Open,
    Rating {
        scale: u8,
        emoji: bool,
        lower: String,
        upper: String,
    },
    Single {
        choices: Vec<String>,
        open_choice: bool,
    },
    Multi {
        choices: Vec<String>,
        open_choice: bool,
    },
    Link {
        url: String,
    },
    /// A question type this version does not know how to draw.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Question(usize),
    End,
}

#[derive(Debug, Clone)]
pub enum Branching {
    Next,
    End,
    SpecificQuestion(usize),
    /// Answer key to target. Choice questions key by choice index, rating
    /// questions by bucket name (see `rating_bucket`).
    ResponseBased(Vec<(String, Target)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Text(String),
    Rating(u8),
    Choice(String),
    Choices(Vec<String>),
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bug,
    Feature,
    Ux,
    Other,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Bug, Kind::Feature, Kind::Ux, Kind::Other];

    pub fn as_str(self) -> &'static str {
        ids::KINDS[self as usize]
    }
}

/// A field that PostHog may send as an explicit `null`: that is the default,
/// the same as a missing one.
fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    surveys: Vec<Value>,
}

#[derive(Deserialize)]
struct RawSurvey {
    id: String,
    #[serde(default, deserialize_with = "or_default")]
    name: String,
    #[serde(default, rename = "type", deserialize_with = "or_default")]
    kind: String,
    #[serde(default, deserialize_with = "or_default")]
    questions: Vec<RawQuestion>,
    start_date: Option<String>,
    end_date: Option<String>,
    #[serde(default, deserialize_with = "or_default")]
    archived: bool,
    conditions: Option<RawConditions>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawConditions {
    url: Option<String>,
    url_match_type: Option<String>,
    seen_survey_wait_period_in_days: Option<u32>,
    events: Option<RawEvents>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEvents {
    #[serde(deserialize_with = "or_default")]
    values: Vec<RawEvent>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEvent {
    #[serde(deserialize_with = "or_default")]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawQuestion {
    #[serde(default, deserialize_with = "or_default")]
    id: String,
    #[serde(default, rename = "type", deserialize_with = "or_default")]
    kind: String,
    #[serde(default, rename = "question", deserialize_with = "or_default")]
    text: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, deserialize_with = "or_default")]
    optional: bool,
    button_text: Option<String>,
    #[serde(default, deserialize_with = "or_default")]
    choices: Vec<String>,
    #[serde(default, deserialize_with = "or_default")]
    has_open_choice: bool,
    display: Option<String>,
    scale: Option<u8>,
    #[serde(default)]
    lower_bound_label: Option<String>,
    #[serde(default)]
    upper_bound_label: Option<String>,
    link: Option<String>,
    branching: Option<RawBranching>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBranching {
    #[serde(default, rename = "type", deserialize_with = "or_default")]
    kind: String,
    index: Option<usize>,
    // A map would lose PostHog's order, but rules are matched by key, so
    // order never matters.
    #[serde(default, deserialize_with = "or_default")]
    response_values: serde_json::Map<String, Value>,
}

/// Every survey in a `/api/surveys/` reply. Malformed entries are dropped.
pub fn parse(json: &str) -> Vec<Survey> {
    let Ok(reply) = serde_json::from_str::<Reply>(json) else {
        return Vec::new();
    };
    reply
        .surveys
        .into_iter()
        .filter_map(|v| {
            let unsupported = has_unsupported_rule(&v);
            let raw = serde_json::from_value::<RawSurvey>(v).ok()?;
            Some(survey_from(raw, unsupported))
        })
        .collect()
}

/// Whether the survey sets a targeting rule this app cannot check.
fn has_unsupported_rule(v: &Value) -> bool {
    let set = |v: Option<&Value>| match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    };
    let top = [
        "linked_flag_key",
        "linkedFlagKey",
        "linked_flag",
        "targeting_flag_key",
        "targeting_flag",
    ];
    // `internal_targeting_flag_key` is not listed: PostHog sets it on every
    // launched survey, and it only hides a survey from a person who saw it or
    // another survey too recently. `surveys-seen.json` and `wait_days` do that.
    let cond = [
        "url",
        "selector",
        "deviceTypes",
        "linkedFlagKey",
        "linked_flag_key",
        "actions",
    ];
    top.iter().any(|k| set(v.get(k)))
        || v.get("conditions")
            .is_some_and(|c| cond.iter().any(|k| set(c.get(k))))
}

fn survey_from(raw: RawSurvey, unsupported: bool) -> Survey {
    let c = raw.conditions.unwrap_or_default();
    Survey {
        id: raw.id,
        name: raw.name,
        kind: match raw.kind.as_str() {
            "popover" => SurveyType::Popover,
            "api" => SurveyType::Api,
            _ => SurveyType::Other,
        },
        questions: raw.questions.into_iter().map(question_from).collect(),
        start_date: raw.start_date,
        end_date: raw.end_date,
        archived: raw.archived,
        conditions: Conditions {
            url: c.url.filter(|u| !u.is_empty()),
            url_match_type: c.url_match_type,
            wait_days: c.seen_survey_wait_period_in_days,
            events: c
                .events
                .map(|e| e.values.into_iter().map(|v| v.name).collect())
                .unwrap_or_default(),
            unsupported,
        },
    }
}

fn question_from(raw: RawQuestion) -> Question {
    let body = match raw.kind.as_str() {
        "open" => Body::Open,
        "rating" => Body::Rating {
            scale: raw.scale.unwrap_or(5),
            emoji: raw.display.as_deref() == Some("emoji"),
            lower: raw.lower_bound_label.unwrap_or_default(),
            upper: raw.upper_bound_label.unwrap_or_default(),
        },
        "single_choice" => Body::Single {
            choices: raw.choices,
            open_choice: raw.has_open_choice,
        },
        "multiple_choice" => Body::Multi {
            choices: raw.choices,
            open_choice: raw.has_open_choice,
        },
        "link" => Body::Link {
            url: raw.link.unwrap_or_default(),
        },
        _ => Body::Unsupported,
    };
    let branching = match raw.branching {
        None => Branching::Next,
        Some(b) => match b.kind.as_str() {
            "end" => Branching::End,
            "specific_question" => b.index.map_or(Branching::Next, Branching::SpecificQuestion),
            "response_based" => Branching::ResponseBased(
                b.response_values
                    .into_iter()
                    .filter_map(|(key, v)| Some((key, target_from(&v)?)))
                    .collect(),
            ),
            _ => Branching::Next,
        },
    };
    Question {
        id: raw.id,
        text: raw.text,
        description: raw.description.unwrap_or_default(),
        optional: raw.optional,
        button_text: raw.button_text,
        body,
        branching,
    }
}

fn target_from(v: &Value) -> Option<Target> {
    match v {
        Value::String(s) if s == "end" => Some(Target::End),
        Value::String(s) => s.parse().ok().map(Target::Question),
        Value::Number(n) => n.as_u64().map(|n| Target::Question(n as usize)),
        _ => None,
    }
}

/// The bucket name PostHog uses for a rating, as in posthog-js
/// `getRatingBucketForResponseValue`. A 10-point scale is 0 to 10 (NPS).
fn rating_bucket(scale: u8, value: u8) -> Option<&'static str> {
    match scale {
        3 => Some(match value {
            ..=1 => "negative",
            2 => "neutral",
            _ => "positive",
        }),
        5 => Some(match value {
            ..=2 => "negative",
            3 => "neutral",
            _ => "positive",
        }),
        7 => Some(match value {
            ..=3 => "negative",
            4 => "neutral",
            _ => "positive",
        }),
        10 => Some(match value {
            ..=6 => "detractors",
            7 | 8 => "passives",
            _ => "promoters",
        }),
        _ => None,
    }
}

/// The question to show after `at` was answered. `None` means the end.
/// Optional questions this version cannot draw are skipped.
pub fn next(s: &Survey, at: usize, answer: &Answer) -> Option<usize> {
    let mut at = at;
    let mut answer = answer;
    // A survey with a branching loop through skipped questions must end,
    // not hang.
    for _ in 0..=s.questions.len() {
        let q = s.questions.get(at)?;
        let target = match &q.branching {
            Branching::End => Target::End,
            Branching::SpecificQuestion(i) => Target::Question(*i),
            Branching::ResponseBased(rules) => answer_key(q, answer)
                .and_then(|key| rules.iter().find(|(k, _)| *k == key))
                .map_or(Target::Question(at + 1), |(_, t)| *t),
            Branching::Next => Target::Question(at + 1),
        };
        let Target::Question(i) = target else {
            return None;
        };
        let skippable = s
            .questions
            .get(i)
            .is_some_and(|n| n.optional && matches!(n.body, Body::Unsupported));
        if !skippable {
            return (i < s.questions.len()).then_some(i);
        }
        at = i;
        answer = &Answer::Skipped;
    }
    None
}

/// The position of a choice. Text typed into the open choice is not in
/// `choices`, and counts as the last one, which is the open choice itself.
fn choice_index(choices: &[String], open_choice: bool, answer: &str) -> Option<usize> {
    choices
        .iter()
        .position(|x| x == answer)
        .or_else(|| (open_choice && !choices.is_empty()).then(|| choices.len() - 1))
}

/// The key a response-based rule uses for this answer.
fn answer_key(q: &Question, answer: &Answer) -> Option<String> {
    match (&q.body, answer) {
        (Body::Rating { scale, .. }, Answer::Rating(v)) => {
            rating_bucket(*scale, *v).map(Into::into)
        }
        (
            Body::Single {
                choices,
                open_choice,
            },
            Answer::Choice(c),
        ) => choice_index(choices, *open_choice, c).map(|i| i.to_string()),
        _ => None,
    }
}

/// The kind a person picked, by the position of the chosen choice. Position
/// rather than text, so rewording a choice in PostHog changes nothing.
pub fn kind_of(q: &Question, answer: &Answer) -> Option<Kind> {
    let (
        Body::Single {
            choices,
            open_choice,
        },
        Answer::Choice(c),
    ) = (&q.body, answer)
    else {
        return None;
    };
    let i = choice_index(choices, *open_choice, c)?;
    Kind::ALL.get(i).copied()
}

/// False if a required question cannot be drawn, since it could not be answered.
pub fn drawable(s: &Survey) -> bool {
    s.questions
        .iter()
        .all(|q| q.optional || !matches!(q.body, Body::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/surveys.json");

    fn q(body: Body, optional: bool, branching: Branching) -> Question {
        Question {
            id: "q".into(),
            text: String::new(),
            description: String::new(),
            optional,
            button_text: None,
            body,
            branching,
        }
    }

    fn survey(questions: Vec<Question>) -> Survey {
        Survey {
            id: "s".into(),
            name: "s".into(),
            kind: SurveyType::Popover,
            questions,
            start_date: None,
            end_date: None,
            archived: false,
            conditions: Conditions::default(),
        }
    }

    fn rating(scale: u8) -> Body {
        Body::Rating {
            scale,
            emoji: false,
            lower: String::new(),
            upper: String::new(),
        }
    }

    #[test]
    fn parses_real_reply() {
        let all = parse(FIXTURE);
        assert_eq!(all.len(), 3);
        assert!(matches!(all[0].questions[0].body, Body::Single { .. }));
        assert!(matches!(all[0].questions[1].body, Body::Open));
        assert!(all[0].questions[2].optional);
        assert_eq!(all[2].kind, SurveyType::Popover);
        assert_eq!(all[0].kind, SurveyType::Api);
        match &all[2].questions[1].body {
            Body::Rating {
                scale,
                emoji,
                lower,
                upper,
            } => {
                assert_eq!(*scale, 5);
                assert!(!*emoji);
                assert_eq!(lower, "Minor");
                assert_eq!(upper, "Blocking");
            }
            other => panic!("not a rating: {other:?}"),
        }
        assert_eq!(all[2].conditions.events, vec!["$exception".to_string()]);
        assert_eq!(all[2].conditions.wait_days, Some(7));
        assert!(all.iter().all(drawable));
    }

    #[test]
    fn malformed_entries_are_dropped() {
        let json =
            r#"{"surveys":[{"id":"a","name":"a","type":"api","questions":[]},{"name":1},"x"]}"#;
        assert_eq!(parse(json).len(), 1);
        assert!(parse("not json").is_empty());
    }

    #[test]
    fn unknown_type_is_unsupported() {
        let json = r#"{"surveys":[{"id":"a","name":"a","type":"widget","questions":[
            {"id":"q","type":"hologram","question":"?","optional":false}]}]}"#;
        let all = parse(json);
        assert_eq!(all[0].kind, SurveyType::Other);
        assert!(matches!(all[0].questions[0].body, Body::Unsupported));
    }

    #[test]
    fn unsupported_optional_is_skipped() {
        let s = survey(vec![
            q(Body::Open, false, Branching::Next),
            q(Body::Unsupported, true, Branching::Next),
            q(Body::Open, false, Branching::Next),
        ]);
        assert_eq!(next(&s, 0, &Answer::Text("a".into())), Some(2));
        assert_eq!(next(&s, 2, &Answer::Text("a".into())), None);
    }

    #[test]
    fn unsupported_optional_at_the_end_ends() {
        let s = survey(vec![
            q(Body::Open, false, Branching::Next),
            q(Body::Unsupported, true, Branching::Next),
        ]);
        assert_eq!(next(&s, 0, &Answer::Text("a".into())), None);
    }

    #[test]
    fn branching_response_based() {
        let branching = Branching::ResponseBased(vec![
            ("negative".into(), Target::Question(2)),
            ("positive".into(), Target::End),
        ]);
        let s = survey(vec![
            q(rating(5), false, branching),
            q(Body::Open, false, Branching::Next),
            q(Body::Open, false, Branching::Next),
        ]);
        assert_eq!(next(&s, 0, &Answer::Rating(1)), Some(2));
        assert_eq!(next(&s, 0, &Answer::Rating(5)), None);
        // No rule for neutral, so it falls through to the next question.
        assert_eq!(next(&s, 0, &Answer::Rating(3)), Some(1));
        assert_eq!(next(&s, 0, &Answer::Skipped), Some(1));
    }

    #[test]
    fn rating_buckets_follow_posthog_js() {
        assert_eq!(rating_bucket(3, 1), Some("negative"));
        assert_eq!(rating_bucket(3, 2), Some("neutral"));
        assert_eq!(rating_bucket(3, 3), Some("positive"));
        assert_eq!(rating_bucket(5, 2), Some("negative"));
        assert_eq!(rating_bucket(5, 3), Some("neutral"));
        assert_eq!(rating_bucket(5, 4), Some("positive"));
        assert_eq!(rating_bucket(7, 3), Some("negative"));
        assert_eq!(rating_bucket(7, 4), Some("neutral"));
        assert_eq!(rating_bucket(7, 5), Some("positive"));
        assert_eq!(rating_bucket(10, 0), Some("detractors"));
        assert_eq!(rating_bucket(10, 6), Some("detractors"));
        assert_eq!(rating_bucket(10, 7), Some("passives"));
        assert_eq!(rating_bucket(10, 8), Some("passives"));
        assert_eq!(rating_bucket(10, 9), Some("promoters"));
        assert_eq!(rating_bucket(10, 10), Some("promoters"));
        assert_eq!(rating_bucket(4, 2), None);
    }

    #[test]
    fn branching_parses_from_json() {
        let json = r#"{"surveys":[{"id":"a","name":"a","type":"popover","questions":[
            {"id":"r","type":"rating","question":"?","optional":false,"scale":5,
             "branching":{"type":"response_based","responseValues":{"negative":2,"positive":"end"}}},
            {"id":"o","type":"open","question":"?","optional":false},
            {"id":"p","type":"open","question":"?","optional":false},
            {"id":"c","type":"single_choice","question":"?","optional":false,"choices":["x","y"],
             "branching":{"type":"response_based","responseValues":{"1":0}}},
            {"id":"j","type":"open","question":"?","optional":false,
             "branching":{"type":"specific_question","index":0}},
            {"id":"e","type":"open","question":"?","optional":false,"branching":{"type":"end"}}
        ]}]}"#;
        let s = &parse(json)[0];
        assert_eq!(next(s, 0, &Answer::Rating(2)), Some(2));
        assert_eq!(next(s, 0, &Answer::Rating(4)), None);
        assert_eq!(next(s, 3, &Answer::Choice("y".into())), Some(0));
        assert_eq!(next(s, 3, &Answer::Choice("x".into())), Some(4));
        assert_eq!(next(s, 4, &Answer::Text("a".into())), Some(0));
        assert_eq!(next(s, 5, &Answer::Text("a".into())), None);
    }

    #[test]
    fn branching_cycle_through_unsupported_ends() {
        let s = survey(vec![
            q(Body::Unsupported, true, Branching::SpecificQuestion(1)),
            q(Body::Unsupported, true, Branching::SpecificQuestion(0)),
        ]);
        assert_eq!(next(&s, 0, &Answer::Skipped), None);
    }

    #[test]
    fn kind_by_position() {
        let kind_q = q(
            Body::Single {
                choices: vec![
                    "Broken thing".into(),
                    "Wish".into(),
                    "Looks".into(),
                    "Else".into(),
                ],
                open_choice: false,
            },
            false,
            Branching::Next,
        );
        let pick = |c: &str| kind_of(&kind_q, &Answer::Choice(c.into()));
        assert_eq!(pick("Broken thing"), Some(Kind::Bug));
        assert_eq!(pick("Wish"), Some(Kind::Feature));
        assert_eq!(pick("Looks"), Some(Kind::Ux));
        assert_eq!(pick("Else"), Some(Kind::Other));
        assert_eq!(pick("Nope"), None);
        assert_eq!(kind_of(&kind_q, &Answer::Skipped), None);
        assert_eq!(Kind::Bug.as_str(), "bug");
        assert_eq!(Kind::Other.as_str(), "other");
    }

    #[test]
    fn kind_order_matches_ids() {
        let kinds = [Kind::Bug, Kind::Feature, Kind::Ux, Kind::Other];
        for (kind, name) in kinds.iter().zip(super::super::ids::KINDS) {
            assert_eq!(kind.as_str(), name);
        }
    }

    #[test]
    fn null_fields_parse_as_defaults() {
        let json = r#"{"surveys":[{"id":"a","name":null,"type":null,"archived":null,
            "start_date":null,"end_date":null,"conditions":null,"questions":[
            {"id":"q","type":"single_choice","question":"?","description":null,
             "optional":null,"choices":null,"hasOpenChoice":null,"buttonText":null,
             "branching":{"type":null,"responseValues":null}},
            {"id":"r","type":"rating","question":"?","scale":null,
             "lowerBoundLabel":null,"upperBoundLabel":null}]},
            {"id":"b","type":"popover","questions":[],
             "conditions":{"url":null,"events":{"values":null},"seenSurveyWaitPeriodInDays":null}},
            {"id":"c","type":"popover","questions":[],
             "conditions":{"events":{"values":[{"name":null}]}}}]}"#;
        let all = parse(json);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].questions.len(), 2);
        assert!(!all[0].questions[0].optional);
        assert!(all[1].conditions.events.is_empty());
        assert!(!all[1].conditions.unsupported);
    }

    #[test]
    fn internal_flag_is_supported_but_targeting_flag_is_not() {
        // The shape of a launched popover in a real `/api/surveys/?token=` reply.
        let json = r#"{"surveys":[{"id":"a","name":"a","type":"popover",
            "internal_targeting_flag_key":"survey-targeting-2bd88208c6-custom",
            "schedule":"once","start_date":"2026-11-22T00:00:00Z","end_date":null,
            "conditions":{"events":{"values":[{"name":"agent turn finished"}]}},
            "questions":[{"id":"q","type":"rating","question":"?","scale":5,"display":"number"}]},
            {"id":"b","name":"b","type":"popover","targeting_flag_key":"beta-users",
             "questions":[]}]}"#;
        let all = parse(json);
        assert!(!all[0].conditions.unsupported);
        assert!(all[1].conditions.unsupported);
    }

    #[test]
    fn open_choice_text_counts_as_last_choice() {
        let choices: Vec<String> = ["Bug", "Idea", "Looks", "Other"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let branching = Branching::ResponseBased(vec![("3".into(), Target::End)]);
        let kind_q = q(
            Body::Single {
                choices,
                open_choice: true,
            },
            false,
            branching,
        );
        let s = survey(vec![kind_q.clone(), q(Body::Open, false, Branching::Next)]);
        let typed = Answer::Choice("something I typed".into());
        assert_eq!(kind_of(&kind_q, &typed), Some(Kind::Other));
        assert_eq!(next(&s, 0, &typed), None);
        // Without an open choice, unknown text matches nothing.
        let closed = Body::Single {
            choices: vec!["a".into()],
            open_choice: false,
        };
        assert_eq!(kind_of(&q(closed, false, Branching::Next), &typed), None);
    }

    #[test]
    fn required_unsupported_not_drawable() {
        let ok = survey(vec![
            q(Body::Open, false, Branching::Next),
            q(Body::Unsupported, true, Branching::Next),
        ]);
        assert!(drawable(&ok));
        let bad = survey(vec![q(Body::Unsupported, false, Branching::Next)]);
        assert!(!drawable(&bad));
    }
}
