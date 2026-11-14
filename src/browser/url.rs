//! What the address bar turns typed text into.

const SEARCH: &str = "https://duckduckgo.com/?q=";

/// `github.com` → `https://github.com`, `localhost:3000` → `http://localhost:3000`,
/// `~/notes.html` → `file:///home/…`, anything else that is not an address → a search.
pub fn from_input(input: &str) -> String {
    let text = input.trim();
    if text.is_empty() {
        return "about:blank".into();
    }
    if has_scheme(text) {
        return text.into();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        let home = crate::platform::home().display().to_string().replace('\\', "/");
        let sep = if home.starts_with('/') { "" } else { "/" };
        return format!("file://{sep}{home}/{rest}");
    }
    if text.starts_with('/') {
        return format!("file://{text}");
    }
    if text.contains(char::is_whitespace) {
        return search(text);
    }
    let host_port = text.split(['/', '?', '#']).next().unwrap_or(text);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        Some(_) => return search(text),
        None => (host_port, None),
    };
    let host = host.to_ascii_lowercase();
    let local = host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".test")
        || is_ipv4(&host)
        || host.starts_with('[');
    if local || (port.is_some() && !host.is_empty() && !host.contains('.')) {
        return format!("http://{text}");
    }
    let looks_like_domain = host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b >= 0x80)
        && host.rsplit('.').next().is_some_and(|tld| tld.len() >= 2 && !tld.bytes().all(|b| b.is_ascii_digit()));
    if looks_like_domain {
        return format!("https://{text}");
    }
    search(text)
}

fn has_scheme(text: &str) -> bool {
    let Some((scheme, rest)) = text.split_once(':') else { return false };
    let valid = scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b));
    valid
        && (rest.starts_with("//")
            || matches!(
                scheme.to_ascii_lowercase().as_str(),
                "about" | "data" | "javascript" | "mailto" | "view-source" | "blob" | "chrome" | "devtools"
            ))
}

fn is_ipv4(host: &str) -> bool {
    let parts: Vec<_> = host.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok())
}

fn search(query: &str) -> String {
    let mut out = String::from(SEARCH);
    for b in query.bytes() {
        match b {
            b' ' => out.push('+'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The address shown while the field is not being edited: `https://github.com/` →
/// `github.com`, other schemes in full.
pub fn display(url: &str) -> String {
    let short = url
        .strip_prefix("https://")
        .map(|rest| rest.strip_prefix("www.").unwrap_or(rest));
    match short {
        Some(rest) => rest.strip_suffix('/').filter(|r| !r.contains('/')).unwrap_or(rest).to_string(),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::from_input;

    #[test]
    fn inputs() {
        assert_eq!(from_input("github.com"), "https://github.com");
        assert_eq!(from_input("localhost:3000"), "http://localhost:3000");
        assert_eq!(from_input("127.0.0.1:8080/x"), "http://127.0.0.1:8080/x");
        assert_eq!(from_input("myhost:8080"), "http://myhost:8080");
        assert_eq!(from_input("https://a.b/c"), "https://a.b/c");
        assert_eq!(from_input("about:blank"), "about:blank");
        assert_eq!(from_input("rust gpui"), "https://duckduckgo.com/?q=rust+gpui");
        assert_eq!(from_input("ñandú"), "https://duckduckgo.com/?q=%C3%B1and%C3%BA");
        assert_eq!(from_input("foo"), "https://duckduckgo.com/?q=foo");
        assert_eq!(from_input("v1.2"), "https://duckduckgo.com/?q=v1.2");
        assert_eq!(from_input("developer.mozilla.org/en-US/"), "https://developer.mozilla.org/en-US/");
    }
}
