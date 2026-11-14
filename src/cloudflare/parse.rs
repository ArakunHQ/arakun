//! Turning Cloudflare's API answers into the tab's data.

use std::collections::HashMap;

use serde_json::Value;

use super::*;

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn time_of(v: &Value) -> u64 {
    v.as_str()
        .and_then(crate::account::parse_rfc3339)
        .unwrap_or(0)
}

fn items(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}

pub(super) fn parse_zones(v: &Value) -> Vec<Zone> {
    let mut zones: Vec<Zone> = items(&v["result"])
        .map(|z| Zone {
            id: str_of(&z["id"]),
            name: str_of(&z["name"]),
            plan: str_of(&z["plan"]["name"])
                .trim_end_matches(" Website")
                .to_string(),
            status: str_of(&z["status"]),
            ..Default::default()
        })
        .collect();
    zones.sort_by(|a, b| a.name.cmp(&b.name));
    zones
}

pub(super) fn parse_traffic(v: &Value, zones: &mut [Zone]) {
    for z in items(&v["data"]["viewer"]["zones"]) {
        let Some(zone) = zones
            .iter_mut()
            .find(|x| z["zoneTag"].as_str() == Some(x.id.as_str()))
        else {
            continue;
        };
        zone.days = items(&z["httpRequests1dGroups"])
            .map(|d| {
                let n = |k: &str| d["sum"][k].as_u64().unwrap_or(0);
                Day {
                    requests: n("requests"),
                    cached: n("cachedRequests"),
                    threats: n("threats"),
                    bytes: n("bytes"),
                    visitors: d["uniq"]["uniques"].as_u64().unwrap_or(0),
                }
            })
            .collect();
        let status = |g: &Value| {
            g["dimensions"]["edgeResponseStatus"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .unwrap_or(0)
        };
        zone.statuses = items(&z["statuses"])
            .map(|g| (status(g), g["count"].as_u64().unwrap_or(0)))
            .collect();
        zone.failing = items(&z["failing"])
            .map(|g| Failing {
                host: str_of(&g["dimensions"]["clientRequestHTTPHost"]),
                path: str_of(&g["dimensions"]["clientRequestPath"]),
                status: status(g),
                count: g["count"].as_u64().unwrap_or(0),
            })
            .collect();
        zone.bad_hosts = items(&z["bad_hosts"])
            .map(|g| {
                (
                    str_of(&g["dimensions"]["clientRequestHTTPHost"]),
                    g["count"].as_u64().unwrap_or(0),
                )
            })
            .collect();
    }
}

pub(super) fn parse_workers(scripts: &Value, domains: &Value) -> Vec<Worker> {
    items(&scripts["result"])
        .map(|s| {
            let name = str_of(&s["id"]);
            let domains = items(&domains["result"])
                .filter(|d| d["service"].as_str() == Some(name.as_str()))
                .map(|d| str_of(&d["hostname"]))
                .collect();
            Worker {
                deployed: time_of(&s["modified_on"]),
                deployed_from: str_of(&s["last_deployed_from"]),
                domains,
                name,
                ..Default::default()
            }
        })
        .collect()
}

/// The resource bindings of a Worker's settings; variables and secrets are left out.
pub(super) fn parse_bindings(v: &Value) -> Vec<Binding> {
    let mut out: Vec<Binding> = items(&v["result"]["bindings"])
        .filter_map(|b| {
            let name = str_of(&b["name"]);
            let or_name = |k: &str| b[k].as_str().map_or_else(|| name.clone(), str::to_string);
            let (kind, target) = match b["type"].as_str()? {
                "d1" => ("D1", name.clone()),
                "r2_bucket" => ("R2", or_name("bucket_name")),
                "kv_namespace" => ("KV", name.clone()),
                "durable_object_namespace" => ("Durable Object", or_name("class_name")),
                "workflow" => ("Workflow", or_name("workflow_name")),
                "queue" => ("Queue", or_name("queue_name")),
                "service" => ("Service", or_name("service")),
                "hyperdrive" => ("Hyperdrive", name.clone()),
                "vectorize" => ("Vectorize", or_name("index_name")),
                "ai" => ("Workers AI", name.clone()),
                "browser" => ("Browser", name.clone()),
                "send_email" => ("Email", name.clone()),
                "analytics_engine" => ("Analytics", or_name("dataset")),
                "images" => ("Images", name.clone()),
                "ratelimit" => ("Rate limit", name.clone()),
                _ => return None,
            };
            Some(Binding {
                kind: kind.into(),
                target,
                id: b["database_id"]
                    .as_str()
                    .or(b["id"].as_str())
                    .filter(|_| kind == "D1")
                    .map(str::to_string),
            })
        })
        .collect();
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.target.cmp(&b.target)));
    out.dedup();
    out
}

pub(super) fn apply_error_counts(v: &Value, workers: &mut [Worker]) {
    for a in items(&v["result"]["calculations"][0]["aggregates"]) {
        let service = a["groupKey"].as_str().unwrap_or_default();
        if let Some(w) = workers.iter_mut().find(|w| w.name == service) {
            w.errors = a["count"]
                .as_u64()
                .or_else(|| a["value"].as_f64().map(|f| f as u64))
                .unwrap_or(0);
        }
    }
}

pub(super) fn apply_error_samples(v: &Value, workers: &mut [Worker]) {
    for e in items(&v["result"]["events"]["events"]) {
        let meta = &e["$metadata"];
        let service = meta["service"]
            .as_str()
            .or(e["$workers"]["scriptName"].as_str())
            .unwrap_or_default();
        let Some(w) = workers.iter_mut().find(|w| w.name == service) else {
            continue;
        };
        if w.samples.len() >= 5 {
            continue;
        }
        let message = meta["error"]
            .as_str()
            .or(meta["message"].as_str())
            .unwrap_or_default();
        w.samples.push(ErrorSample {
            message: clip(message, 1500),
            when: e["timestamp"].as_u64().unwrap_or(0) / 1000,
            trigger: str_of(&meta["trigger"]),
            url: str_of(&e["$workers"]["event"]["request"]["url"]),
        });
    }
    for w in workers.iter_mut() {
        w.samples.sort_by_key(|s| std::cmp::Reverse(s.when));
    }
}

/// Workers' requests and CPU, R2 sizes and operations, and the D1 databases
/// that saw queries (unnamed: the API only gives their ids).
pub(super) fn apply_usage(
    v: &Value,
    workers: &mut [Worker],
    buckets: &mut Vec<Bucket>,
) -> Vec<Database> {
    let acc = &v["data"]["viewer"]["accounts"][0];
    for g in items(&acc["workers"]) {
        let name = g["dimensions"]["scriptName"].as_str().unwrap_or_default();
        if let Some(w) = workers.iter_mut().find(|w| w.name == name) {
            w.requests += g["sum"]["requests"].as_u64().unwrap_or(0);
            w.subrequests += g["sum"]["subrequests"].as_u64().unwrap_or(0);
            // Cloudflare reports CPU time in microseconds.
            w.cpu_p50 = w
                .cpu_p50
                .max(g["quantiles"]["cpuTimeP50"].as_f64().unwrap_or(0.) / 1000.);
            w.cpu_p99 = w
                .cpu_p99
                .max(g["quantiles"]["cpuTimeP99"].as_f64().unwrap_or(0.) / 1000.);
        }
    }
    for g in items(&acc["r2"]) {
        let name = str_of(&g["dimensions"]["bucketName"]);
        let (size, objects) = (
            g["max"]["payloadSize"].as_u64().unwrap_or(0),
            g["max"]["objectCount"].as_u64().unwrap_or(0),
        );
        match buckets.iter_mut().find(|b| b.name == name) {
            Some(b) => (b.size, b.objects) = (size, objects),
            None if !name.is_empty() => buckets.push(Bucket {
                name,
                size,
                objects,
                ..Default::default()
            }),
            None => {}
        }
    }
    for g in items(&acc["r2ops"]) {
        let name = g["dimensions"]["bucketName"].as_str().unwrap_or_default();
        let op = g["dimensions"]["actionType"].as_str().unwrap_or_default();
        let n = g["sum"]["requests"].as_u64().unwrap_or(0);
        if let Some(b) = buckets.iter_mut().find(|b| b.name == name) {
            if op.starts_with("Get") || op.starts_with("Head") {
                b.reads += n;
            } else if !op.starts_with("Delete") {
                b.writes += n;
            }
        }
    }
    let mut dbs: HashMap<String, Database> = HashMap::new();
    for g in items(&acc["d1"]) {
        let id = str_of(&g["dimensions"]["databaseId"]);
        let n = |k: &str| g["sum"][k].as_u64().unwrap_or(0);
        let db = dbs.entry(id.clone()).or_insert_with(|| Database {
            id,
            ..Default::default()
        });
        db.reads += n("readQueries");
        db.writes += n("writeQueries");
        db.rows_read += n("rowsRead");
        db.rows_written += n("rowsWritten");
    }
    let mut dbs: Vec<Database> = dbs.into_values().collect();
    dbs.sort_by_key(|d| std::cmp::Reverse(d.reads + d.writes));
    dbs
}

/// Fills in who uses each database (and adds the bound ones that saw no
/// queries), naming them from `names` (id → name) or after their binding.
pub(super) fn name_databases(
    dbs: &mut Vec<Database>,
    workers: &[Worker],
    names: &HashMap<String, String>,
) {
    for w in workers {
        for b in w.bindings.iter().filter(|b| b.kind == "D1") {
            let Some(id) = &b.id else { continue };
            let db = match dbs.iter_mut().position(|d| &d.id == id) {
                Some(i) => &mut dbs[i],
                None => {
                    dbs.push(Database {
                        id: id.clone(),
                        ..Default::default()
                    });
                    dbs.last_mut().expect("just pushed")
                }
            };
            if !db.used_by.contains(&w.name) {
                db.used_by.push(w.name.clone());
            }
            if db.name.is_empty() {
                db.name = format!("{} ({})", b.target, w.name);
            }
        }
    }
    for db in dbs.iter_mut() {
        if let Some(n) = names.get(&db.id) {
            db.name = n.clone();
        } else if db.name.is_empty() {
            db.name = db.id.chars().take(8).collect();
        }
    }
}

/// The account's D1 list: every database (queried or not), its real name and size.
pub(super) fn apply_d1_list(v: &Value, dbs: &mut Vec<Database>) {
    for d in items(&v["result"]) {
        let id = str_of(&d["uuid"]);
        let db = match dbs.iter_mut().position(|x| x.id == id) {
            Some(i) => &mut dbs[i],
            None => {
                dbs.push(Database {
                    id,
                    ..Default::default()
                });
                dbs.last_mut().expect("just pushed")
            }
        };
        if let Some(name) = d["name"].as_str() {
            db.name = name.to_string();
        }
        db.size = d["file_size"].as_u64().unwrap_or(0);
        db.tables = d["num_tables"].as_u64().unwrap_or(0);
    }
    dbs.sort_by(|a, b| {
        (b.reads + b.writes)
            .cmp(&(a.reads + a.writes))
            .then(b.size.cmp(&a.size))
            .then(a.name.cmp(&b.name))
    });
}

pub(super) fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

pub(super) fn parse_alerts(v: &Value) -> Vec<Alert> {
    items(&v["result"])
        .map(|a| {
            let body: Value = a["alert_body"]
                .as_str()
                .and_then(|b| serde_json::from_str(b).ok())
                .unwrap_or(Value::Null);
            let detail = match body["total_spend_dollars_formatted"].as_str() {
                Some(spend) => format!("Spend this period: {spend}"),
                None => {
                    let d = str_of(&a["description"]);
                    if d.is_empty() {
                        str_of(&a["alert_type"]).replace('_', " ")
                    } else {
                        d
                    }
                }
            };
            Alert {
                name: str_of(&a["name"]),
                detail,
                sent: time_of(&a["sent"]),
            }
        })
        .collect()
}

pub(super) fn parse_audit(v: &Value) -> Vec<Audit> {
    items(&v["result"])
        .map(|e| {
            let info = e["action"]["info"].as_str().map(str::to_string);
            let kind = capitalize(&str_of(&e["action"]["type"]).replace('_', " "));
            let resource = match (e["resource"]["type"].as_str(), e["resource"]["id"].as_str()) {
                (Some("account"), _) | (None, _) => String::new(),
                (Some(t), Some(id)) => format!("{t} {id}"),
                (Some(t), None) => t.to_string(),
            };
            let actor = e["actor"]["email"]
                .as_str()
                .or(e["actor"]["type"].as_str())
                .unwrap_or_default()
                .to_string();
            Audit {
                when: time_of(&e["when"]),
                actor,
                action: info.unwrap_or(kind),
                resource,
            }
        })
        .collect()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

pub(super) fn parse_buckets(v: &Value) -> Vec<Bucket> {
    items(&v["result"]["buckets"])
        .map(|b| Bucket {
            name: str_of(&b["name"]),
            created: time_of(&b["creation_date"]),
            ..Default::default()
        })
        .collect()
}

pub(super) fn parse_queues(v: &Value) -> Vec<Queue> {
    let scripts = |list: &Value| {
        items(list)
            .filter_map(|p| {
                p["script"]
                    .as_str()
                    .or(p["service"].as_str())
                    .map(str::to_string)
            })
            .collect()
    };
    items(&v["result"])
        .map(|q| Queue {
            name: str_of(&q["queue_name"]),
            producers: scripts(&q["producers"]),
            consumers: scripts(&q["consumers"]),
            paused: q["settings"]["delivery_paused"].as_bool() == Some(true),
            created: time_of(&q["created_on"]),
        })
        .collect()
}

pub(super) fn parse_flows(v: &Value) -> Vec<Flow> {
    items(&v["result"])
        .map(|f| {
            let n = |k: &str| f["instances"][k].as_u64().unwrap_or(0);
            Flow {
                name: str_of(&f["name"]),
                script: str_of(&f["script_name"]),
                running: n("running") + n("waiting"),
                queued: n("queued"),
                errored: n("errored"),
                complete: n("complete"),
                triggered: time_of(&f["triggered_on"]),
            }
        })
        .collect()
}

pub(super) fn parse_gateway(id: &str, logs: &Value) -> Gateway {
    let recent = items(&logs["result"])
        .map(|l| AiCall {
            when: time_of(&l["created_at"]),
            provider: str_of(&l["provider"]),
            model: str_of(&l["model"]),
            status: l["status_code"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .unwrap_or(0),
            ok: l["success"].as_bool().unwrap_or(false),
            cached: l["cached"].as_bool().unwrap_or(false),
            cost: l["cost"].as_f64().unwrap_or(0.),
            tokens_in: l["tokens_in"].as_u64().unwrap_or(0),
            tokens_out: l["tokens_out"].as_u64().unwrap_or(0),
        })
        .collect::<Vec<_>>();
    Gateway {
        id: id.to_string(),
        logged: logs["result_info"]["total_count"]
            .as_u64()
            .unwrap_or(recent.len() as u64),
        recent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn traffic_and_errors_parse() {
        let mut zones = parse_zones(&json!({"result": [
            {"id": "z2", "name": "b.dev", "plan": {"name": "Free Website"}, "status": "active"},
            {"id": "z1", "name": "a.dev", "plan": {"name": "Pro Website"}, "status": "active"}
        ]}));
        assert_eq!(zones[0].name, "a.dev");
        assert_eq!(zones[0].plan, "Pro");
        parse_traffic(
            &json!({"data": {"viewer": {"zones": [{"zoneTag": "z1",
                "httpRequests1dGroups": [
                    {"dimensions": {"date": "2026-10-18"}, "sum": {"requests": 100, "cachedRequests": 25, "threats": 3, "bytes": 2000}, "uniq": {"uniques": 10}},
                    {"dimensions": {"date": "2026-10-19"}, "sum": {"requests": 100, "cachedRequests": 25, "threats": 1, "bytes": 2000}, "uniq": {"uniques": 20}}
                ],
                "statuses": [{"count": 90, "dimensions": {"edgeResponseStatus": 200}}, {"count": 9, "dimensions": {"edgeResponseStatus": 530}}],
                "failing": [{"count": 9, "dimensions": {"clientRequestHTTPHost": "dev.a.dev", "clientRequestPath": "/x", "edgeResponseStatus": 530}}]
            }]}}}),
            &mut zones,
        );
        assert_eq!(
            (
                zones[0].requests(),
                zones[0].cache_pct(),
                zones[0].threats(),
                zones[0].visitors_per_day()
            ),
            (200, 25, 4, 15)
        );
        assert_eq!((zones[0].served(), zones[0].server_errors()), (99, 9));
        assert_eq!(zones[0].failing[0].host, "dev.a.dev");
        assert!(zones[1].days.is_empty());

        let mut workers = parse_workers(
            &json!({"result": [{"id": "blog", "modified_on": "2026-08-08T04:42:52.377252Z", "last_deployed_from": "wrangler"}, {"id": "api"}]}),
            &json!({"result": [{"hostname": "blog.a.dev", "service": "blog"}, {"hostname": "x.a.dev", "service": "other"}]}),
        );
        assert_eq!(workers[0].domains, vec!["blog.a.dev".to_string()]);
        assert!(workers[0].deployed > 0);
        apply_error_counts(
            &json!({"result": {"calculations": [{"aggregates": [{"groupKey": "blog", "count": 108}]}]}}),
            &mut workers,
        );
        apply_error_samples(
            &json!({"result": {"events": {"events": [
                {"timestamp": 1_792_457_582_215u64, "$metadata": {"service": "blog", "error": "boom\n at x", "trigger": "GET /p"}, "$workers": {"event": {"request": {"url": "https://blog.a.dev/p"}}}}
            ]}}}),
            &mut workers,
        );
        assert_eq!(workers[0].errors, 108);
        assert_eq!(workers[0].samples[0].when, 1_792_457_582);
    }

    #[test]
    fn usage_bindings_and_databases() {
        let mut workers = parse_workers(
            &json!({"result": [{"id": "api"}, {"id": "web"}]}),
            &Value::Null,
        );
        workers[0].bindings = parse_bindings(&json!({"result": {"bindings": [
            {"name": "DB", "type": "d1", "id": "d-1", "database_id": "d-1"},
            {"name": "FILES", "type": "r2_bucket", "bucket_name": "files"},
            {"name": "SECRET", "type": "secret_text"},
            {"name": "RC", "type": "durable_object_namespace", "class_name": "Room"}
        ]}}));
        assert_eq!(
            workers[0]
                .bindings
                .iter()
                .map(|b| format!("{} {}", b.kind, b.target))
                .collect::<Vec<_>>(),
            vec!["D1 DB", "Durable Object Room", "R2 files"]
        );
        let mut buckets = vec![Bucket {
            name: "files".into(),
            ..Default::default()
        }];
        let mut dbs = apply_usage(
            &json!({"data": {"viewer": {"accounts": [{
                "workers": [{"dimensions": {"scriptName": "api"}, "sum": {"requests": 3332, "subrequests": 40, "errors": 0}, "quantiles": {"cpuTimeP50": 19982.0, "cpuTimeP99": 525618.0}}],
                "r2": [{"dimensions": {"bucketName": "files"}, "max": {"payloadSize": 2_000_000_000u64, "objectCount": 7}}, {"dimensions": {"bucketName": "other"}, "max": {"payloadSize": 5, "objectCount": 1}}],
                "r2ops": [{"dimensions": {"bucketName": "files", "actionType": "GetObject"}, "sum": {"requests": 10}}, {"dimensions": {"bucketName": "files", "actionType": "PutObject"}, "sum": {"requests": 3}}, {"dimensions": {"bucketName": "files", "actionType": "DeleteObject"}, "sum": {"requests": 2}}],
                "d1": [{"dimensions": {"databaseId": "d-1"}, "sum": {"readQueries": 5, "writeQueries": 1, "rowsRead": 9, "rowsWritten": 1}}, {"dimensions": {"databaseId": "d-2"}, "sum": {"readQueries": 50, "writeQueries": 0, "rowsRead": 50, "rowsWritten": 0}}]
            }]}}}),
            &mut workers,
            &mut buckets,
        );
        assert_eq!(
            (
                workers[0].requests,
                workers[0].cpu_p50.round(),
                workers[0].cpu_p99.round()
            ),
            (3332, 20., 526.)
        );
        assert_eq!(
            (
                buckets[0].size,
                buckets[0].objects,
                buckets[0].reads,
                buckets[0].writes
            ),
            (2_000_000_000, 7, 10, 3)
        );
        assert_eq!(buckets.len(), 2);
        assert_eq!(dbs[0].id, "d-2");
        name_databases(
            &mut dbs,
            &workers,
            &HashMap::from([("d-2".to_string(), "prod".to_string())]),
        );
        assert_eq!(
            (dbs[0].name.as_str(), dbs[1].name.as_str()),
            ("prod", "DB (api)")
        );
        assert_eq!(dbs[1].used_by, vec!["api".to_string()]);
    }

    #[test]
    fn alerts_audit_queues_flows_gateways_parse() {
        let alerts = parse_alerts(
            &json!({"result": [{"name": "Billing Budget Alert", "alert_type": "billing_budget_alert",
            "alert_body": "{\"total_spend_dollars_formatted\":\"$16.83\"}", "sent": "2026-10-07T12:04:04.795235Z"}]}),
        );
        assert_eq!(alerts[0].detail, "Spend this period: $16.83");
        let audit = parse_audit(&json!({"result": [
            {"action": {"type": "token_revoke"}, "actor": {"email": "k@x.dev"}, "resource": {"type": "account", "id": "a"}, "when": "2026-10-17T00:19:01Z"},
            {"action": {"type": "script_on_subdomain", "info": "Worker script acme: previews disabled"}, "actor": {"email": "k@x.dev"}, "resource": {"type": "script", "id": "acme"}, "when": "2026-10-16T11:54:18.97974Z"}
        ]}));
        assert_eq!(
            (audit[0].action.as_str(), audit[0].resource.as_str()),
            ("Token revoke", "")
        );
        assert_eq!(
            (audit[1].action.as_str(), audit[1].resource.as_str()),
            ("Worker script acme: previews disabled", "script acme")
        );
        let queues = parse_queues(
            &json!({"result": [{"queue_name": "jobs", "settings": {"delivery_paused": true},
            "producers": [{"script": "api", "type": "worker"}], "consumers": [{"script": "worker", "type": "worker"}], "created_on": "2026-08-17T07:48:33Z"}]}),
        );
        assert_eq!(
            (
                queues[0].producers[0].as_str(),
                queues[0].consumers[0].as_str(),
                queues[0].paused
            ),
            ("api", "worker", true)
        );
        let flows = parse_flows(
            &json!({"result": [{"name": "render", "script_name": "app", "instances": {"running": 1, "waiting": 1, "errored": 2, "complete": 5}}]}),
        );
        assert_eq!(
            (flows[0].running, flows[0].errored, flows[0].complete),
            (2, 2, 5)
        );
        let g = parse_gateway(
            "default",
            &json!({"result": [
            {"created_at": "2026-10-05T03:11:12.860Z", "provider": "anthropic", "model": "anthropic/claude", "status_code": 402, "success": false},
            {"created_at": "2026-10-05T03:10:12.860Z", "provider": "openai", "model": "gpt", "status_code": 200, "success": true}
        ], "result_info": {"total_count": 8}}),
        );
        assert_eq!((g.logged, g.failures(), g.recent[0].status), (8, 1, 402));
    }
}
