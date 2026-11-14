//! The rest of the account the token can read: certificates and Email Routing
//! per zone, Security Center findings, Containers, Worker deploys and crons,
//! members, Access apps and Stream.

use serde_json::Value;

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn time_of(v: &Value) -> u64 {
    v.as_str().and_then(crate::account::parse_rfc3339).unwrap_or(0)
}

fn items(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}

/// An edge certificate of a zone.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Cert {
    pub hosts: Vec<String>,
    /// active, pending_validation, expired...
    pub status: String,
    /// lets_encrypt, google...
    pub authority: String,
    /// universal, advanced, custom.
    pub kind: String,
    /// Unix seconds; 0 when not issued yet.
    pub expires: u64,
}

impl Cert {
    pub fn days_left(&self, now: u64) -> i64 {
        (self.expires as i64 - now as i64) / 86_400
    }
}

/// An Email Routing rule: mail to `to` goes to `targets`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct EmailRule {
    pub to: String,
    /// forward, worker, drop.
    pub action: String,
    pub targets: Vec<String>,
    pub enabled: bool,
}

/// A Security Center finding.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Insight {
    pub subject: String,
    /// waf_not_enabled, a_record_dangling...
    pub class: String,
    /// Critical, High, Moderate, Low.
    pub severity: String,
    pub since: u64,
    /// Path in the dashboard that fixes it.
    pub link: String,
    /// The zone it's about; empty for account-wide ones (MFA).
    #[serde(default)]
    pub zone_id: String,
    /// The DNS record it names, and what it points at.
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub content: String,
}

impl Insight {
    /// 0 for Critical up to 3 for Low.
    pub fn rank(&self) -> u8 {
        match self.severity.as_str() {
            "Critical" => 0,
            "High" => 1,
            "Moderate" => 2,
            _ => 3,
        }
    }

    /// `waf_not_enabled` → "WAF not enabled".
    pub fn title(&self) -> String {
        let t = self.class.replace('_', " ");
        let t = t.replace("waf", "WAF").replace("hsts", "HSTS").replace("dmarc", "DMARC").replace("tls", "TLS").replace("https", "HTTPS").replace("cname", "CNAME").replace("mfa", "MFA");
        let mut c = t.chars();
        c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Container {
    pub name: String,
    pub instances: u64,
    pub max_instances: u64,
    pub healthy: u64,
    pub failed: u64,
    pub vcpu: f64,
    pub memory: String,
    pub version: u64,
    pub updated: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Member {
    pub email: String,
    pub status: String,
    pub roles: Vec<String>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AccessApp {
    pub name: String,
    pub domain: String,
    pub kind: String,
}

/// A charge or invoice line from the billing history.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Charge {
    pub when: u64,
    pub amount: f64,
    pub currency: String,
    pub description: String,
}

pub(super) fn parse_charges(v: &Value) -> Vec<Charge> {
    items(&v["result"])
        .map(|c| Charge {
            when: time_of(&c["occurred_at"]),
            amount: c["amount"].as_f64().unwrap_or(0.),
            currency: str_of(&c["currency"]),
            description: str_of(&c["description"]),
        })
        .collect()
}

/// "Spend this period: $16.83" in a budget alert → 16.83.
pub fn spend_in(detail: &str) -> Option<f64> {
    let i = detail.find('$')?;
    detail[i + 1..].split(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).next()?.replace(',', "").parse().ok()
}

pub(super) fn parse_certs(v: &Value) -> Vec<Cert> {
    let mut out: Vec<Cert> = items(&v["result"])
        .map(|p| {
            let expires = items(&p["certificates"]).map(|c| time_of(&c["expires_on"])).filter(|t| *t > 0).min().unwrap_or(0);
            Cert {
                hosts: items(&p["hosts"]).map(str_of).collect(),
                status: str_of(&p["status"]),
                authority: str_of(&p["certificate_authority"]),
                kind: str_of(&p["type"]),
                expires,
            }
        })
        .collect();
    out.sort_by_key(|c| if c.expires == 0 { u64::MAX } else { c.expires });
    out
}

/// Whether Email Routing is on for the zone.
pub(super) fn parse_email_enabled(v: &Value) -> Option<bool> {
    v["result"]["enabled"].as_bool()
}

pub(super) fn parse_email_rules(v: &Value) -> Vec<EmailRule> {
    items(&v["result"])
        .map(|r| {
            let to = items(&r["matchers"]).find(|m| m["field"] == "to").map(|m| str_of(&m["value"])).unwrap_or_else(|| "everything else".into());
            let action = items(&r["actions"]).next();
            EmailRule {
                to,
                action: action.map(|a| str_of(&a["type"])).unwrap_or_default(),
                targets: action.map(|a| items(&a["value"]).map(str_of).collect()).unwrap_or_default(),
                enabled: r["enabled"].as_bool().unwrap_or(true),
            }
        })
        .collect()
}

/// The findings not dismissed, worst first, and how many there are in all.
pub(super) fn parse_insights(v: &Value) -> (Vec<Insight>, u64) {
    let r = &v["result"];
    let mut out: Vec<Insight> = items(&r["issues"])
        .filter(|i| !i["dismissed"].as_bool().unwrap_or(false))
        .map(|i| Insight {
            subject: str_of(&i["subject"]),
            class: str_of(&i["issue_class"]),
            severity: str_of(&i["severity"]),
            since: time_of(&i["since"]),
            link: str_of(&i["resolve_link"]),
            zone_id: str_of(&i["payload"]["zone_tag"]),
            domain: str_of(&i["payload"]["domain"]),
            content: str_of(&i["payload"]["dns_record_content"]),
        })
        .collect();
    out.sort_by(|a, b| a.rank().cmp(&b.rank()).then_with(|| a.subject.cmp(&b.subject)));
    (out, r["count"].as_u64().unwrap_or(0))
}

pub(super) fn parse_containers(v: &Value) -> Vec<Container> {
    items(&v["result"])
        .map(|c| {
            let h = &c["health"]["instances"];
            Container {
                name: str_of(&c["name"]),
                instances: c["instances"].as_u64().unwrap_or(0),
                max_instances: c["max_instances"].as_u64().unwrap_or(0),
                healthy: h["healthy"].as_u64().unwrap_or(0),
                failed: h["failed"].as_u64().unwrap_or(0),
                vcpu: c["configuration"]["vcpu"].as_f64().unwrap_or(0.),
                memory: str_of(&c["configuration"]["memory"]),
                version: c["version"].as_u64().unwrap_or(0),
                updated: time_of(&c["updated_at"]),
            }
        })
        .collect()
}

pub(super) fn parse_members(v: &Value) -> Vec<Member> {
    items(&v["result"])
        .map(|m| Member {
            email: str_of(&m["email"]),
            status: str_of(&m["status"]),
            roles: items(&m["roles"]).map(|r| str_of(&r["name"])).collect(),
        })
        .collect()
}

pub(super) fn parse_access(v: &Value) -> Vec<AccessApp> {
    items(&v["result"]).map(|a| AccessApp { name: str_of(&a["name"]), domain: str_of(&a["domain"]), kind: str_of(&a["type"]) }).collect()
}

/// The newest deploy of a Worker: who and the version number it serves.
pub(super) fn parse_deploy(v: &Value) -> (String, String) {
    let d = items(&v["result"]["deployments"]).next();
    (d.map(|d| str_of(&d["author_email"])).unwrap_or_default(), d.map(|d| str_of(&d["source"])).unwrap_or_default())
}

pub(super) fn parse_crons(v: &Value) -> Vec<String> {
    items(&v["result"]["schedules"]).map(|s| str_of(&s["cron"])).filter(|c| !c.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_rest() {
        let certs = parse_certs(&json!({"result": [
            {"type": "universal", "status": "active", "certificate_authority": "lets_encrypt", "hosts": ["a.dev"], "certificates": [{"expires_on": "2027-01-22T18:51:11Z"}]},
            {"type": "advanced", "status": "pending_validation", "hosts": ["b.dev"], "certificates": []}]}));
        assert_eq!((certs[0].kind.as_str(), certs[1].expires), ("universal", 0));
        let rules = parse_email_rules(&json!({"result": [{"matchers": [{"type": "literal", "field": "to", "value": "k@h.mx"}], "actions": [{"type": "forward", "value": ["k@gmail.com"]}], "enabled": true}]}));
        assert_eq!((rules[0].to.as_str(), rules[0].targets[0].as_str()), ("k@h.mx", "k@gmail.com"));
        let (ins, total) = parse_insights(&json!({"result": {"count": 3, "issues": [
            {"subject": "b.dev", "issue_class": "dmarc", "severity": "Low", "dismissed": false},
            {"subject": "a.dev", "issue_class": "waf_not_enabled", "severity": "Critical", "dismissed": false},
            {"subject": "c.dev", "issue_class": "x", "severity": "Low", "dismissed": true}]}}));
        assert_eq!((ins.len(), total, ins[0].title()), (2, 3, "WAF not enabled".to_string()));
        assert_eq!(spend_in("Spend this period: $1,016.83"), Some(1016.83));
        assert_eq!(parse_crons(&json!({"result": {"schedules": [{"cron": "*/5 * * * *"}]}})), vec!["*/5 * * * *".to_string()]);
    }
}
