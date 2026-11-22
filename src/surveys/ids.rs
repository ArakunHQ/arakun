//! The PostHog surveys Arakun shows natively, and their question ids.
//!
//! Responses are keyed by question id, so these ids must match the surveys
//! in Arakun's PostHog project. Survey ids are public, like the project token.
//! The questions here are the offline fallback: they mirror PostHog.

/// The values of the `kind` property. The choices of a kind question map to
/// these by position, so rewording a choice in PostHog changes nothing here.
pub const KINDS: [&str; 4] = ["bug", "feature", "ux", "other"];

/// One survey Arakun knows about.
pub struct Builtin {
    /// The survey id. A build can point at another survey with `ARAKUN_SURVEY_<NAME>`.
    pub survey_id: &'static str,
    /// The id of the single-choice kind question, if the survey has one.
    pub kind_question: Option<&'static str>,
    /// Every question, in the order PostHog shows them.
    pub questions: &'static [Fallback],
}

/// A question as the app shows it when PostHog cannot be reached.
pub struct Fallback {
    pub id: &'static str,
    pub text: &'static str,
    pub optional: bool,
    pub body: FallbackBody,
}

/// What a question asks for.
pub enum FallbackBody {
    Open,
    SingleChoice {
        choices: &'static [&'static str],
    },
    /// A rating shown as numbers, from 1 to `scale`.
    Rating {
        scale: u8,
    },
}

const KIND_TEXT: &str = "What kind of feedback is it?";
const KIND_CHOICES: &[&str] = &["Bug", "Feature request", "UX", "Other"];
// Choices map to `KINDS` by position, so the lengths must match.
const _: () = assert!(KIND_CHOICES.len() == KINDS.len());

/// Feedback a person writes from the app. The third question is an optional
/// email and must never reach a public place such as a GitHub issue.
pub const USER: Builtin = Builtin {
    survey_id: match option_env!("ARAKUN_SURVEY_USER") {
        Some(v) => v,
        None => "01a200ad-5d58-0000-35ae-dc63b34e34b9",
    },
    kind_question: Some("c08d98b6-2f22-454e-ba14-e548802e645c"),
    questions: &[
        Fallback {
            id: "c08d98b6-2f22-454e-ba14-e548802e645c",
            text: KIND_TEXT,
            optional: false,
            body: FallbackBody::SingleChoice {
                choices: KIND_CHOICES,
            },
        },
        Fallback {
            id: "8875e82f-96f5-4071-b532-0402de7119fe",
            text: "What's on your mind?",
            optional: false,
            body: FallbackBody::Open,
        },
        Fallback {
            id: "9404c016-db43-474c-96fd-4d0319bce760",
            text: "Can we follow up? Leave an email",
            optional: true,
            body: FallbackBody::Open,
        },
    ],
};

/// Feedback an agent sends on its own.
pub const AGENT: Builtin = Builtin {
    survey_id: match option_env!("ARAKUN_SURVEY_AGENT") {
        Some(v) => v,
        None => "01a200ad-5f88-0000-364b-2b778e447d4d",
    },
    kind_question: Some("ce6cdbfc-07dc-4126-aeca-3246f24ac967"),
    questions: &[
        Fallback {
            id: "ce6cdbfc-07dc-4126-aeca-3246f24ac967",
            text: KIND_TEXT,
            optional: false,
            body: FallbackBody::SingleChoice {
                choices: KIND_CHOICES,
            },
        },
        Fallback {
            id: "2a877015-575c-46de-93dd-787fdc7a52ae",
            text: "Feedback",
            optional: false,
            body: FallbackBody::Open,
        },
    ],
};

/// Feedback after an error. It has no kind question because its kind is
/// always `bug`.
pub const ERROR: Builtin = Builtin {
    survey_id: match option_env!("ARAKUN_SURVEY_ERROR") {
        Some(v) => v,
        None => "01a200ad-6257-0000-3340-f5cbf139dda4",
    },
    kind_question: None,
    questions: &[
        Fallback {
            id: "17c2138e-79f3-4590-b91c-cc8b442026be",
            text: "What were you doing when it happened?",
            optional: false,
            body: FallbackBody::Open,
        },
        Fallback {
            id: "4364e5db-a705-480b-85c2-3c46c88eb247",
            text: "How bad was it?",
            optional: true,
            body: FallbackBody::Rating { scale: 5 },
        },
    ],
};
