//! Connecting arakun-pilot without writing `client.json` by hand: the
//! settings are checked against the Worker, then saved private. The
//! connect form in the GitHub tab and `arakun github pilot` both use this.

use std::io;
use std::path::PathBuf;

use super::pilot::{self, Config};

/// What the user typed, before it is checked. An empty token keeps the one
/// already saved for the same address, so the admin token can be added
/// without typing the read token again.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub url: String,
    pub token: String,
    /// `None` keeps the saved one (same address), `Some("")` drops it.
    pub admin_token: Option<String>,
}

/// The settings `draft` makes, with `saved` filling the gaps, or what is
/// wrong in words for the user.
pub fn settings(draft: &Draft, saved: Option<&Config>) -> Result<Config, String> {
    let url = draft.url.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return Err("Type the Worker's address.".into());
    }
    if let Some(problem) = url_problem(&url) {
        return Err(problem.into());
    }
    let same = saved.filter(|s| s.url == url);
    let token = match draft.token.trim() {
        "" => same.map(|s| s.token.clone()).unwrap_or_default(),
        t => t.to_string(),
    };
    if token.is_empty() {
        return Err("Type the token: the Worker refuses reads without it.".into());
    }
    let admin_token = match draft.admin_token.as_deref().map(str::trim) {
        None => same.and_then(|s| s.admin_token.clone()),
        Some("") => None,
        Some(t) => Some(t.to_string()),
    };
    let json = serde_json::json!({ "url": url, "token": token, "admin_token": admin_token });
    // The same rules as reading the file, so what is saved is always read back.
    pilot::parse_config(json.to_string().as_bytes())
        .ok_or_else(|| "These settings can't be read back.".into())
}

/// Why `url` can't hold the tokens, or None when it can.
pub fn url_problem(url: &str) -> Option<&'static str> {
    if url.starts_with("https://") {
        return (url.len() == "https://".len())
            .then_some("The address has no host after https://.");
    }
    if url.starts_with("http://") {
        return (!pilot::local_http(url)).then_some(
            "Use https://: plain http:// would send the tokens unencrypted. It works only for localhost.",
        );
    }
    Some("The address starts with https:// (for example https://pilot.example.workers.dev).")
}

/// What reading the Worker with `cfg` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// Open cards at `/items`.
    pub cards: usize,
    /// Whether `/board` answered; a Worker without one fills only "Your agent".
    pub board: bool,
}

/// Reads `/items` and `/board` with the read token. The admin token is not
/// tried: only sending a task uses it, and a check must not send one.
pub fn check(cfg: &Config) -> Result<Checked, String> {
    let cards = match pilot::get(cfg, "/items", 8 * 1024 * 1024)? {
        (200, bytes) => pilot::parse_items(&bytes)
            .map_err(|e| {
                format!(
                    "{} answered, but not with cards ({e}). Is it arakun-pilot?",
                    cfg.url
                )
            })?
            .len(),
        (status, _) => return Err(refused(status)),
    };
    let board = super::board::fetch(Some(cfg)).is_ok();
    Ok(Checked { cards, board })
}

/// A refused read in words for someone typing the settings.
pub fn refused(status: u16) -> String {
    match status {
        401 | 403 => "The Worker refused the token. Copy it again from the Worker's secrets.".into(),
        404 => "Nothing answers at /items there. Check the address: it is the Worker's, without a path.".into(),
        s if s >= 500 => format!("The Worker failed ({s}). Try again in a moment."),
        s => format!("The Worker answered {s}."),
    }
}

/// Saves `cfg` where Arakun reads it, readable only by this user.
pub fn save(cfg: &Config) -> io::Result<PathBuf> {
    let path = pilot::config_file();
    let mut json = serde_json::json!({ "url": cfg.url, "token": cfg.token });
    if let Some(admin) = &cfg.admin_token {
        json["admin_token"] = admin.as_str().into();
    }
    let mut bytes = serde_json::to_vec_pretty(&json).map_err(io::Error::other)?;
    bytes.push(b'\n');
    crate::durable::write_private(&path, &bytes)?;
    Ok(path)
}

/// Removes the settings; Ok(false) when there were none.
pub fn remove() -> io::Result<bool> {
    match std::fs::remove_file(pilot::config_file()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// The command line

pub const USAGE: &str = "arakun github pilot connect <url> [--token <t|->] [--admin-token <t|->] [--no-admin-token] [--no-check]
    check the Worker's address and token, then save them so the GitHub tab shows
    pilot's board. `-` reads the token from stdin (keeps it out of shell history);
    without --token, ARAKUN_PILOT_TOKEN, or the token saved for the same address.
    --admin-token (or ARAKUN_PILOT_ADMIN_TOKEN) lets Arakun send pilot tasks.
arakun github pilot status
    say where the settings are and whether the Worker takes them.
arakun github pilot disconnect
    remove the settings; the GitHub tab goes back to the review queue.";

/// `arakun github pilot …`.
pub fn cli(args: &[String]) -> i32 {
    if args.iter().any(|a| crate::control::is_help(a)) {
        println!("{USAGE}");
        return 0;
    }
    let result = match args.first().map(String::as_str) {
        Some("connect") => connect_cli(&args[1..]),
        Some("status") if args.len() == 1 => status_cli(),
        Some("disconnect") if args.len() == 1 => disconnect_cli(),
        Some("help") | None => {
            println!("{USAGE}");
            return 0;
        }
        Some(other) => {
            eprintln!("arakun github pilot: unknown command `{other}`\n{USAGE}");
            return 2;
        }
    };
    match result {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("arakun github pilot: {e}");
            1
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ConnectArgs {
    url: String,
    token: Option<String>,
    admin_token: Option<String>,
    no_admin: bool,
    no_check: bool,
}

fn parse_connect(args: &[String]) -> Result<ConnectArgs, String> {
    let mut out = ConnectArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--token" => out.token = Some(value("--token")?),
            "--admin-token" => out.admin_token = Some(value("--admin-token")?),
            "--no-admin-token" => out.no_admin = true,
            "--no-check" => out.no_check = true,
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            url if out.url.is_empty() => out.url = url.to_string(),
            extra => return Err(format!("one address only, not also {extra}")),
        }
    }
    if out.url.is_empty() {
        return Err(format!("the Worker's address is missing\n{USAGE}"));
    }
    if out.token.as_deref() == Some("-") && out.admin_token.as_deref() == Some("-") {
        return Err("only one token can come from stdin".into());
    }
    if out.no_admin && out.admin_token.is_some() {
        return Err("--admin-token and --no-admin-token together".into());
    }
    Ok(out)
}

fn stdin_line() -> Result<String, String> {
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("could not read stdin: {e}"))?;
    Ok(line.trim().to_string())
}

/// A flag's token, `-` read from stdin, else the environment variable.
fn token_from(flag: Option<String>, env: &str) -> Result<Option<String>, String> {
    match flag.as_deref() {
        Some("-") => stdin_line().map(Some),
        Some(_) => Ok(flag),
        None => Ok(std::env::var(env).ok().filter(|t| !t.trim().is_empty())),
    }
}

fn connect_cli(args: &[String]) -> Result<String, String> {
    let a = parse_connect(args)?;
    let token = token_from(a.token, "ARAKUN_PILOT_TOKEN")?;
    let admin = if a.no_admin {
        Some(String::new())
    } else {
        token_from(a.admin_token, "ARAKUN_PILOT_ADMIN_TOKEN")?
    };
    let draft = Draft {
        url: a.url,
        token: token.unwrap_or_default(),
        admin_token: admin,
    };
    let cfg = settings(&draft, pilot::load_config().as_ref())?;
    let checked = if a.no_check {
        None
    } else {
        Some(check(&cfg).map_err(|e| format!("{e}\nNothing was saved; --no-check saves anyway."))?)
    };
    let path = save(&cfg).map_err(|e| format!("could not save the settings: {e}"))?;
    let mut msg = format!("Saved {}.", path.display());
    if let Some(c) = checked {
        msg.push_str(&format!(" {}", found(&c)));
    }
    if cfg.admin_token.is_none() {
        msg.push_str(" Without an admin token Arakun can't send pilot tasks.");
    }
    msg.push_str(" A running Arakun picks it up within a minute.");
    Ok(msg)
}

/// What a check found, in one sentence.
pub fn found(c: &Checked) -> String {
    let cards = match c.cards {
        1 => "1 open card".to_string(),
        n => format!("{n} open cards"),
    };
    if c.board {
        format!("The Worker answered: {cards} and a board.")
    } else {
        format!("The Worker answered: {cards}, no board (its cards show under \"Your agent\").")
    }
}

fn status_cli() -> Result<String, String> {
    let path = pilot::config_file();
    let Ok(bytes) = std::fs::read(&path) else {
        return Err(format!(
            "not connected ({} doesn't exist). Connect with: arakun github pilot connect <url> --token -",
            path.display()
        ));
    };
    let cfg = pilot::parse_config(&bytes).ok_or_else(|| {
        format!(
            "{} can't be read: it needs \"url\" (https://) and \"token\". Connect again to rewrite it.",
            path.display()
        )
    })?;
    let admin = if cfg.admin_token.is_some() {
        "yes"
    } else {
        "no"
    };
    let head = format!(
        "Settings: {}\nWorker: {}\nAdmin token (send tasks): {admin}",
        path.display(),
        cfg.url
    );
    match check(&cfg) {
        Ok(c) => Ok(format!("{head}\n{}", found(&c))),
        Err(e) => Err(format!("{head}\n{e}")),
    }
}

fn disconnect_cli() -> Result<String, String> {
    match remove() {
        Ok(true) => Ok(format!("Removed {}.", pilot::config_file().display())),
        Ok(false) => Ok("Not connected: there was nothing to remove.".into()),
        Err(e) => Err(format!("could not remove the settings: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> Config {
        Config {
            url: "https://p.dev".into(),
            token: "old".into(),
            admin_token: Some("adm".into()),
        }
    }

    fn draft(url: &str, token: &str, admin: Option<&str>) -> Draft {
        Draft {
            url: url.into(),
            token: token.into(),
            admin_token: admin.map(Into::into),
        }
    }

    #[test]
    fn an_empty_token_keeps_the_saved_one_for_the_same_address() {
        let cfg = settings(&draft("https://p.dev/", "", None), Some(&saved())).unwrap();
        assert_eq!(cfg, saved());
        let err = settings(&draft("https://other.dev", "", None), Some(&saved())).unwrap_err();
        assert!(err.contains("token"), "{err}");
    }

    #[test]
    fn the_admin_token_can_be_dropped_or_replaced() {
        let s = saved();
        let dropped = settings(&draft("https://p.dev", "", Some("")), Some(&s)).unwrap();
        assert_eq!(dropped.admin_token, None);
        let new = settings(&draft("https://p.dev", " t ", Some(" a2 ")), Some(&s)).unwrap();
        assert_eq!(new.token, "t");
        assert_eq!(new.admin_token.as_deref(), Some("a2"));
    }

    #[test]
    fn only_https_or_this_machine_takes_the_tokens() {
        assert!(url_problem("https://p.dev").is_none());
        assert!(url_problem("http://localhost:8787").is_none());
        assert!(url_problem("http://p.dev").unwrap().contains("https"));
        assert!(url_problem("p.dev").is_some());
        assert!(url_problem("https://").is_some());
        assert!(settings(&draft("http://p.dev", "t", None), None).is_err());
    }

    #[test]
    fn reads_the_connect_flags() {
        let args: Vec<String> = ["https://p.dev", "--token", "-", "--no-check"]
            .map(String::from)
            .to_vec();
        let a = parse_connect(&args).unwrap();
        assert_eq!(a.url, "https://p.dev");
        assert_eq!(a.token.as_deref(), Some("-"));
        assert!(a.no_check);
        let both = ["u", "--token", "-", "--admin-token", "-"].map(String::from);
        assert!(parse_connect(&both).is_err());
        assert!(parse_connect(&["--token".to_string()]).is_err());
        assert!(parse_connect(&["--bogus".to_string()]).is_err());
    }

    #[test]
    fn check_reads_the_cards_and_the_board() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let reply: &[u8] = if req.starts_with("GET /items") {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]"
                } else {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                let _ = stream.write_all(reply);
            }
        });
        let cfg = Config {
            url: format!("http://127.0.0.1:{port}"),
            token: "t".into(),
            admin_token: None,
        };
        assert_eq!(
            check(&cfg).unwrap(),
            Checked {
                cards: 0,
                board: false
            }
        );
    }

    #[test]
    fn a_refused_token_says_what_to_do() {
        assert!(refused(401).contains("token"));
        assert!(refused(404).contains("address"));
    }
}
