//! Short spans of time ("26m", "1h 10m") for the GitHub tab and the inbox.

/// A span as the queue writes it: `1h 10m`, `26m`, `3d 4h`, `now`.
pub fn span(secs: u64) -> String {
    let (m, h, d) = (secs / 60, secs / 3600, secs / 86_400);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{m}m"),
        3600..86_400 if m % 60 == 0 => format!("{h}h"),
        3600..86_400 => format!("{h}h {}m", m % 60),
        _ if h % 24 == 0 => format!("{d}d"),
        _ => format!("{d}d {}h", h % 24),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_read_short() {
        assert_eq!(span(20), "now");
        assert_eq!(span(26 * 60), "26m");
        assert_eq!(span(70 * 60), "1h 10m");
        assert_eq!(span(2 * 3600), "2h");
        assert_eq!(span(3 * 86_400 + 4 * 3600), "3d 4h");
        assert_eq!(span(2 * 86_400), "2d");
    }
}
