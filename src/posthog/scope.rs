//! Which personal API key scope a refused call needed, so the tab can name
//! it and only the part that needs it stops.

/// The scope behind an endpoint, by its path.
pub fn of_path(path: &str) -> Option<&'static str> {
    let path = path.split('?').next().unwrap_or(path);
    const BY_PATH: &[(&str, &str)] = &[
        ("/query", "query:read"),
        ("/persons", "person:read"),
        ("/cohorts", "cohort:read"),
        ("/event_definitions", "event_definition:read"),
        ("/property_definitions", "property_definition:read"),
        ("/session_recordings", "session_recording:read"),
        ("/feature_flags", "feature_flag:read"),
        ("/experiments", "experiment:read"),
        ("/error_tracking", "error_tracking:read"),
    ];
    BY_PATH
        .iter()
        .find(|(p, _)| path.contains(p))
        .map(|(_, s)| *s)
}

/// The scope PostHog names in a 403's detail: "API key missing required
/// scope 'person:read'".
fn named(detail: &str) -> Option<String> {
    let at = detail.to_ascii_lowercase().find("scope")?;
    detail[at..]
        .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | ',' | '.' | '(' | ')'))
        .find(|w| {
            let mut parts = w.split(':');
            matches!(
                (parts.next(), parts.next(), parts.next()),
                (Some(a), Some("read" | "write"), None)
                    if !a.is_empty() && a.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            )
        })
        .map(str::to_string)
}

/// The missing scope of a 403 on `path`: the one PostHog names, else the
/// endpoint's own. None when neither says (the key itself was refused).
pub fn forbidden(path: &str, detail: &str) -> Option<String> {
    named(detail).or_else(|| of_path(path).map(str::to_string))
}

/// Where the account's personal API keys are edited.
pub fn keys_url(host: &str) -> String {
    format!("{}/settings/user-api-keys", host.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_endpoints_to_scopes() {
        assert_eq!(of_path("/api/projects/1/query/"), Some("query:read"));
        assert_eq!(
            of_path("/api/projects/1/persons/?distinct_id=a"),
            Some("person:read")
        );
        assert_eq!(
            of_path("/api/projects/1/event_definitions/?limit=500"),
            Some("event_definition:read")
        );
        assert_eq!(
            of_path("/api/projects/1/property_definitions/?type=event"),
            Some("property_definition:read")
        );
        assert_eq!(
            of_path("/api/projects/1/session_recordings/abc/"),
            Some("session_recording:read")
        );
        assert_eq!(of_path("/api/users/@me/"), None);
        // The query string can't fake an endpoint.
        assert_eq!(of_path("/api/users/@me/?x=/persons"), None);
    }

    #[test]
    fn reads_the_scope_posthog_names() {
        assert_eq!(
            forbidden(
                "/api/projects/1/query/",
                "API key missing required scope 'person:read'"
            ),
            Some("person:read".into())
        );
        assert_eq!(
            forbidden("/api/projects/1/persons/", "You do not have permission."),
            Some("person:read".into())
        );
        assert_eq!(forbidden("/api/users/@me/", "Invalid token"), None);
        assert_eq!(
            forbidden("/api/users/@me/", "This action needs scope: user:read."),
            Some("user:read".into())
        );
    }

    #[test]
    fn keys_page_follows_the_region() {
        assert_eq!(
            keys_url("https://eu.posthog.com/"),
            "https://eu.posthog.com/settings/user-api-keys"
        );
        assert_eq!(
            keys_url("https://us.posthog.com"),
            "https://us.posthog.com/settings/user-api-keys"
        );
    }
}
