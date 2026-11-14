//! Reading the snapshot: what needs attention, the prompts that hand a
//! problem to an agent session, and how numbers are written.

use super::*;

/// What an issue is about, to build its prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    Worker(String),
    Zone(String),
    Gateway(String),
    Workflow(String),
    Queue(String),
    Tunnel(String),
    /// DNS records of a zone that route nowhere.
    Dns(String),
    /// A zone's SSL/TLS settings.
    Ssl(String),
    /// A domain registered through Cloudflare.
    Registration(String),
    Pages(String),
    /// `cloudflared` on this machine.
    Cloudflared,
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub high: bool,
    /// Errors that bots probing for files cause; nothing a visitor sees.
    pub bots: bool,
    pub title: String,
    pub detail: String,
    pub subject: Subject,
}

/// An agent session to start: its tab title, first prompt, and the Workers whose
/// project should host it.
#[derive(Clone, Debug)]
pub struct Ask {
    pub title: String,
    pub prompt: String,
    pub workers: Vec<String>,
}

/// What a 5xx Cloudflare answers with means.
pub fn status_meaning(code: u16) -> &'static str {
    match code {
        500 => "the Worker or origin threw an error",
        502 => "the origin sent an invalid response",
        503 => "the origin is unavailable or overloaded",
        504 => "the origin or a subrequest timed out",
        520 => "the origin returned an unexpected response",
        521 => "the origin refused the connection",
        522 => "the connection to the origin timed out",
        523 => "the origin is unreachable",
        524 => "the origin took too long to answer",
        525 => "the SSL handshake with the origin failed",
        526 => "the origin's SSL certificate isn't valid",
        530 => "the origin can't be reached, often a Cloudflare Tunnel that's down",
        _ => "server error",
    }
}

/// The share of 5xx at which a domain counts as failing.
const FAILING_PCT: u64 = 2;

/// A gateway whose last request is older than this has no caller left, so its
/// failures are history, not something to fix.
const GATEWAY_STALE_SECS: u64 = 7 * 86_400;

impl Snapshot {
    /// Problems worth a look, the worst first.
    pub fn issues(&self) -> Vec<Issue> {
        let mut out = Vec::new();
        for w in self.workers.iter().filter(|w| w.errors > 0) {
            let errors = format!("{} {}", count(w.errors), plural(w.errors, "error", "errors"));
            let issue = if w.bot_noise() {
                Issue {
                    high: false,
                    bots: true,
                    title: format!("{}: {errors} in 24 h, from bots probing for files", w.name),
                    detail: format!("Scanners asked for {}. Visitors aren't affected.", w.probed().iter().take(3).cloned().collect::<Vec<_>>().join(", ")),
                    subject: Subject::Worker(w.name.clone()),
                }
            } else {
                Issue {
                    high: w.errors >= 10,
                    bots: false,
                    title: format!("{} logged {errors} in 24 h", w.name),
                    detail: w.samples.first().map_or_else(|| "Open the Worker's logs to see them.".into(), |s| one_line(&s.message)),
                    subject: Subject::Worker(w.name.clone()),
                }
            };
            out.push(issue);
        }
        for z in &self.zones {
            // Errors of hostnames since removed are fixed: they only wait to leave the window.
            let (bad, all, gone) = (z.live_server_errors(), z.served(), z.gone_errors());
            if bad == 0 || (bad < 20 && bad * 100 < all * FAILING_PCT) {
                continue;
            }
            let worst: Vec<(u16, u64)> = if gone > 0 {
                // By status, among the hostnames that still answer.
                let mut by: Vec<(u16, u64)> = Vec::new();
                for f in z.failing.iter().filter(|f| z.resolves(&f.host)) {
                    match by.iter_mut().find(|(c, _)| *c == f.status) {
                        Some((_, n)) => *n += f.count,
                        None => by.push((f.status, f.count)),
                    }
                }
                by.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                by
            } else {
                z.server_statuses()
            };
            let mut detail = worst.iter().take(2).map(|(c, n)| format!("{} × {c} ({})", count(*n), status_meaning(*c))).collect::<Vec<_>>().join(" · ");
            if gone > 0 {
                detail.push_str(&format!(" · {} more from hostnames since removed", count(gone)));
            }
            out.push(Issue {
                high: bad >= 100,
                bots: false,
                title: format!("{} answered {} server errors (5xx) in 24 h", z.name, count(bad)),
                detail,
                subject: Subject::Zone(z.name.clone()),
            });
        }
        for g in &self.gateways {
            let failed = g.failures();
            let Some(last) = g.recent.first() else { continue };
            if failed == 0 || last.ok || self.fetched.saturating_sub(last.when) > GATEWAY_STALE_SECS {
                continue;
            }
            let mut detail = format!("Latest: {} answered {} {}", last.model, last.status, ago(last.when, self.fetched));
            if last.status == 402 {
                detail.push_str(". Out of credit: add funds or use your own provider key.");
            }
            out.push(Issue {
                high: false,
                bots: false,
                title: format!("AI Gateway {}: {failed} of the last {} requests failed", g.id, g.recent.len()),
                detail,
                subject: Subject::Gateway(g.id.clone()),
            });
        }
        for f in self.flows.iter().filter(|f| f.errored > 0) {
            out.push(Issue {
                high: false,
                bots: false,
                title: format!("Workflow {} has {} errored {}", f.name, f.errored, plural(f.errored, "run", "runs")),
                detail: format!("Runs in the Worker {}", f.script),
                subject: Subject::Workflow(f.name.clone()),
            });
        }
        for q in self.queues.iter().filter(|q| q.paused) {
            out.push(Issue {
                high: false,
                bots: false,
                title: format!("Queue {} is paused", q.name),
                detail: "Messages wait until delivery is resumed.".into(),
                subject: Subject::Queue(q.name.clone()),
            });
        }
        self.network_issues(&mut out);
        out.sort_by_key(|i| (!i.high, i.bots));
        out
    }

    /// Tunnels, DNS, SSL, registrations, Pages and the local `cloudflared`.
    fn network_issues(&self, out: &mut Vec<Issue>) {
        let latest = self.latest_cloudflared();
        for t in &self.tunnels {
            if !t.up() && !t.hostnames.is_empty() {
                // Down for weeks is a leftover to clean up; down this week is an outage.
                let days = if t.inactive_at > 0 { self.fetched.saturating_sub(t.inactive_at) / 86_400 } else { u64::MAX };
                let routes = if t.hostnames.len() == 1 { "routes" } else { "route" };
                let (high, title, detail) = if days <= 7 {
                    (
                        true,
                        format!("Tunnel {} is {}, but {} still {routes} to it", t.name, t.status, plural_list(&t.hostnames)),
                        "Visitors get a 530 error. Start its cloudflared again, or delete the DNS records if the site is gone.",
                    )
                } else {
                    (
                        false,
                        format!(
                            "Tunnel {} has been {} {}; {} still {routes} to it",
                            t.name,
                            t.status,
                            if days == u64::MAX { "since it was created".to_string() } else { format!("for {days} days") },
                            plural_list(&t.hostnames)
                        ),
                        "Those hostnames answer 530. Delete the DNS records (and the Tunnel) if the site is gone.",
                    )
                };
                out.push(Issue {
                    high,
                    bots: false,
                    title,
                    detail: detail.into(),
                    subject: Subject::Tunnel(t.name.clone()),
                });
            }
            let old: Vec<&Connector> = t.connectors.iter().filter(|c| outdated(&c.version, &latest)).collect();
            if let Some(c) = old.first() {
                out.push(Issue {
                    high: false,
                    bots: false,
                    title: format!("Tunnel {} runs cloudflared {}; the latest is {}", t.name, c.version, latest),
                    detail: format!("{} from {}. Update and restart it: releases often carry security fixes.", plural(old.len() as u64, "1 connector", &format!("{} connectors", old.len())), c.origin_ip),
                    subject: Subject::Tunnel(t.name.clone()),
                });
            }
        }
        let local = &self.cloudflared;
        if outdated(&local.installed, &latest) {
            out.push(Issue {
                high: false,
                bots: false,
                title: format!("cloudflared on this machine is {}; the latest is {latest}", local.installed),
                detail: "Update it before you run a Tunnel or share a local server from here.".into(),
                subject: Subject::Cloudflared,
            });
        }
        for z in &self.zones {
            let dangling: Vec<String> = self.dangling_tunnel_records().into_iter().filter(|r| z.dns.iter().any(|x| std::ptr::eq(x, *r))).map(|r| r.name.clone()).collect();
            if !dangling.is_empty() {
                out.push(Issue {
                    high: false,
                    bots: false,
                    title: format!("{}: {} {} to a deleted Tunnel", z.name, plural_list(&dangling), if dangling.len() == 1 { "points" } else { "point" }),
                    detail: "Those hostnames answer 530. Delete the records or route them to a live Tunnel.".into(),
                    subject: Subject::Dns(z.name.clone()),
                });
            }
            if z.status == "pending" {
                out.push(Issue {
                    high: false,
                    bots: false,
                    title: format!("{} isn't active yet", z.name),
                    detail: "Its nameservers still point elsewhere, so Cloudflare doesn't serve it.".into(),
                    subject: Subject::Dns(z.name.clone()),
                });
            }
            let st = &z.settings;
            if st.read && st.weak_ssl() && z.status == "active" {
                out.push(Issue {
                    high: st.ssl == "off",
                    bots: false,
                    title: format!("{} uses SSL mode {}", z.name, st.ssl_label()),
                    detail: "Cloudflare talks to the origin unencrypted. Use Full (strict) if the origin has a certificate.".into(),
                    subject: Subject::Ssl(z.name.clone()),
                });
            }
            if st.read && st.dev_mode {
                out.push(Issue {
                    high: false,
                    bots: false,
                    title: format!("{} has Development Mode on", z.name),
                    detail: "Nothing is cached while it's on; it turns itself off after 3 hours.".into(),
                    subject: Subject::Ssl(z.name.clone()),
                });
            }
        }
        for r in &self.registrations {
            let days = r.expires.saturating_sub(self.fetched) / 86_400;
            if r.expires > 0 && days <= 30 && !r.auto_renew {
                out.push(Issue {
                    high: true,
                    bots: false,
                    title: format!("{} expires in {days} days and won't renew", r.name),
                    detail: "Turn on auto-renew or renew it in Registrar.".into(),
                    subject: Subject::Registration(r.name.clone()),
                });
            }
        }
        for p in self.pages.iter().filter(|p| p.failed()) {
            out.push(Issue {
                high: false,
                bots: false,
                title: format!("Pages {}: the last deploy failed at {}", p.name, if p.stage.is_empty() { "an unknown stage" } else { &p.stage }),
                detail: format!("{}{}", if p.branch.is_empty() { String::new() } else { format!("Branch {} · ", p.branch) }, ago(p.deployed, self.fetched)),
                subject: Subject::Pages(p.name.clone()),
            });
        }
    }

    /// The Droid session for an issue.
    pub fn ask(&self, subject: &Subject, mcp: bool, now: u64) -> Option<Ask> {
        match subject {
            Subject::Worker(name) => {
                let w = self.worker(name)?;
                Some(Ask { title: format!("Worker {name}"), prompt: worker_prompt(w, mcp, now), workers: vec![name.clone()] })
            }
            Subject::Zone(name) => {
                let z = self.zones.iter().find(|z| &z.name == name)?;
                let workers = self.workers_on(z);
                Some(Ask {
                    title: format!("5xx on {name}"),
                    prompt: zone_prompt(z, &workers, mcp),
                    workers: workers.iter().map(|w| w.name.clone()).collect(),
                })
            }
            Subject::Gateway(id) => {
                let g = self.gateways.iter().find(|g| &g.id == id)?;
                Some(Ask { title: format!("AI Gateway {id}"), prompt: gateway_prompt(g, mcp, now), workers: Vec::new() })
            }
            Subject::Workflow(name) => {
                let f = self.flows.iter().find(|f| &f.name == name)?;
                let prompt = format!(
                    "The Cloudflare Workflow `{}` (Worker `{}`) has {} errored runs ({} complete, {} running).\n\nFind out why they fail and fix the cause in the code. {}",
                    f.name,
                    f.script,
                    f.errored,
                    f.complete,
                    f.running,
                    tools_hint(mcp, &format!("list the errored instances of the workflow `{}` and read their step errors", f.name))
                );
                Some(Ask { title: format!("Workflow {name}"), prompt, workers: vec![f.script.clone()] })
            }
            Subject::Tunnel(name) => {
                let t = self.tunnel(name)?;
                Some(Ask { title: format!("Tunnel {name}"), prompt: tunnel_prompt(t, &self.latest_cloudflared(), now), workers: Vec::new() })
            }
            Subject::Dns(zone) => {
                let z = self.zone(zone)?;
                let mut p = format!("Check the DNS of my Cloudflare zone `{}` (status {}). Records:\n", z.name, z.status);
                for r in z.dns.iter().take(60) {
                    let note = match r.tunnel() {
                        Some(id) => match self.tunnels.iter().find(|t| t.id == id) {
                            Some(t) => format!(" (Tunnel {}, {})", t.name, t.status),
                            None if self.tunnels_read => " (a Tunnel that no longer exists)".into(),
                            None => String::new(),
                        },
                        None => String::new(),
                    };
                    p.push_str(&format!("- {} {} → {}{}{}\n", r.kind, r.name, r.content, if r.proxied { " · proxied" } else { "" }, note));
                }
                p.push_str("\nFind records that route nowhere (deleted Tunnels, Workers or hosts that no longer answer), records that expose an origin IP without the proxy, and anything missing (like www, or mail records). Say what each finding breaks and the exact change. Don't change DNS without asking me first.");
                Some(Ask { title: format!("DNS of {zone}"), prompt: with_tools(p, mcp), workers: Vec::new() })
            }
            Subject::Ssl(zone) => {
                let z = self.zone(zone)?;
                let st = &z.settings;
                let p = format!(
                    "My Cloudflare zone `{}` has SSL/TLS mode {}, Always Use HTTPS {}, minimum TLS {} and Development Mode {}.\n\nCheck whether the origins behind it (DNS records, Workers, Tunnels) can take Full (strict), and what else should change. Don't change settings without asking me first.",
                    z.name,
                    st.ssl_label(),
                    on_off(st.always_https),
                    if st.min_tls.is_empty() { "unknown" } else { &st.min_tls },
                    on_off(st.dev_mode)
                );
                Some(Ask { title: format!("SSL of {zone}"), prompt: with_tools(p, mcp), workers: Vec::new() })
            }
            Subject::Registration(name) => {
                let r = self.registrations.iter().find(|r| &r.name == name)?;
                let p = format!(
                    "My domain `{}` is registered through Cloudflare and expires {} (auto-renew {}). Tell me what I need to do so it doesn't lapse; don't renew or pay for anything yourself.",
                    r.name,
                    date(r.expires),
                    on_off(r.auto_renew)
                );
                Some(Ask { title: format!("Domain {name}"), prompt: with_tools(p, mcp), workers: Vec::new() })
            }
            Subject::Pages(name) => {
                let pr = self.pages.iter().find(|p| &p.name == name)?;
                let p = format!(
                    "The latest deployment of my Cloudflare Pages project `{}`{} ended with status {} at the {} stage ({}).\n\nRead its build log, find why it failed and fix the cause in this project if the code lives here.",
                    pr.name,
                    if pr.repo.is_empty() { String::new() } else { format!(" (repo {}, branch {})", pr.repo, pr.branch) },
                    pr.status,
                    pr.stage,
                    ago(pr.deployed, now)
                );
                Some(Ask { title: format!("Pages {name}"), prompt: with_tools(p, mcp), workers: Vec::new() })
            }
            Subject::Cloudflared => {
                let p = format!(
                    "`cloudflared` on this machine is {} and the latest release is {}. Update it the way it was installed (check with `which -a cloudflared` and the package manager) and confirm `cloudflared --version`. If a system service or container runs it, restart that too. Ask me before anything that needs sudo.",
                    self.cloudflared.installed,
                    self.latest_cloudflared()
                );
                Some(Ask { title: "Update cloudflared".into(), prompt: p, workers: Vec::new() })
            }
            Subject::Queue(name) => {
                let q = self.queues.iter().find(|q| &q.name == name)?;
                let prompt = format!(
                    "The Cloudflare Queue `{}` has delivery paused (producers: {}; consumers: {}). Find out whether that's intended and what resuming it would do. Don't resume it without asking me.",
                    q.name,
                    list_or_none(&q.producers),
                    list_or_none(&q.consumers)
                );
                Some(Ask { title: format!("Queue {name}"), prompt, workers: q.consumers.clone() })
            }
        }
    }

    /// A session that reviews the whole account.
    pub fn review(&self, mcp: bool, now: u64) -> Ask {
        let mut p = format!("Review my Cloudflare account \"{}\". What Arakun sees right now:\n\n", self.account_name);
        let issues = self.issues();
        if issues.is_empty() {
            p.push_str("- No open problems.\n");
        }
        for i in &issues {
            p.push_str(&format!("- {}: {}\n", i.title, i.detail));
        }
        p.push_str("\nDomains (requests in 7 days · 5xx in 24 h · cache hit · SSL mode · DNS records):\n");
        for z in &self.zones {
            p.push_str(&format!(
                "- {} ({}): {} · {} · {}% · {} · {}\n",
                z.name,
                z.plan,
                count(z.requests()),
                count(z.server_errors()),
                z.cache_pct(),
                z.settings.ssl_label(),
                z.dns.len()
            ));
        }
        if !self.tunnels.is_empty() {
            p.push_str("\nTunnels (status · hostnames routed · connector versions):\n");
            for t in &self.tunnels {
                let versions: Vec<&str> = t.connectors.iter().map(|c| c.version.as_str()).collect();
                p.push_str(&format!("- {}: {} · {} · {}\n", t.name, t.status, list_or_none(&t.hostnames), if versions.is_empty() { "none".into() } else { versions.join(", ") }));
            }
        }
        p.push_str("\nBusiest Workers in 24 h (requests · CPU p99):\n");
        for w in self.workers.iter().filter(|w| w.requests > 0).take(8) {
            p.push_str(&format!("- {}: {} · {}\n", w.name, count(w.requests), ms(w.cpu_p99)));
        }
        let stored = self.r2_bytes();
        if stored > 0 {
            p.push_str(&format!("\nR2 stores {} (≈ ${:.0}/month at Standard rates); largest:\n", bytes(stored), self.r2_monthly()));
            for b in self.buckets.iter().take(4) {
                p.push_str(&format!("- {}: {} in {} objects\n", b.name, bytes(b.size), count(b.objects)));
            }
        }
        let _ = now;
        p.push_str(
            "\nGive me the five most valuable things to do, ordered by impact: reliability first, then security, then cost. For each, say what you found, why it matters and the exact change. ",
        );
        if mcp {
            p.push_str("Use the Cloudflare MCP server to dig into each one (DNS, Tunnels, SSL, Workers logs, R2 lifecycle rules). ");
        }
        p.push_str(CF_CLI);
        p.push(' ');
        p.push_str("This is a review: don't change anything in Cloudflare.");
        Ask { title: "Cloudflare review".into(), prompt: p, workers: Vec::new() }
    }
}

fn plural<'a>(n: u64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn list_or_none(v: &[String]) -> String {
    if v.is_empty() { "none".into() } else { v.join(", ") }
}

/// What a session can use to read the account.
const CF_CLI: &str = "The `cf` CLI on this machine is signed in to this account with read access to everything (find a command with `cf cli search \"<what you want to do>\"`; `cf schema <command>` shows the API call).";

fn tools_hint(mcp: bool, what: &str) -> String {
    if mcp {
        format!("The Cloudflare MCP server is connected: use it to {what}. {CF_CLI}")
    } else {
        format!("Use the `cf` CLI to {what}. {CF_CLI}")
    }
}

fn with_tools(mut p: String, mcp: bool) -> String {
    p.push_str("\n\n");
    p.push_str(CF_CLI);
    if mcp {
        p.push_str(" The Cloudflare MCP server is connected too.");
    }
    p
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// "a.dev", "a.dev and b.dev", "a.dev, b.dev and 3 more".
fn plural_list(v: &[String]) -> String {
    match v {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

/// The prompt about a Tunnel: down with hostnames routed to it, or connectors behind.
pub fn tunnel_prompt(t: &Tunnel, latest: &str, now: u64) -> String {
    let mut p = format!("My Cloudflare Tunnel `{}` ({}) is {}.", t.name, t.id, t.status);
    if t.hostnames.is_empty() {
        p.push_str(" No DNS record routes to it.");
    } else {
        p.push_str(&format!(" DNS routes {} to it.", t.hostnames.join(", ")));
    }
    if t.inactive_at > 0 && !t.up() {
        p.push_str(&format!(" Its last connector left {}.", ago(t.inactive_at, now)));
    }
    p.push_str(if t.remote { " It's configured from the dashboard." } else { " It's configured by a local config file (look in ~/.cloudflared)." });
    p.push('\n');
    for c in &t.connectors {
        p.push_str(&format!(
            "\n- connector cloudflared {} from {} (connected {}, data centers {}){}",
            c.version,
            c.origin_ip,
            ago(c.opened, now),
            c.colos.join(", "),
            if outdated(&c.version, latest) { format!(" — older than the latest {latest}") } else { String::new() }
        ));
    }
    p.push_str("\n\nIf it's down: find which machine or container should run it and why it stopped; if it's no longer needed, say which DNS records to delete. If connectors are outdated: say how to update each one without dropping traffic. Don't delete Tunnels or DNS records without asking me first.\n\n");
    p.push_str(CF_CLI);
    p
}

/// The prompt that hands a Worker's errors to Droid.
pub fn worker_prompt(w: &Worker, mcp: bool, now: u64) -> String {
    let mut p = if w.errors > 0 {
        format!("The Cloudflare Worker `{}` logged {} errors in the last 24 hours.", w.name, w.errors)
    } else {
        format!("Check the health of the Cloudflare Worker `{}`.", w.name)
    };
    if !w.domains.is_empty() {
        p.push_str(&format!(" It serves {}.", w.domains.join(", ")));
    }
    if w.requests > 0 {
        p.push_str(&format!(" In 24 h it handled {} requests; CPU per request p50 {}, p99 {}.", w.requests, ms(w.cpu_p50), ms(w.cpu_p99)));
    }
    if w.bot_noise() {
        p.push_str(&format!(
            " Nearly all of them come from bots probing for files that don't exist ({}), so visitors aren't affected.",
            w.probed().iter().take(6).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    p.push('\n');
    for (i, s) in w.samples.iter().take(3).enumerate() {
        let what = if s.url.is_empty() { s.trigger.clone() } else { format!("{} {}", s.trigger.split(' ').next().unwrap_or_default(), s.url) };
        p.push_str(&format!(
            "\n{} ({}, {}):\n```\n{}\n```\n",
            if i == 0 { "Latest error" } else { "Earlier error" },
            ago(s.when, now),
            what.trim(),
            s.message
        ));
    }
    if w.bot_noise() {
        p.push_str("\nIf this project doesn't hold the Worker's code, say so before changing anything. Check whether the zone already blocks these paths (a WAF custom rule) and whether real routes also log errors; if it's only bots, propose the smallest change that keeps them out of the Worker and its logs, without touching how real pages render.");
    } else {
        p.push_str("\nIf this project doesn't hold the Worker's code, say so before changing anything. Otherwise find the cause in the code and fix it.");
    }
    if mcp {
        p.push_str(" The Cloudflare MCP server is connected: use it to query this Worker's logs (Workers Observability) for more context.");
    } else {
        p.push_str(&format!(" For live logs you can run `npx wrangler tail {} --format pretty`.", w.name));
    }
    p.push(' ');
    p.push_str(CF_CLI);
    p
}

/// The prompt that asks Droid where a domain's 5xx come from.
pub fn zone_prompt(z: &Zone, workers: &[&Worker], mcp: bool) -> String {
    let mut p = format!(
        "The domain `{}` (Cloudflare zone, {} plan) answered {} server errors (5xx) out of {} requests in the last 24 hours.\n\nBy status:\n",
        z.name,
        z.plan,
        z.server_errors(),
        z.served()
    );
    for (code, n) in z.server_statuses() {
        p.push_str(&format!("- {n} × {code}: {}\n", status_meaning(code)));
    }
    if !z.failing.is_empty() {
        p.push_str("\nURLs with the most 5xx:\n");
        for f in &z.failing {
            p.push_str(&format!("- {} × {} https://{}{}\n", f.count, f.status, f.host, f.path));
        }
    }
    if !workers.is_empty() {
        p.push_str("\nWorkers on this domain:\n");
        for w in workers {
            p.push_str(&format!("- {} ({})\n", w.name, w.domains.join(", ")));
        }
    }
    p.push_str("\nFind out where each kind of error comes from and what fixes it. Tell real failures apart from bots probing for files that don't exist. ");
    if mcp {
        p.push_str("The Cloudflare MCP server is connected: use it to check the zone's DNS records, Cloudflare Tunnels, SSL/TLS mode and the Workers' logs. ");
    }
    p.push_str("Don't change Cloudflare settings without asking me first; if the fix is in this project's code, make it. ");
    p.push_str(CF_CLI);
    p
}

/// The prompt that asks Droid why AI Gateway requests fail.
pub fn gateway_prompt(g: &Gateway, mcp: bool, now: u64) -> String {
    let mut p = format!(
        "Requests through my Cloudflare AI Gateway `{}` are failing: {} of the last {} failed ({} logged in total).\n\nLatest requests:\n",
        g.id,
        g.failures(),
        g.recent.len(),
        g.logged
    );
    for c in g.recent.iter().take(8) {
        p.push_str(&format!("- {} {} → {} ({})\n", c.provider, c.model, c.status, ago(c.when, now)));
    }
    p.push_str("\nFind which app sends these requests and why they fail (for example, 401 is a bad key, 402 missing credits or billing, 429 a rate limit), then fix it if the cause is in this project. ");
    p.push_str(&tools_hint(mcp, &format!("read the gateway `{}`'s logs and settings", g.id)));
    p
}

/// "just now", "4m ago", "2h ago", "3d ago".
pub fn ago(t: u64, now: u64) -> String {
    let d = now.saturating_sub(t);
    if d >= 86_400 { format!("{}d ago", d / 86_400) } else { crate::account::ago(t, now) }
}

/// 950 → "950", 12_643 → "12.6k", 3_400_000 → "3.4M".
pub fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => trim_unit(n as f64 / 1e3, "k"),
        _ => trim_unit(n as f64 / 1e6, "M"),
    }
}

pub fn bytes(n: u64) -> String {
    match n {
        0 => "0 B".into(),
        1..1_000_000 => format!("{} KB", n.div_ceil(1000)),
        1_000_000..1_000_000_000 => trim_unit(n as f64 / 1e6, " MB"),
        1_000_000_000..1_000_000_000_000 => trim_unit(n as f64 / 1e9, " GB"),
        _ => trim_unit(n as f64 / 1e12, " TB"),
    }
}

/// Milliseconds: "0.4 ms", "19 ms", "1.2 s".
pub fn ms(v: f64) -> String {
    if v >= 1000. {
        format!("{} s", trim_unit(v / 1000., "").trim())
    } else if v >= 10. {
        format!("{v:.0} ms")
    } else {
        format!("{v:.1} ms")
    }
}

fn trim_unit(v: f64, unit: &str) -> String {
    if v >= 100. { format!("{v:.0}{unit}") } else { format!("{}{unit}", format!("{v:.1}").trim_end_matches(".0")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            account_name: "Acme".into(),
            fetched: 1_000_000,
            zones: vec![
                Zone {
                    name: "example.com".into(),
                    plan: "Free".into(),
                    statuses: vec![(200, 2329), (530, 1446), (504, 259)],
                    failing: vec![Failing { host: "dev.example.com".into(), path: "/x".into(), status: 530, count: 37 }],
                    ..Default::default()
                },
                Zone { name: "quiet.dev".into(), statuses: vec![(200, 1000), (500, 3)], ..Default::default() },
            ],
            workers: vec![
                Worker {
                    name: "blog".into(),
                    errors: 108,
                    domains: vec!["blog.example.com".into()],
                    samples: vec![ErrorSample { message: "boom   at x\nmore".into(), when: 999_880, trigger: "GET /p".into(), url: "https://blog.example.com/p".into() }],
                    ..Default::default()
                },
                Worker { name: "api".into(), errors: 1, ..Default::default() },
                Worker {
                    name: "site".into(),
                    errors: 40,
                    samples: vec![
                        ErrorSample { url: "https://k.xyz/.env".into(), ..Default::default() },
                        ErrorSample { url: "https://k.xyz/wp-login.php".into(), ..Default::default() },
                    ],
                    ..Default::default()
                },
            ],
            gateways: vec![Gateway {
                id: "default".into(),
                logged: 8,
                recent: vec![AiCall { model: "claude".into(), provider: "anthropic".into(), status: 402, when: 999_000, ..Default::default() }],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn issues_rank_and_explain() {
        let s = snapshot();
        let issues = s.issues();
        let titles: Vec<&str> = issues.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "blog logged 108 errors in 24 h",
                "example.com answered 1.7k server errors (5xx) in 24 h",
                "api logged 1 error in 24 h",
                "AI Gateway default: 1 of the last 1 requests failed",
                "site: 40 errors in 24 h, from bots probing for files",
            ]
        );
        assert_eq!(issues[4].detail, "Scanners asked for /.env, /wp-login.php. Visitors aren't affected.");
        let site = s.ask(&Subject::Worker("site".into()), true, s.fetched).unwrap();
        assert!(site.prompt.contains("bots probing for files that don't exist (/.env, /wp-login.php)"));
        assert!(site.prompt.contains("WAF custom rule"));
        assert_eq!(issues[0].detail, "boom at x");
        assert!(issues[1].detail.starts_with("1.4k × 530 (the origin can't be reached"));
        assert!(issues[3].detail.ends_with("Out of credit: add funds or use your own provider key."));
    }

    #[test]
    fn network_issues() {
        let record = |name: &str, content: &str| DnsRecord { id: String::new(), kind: "CNAME".into(), name: name.into(), content: content.into(), proxied: true };
        let s = Snapshot {
            fetched: 1_000_000,
            tunnels_read: true,
            cloudflared: LocalCloudflared { installed: "2026.9.3".into(), latest: "2026.10.4".into() },
            tunnels: vec![
                Tunnel { id: "t1".into(), name: "dev".into(), status: "down".into(), hostnames: vec!["dev.a.mx".into()], inactive_at: 1_000_000 - 3600, ..Default::default() },
                Tunnel { id: "t4".into(), name: "stale".into(), status: "down".into(), hostnames: vec!["s.a.mx".into()], inactive_at: 1_000_000 - 9 * 86_400, ..Default::default() },
                Tunnel {
                    id: "t2".into(),
                    name: "web".into(),
                    status: "healthy".into(),
                    connectors: vec![Connector { version: "2026.9.0".into(), origin_ip: "1.2.3.4".into(), ..Default::default() }],
                    ..Default::default()
                },
                Tunnel { id: "t3".into(), name: "old".into(), status: "inactive".into(), ..Default::default() },
            ],
            zones: vec![Zone {
                name: "a.mx".into(),
                status: "active".into(),
                dns: vec![record("dev.a.mx", "t1.cfargotunnel.com"), record("gone.a.mx", "t9.cfargotunnel.com")],
                settings: ZoneSettings { read: true, ssl: "flexible".into(), ..Default::default() },
                ..Default::default()
            }],
            registrations: vec![Registration { name: "a.mx".into(), expires: 1_000_000 + 10 * 86_400, auto_renew: false }],
            ..Default::default()
        };
        let titles: Vec<String> = s.issues().into_iter().map(|i| i.title).collect();
        assert_eq!(
            titles,
            vec![
                "Tunnel dev is down, but dev.a.mx still routes to it",
                "a.mx expires in 10 days and won't renew",
                "Tunnel stale has been down for 9 days; s.a.mx still routes to it",
                "Tunnel web runs cloudflared 2026.9.0; the latest is 2026.10.4",
                "cloudflared on this machine is 2026.9.3; the latest is 2026.10.4",
                "a.mx: gone.a.mx points to a deleted Tunnel",
                "a.mx uses SSL mode Flexible",
            ]
        );
        let ask = s.ask(&Subject::Tunnel("dev".into()), false, s.fetched).unwrap();
        assert!(ask.prompt.contains("DNS routes dev.a.mx to it") && ask.prompt.contains("`cf` CLI"));
        assert!(s.ask(&Subject::Dns("a.mx".into()), false, s.fetched).unwrap().prompt.contains("(a Tunnel that no longer exists)"));
    }

    #[test]
    fn old_gateway_failures_are_not_issues() {
        let mut s = snapshot();
        s.gateways[0].recent[0].when = s.fetched - 8 * 86_400;
        assert!(s.issues().iter().all(|i| i.subject != Subject::Gateway("default".into())));
    }

    #[test]
    fn prompts_carry_the_evidence() {
        let s = snapshot();
        let zone = s.ask(&Subject::Zone("example.com".into()), true, s.fetched).unwrap();
        assert_eq!(zone.workers, vec!["blog".to_string()]);
        assert!(zone.prompt.contains("1446 × 530"));
        assert!(zone.prompt.contains("37 × 530 https://dev.example.com/x"));
        assert!(zone.prompt.contains("Cloudflare Tunnels"));
        let worker = s.ask(&Subject::Worker("blog".into()), false, s.fetched).unwrap();
        assert!(worker.prompt.contains("`blog` logged 108 errors"));
        assert!(worker.prompt.contains("2m ago, GET https://blog.example.com/p"));
        assert!(worker.prompt.contains("wrangler tail blog"));
        let gw = s.ask(&Subject::Gateway("default".into()), true, s.fetched).unwrap();
        assert!(gw.prompt.contains("anthropic claude → 402"));
        let review = s.review(true, s.fetched);
        assert!(review.prompt.contains("example.com (Free)"));
        assert!(review.prompt.ends_with("don't change anything in Cloudflare."));
    }

    #[test]
    fn formatting() {
        assert_eq!((count(950), count(12_643), count(3_400_000), count(1_000)), ("950".into(), "12.6k".into(), "3.4M".into(), "1k".into()));
        assert_eq!((bytes(89_526_865), bytes(887_226_369_364), bytes(1_735_000_000_000), bytes(0)), ("89.5 MB".into(), "887 GB".into(), "1.7 TB".into(), "0 B".into()));
        assert_eq!((ms(0.42), ms(19.98), ms(525.6), ms(1240.)), ("0.4 ms".into(), "20 ms".into(), "526 ms".into(), "1.2 s".into()));
    }
}
