//! Changes the tab can make itself, with what the sign-in may write (zone
//! settings and DNS): what each Security Center finding or broken Tunnel route
//! needs, said in plain words before it's applied.

use serde_json::{Value, json};

use super::*;

/// One change to the account, ready to apply.
#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    /// A zone setting (`always_use_https`, `min_tls_version`, `security_header`).
    Setting { zone_id: String, zone: String, setting: &'static str, value: Value, what: &'static str },
    /// Put a record behind Cloudflare's proxy.
    Proxy { zone_id: String, record_id: String, name: String },
    /// Delete records that route nowhere.
    Delete { zone_id: String, records: Vec<(String, String)> },
    /// A DMARC record that only reports (p=none).
    Dmarc { zone_id: String, domain: String },
}

impl Fix {
    /// Fixes that change the same thing share a key: one zone setting fixes
    /// every hostname of the zone Security Center lists.
    pub fn key(&self) -> String {
        match self {
            Fix::Setting { zone_id, setting, .. } => format!("{zone_id}/{setting}"),
            Fix::Proxy { record_id, .. } => format!("proxy/{record_id}"),
            Fix::Delete { records, .. } => format!("delete/{}", records.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>().join(",")),
            Fix::Dmarc { zone_id, .. } => format!("{zone_id}/dmarc"),
        }
    }

    /// The button.
    pub fn label(&self) -> &'static str {
        match self {
            Fix::Setting { .. } => "Turn on",
            Fix::Proxy { .. } => "Proxy it",
            Fix::Delete { .. } => "Delete records",
            Fix::Dmarc { .. } => "Add DMARC",
        }
    }

    /// What will change, said before it does.
    pub fn describe(&self) -> String {
        match self {
            Fix::Setting { zone, what, .. } => format!("{what} for every hostname of {zone}."),
            Fix::Proxy { name, .. } => format!("Route {name} through Cloudflare's proxy, hiding the server's address."),
            Fix::Delete { records, .. } => {
                let names: Vec<&str> = records.iter().map(|(_, n)| n.as_str()).collect();
                format!("Delete the DNS record{} {}.", if names.len() == 1 { "" } else { "s" }, names.join(", "))
            }
            Fix::Dmarc { domain, .. } => format!("Add _dmarc.{domain} = \"v=DMARC1; p=none\": mail receivers start reporting, nothing is rejected."),
        }
    }

    /// What could go wrong, when something could.
    pub fn warning(&self) -> Option<&'static str> {
        match self {
            Fix::Setting { setting: "security_header", .. } => {
                Some("Browsers then refuse plain HTTP to these hostnames for 6 months: every one must serve HTTPS.")
            }
            Fix::Setting { setting: "min_tls_version", .. } => Some("Very old browsers and devices (TLS 1.0/1.1 only) can no longer connect."),
            Fix::Proxy { .. } => Some("Only for websites: mail, SSH or a service that manages its own certificates (Vercel) needs the direct connection."),
            Fix::Delete { .. } => Some("The hostnames stop resolving. Recreate the records to bring them back."),
            _ => None,
        }
    }
}

/// The fix for a Security Center finding, when the sign-in can make it.
pub fn for_insight(i: &Insight, snap: &Snapshot) -> Option<Fix> {
    let zone = snap.zones.iter().find(|z| z.id == i.zone_id);
    let zone_name = zone.map_or_else(|| i.subject.clone(), |z| z.name.clone());
    let zone_id = i.zone_id.clone();
    if zone_id.is_empty() {
        return None;
    }
    let setting = |setting: &'static str, value: Value, what: &'static str| Some(Fix::Setting { zone_id: zone_id.clone(), zone: zone_name.clone(), setting, value, what });
    // The record a finding names, by name and (when given) content.
    let record = || {
        zone?.dns.iter().find(|r| r.name == i.domain && (i.content.is_empty() || r.content.trim_end_matches('.') == i.content.trim_end_matches('.')) && !r.id.is_empty())
    };
    match i.class.as_str() {
        "always_https_not_enabled" => setting("always_use_https", json!("on"), "Redirect http:// to https://"),
        "tls_version_old" => setting("min_tls_version", json!("1.2"), "Require TLS 1.2 or newer"),
        "hsts_not_enabled" => setting(
            "security_header",
            json!({"strict_transport_security": {"enabled": true, "max_age": 15_552_000, "include_subdomains": false, "preload": false, "nosniff": true}}),
            "Send HSTS (6 months, no subdomains, no preload)",
        ),
        "a_record_not_proxied" | "cname_record_not_proxied" => {
            let r = record()?;
            // Vercel (and hosts like it) issue their own certificates: behind the proxy they break.
            if ["vercel-dns", "vercel.app", "netlify", "github.io", "pages.dev"].iter().any(|h| r.content.contains(h)) {
                return None;
            }
            Some(Fix::Proxy { zone_id, record_id: r.id.clone(), name: r.name.clone() })
        }
        "a_record_dangling" => {
            let r = record()?;
            Some(Fix::Delete { zone_id, records: vec![(r.id.clone(), r.name.clone())] })
        }
        "dmarc" => {
            let domain = if i.domain.is_empty() { zone_name } else { i.domain.clone() };
            Some(Fix::Dmarc { zone_id, domain })
        }
        _ => None,
    }
}

/// Whether what `i` asks for is already so (Security Center rescans within a
/// day, so a finding outlives its fix for a while).
pub fn settled(i: &Insight, snap: &Snapshot) -> bool {
    let Some(z) = snap.zones.iter().find(|z| z.id == i.zone_id) else { return false };
    let st = &z.settings;
    let record = z.dns.iter().find(|r| r.name == i.domain && (i.content.is_empty() || r.content.trim_end_matches('.') == i.content.trim_end_matches('.')));
    match i.class.as_str() {
        "always_https_not_enabled" => st.read && st.always_https,
        "tls_version_old" => st.read && matches!(st.min_tls.as_str(), "1.2" | "1.3"),
        "hsts_not_enabled" => st.read && st.hsts,
        "a_record_not_proxied" | "cname_record_not_proxied" => !z.dns.is_empty() && record.is_none_or(|r| r.proxied),
        "a_record_dangling" => !z.dns.is_empty() && record.is_none(),
        "dmarc" => {
            let domain = if i.domain.is_empty() { z.name.clone() } else { i.domain.clone() };
            z.dns.iter().any(|r| r.kind == "TXT" && r.name == format!("_dmarc.{domain}"))
        }
        _ => false,
    }
}

/// For a Tunnel that's down or DNS routing to a deleted one: delete the records, zone by zone.
pub fn for_issue(subject: &Subject, snap: &Snapshot) -> Vec<Fix> {
    let mut out = Vec::new();
    for z in &snap.zones {
        let records: Vec<(String, String)> = match subject {
            Subject::Tunnel(name) => {
                let Some(t) = snap.tunnel(name).filter(|t| !t.up()) else { return out };
                let target = t.target();
                z.dns.iter().filter(|r| r.content == target && !r.id.is_empty()).map(|r| (r.id.clone(), r.name.clone())).collect()
            }
            Subject::Dns(zone) if *zone == z.name => snap
                .dangling_tunnel_records()
                .into_iter()
                .filter(|r| z.dns.iter().any(|x| std::ptr::eq(x, *r)) && !r.id.is_empty())
                .map(|r| (r.id.clone(), r.name.clone()))
                .collect(),
            _ => Vec::new(),
        };
        if !records.is_empty() {
            out.push(Fix::Delete { zone_id: z.id.clone(), records });
        }
    }
    out
}

/// Makes the change. Blocking.
pub fn apply(fix: &Fix) -> Result<(), String> {
    let s = quick_session().map(Ok).unwrap_or_else(whoami).map_err(|e| match e {
        LoadError::NoCli => "The cf CLI isn't installed".to_string(),
        LoadError::SignedOut(m) | LoadError::Failed(m) => m,
    })?;
    let t = s.token.as_str();
    match fix {
        Fix::Setting { zone_id, setting, value, .. } => call(t, "PATCH", &format!("{API}/zones/{zone_id}/settings/{setting}"), Some(&json!({ "value": value }))).map(|_| ()),
        Fix::Proxy { zone_id, record_id, .. } => {
            call(t, "PATCH", &format!("{API}/zones/{zone_id}/dns_records/{record_id}"), Some(&json!({ "proxied": true }))).map(|_| ())
        }
        Fix::Delete { zone_id, records } => {
            for (id, name) in records {
                call(t, "DELETE", &format!("{API}/zones/{zone_id}/dns_records/{id}"), None).map_err(|e| format!("{name}: {e}"))?;
            }
            Ok(())
        }
        Fix::Dmarc { zone_id, domain } => call(
            t,
            "POST",
            &format!("{API}/zones/{zone_id}/dns_records"),
            Some(&json!({ "type": "TXT", "name": format!("_dmarc.{domain}"), "content": "\"v=DMARC1; p=none\"", "ttl": 1 })),
        )
        .map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> Snapshot {
        let mut z = Zone { id: "z1".into(), name: "a.dev".into(), ..Default::default() };
        z.dns = vec![
            DnsRecord { id: "r1".into(), kind: "A".into(), name: "claw.a.dev".into(), content: "185.0.0.1".into(), proxied: false },
            DnsRecord { id: "r2".into(), kind: "CNAME".into(), name: "dev.a.dev".into(), content: "t1.cfargotunnel.com".into(), proxied: true },
        ];
        let tunnel = Tunnel { id: "t1".into(), name: "dev".into(), ..Default::default() };
        Snapshot { zones: vec![z], tunnels: vec![tunnel], tunnels_read: true, ..Default::default() }
    }

    fn insight(class: &str, domain: &str, content: &str) -> Insight {
        Insight { class: class.into(), subject: domain.into(), zone_id: "z1".into(), domain: domain.into(), content: content.into(), ..Default::default() }
    }

    #[test]
    fn findings_to_fixes() {
        let s = snap();
        let f = for_insight(&insight("always_https_not_enabled", "x.a.dev", ""), &s).unwrap();
        assert_eq!(f.key(), "z1/always_use_https");
        assert!(f.describe().contains("a.dev"));
        assert_eq!(for_insight(&insight("a_record_dangling", "claw.a.dev", "185.0.0.1"), &s).unwrap(), Fix::Delete { zone_id: "z1".into(), records: vec![("r1".into(), "claw.a.dev".into())] });
        assert!(matches!(for_insight(&insight("a_record_not_proxied", "claw.a.dev", "185.0.0.1"), &s), Some(Fix::Proxy { .. })));
        assert!(for_insight(&insight("waf_not_enabled", "a.dev", ""), &s).is_none());
        assert!(!settled(&insight("a_record_dangling", "claw.a.dev", "185.0.0.1"), &s));
        assert!(settled(&insight("a_record_dangling", "gone.a.dev", "1.2.3.4"), &s));
        assert!(for_insight(&Insight { zone_id: String::new(), ..insight("mfa_not_enabled", "", "") }, &s).is_none());
        // A Tunnel that's down: its records go.
        assert_eq!(for_issue(&Subject::Tunnel("dev".into()), &s), vec![Fix::Delete { zone_id: "z1".into(), records: vec![("r2".into(), "dev.a.dev".into())] }]);
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "reads this machine's Cloudflare cache"]
    fn plan() {
        let s = crate::cloudflare::cached().expect("cache");
        let mut seen = std::collections::HashSet::new();
        for i in &s.insights {
            let settled = super::settled(i, &s);
            match super::for_insight(i, &s) {
                Some(f) if !settled => {
                    if seen.insert(f.key()) {
                        eprintln!("FIX  {:<28} {:<40} {}", i.class, i.subject, f.describe());
                    }
                }
                _ => eprintln!("{} {:<28} {:<40} {}", if settled { "DONE" } else { "MANU" }, i.class, i.subject, i.content),
            }
        }
    }
}
