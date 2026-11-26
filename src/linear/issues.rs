//! Reading issues with a saved sign-in. The reads themselves are in core.

use arakun_core::linear::issues as shared;
pub use arakun_core::linear::issues::*;

use super::{ApiError, Session, graphql};

/// The issues assigned to the signed-in user that are not done or canceled.
pub fn assigned(s: &Session) -> Result<Assigned, ApiError> {
    shared::assigned(|q, v| graphql(s, q, v))
}

/// One issue with its description and its comments, oldest first.
pub fn detail(s: &Session, id: &str) -> Result<Detail, ApiError> {
    shared::detail(|q, v| graphql(s, q, v), id)
}

/// A follow-up's text, read with the person's own key.
pub fn prompt_text(
    s: &Session,
    activity_id: &str,
    agent_session: &str,
) -> Result<String, ApiError> {
    shared::prompt_text(|q, v| graphql(s, q, v), activity_id, agent_session)
}
