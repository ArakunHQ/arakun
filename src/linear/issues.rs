//! Reading issues with a saved sign-in. The reads themselves are in core.

use arakun_core::linear::issues as shared;
pub use arakun_core::linear::issues::*;

use std::path::Path;

use super::uploads::{self, Fetched};

use super::{ApiError, Session, graphql, key_for};

/// The issues assigned to the signed-in user that are not done or canceled.
pub fn assigned(s: &Session) -> Result<Assigned, ApiError> {
    shared::assigned(|q, v| graphql(s, q, v))
}

/// One issue in full, comments oldest first. `mention` is the comment that
/// mentioned Arakun, when a mention started the session.
pub fn detail(s: &Session, id: &str, mention: Option<&str>) -> Result<Detail, ApiError> {
    shared::detail(|q, v| graphql(s, q, v), id, mention)
}

/// A follow-up's text, read with the person's own key.
pub fn prompt_text(
    s: &Session,
    activity_id: &str,
    agent_session: &str,
) -> Result<String, ApiError> {
    shared::prompt_text(|q, v| graphql(s, q, v), activity_id, agent_session)
}

/// Downloads the issue's uploads into the environment at `env` with the
/// person's own key, and puts the files' paths in `d` in place of the links.
pub fn fetch_uploads(s: &Session, env: &Path, d: &mut Detail) -> Fetched {
    if uploads::links(d).is_empty() {
        return Fetched::default();
    }
    match key_for(s) {
        Ok(key) => uploads::fetch(|link, limit| uploads::download(&key, link, limit), env, d),
        Err(_) => uploads::unfetched(d),
    }
}
