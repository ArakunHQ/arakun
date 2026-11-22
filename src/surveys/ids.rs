//! The PostHog surveys Arakun shows natively, and their question ids.
//!
//! Responses are keyed by question id, so these ids must match the surveys
//! in Arakun's PostHog project. Survey ids are public, like the project token.

/// One survey Arakun knows about.
pub struct Builtin {
    /// The survey id. A build can point at another survey with the env var.
    pub survey_id: &'static str,
    /// The build-time variable that overrides `survey_id`.
    pub env_var: &'static str,
    /// The id of the single-choice "kind" question, if the survey has one.
    pub kind_question: Option<&'static str>,
    /// Every question as (id, text), in the order PostHog shows them.
    pub questions: &'static [(&'static str, &'static str)],
}

/// Feedback a person writes from the app. The third question is an optional
/// email and must never reach a public place such as a GitHub issue.
pub const USER: Builtin = Builtin {
    survey_id: match option_env!("ARAKUN_SURVEY_USER") {
        Some(v) => v,
        None => "01a200ad-5d58-0000-35ae-dc63b34e34b9",
    },
    env_var: "ARAKUN_SURVEY_USER",
    kind_question: Some("c08d98b6-2f22-454e-ba14-e548802e645c"),
    questions: &[
        ("c08d98b6-2f22-454e-ba14-e548802e645c", "kind"),
        (
            "8875e82f-96f5-4071-b532-0402de7119fe",
            "What's on your mind?",
        ),
        (
            "9404c016-db43-474c-96fd-4d0319bce760",
            "Can we follow up? Leave an email",
        ),
    ],
};

/// Feedback an agent sends on its own.
pub const AGENT: Builtin = Builtin {
    survey_id: match option_env!("ARAKUN_SURVEY_AGENT") {
        Some(v) => v,
        None => "01a200ad-5f88-0000-364b-2b778e447d4d",
    },
    env_var: "ARAKUN_SURVEY_AGENT",
    kind_question: Some("ce6cdbfc-07dc-4126-aeca-3246f24ac967"),
    questions: &[
        ("ce6cdbfc-07dc-4126-aeca-3246f24ac967", "kind"),
        ("2a877015-575c-46de-93dd-787fdc7a52ae", "Feedback"),
    ],
};

/// Feedback after an error. It has no kind question: an error is not a
/// bug, feature request or UX report.
pub const ERROR: Builtin = Builtin {
    survey_id: match option_env!("ARAKUN_SURVEY_ERROR") {
        Some(v) => v,
        None => "01a200ad-6257-0000-3340-f5cbf139dda4",
    },
    env_var: "ARAKUN_SURVEY_ERROR",
    kind_question: None,
    questions: &[
        (
            "17c2138e-79f3-4590-b91c-cc8b442026be",
            "What were you doing when it happened?",
        ),
        ("4364e5db-a705-480b-85c2-3c46c88eb247", "How bad was it?"),
    ],
};
