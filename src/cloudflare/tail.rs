//! A Worker's live log: `wrangler tail --format json` (its sign-in can open the
//! tail; the `cf` one only reads), one event per request, cron or queue batch
//! the Worker runs, as it runs.

use std::io::BufReader;
use std::process::{Child, Stdio};
use std::sync::mpsc;

use serde_json::Value;

/// One run of the Worker.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TailEvent {
    /// Unix milliseconds.
    pub when: u64,
    /// ok, exception, exceededCpu, canceled...
    pub outcome: String,
    pub method: String,
    pub url: String,
    /// The cron that ran it, for scheduled events.
    pub cron: Option<String>,
    pub status: Option<u16>,
    pub country: String,
    pub cpu_ms: u64,
    /// (level, message) of each console call.
    pub logs: Vec<(String, String)>,
    /// "Name: message" of each uncaught exception.
    pub exceptions: Vec<String>,
}

impl TailEvent {
    pub fn failed(&self) -> bool {
        self.outcome != "ok" || !self.exceptions.is_empty() || self.status.is_some_and(|s| s >= 500)
    }

    /// The request path, or what triggered it.
    pub fn what(&self) -> String {
        if let Some(cron) = &self.cron {
            return format!("cron {cron}");
        }
        let path = self
            .url
            .split_once("://")
            .map_or(self.url.as_str(), |(_, r)| {
                r.find('/').map_or("/", |i| &r[i..])
            });
        format!("{} {}", self.method, path)
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(text).collect::<Vec<_>>().join(" "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn parse_event(v: &Value) -> TailEvent {
    let e = &v["event"];
    let r = &e["request"];
    TailEvent {
        when: v["eventTimestamp"].as_u64().unwrap_or(0),
        outcome: text(&v["outcome"]),
        method: text(&r["method"]),
        url: text(&r["url"]),
        cron: e["cron"].as_str().map(str::to_string),
        status: e["response"]["status"].as_u64().map(|s| s as u16),
        country: text(&r["cf"]["country"]),
        cpu_ms: v["cpuTime"].as_u64().unwrap_or(0),
        logs: v["logs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| (text(&l["level"]), text(&l["message"])))
            .collect(),
        exceptions: v["exceptions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| format!("{}: {}", text(&x["name"]), text(&x["message"])))
            .collect(),
    }
}

/// A running tail: events arrive on `events`; dropping it stops `wrangler`.
pub struct Tail {
    child: Child,
    pub events: mpsc::Receiver<TailEvent>,
}

impl Drop for Tail {
    fn drop(&mut self) {
        // npx starts wrangler as its own child: end the whole group.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGTERM);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `wrangler tail` for `worker` in `account`. `wrangler` from PATH,
/// else through npm's `npx`.
pub fn start(worker: &str, account: &str) -> Result<Tail, String> {
    let (program, mut args): (std::path::PathBuf, Vec<String>) =
        match crate::agent::which("wrangler") {
            Some(w) => (w, Vec::new()),
            None => {
                let npx =
                    crate::agent::which("npx").ok_or("Live logs need wrangler or Node's npx")?;
                (npx, vec!["-y".into(), "wrangler".into()])
            }
        };
    args.extend([
        "tail".into(),
        worker.into(),
        "--format".into(),
        "json".into(),
    ]);
    let mut cmd = crate::platform::command(program);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Its own process group, so stopping it ends npx's wrangler too.
        cmd.process_group(0);
        // And if Arakun dies without stopping it, Linux ends it.
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    let mut child = cmd
        .args(&args)
        .env("CLOUDFLARE_ACCOUNT_ID", account)
        .env("WRANGLER_SEND_METRICS", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start wrangler: {e}"))?;
    let stdout = child.stdout.take().ok_or("wrangler has no output")?;
    let (tx, events) = mpsc::channel();
    std::thread::spawn(move || {
        // Pretty-printed objects, one after another.
        for v in serde_json::Deserializer::from_reader(BufReader::new(stdout)).into_iter::<Value>()
        {
            let Ok(v) = v else { break };
            if tx.send(parse_event(&v)).is_err() {
                break;
            }
        }
    });
    Ok(Tail { child, events })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_events() {
        let e = parse_event(&json!({
            "outcome": "ok", "cpuTime": 3, "eventTimestamp": 1793081270432u64,
            "logs": [{"level": "log", "message": ["hit", 2]}],
            "exceptions": [],
            "event": {"request": {"url": "https://a.app/.well-known/x?y=1", "method": "GET", "cf": {"country": "US"}}, "response": {"status": 404}}
        }));
        assert_eq!(
            (e.what().as_str(), e.status, e.country.as_str(), e.failed()),
            ("GET /.well-known/x?y=1", Some(404), "US", false)
        );
        assert_eq!(e.logs, vec![("log".to_string(), "hit 2".to_string())]);
        let cron = parse_event(
            &json!({"outcome": "exception", "exceptions": [{"name": "Error", "message": "boom"}], "event": {"cron": "*/15 * * * *"}}),
        );
        assert_eq!(
            (
                cron.what().as_str(),
                cron.failed(),
                cron.exceptions[0].as_str()
            ),
            ("cron */15 * * * *", true, "Error: boom")
        );
    }
}
