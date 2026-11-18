//! `arakun fetch` and `arakun crawl`: web pages for agents through Cloudflare
//! Browser Run, a real browser on Cloudflare's network, so sites that turn
//! `curl` away (Steam's workshop, for one) still answer. Works without the app.
//!
//! Needs an API token with the "Browser Run - Edit" permission (the `cf`
//! sign-in doesn't carry it) and the account it belongs to: from
//! `CLOUDFLARE_BROWSER_RUN_TOKEN` / `CLOUDFLARE_ACCOUNT_ID`, or the keyring
//! (`arakun fetch --set-token`).

use std::io::Read as _;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::platform::keyring;

const TOKEN_KEY: &str = "cloudflare-browser-run";
const ACCOUNT_KEY: &str = "cloudflare-browser-run-account";
const API: &str = "https://api.cloudflare.com/client/v4/accounts";
/// What a quick action doesn't need to load to read the page.
const SKIP: &[&str] = &["image", "font", "media", "stylesheet"];
/// Times a 429 is waited out before giving up.
const RETRIES: u32 = 3;

const USAGE: &str = "\
arakun fetch [--html | --links] <url>   a page as Markdown (or HTML, or its links)
arakun crawl [--limit N] [--depth N] [--render] [--include <glob>] [--exclude <glob>] <url>
                                        a site's pages as Markdown, one after another
arakun fetch --set-token <account-id>  store the API token (read from stdin) and its account

Pages load in Cloudflare Browser Run. Free plan: one fetch every 10 s, 10 min of
browser a day, 5 crawls of 100 pages a day. It doesn't get past captchas, and
crawl follows robots.txt.";

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            .user_agent(concat!("arakun/", env!("ARAKUN_VERSION")))
            .build()
            .into()
    })
}

struct Creds {
    token: String,
    account: String,
}

fn creds() -> Result<Creds, String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let token = env("CLOUDFLARE_BROWSER_RUN_TOKEN").or_else(|| keyring::lookup(TOKEN_KEY));
    let account = env("CLOUDFLARE_ACCOUNT_ID").or_else(|| keyring::lookup(ACCOUNT_KEY));
    match (token, account) {
        (Some(token), Some(account)) => Ok(Creds { token, account }),
        _ => Err(
            "no Browser Run token: create one with the \"Browser Run - Edit\" permission and run \
                  `arakun fetch --set-token <account-id>` with it on stdin"
                .into(),
        ),
    }
}

/// One call to the API, waiting out rate limits; the `result` of the answer.
fn call(c: &Creds, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let url = format!("{API}/{}/browser-run/{path}", c.account);
    for attempt in 0..=RETRIES {
        let request = ureq::http::Request::builder()
            .method(method)
            .uri(&url)
            .header("Authorization", format!("Bearer {}", c.token))
            .header("Content-Type", "application/json");
        let sent = match body {
            Some(b) => request
                .body(b.to_string())
                .map_err(|e| e.to_string())
                .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
            None => request
                .body(())
                .map_err(|e| e.to_string())
                .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
        };
        let mut response = sent?;
        let status = response.status().as_u16();
        if status == 429 && attempt < RETRIES {
            let wait = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()?.parse().ok())
                .unwrap_or(10);
            std::thread::sleep(Duration::from_secs(wait.min(120)));
            continue;
        }
        let text = response
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&text)
            .map_err(|_| format!("Browser Run answered {status}: {}", text.trim()))?;
        if v["success"].as_bool() != Some(true) {
            let why = v["errors"][0]["message"].as_str().unwrap_or("refused");
            return Err(format!("Browser Run answered {status}: {why}"));
        }
        return Ok(v["result"].clone());
    }
    Err("Browser Run kept answering 429 (rate limit)".into())
}

fn fetch(args: &[String]) -> Result<String, String> {
    let (action, url) = match args {
        [flag, url] if flag == "--html" => ("content", url),
        [flag, url] if flag == "--links" => ("links", url),
        [url] if !url.starts_with('-') => ("markdown", url),
        _ => return Err(USAGE.into()),
    };
    page(action, url)
}

/// Whether Browser Run credentials are set up.
pub fn available() -> bool {
    creds().is_ok()
}

/// One page through Browser Run: `action` is `markdown`, `content` (the
/// rendered HTML) or `links`. A 429 is waited out, as the free plan's one
/// fetch every 10 s asks.
pub fn page(action: &str, url: &str) -> Result<String, String> {
    let c = creds()?;
    let result = call(
        &c,
        "POST",
        action,
        Some(&json!({ "url": url, "rejectResourceTypes": SKIP })),
    )?;
    Ok(match result {
        Value::String(s) => s,
        Value::Array(links) => links
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    })
}

struct CrawlArgs {
    url: String,
    limit: u64,
    depth: Option<u64>,
    render: bool,
    include: Vec<String>,
    exclude: Vec<String>,
}

fn crawl_args(args: &[String]) -> Option<CrawlArgs> {
    let mut out = CrawlArgs {
        url: String::new(),
        limit: 20,
        depth: None,
        render: false,
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--limit" => out.limit = it.next()?.parse().ok()?,
            "--depth" => out.depth = Some(it.next()?.parse().ok()?),
            "--render" => out.render = true,
            "--include" => out.include.push(it.next()?.clone()),
            "--exclude" => out.exclude.push(it.next()?.clone()),
            url if !url.starts_with('-') && out.url.is_empty() => out.url = url.to_string(),
            _ => return None,
        }
    }
    (!out.url.is_empty()).then_some(out)
}

/// Submits the crawl, waits for it, and prints each page as it is read.
fn crawl(args: &[String]) -> Result<(), String> {
    let a = crawl_args(args).ok_or(USAGE)?;
    let c = creds()?;
    let mut body =
        json!({ "url": a.url, "limit": a.limit, "formats": ["markdown"], "render": a.render });
    if let Some(d) = a.depth {
        body["depth"] = json!(d);
    }
    if !a.include.is_empty() || !a.exclude.is_empty() {
        body["options"] = json!({ "includePatterns": a.include, "excludePatterns": a.exclude });
    }
    let job = call(&c, "POST", "crawl", Some(&body))?;
    let id = job
        .as_str()
        .map(str::to_string)
        .or_else(|| job["id"].as_str().map(str::to_string))
        .ok_or("no crawl job id")?;
    eprintln!("crawl {id}: waiting");
    let started = Instant::now();
    loop {
        let r = call(&c, "GET", &format!("crawl/{id}?limit=1"), None)?;
        let status = r["status"].as_str().unwrap_or("running");
        if status != "running" {
            if status != "completed" {
                eprintln!("crawl {id}: {status}");
            }
            break;
        }
        if started.elapsed() > Duration::from_secs(1800) {
            return Err(format!(
                "crawl {id} still running after 30 min (cancel: DELETE crawl/{id})"
            ));
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(cur) => format!("crawl/{id}?status=completed&cursor={cur}"),
            None => format!("crawl/{id}?status=completed"),
        };
        let r = call(&c, "GET", &path, None)?;
        for page in r["records"].as_array().into_iter().flatten() {
            let url = page["url"].as_str().unwrap_or("?");
            let text = page["markdown"]
                .as_str()
                .or(page["html"].as_str())
                .unwrap_or("");
            println!("\n===== {url} =====\n\n{}", text.trim());
        }
        cursor = r["cursor"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(())
}

fn set_token(args: &[String]) -> Result<(), String> {
    let [account] = args else {
        return Err(USAGE.into());
    };
    let mut token = String::new();
    std::io::stdin()
        .read_to_string(&mut token)
        .map_err(|e| e.to_string())?;
    let token = token.trim();
    if token.is_empty() {
        return Err("no token on stdin".into());
    }
    keyring::store(TOKEN_KEY, "arakun: Cloudflare Browser Run token", token)?;
    keyring::store(
        ACCOUNT_KEY,
        "arakun: Cloudflare Browser Run account",
        account,
    )
}

/// `arakun fetch …` (`crawl` true for `arakun crawl …`); the exit code.
pub fn cli(args: &[String], crawling: bool) -> i32 {
    let help = args.iter().any(|a| a == "-h" || a == "--help");
    let done = if crawling && !help {
        crawl(args)
    } else if args.first().is_some_and(|a| a == "--set-token") {
        set_token(&args[1..])
    } else if args.is_empty() || help {
        println!("{USAGE}");
        return if args.is_empty() { 2 } else { 0 };
    } else {
        fetch(args).map(|text| println!("{text}"))
    };
    match done {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn crawl_flags() {
        let a = crawl_args(&s(&[
            "--limit",
            "50",
            "--depth",
            "2",
            "--include",
            "https://x.dev/docs/**",
            "https://x.dev/docs",
        ]))
        .unwrap();
        assert_eq!(
            (a.limit, a.depth, a.render, a.url.as_str()),
            (50, Some(2), false, "https://x.dev/docs")
        );
        assert_eq!(a.include, ["https://x.dev/docs/**"]);
        assert!(crawl_args(&s(&["--limit"])).is_none());
        assert!(crawl_args(&s(&[])).is_none());
    }

    #[test]
    fn crawl_takes_help_too() {
        assert_eq!(cli(&s(&["--help"]), true), 0);
        assert_eq!(cli(&s(&["--limit", "3", "-h"]), true), 0);
    }
}
