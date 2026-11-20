//! `web_fetch` and `web_search`, the Workers AI loop's web tools. Both only
//! read, so they never ask; `ARAKUN_WEB_BLOCK` (comma-separated domains,
//! matched as suffixes of the host) keeps them off some sites.
//!
//! Pages load through Cloudflare Browser Run when its credentials are set up
//! (what `arakun fetch` uses, src/fetch.rs), else with a plain GET whose HTML
//! is turned into text here. Searches go to DuckDuckGo's HTML page
//! (`html.duckduckgo.com/html/?q=`): no API key, no account, and plain markup
//! that a few string searches read. When it answers with its bot check, the
//! same page is asked of Browser Run's real browser instead.
//!
//! `web_fetch` takes an optional `query`. With it, the page is cut down to
//! its title, its headings and the paragraphs, list items and table rows that
//! contain the query's words, so a fact in a long page costs a few hundred
//! tokens instead of the whole page. It stays optional: on a small page the
//! outline can weigh more than the page.
//!
//! Lab mode: with `ARAKUN_WEB_URL` set (like `http://lab.internal/web`), both
//! tools ask the lab's Worker instead, with GETs that answer JSON:
//!
//! ```text
//! GET {ARAKUN_WEB_URL}/fetch?url=<url-encoded>
//!     200 {"url": "<final url>", "content": "<the page as Markdown>"}
//! GET {ARAKUN_WEB_URL}/search?q=<url-encoded>&n=<max results>
//!     200 {"results": [{"title": "...", "url": "...", "snippet": "..."}]}
//! anything else: a non-2xx status, with {"error": "<why>"} or plain text
//! ```

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

/// What a page may weigh before it is refused.
const PAGE_BYTES: u64 = 10 * 1024 * 1024;
const DDG: &str = "https://html.duckduckgo.com/html/?q=";

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn http() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .user_agent(concat!("arakun/", env!("ARAKUN_VERSION")))
            .build()
            .into()
    })
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The lab's web service, when this runs in Arakun Lab.
fn lab() -> Option<String> {
    env("ARAKUN_WEB_URL").map(|u| u.trim_end_matches('/').to_string())
}

fn blocklist() -> Vec<String> {
    parse_blocklist(&env("ARAKUN_WEB_BLOCK").unwrap_or_default())
}

fn parse_blocklist(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|d| d.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

/// `host` is one of the blocked domains or under one.
fn blocked(host: &str, list: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    list.iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// The host of an http(s) URL.
fn host(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Refuses what isn't http(s) and the blocked hosts.
fn check(url: &str, list: &[String]) -> Result<(), String> {
    let host = host(url).ok_or_else(|| format!("not an http(s) URL: {url}"))?;
    if blocked(&host, list) {
        return Err(format!("{host} is blocked here (ARAKUN_WEB_BLOCK)"));
    }
    Ok(())
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A query-string value decoded: `%XX` and `+`.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len()
                && b[i + 1].is_ascii_hexdigit()
                && b[i + 2].is_ascii_hexdigit() =>
            {
                let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
                i += 3;
                continue;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn lab_fetch_url(base: &str, url: &str) -> String {
    format!("{base}/fetch?url={}", encode(url))
}

fn lab_search_url(base: &str, query: &str, n: usize) -> String {
    format!("{base}/search?q={}&n={n}", encode(query))
}

/// A GET; the final URL after redirects, the status, the content type and the body.
fn get(url: &str, accept: &str) -> Result<(String, u16, String, String), String> {
    use ureq::ResponseExt as _;
    let mut r = http()
        .get(url)
        .header("Accept", accept)
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    let status = r.status().as_u16();
    let ty = r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let last = r.get_uri().to_string();
    let body = r
        .body_mut()
        .with_config()
        .limit(PAGE_BYTES)
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))?;
    Ok((last, status, ty, body))
}

/// A JSON answer from the lab's service.
fn lab_get(url: &str) -> Result<Value, String> {
    let (_, status, _, body) = get(url, "application/json")?;
    let v: Option<Value> = serde_json::from_str(&body).ok();
    if !(200..300).contains(&status) {
        let why = v
            .as_ref()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| body.trim().chars().take(500).collect());
        return Err(format!("the lab's web service answered {status}: {why}"));
    }
    v.ok_or_else(|| "the lab's web service did not answer JSON".into())
}

/// A sandboxed headless run without the `net` cap has no network for its tools, and
/// these run in the host process: without this check they'd be its way out.
fn net_allowed() -> Result<(), String> {
    use crate::headless::sandbox::{self, Cap};
    match sandbox::current() {
        Some(j) if j.sandbox.is_some() && !j.caps.contains(&Cap::Net) => Err(
            "this sandboxed run has no network; start it with --allow net to read the web".into(),
        ),
        _ => Ok(()),
    }
}

// ---- web_fetch --------------------------------------------------------------------

pub fn fetch(args: &Value) -> Result<String, String> {
    net_allowed()?;
    let raw = args["url"]
        .as_str()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("url is missing")?;
    let url = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let markdown = match args["format"].as_str().unwrap_or("markdown") {
        "markdown" | "" => true,
        "text" => false,
        other => return Err(format!("format is markdown or text, not {other}")),
    };
    let query = args["query"]
        .as_str()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    // Matching needs the headings and table rows, so a query reads Markdown
    // and turns the result into text at the end.
    let want_text = !markdown;
    let markdown = markdown || query.is_some();
    let list = blocklist();
    check(&url, &list)?;
    let (last, content, note) = if let Some(base) = lab() {
        let v = lab_get(&lab_fetch_url(&base, &url))?;
        let last = v["url"].as_str().unwrap_or(&url).to_string();
        let content = v["content"]
            .as_str()
            .ok_or("the lab's web service answered no content")?
            .to_string();
        (
            last,
            if markdown {
                content
            } else {
                markdown_to_text(&content)
            },
            None,
        )
    } else if crate::fetch::available() {
        let page = if markdown {
            crate::fetch::page("markdown", &url)
        } else {
            crate::fetch::page("content", &url).map(|h| html_to_text(&h, false))
        };
        match page {
            Ok(content) => (url.clone(), content, None),
            Err(e) => {
                let (last, content) = direct(&url, markdown)?;
                (
                    last,
                    content,
                    Some(format!("(Browser Run failed: {e}; fetched directly)")),
                )
            }
        }
    } else {
        let (last, content) = direct(&url, markdown)?;
        (last, content, None)
    };
    check(&last, &list)?;
    let content = content.trim();
    let mut out = format!("URL: {last}\n");
    if let Some(n) = note {
        out.push_str(&n);
        out.push('\n');
    }
    if content.is_empty() {
        out.push_str("\n(the page has no text)");
        return Ok(out);
    }
    match query {
        Some(q) => {
            let part = focus(content, q);
            out.push_str(&if want_text {
                markdown_to_text(&part)
            } else {
                part
            });
        }
        None => {
            out.push('\n');
            out.push_str(content);
        }
    }
    Ok(out)
}

/// Headings the outline lists at most.
const OUTLINE_MAX: usize = 60;
/// Matching paragraphs, items or rows shown at most.
const PARTS_MAX: usize = 40;
/// A paragraph longer than this many lines is read line by line, so a page
/// of plain lines (a text file, a log) does not come back whole.
const PARA_LINES: usize = 20;
/// Query words matched at most; each one is a bit in a mask.
const TERMS_MAX: usize = 64;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "how",
    "i", "in", "is", "it", "of", "on", "or", "the", "this", "to", "was", "what", "when", "where",
    "which", "who", "why", "with",
];

/// The words of a query that are worth matching, lower case. Sentence
/// punctuation goes; the marks that make a name (`C#`, `C++`) stay.
fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in query.split(|c: char| !(c.is_alphanumeric() || "._-/+#".contains(c))) {
        let w = w
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .trim_end_matches(['.', '-', '/', '_'])
            .to_lowercase();
        if !w.is_empty() && !STOPWORDS.contains(&w.as_str()) && !out.contains(&w) {
            out.push(w);
        }
    }
    if out.is_empty() {
        // A query of only small words still means something.
        for w in query.split_whitespace().map(str::to_lowercase) {
            if !out.contains(&w) {
                out.push(w);
            }
        }
    }
    out.truncate(TERMS_MAX);
    out
}

/// Whether lower-case `text` holds `term` as a word or the start of one:
/// "version" finds "versions", but "1.14" does not find "1.142" or
/// "11.14", and "install" does not find "reinstall".
fn has(text: &str, term: &str) -> bool {
    let (Some(first), Some(last)) = (term.chars().next(), term.chars().next_back()) else {
        return false;
    };
    let same = |a: char, b: char| {
        (a.is_ascii_digit() && b.is_ascii_digit()) || (a.is_alphabetic() && b.is_alphabetic())
    };
    text.match_indices(term).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let mut after = text[i + term.len()..].chars();
        let next = after.next();
        before.is_none_or(|b| !same(b, first))
            && (!last.is_ascii_digit()
                || match next {
                    None => true,
                    Some(c) if c.is_ascii_digit() => false,
                    Some('.') => !after.next().is_some_and(|c| c.is_ascii_digit()),
                    Some(_) => true,
                })
    })
}

/// The bits of the terms that `text` holds.
fn mask(text: &str, terms: &[String]) -> u64 {
    let text = text.to_lowercase();
    terms
        .iter()
        .enumerate()
        .filter(|(_, t)| has(&text, t))
        .fold(0, |m, (i, _)| m | 1 << i)
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    let rest = &line[level..];
    ((1..=6).contains(&level) && rest.starts_with(' ')).then(|| (level, rest.trim()))
}

fn list_item(line: &str) -> bool {
    let l = line.trim_start();
    l.starts_with("- ")
        || l.starts_with("* ")
        || l.starts_with("+ ")
        || l.split_once(". ")
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// A code fence's mark and length: ``` or ~~~, three or more.
fn fence(line: &str) -> Option<(char, usize)> {
    let l = line.trim_start();
    let mark = l.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = l.chars().take_while(|c| *c == mark).count();
    (n >= 3).then_some((mark, n))
}

/// A table's delimiter row, with or without the outer pipes: `|---|:--:|`
/// or `--- | ---`.
fn delimiter(line: &str) -> bool {
    let l = line.trim();
    l.contains('|') && l.contains('-') && l.chars().all(|c| "|-: ".contains(c))
}

/// A heading, stored once: the parts under it point here, and a path is
/// only put together for the ones shown.
struct Section<'a> {
    level: usize,
    label: &'a str,
    parent: Option<usize>,
    /// The terms in this heading or the ones above it.
    mask: u64,
}

/// One piece of a block that can match alone: a paragraph, a list item or a
/// table row, with its table's header when it has one.
struct Part {
    section: usize,
    header: Option<usize>,
    text: String,
}

/// The page cut down to what matches `query`: its title, its headings, and
/// the paragraphs, list items and table rows that hold the most of the
/// query's words, in page order under their headings.
fn focus(page: &str, query: &str) -> String {
    let terms = terms(query);
    // Section 0 is the text before the first heading.
    let mut sections = vec![Section {
        level: 0,
        label: "",
        parent: None,
        mask: 0,
    }];
    // The open headings, deepest last.
    let mut path: Vec<usize> = Vec::new();
    // Table headers with their masks, each stored once for all its rows.
    let mut headers: Vec<(String, u64)> = Vec::new();
    let mut parts: Vec<Part> = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let mut open: Option<(char, usize)> = None;
    let mut flush = |block: &mut Vec<&str>, parts: &mut Vec<Part>, section: usize| {
        if block.is_empty() {
            return;
        }
        let lines = std::mem::take(block);
        let one = |text: String| Part {
            section,
            header: None,
            text,
        };
        let piped = lines.iter().all(|l| l.trim_start().starts_with('|'));
        let delimited = lines.len() > 1 && delimiter(lines[1]);
        if fence(lines[0]).is_some() {
            parts.push(one(lines.join("\n")));
        } else if delimited || (piped && lines.len() > 1) {
            let head = if delimited { 2 } else { 1 };
            let header = lines[..head].join("\n");
            let m = mask(&header, &terms);
            headers.push((header, m));
            for l in &lines[head..] {
                parts.push(Part {
                    section,
                    header: Some(headers.len() - 1),
                    text: (*l).to_string(),
                });
            }
        } else if lines.iter().any(|l| list_item(l)) {
            // Items, each with the lines under it; text before the first
            // item stands alone.
            let mut item: Vec<&str> = Vec::new();
            for l in lines {
                if list_item(l) && !item.is_empty() {
                    parts.push(one(std::mem::take(&mut item).join("\n")));
                }
                item.push(l);
            }
            parts.push(one(item.join("\n")));
        } else if lines.len() <= PARA_LINES {
            parts.push(one(lines.join("\n")));
        } else {
            parts.extend(lines.iter().map(|l| one((*l).to_string())));
        }
    };
    for line in page.lines() {
        if let Some((mark, n)) = open {
            block.push(line);
            // Closed only by the same mark, at least as long, and nothing after.
            if fence(line)
                .is_some_and(|(m, k)| m == mark && k >= n && line.trim().chars().all(|c| c == mark))
            {
                open = None;
                flush(&mut block, &mut parts, sections.len() - 1);
            }
            continue;
        }
        if let Some(f) = fence(line) {
            flush(&mut block, &mut parts, sections.len() - 1);
            open = Some(f);
            block.push(line);
            continue;
        }
        if let Some((level, label)) = heading(line) {
            flush(&mut block, &mut parts, sections.len() - 1);
            path.retain(|&i| sections[i].level < level);
            let parent = path.last().copied();
            let above = parent.map_or(0, |i| sections[i].mask);
            sections.push(Section {
                level,
                label,
                parent,
                mask: above | mask(label, &terms),
            });
            path.push(sections.len() - 1);
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut block, &mut parts, sections.len() - 1);
        } else {
            block.push(line);
        }
    }
    flush(&mut block, &mut parts, sections.len() - 1);

    let mut out = String::new();
    if let Some(first) = sections.get(1) {
        out.push_str(&format!("Title: {}\n", first.label));
    }
    if sections.len() > 1 {
        out.push_str("\nOutline:\n");
        for s in sections.iter().skip(1).take(OUTLINE_MAX) {
            out.push_str(&format!("{}- {}\n", "  ".repeat(s.level - 1), s.label));
        }
        if sections.len() - 1 > OUTLINE_MAX {
            out.push_str(&format!(
                "- ({} more headings)\n",
                sections.len() - 1 - OUTLINE_MAX
            ));
        }
    }

    // A part matches with its headings and its table header, so a section
    // or a column named for the query counts. A word found in few parts
    // weighs more than one found everywhere: in a table of versions, "1.142"
    // picks the row and "version" barely counts.
    let masks: Vec<u64> = parts
        .iter()
        .map(|p| {
            mask(&p.text, &terms) | sections[p.section].mask | p.header.map_or(0, |h| headers[h].1)
        })
        .collect();
    let weights: Vec<f64> = (0..terms.len())
        .map(|i| {
            let df = masks.iter().filter(|m| *m & 1 << i != 0).count();
            if df == 0 { 0.0 } else { 1.0 / df as f64 }
        })
        .collect();
    let scores: Vec<f64> = masks
        .iter()
        .map(|m| {
            (0..terms.len())
                .filter(|i| m & 1 << i != 0)
                .map(|i| weights[i])
                .sum()
        })
        .collect();
    let best = scores.iter().copied().fold(0.0, f64::max);
    if best == 0.0 {
        out.push_str(&format!(
            "\nNothing on the page matches \"{query}\". Fetch it without a query to read it all.\n"
        ));
        return out;
    }
    // Parts close to the best one; the rest only share its common words.
    let mut keep: Vec<usize> = (0..parts.len())
        .filter(|&i| scores[i] >= best / 2.0)
        .collect();
    let found = keep.len();
    if found > PARTS_MAX {
        let mut ranked = keep.clone();
        ranked.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
        ranked.truncate(PARTS_MAX);
        ranked.sort_unstable();
        keep = ranked;
    }
    out.push_str(&format!(
        "\nParts that match \"{query}\" ({found} of {}). Fetch without a query for the whole page.\n",
        parts.len()
    ));
    let mut shown_section = usize::MAX;
    let mut shown_header = None;
    let mut prev = usize::MAX;
    for &i in &keep {
        let p = &parts[i];
        // Items next to each other on the page stay a list.
        let item = prev.wrapping_add(1) == i && list_item(&p.text) && list_item(&parts[prev].text);
        prev = i;
        // Right under a heading, the part needs no blank line of its own.
        let mut fresh = false;
        if p.section != shown_section {
            shown_section = p.section;
            shown_header = None;
            out.push('\n');
            let level = sections[p.section].level;
            if level > 0 {
                let mut labels = Vec::new();
                let mut at = Some(p.section);
                while let Some(s) = at {
                    labels.push(sections[s].label);
                    at = sections[s].parent;
                }
                labels.reverse();
                out.push_str(&format!("{} {}\n", "#".repeat(level), labels.join(" > ")));
                fresh = true;
            }
        }
        if p.header.is_none() || p.header != shown_header {
            if !fresh && !item && !out.ends_with("\n\n") {
                out.push('\n');
            }
            if let Some(h) = p.header {
                out.push_str(&headers[h].0);
                out.push('\n');
            }
            shown_header = p.header;
        }
        out.push_str(&p.text);
        out.push('\n');
    }
    if found > keep.len() {
        out.push_str(&format!(
            "\n({} more matching parts not shown; use more specific words.)\n",
            found - keep.len()
        ));
    }
    out
}

/// A page with a plain GET: the final URL and its text.
fn direct(url: &str, markdown: bool) -> Result<(String, String), String> {
    let (last, status, ty, body) = get(
        url,
        "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5",
    )?;
    if status >= 400 {
        return Err(format!("{last} answered {status}"));
    }
    let html = ty.contains("html") || (ty.is_empty() && body.trim_start().starts_with('<'));
    let text = ty.starts_with("text/")
        || ["json", "xml", "javascript", "yaml", "toml"]
            .iter()
            .any(|t| ty.contains(t));
    if html {
        Ok((last, html_to_text(&body, markdown)))
    } else if text || ty.is_empty() {
        Ok((last, body))
    } else {
        Err(format!("{last} is {ty}, not a page to read"))
    }
}

// ---- web_search -------------------------------------------------------------------

pub fn search(args: &Value) -> Result<String, String> {
    net_allowed()?;
    let query = args["query"]
        .as_str()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or("query is missing")?;
    let n = args["max_results"].as_u64().unwrap_or(8).clamp(1, 20) as usize;
    let list = blocklist();
    let (from, hits) = if let Some(base) = lab() {
        let v = lab_get(&lab_search_url(&base, query, n))?;
        let hits = v["results"]
            .as_array()
            .ok_or("the lab's web service answered no results list")?
            .iter()
            .map(|r| {
                let s = |k: &str| r[k].as_str().unwrap_or("").trim().to_string();
                Hit {
                    title: s("title"),
                    url: s("url"),
                    snippet: s("snippet"),
                }
            })
            .collect();
        ("the lab's search", hits)
    } else {
        ("DuckDuckGo", duckduckgo(query)?)
    };
    let hits: Vec<Hit> = hits
        .into_iter()
        .filter(|h| host(&h.url).is_some_and(|host| !blocked(&host, &list)))
        .take(n)
        .collect();
    if hits.is_empty() {
        return Ok(format!("No results from {from} for \"{query}\"."));
    }
    let mut out = format!("Results from {from} for \"{query}\":\n");
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {}\n   {}\n",
            i + 1,
            if h.title.is_empty() { &h.url } else { &h.title },
            h.url
        ));
        if !h.snippet.is_empty() {
            out.push_str(&format!("   {}\n", h.snippet));
        }
    }
    Ok(out)
}

/// DuckDuckGo's bot check instead of results.
fn refused(status: u16, html: &str) -> bool {
    status == 202 || html.contains("anomaly-modal") || html.contains("challenge-form")
}

fn duckduckgo(query: &str) -> Result<Vec<Hit>, String> {
    let url = format!("{DDG}{}", encode(query));
    let (_, status, _, html) =
        get(&url, "text/html").map_err(|e| format!("DuckDuckGo could not be reached: {e}"))?;
    if !refused(status, &html) && status < 400 {
        return Ok(parse_duckduckgo(&html));
    }
    if crate::fetch::available()
        && let Ok(html) = crate::fetch::page("content", &url)
        && !refused(200, &html)
    {
        return Ok(parse_duckduckgo(&html));
    }
    Err(format!(
        "DuckDuckGo answered {status} with its bot check instead of results; try again in a while, \
         or web_fetch a page you already know"
    ))
}

/// The organic results of a DuckDuckGo HTML page, ads left out.
fn parse_duckduckgo(html: &str) -> Vec<Hit> {
    let parts: Vec<&str> = html.split("class=\"result__a\"").collect();
    let mut hits: Vec<Hit> = Vec::new();
    for i in 1..parts.len() {
        let (before, part) = (parts[i - 1], parts[i]);
        // The result's own block, from its opening div to this link.
        let block = before
            .rfind("class=\"result ")
            .map_or("", |at| &before[at..]);
        let open = &before[before.rfind('<').unwrap_or(before.len())..];
        let Some(gt) = part.find('>') else { continue };
        let tag = format!("{open} {}", &part[..gt]);
        let href = attr(&tag, "href").unwrap_or_default();
        if block.contains("result--ad") || href.contains("/y.js") || href.contains("ad_domain=") {
            continue;
        }
        let Some(url) = result_url(&href) else {
            continue;
        };
        let body = &part[gt + 1..];
        let title = strip_tags(&body[..body.find("</a>").unwrap_or(body.len())]);
        let snippet = body
            .find("result__snippet")
            .and_then(|at| {
                let rest = &body[at..];
                let rest = &rest[rest.find('>')? + 1..];
                let end = ["</a>", "</td>", "</div>"]
                    .iter()
                    .filter_map(|t| rest.find(t))
                    .min()
                    .unwrap_or(rest.len());
                Some(strip_tags(&rest[..end]))
            })
            .unwrap_or_default();
        if hits.iter().any(|h| h.url == url) {
            continue;
        }
        hits.push(Hit {
            title,
            url,
            snippet,
        });
    }
    hits
}

/// The site a DuckDuckGo result link leads to (`//duckduckgo.com/l/?uddg=…`).
fn result_url(href: &str) -> Option<String> {
    let href = decode_entities(href);
    let url = match href.split_once("uddg=") {
        Some((_, rest)) => decode(rest.split('&').next().unwrap_or("")),
        None if href.starts_with("//") => format!("https:{href}"),
        None => href,
    };
    host(&url).is_some().then_some(url)
}

// ---- HTML to text -----------------------------------------------------------------

/// An attribute's value in a tag's source.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name).map(|i| i + from) {
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(|c| c.is_whitespace() || c == '"' || c == '\'') {
            continue;
        }
        let rest = lower[from..].trim_start();
        if !rest.starts_with('=') {
            continue;
        }
        let start = tag.len() - rest.len() + 1;
        let value = tag[start..].trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or("").to_string(),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or("")
                .to_string(),
        });
    }
    None
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest[1..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '#')
            .map(|i| i + 1);
        let (name, len) = match end {
            Some(e) if rest[e..].starts_with(';') => (&rest[1..e], e + 1),
            _ => ("", 0),
        };
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16)
                .ok()
                .and_then(char::from_u32),
            n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Markup's text on one line.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' if inside => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A page's readable text; with `markdown`, headings, list items and links
/// keep their Markdown form.
fn html_to_text(html: &str, markdown: bool) -> String {
    const SKIP: &[&str] = &[
        "script", "style", "noscript", "svg", "head", "template", "iframe",
    ];
    const BLOCK: &[&str] = &[
        "p",
        "div",
        "br",
        "tr",
        "ul",
        "ol",
        "table",
        "section",
        "article",
        "header",
        "footer",
        "nav",
        "main",
        "aside",
        "form",
        "blockquote",
        "pre",
        "hr",
        "dl",
        "dt",
        "dd",
        "figure",
        "figcaption",
        "title",
        "body",
    ];
    let mut out = String::new();
    let mut links: Vec<Option<(usize, String)>> = Vec::new();
    let mut rest = html;
    let space = |out: &mut String| {
        if !out.ends_with([' ', '\n']) && !out.is_empty() {
            out.push(' ');
        }
    };
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            push_text(&mut out, rest);
            break;
        };
        push_text(&mut out, &rest[..lt]);
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if !closing && SKIP.contains(&name.as_str()) {
            let close = format!("</{name}");
            rest = rest
                .to_ascii_lowercase()
                .find(&close)
                .map_or("", |e| &rest[e..]);
            continue;
        }
        match name.as_str() {
            h if h.len() == 2 && h.starts_with('h') && h.as_bytes()[1].is_ascii_digit() => {
                out.push_str("\n\n");
                if markdown && !closing {
                    out.push_str(&"#".repeat((h.as_bytes()[1] - b'0') as usize));
                    out.push(' ');
                }
            }
            "li" if !closing => out.push_str(if markdown { "\n- " } else { "\n" }),
            "a" if markdown && !closing => {
                let href = attr(tag, "href")
                    .map(|h| decode_entities(&h))
                    .filter(|h| h.starts_with("http"));
                if let Some(h) = &href {
                    links.push(Some((out.len(), h.clone())));
                    out.push('[');
                } else {
                    links.push(None);
                }
            }
            "a" if markdown => {
                if let Some(Some((start, href))) = links.pop() {
                    let text = out[start + 1..].trim().to_string();
                    out.truncate(start);
                    if !text.is_empty() {
                        out.push_str(&format!("[{text}]({href})"));
                    }
                }
            }
            "td" | "th" => space(&mut out),
            b if BLOCK.contains(&b) => out.push('\n'),
            _ => {}
        }
    }
    let mut text = String::new();
    let mut blank = 0;
    for line in out.lines().map(str::trim) {
        if line.is_empty() || line == "-" {
            blank += 1;
            continue;
        }
        if !text.is_empty() {
            text.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        text.push_str(line);
    }
    text
}

fn push_text(out: &mut String, raw: &str) {
    let text = decode_entities(raw);
    let mut words = text.split_whitespace().peekable();
    if words.peek().is_none() {
        if !raw.is_empty() && !out.ends_with([' ', '\n']) && !out.is_empty() {
            out.push(' ');
        }
        return;
    }
    if text.starts_with(char::is_whitespace) && !out.ends_with([' ', '\n']) && !out.is_empty() {
        out.push(' ');
    }
    out.push_str(&words.collect::<Vec<_>>().join(" "));
    if text.ends_with(char::is_whitespace) {
        out.push(' ');
    }
}

/// Markdown from the lab's service, for `format: text`: links and emphasis
/// marks gone, the words kept.
fn markdown_to_text(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| {
            after[mid + 2..]
                .find(')')
                .map(|close| (mid, mid + 2 + close))
        }) {
            Some((mid, close)) if !after[..mid].contains('\n') => {
                out.push_str(rest[..open].trim_end_matches('!'));
                out.push_str(&after[..mid]);
                rest = &after[close + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.lines()
        .map(|l| l.trim_start_matches('#').trim_start().replace("**", ""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Trimmed from a real answer for "bun latest version", with an ad put back in.
    const DDG_PAGE: &str = r#"<div class="serp__results"><div id="links" class="results">
      <div class="result results_links results_links_deep result--ad ">
        <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=example.com&amp;ad_provider=bing">Buy Bun hosting</a></h2>
        <a class="result__snippet" href="https://duckduckgo.com/y.js?ad_domain=example.com">An ad.</a>
      </div>
      <div class="result results_links results_links_deep web-result ">
        <div class="links_main links_deep result__body">
          <h2 class="result__title">
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fbun.sh%2F&amp;rut=9e6bba93">Bun — A fast all-in-one JavaScript runtime</a>
          </h2>
          <div class="result__extras"><div class="result__extras__url">
            <a class="result__url" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fbun.sh%2F&amp;rut=9e6bba93">bun.sh</a>
          </div></div>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fbun.sh%2F&amp;rut=9e6bba93">NEW <b>Bun</b> v1.4.4 released → <b>Bun</b> is a fast JavaScript runtime &amp; toolkit.</a>
          <div class="clear"></div>
        </div>
      </div>
      <div class="result results_links results_links_deep web-result ">
        <div class="links_main links_deep result__body">
          <h2 class="result__title">
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fgithub.com%2Foven%2Dsh%2Fbun%2Freleases%3Fpage%3D1&amp;rut=abc">Releases · oven-sh/bun · GitHub</a>
          </h2>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fgithub.com%2Foven%2Dsh%2Fbun%2Freleases%3Fpage%3D1&amp;rut=abc">Incredibly fast &#x27;JavaScript&#x27; runtime.</a>
        </div>
      </div>
      <div class="result results_links results_links_deep web-result ">
        <div class="links_main links_deep result__body">
          <h2 class="result__title"><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fbun.sh%2F&amp;rut=zzz" class="result__a">Bun again</a></h2>
        </div>
      </div>
    </div></div>"#;

    #[test]
    fn duckduckgo_results_come_out_without_ads_or_repeats() {
        let hits = parse_duckduckgo(DDG_PAGE);
        assert_eq!(
            hits,
            [
                Hit {
                    title: "Bun — A fast all-in-one JavaScript runtime".into(),
                    url: "https://bun.sh/".into(),
                    snippet:
                        "NEW Bun v1.4.4 released → Bun is a fast JavaScript runtime & toolkit."
                            .into(),
                },
                Hit {
                    title: "Releases · oven-sh/bun · GitHub".into(),
                    url: "https://github.com/oven-sh/bun/releases?page=1".into(),
                    snippet: "Incredibly fast 'JavaScript' runtime.".into(),
                },
            ]
        );
        assert!(parse_duckduckgo("<html><body>No results.</body></html>").is_empty());
        assert!(refused(202, "<form id=\"challenge-form\">"));
        assert!(!refused(200, DDG_PAGE));
    }

    #[test]
    fn the_blocklist_matches_domains_and_their_subdomains() {
        let list = parse_blocklist(" Example.com, .tracker.io ,,");
        assert_eq!(list, ["example.com", "tracker.io"]);
        assert!(blocked("example.com", &list));
        assert!(blocked("www.EXAMPLE.com.", &list));
        assert!(blocked("a.b.tracker.io", &list));
        assert!(!blocked("notexample.com", &list));
        assert!(!blocked("example.com.mx", &list));
        assert!(check("https://user@docs.example.com:8443/x?y", &list).is_err());
        assert!(check("http://example.org/", &list).is_ok());
        assert!(check("file:///etc/passwd", &list).is_err());
        assert!(check("anything", &[]).is_err());
        assert_eq!(host("http://[::1]:8080/"), Some("::1".into()));
    }

    #[test]
    fn lab_urls_carry_their_arguments_encoded() {
        assert_eq!(
            lab_fetch_url("http://lab.internal/web", "https://bun.sh/blog?a=1&b=2#x"),
            "http://lab.internal/web/fetch?url=https%3A%2F%2Fbun.sh%2Fblog%3Fa%3D1%26b%3D2%23x"
        );
        assert_eq!(
            lab_search_url("http://lab.internal/web", "última versión de bun", 5),
            "http://lab.internal/web/search?q=%C3%BAltima%20versi%C3%B3n%20de%20bun&n=5"
        );
        assert_eq!(decode("%C3%BAltima+versi%C3%B3n%2"), "última versión%2");
    }

    #[test]
    fn missing_arguments_are_tool_errors() {
        assert_eq!(fetch(&json!({})), Err("url is missing".into()));
        assert!(
            fetch(&json!({"url": "https://example.com", "format": "pdf"}))
                .unwrap_err()
                .contains("format")
        );
        assert_eq!(
            search(&json!({"query": "  "})),
            Err("query is missing".into())
        );
    }

    #[test]
    fn html_turns_into_readable_markdown_or_text() {
        let html = r#"<html><head><title>T</title><style>p{}</style></head><body>
            <h1>Bun&nbsp;1.4</h1><script>var x = "<p>no</p>";</script>
            <p>Read the <a href="https://bun.sh/blog">release notes</a>, or <a href="/rel">these</a>.</p>
            <ul><li>fast</li><li>small &amp; sharp</li></ul><!-- gone --></body></html>"#;
        assert_eq!(
            html_to_text(html, true),
            "# Bun 1.4\n\nRead the [release notes](https://bun.sh/blog), or these.\n\n- fast\n- small & sharp"
        );
        assert_eq!(
            html_to_text(html, false),
            "Bun 1.4\n\nRead the release notes, or these.\n\nfast\nsmall & sharp"
        );
        assert_eq!(
            markdown_to_text("# Hi\nSee [the **docs**](https://x.y/z) and ![img](a.png)."),
            "Hi\nSee the docs and img."
        );
    }

    #[test]
    fn a_query_keeps_only_the_matching_rows_under_their_header() {
        let mut page = String::from(
            "# Releases\n\nAll versions of the tool.\n\n## Table\n\n| Version | Date |\n|---|---|\n",
        );
        for n in 0..200 {
            page.push_str(&format!("| 1.{n} | 2026-01-{:02} |\n", n % 28 + 1));
        }
        page.push_str("\n## License\n\nMIT.\n");
        let out = focus(&page, "when was version 1.142 released?");
        assert_eq!(
            out,
            "Title: Releases\n\nOutline:\n- Releases\n  - Table\n  - License\n\n\
             Parts that match \"when was version 1.142 released?\" (1 of 202). \
             Fetch without a query for the whole page.\n\n\
             ## Releases > Table\n| Version | Date |\n|---|---|\n| 1.142 | 2026-01-03 |\n"
        );
        assert!(out.len() * 10 < page.len());
    }

    #[test]
    fn a_matching_heading_brings_its_text_and_misses_say_so() {
        let page = "# Bun docs\n\nIntro text.\n\n## Install\n\nRun the script:\n\n```sh\n# not a heading\ncurl -fsSL https://bun.sh/install | bash\n```\n\n- macOS\n- Linux\n\n## Test runner\n\nbun test runs tests.\n";
        let out = focus(page, "install");
        assert!(out.contains("## Bun docs > Install\nRun the script:\n\n```sh\n# not a heading"));
        assert!(out.contains("bash\n```\n\n- macOS\n- Linux\n"), "{out}");
        let linux = focus(page, "linux");
        assert!(
            linux.contains("## Bun docs > Install\n- Linux\n"),
            "{linux}"
        );
        assert!(!linux.contains("macOS\n"));
        assert!(!out.contains("bun test runs"));
        assert!(!out.contains("Intro text"));
        assert!(!out.contains("  - not a heading"));
        let miss = focus(page, "windows");
        assert!(miss.contains("Nothing on the page matches \"windows\""));
        assert!(miss.contains("  - Test runner"));
        assert_eq!(
            terms("What is the Bun 1.4 release date?"),
            ["bun", "1.4", "release", "date"]
        );
        assert_eq!(terms("the"), ["the"]);
        assert_eq!(terms("C# vs C++, in .NET."), ["c#", "vs", "c++", "net"]);
    }

    #[test]
    fn words_match_whole_versions_and_word_starts() {
        assert!(has("1.14 is out", "1.14"));
        assert!(has("see v1.14.", "1.14"));
        assert!(!has("1.142 is out", "1.14"));
        assert!(!has("11.14", "1.14"));
        assert!(!has("1.14.2", "1.14"));
        assert!(has("all versions", "version"));
        assert!(!has("reinstall", "install"));
        assert!(has("use c# here", "c#"));
        assert!(!has("use c here", "c#"));
    }

    #[test]
    fn fences_tables_and_headings_parse_as_markdown_means() {
        // A four-backtick block holds a three-backtick one and a `#` line.
        let page = "# Doc\n\n````md\n```\n# inside\n```\n````\n\n## Retry when load > 80%\n\nPlan | Price\n--- | ---\nFree | 0\nPro | 20\n\nThis paragraph wraps\nover four lines and\nends with the part\nthat says pro matters.\n";
        let out = focus(page, "pro");
        assert!(!out.contains("- inside"), "{out}");
        assert!(out.contains("  - Retry when load > 80%\n"), "{out}");
        assert!(
            out.contains("## Doc > Retry when load > 80%\nPlan | Price\n--- | ---\nPro | 20\n\nThis paragraph wraps\nover four lines and\nends with the part\nthat says pro matters.\n"),
            "{out}"
        );
        assert!(!out.contains("Free"));
    }

    #[test]
    fn rows_of_a_table_turned_to_text_match_alone() {
        // The plain GET's converter leaves a blank line between rows, so each
        // row stands alone, without the header.
        let html = "<h1>Prices</h1><table><tr><th>Plan</th><th>Price</th></tr><tr><td>Free</td><td>0</td></tr><tr><td>Pro</td><td>20</td></tr><tr><td>Team</td><td>50</td></tr></table>";
        let out = focus(&html_to_text(html, true), "pro");
        assert!(
            out.ends_with(
                "(1 of 4). Fetch without a query for the whole page.\n\n# Prices\nPro 20\n"
            ),
            "{out}"
        );
    }
}
