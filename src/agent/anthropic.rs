//! Anthropic's Messages API for the loop in [`workers_ai`](super::workers_ai). The loop keeps
//! its history and its requests in the Chat Completions shape; this file turns each request
//! into a Messages request and each reply stream back into Chat Completions chunks, so the
//! same loop drives a Claude model.
//!
//! What the Chat Completions shape cannot carry, the reply's own content blocks (thinking
//! blocks with their signatures), rides in the assistant message under [`NATIVE`] and goes
//! back verbatim on later calls, so the model keeps its reasoning across the tool loop.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::launch;

const API: &str = "https://api.anthropic.com";
pub const VERSION: &str = "2026-10-21";
/// `thinking.block_binding`: a history the loop edited (its prune knob, a model switch) drops
/// the thinking blocks after the edit instead of failing the call with a 400.
pub const BETA: &str = "thinking-binding-controls-2026-08-01";
/// The key, in an assistant message of the history, of the content blocks Anthropic sent.
pub const NATIVE: &str = "_anthropic";

// ---- the key -------------------------------------------------------------------------

pub fn key_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| launch::home().join(".config"))
        .join("arakun")
        .join("anthropic-key")
}

/// The Messages URL and the key for it: `ARAKUN_ANTHROPIC_KEY`, else the file at
/// [`key_path`]. `ARAKUN_ANTHROPIC_URL` names another base (a proxy that holds the key, as in
/// the bench's containers), and then the key may be missing. `ANTHROPIC_API_KEY` is left
/// alone on purpose: it belongs to whatever else runs in this environment.
pub fn endpoint() -> Result<(String, String), String> {
    let env = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let base = env("ARAKUN_ANTHROPIC_URL");
    let key = env("ARAKUN_ANTHROPIC_KEY")
        .or_else(|| std::fs::read_to_string(key_path()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .or_else(|| base.as_ref().map(|_| "arakun".to_string()))
        .ok_or_else(|| {
            format!(
                "Claude models in this loop need an Anthropic API key (pay per use). Put it in {} (one line, \
                 chmod 600) or set ARAKUN_ANTHROPIC_KEY.",
                key_path().display()
            )
        })?;
    let base = base.unwrap_or_else(|| API.to_string());
    Ok((format!("{}/v1/messages?beta=true", base.trim_end_matches('/')), key))
}

// ---- request: Chat Completions -> Messages -------------------------------------------

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().map(|p| p.as_str().or(p["text"].as_str()).unwrap_or("")).collect(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn or_text(text: String, empty: &str) -> String {
    if text.trim().is_empty() { empty.to_string() } else { text }
}

fn user_blocks(content: &Value) -> Vec<Value> {
    let Some(parts) = content.as_array() else {
        return vec![json!({"type": "text", "text": or_text(text_of(content), "(empty)")})];
    };
    let mut out = Vec::new();
    for p in parts {
        let url = if p["type"] == "image_url" { p["image_url"]["url"].as_str().unwrap_or("") } else { "" };
        let data = url.strip_prefix("data:").and_then(|d| d.split_once(";base64,"));
        if let Some((mime, data)) = data {
            out.push(json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}}));
        } else if !url.is_empty() {
            out.push(json!({"type": "image", "source": {"type": "url", "url": url}}));
        } else {
            let text = p.as_str().or(p["text"].as_str()).unwrap_or("");
            if !text.trim().is_empty() {
                out.push(json!({"type": "text", "text": text}));
            }
        }
    }
    if out.is_empty() {
        out.push(json!({"type": "text", "text": "(empty)"}));
    }
    out
}

fn parse_args(raw: &Value) -> Value {
    if raw.is_object() {
        return raw.clone();
    }
    serde_json::from_str::<Value>(raw.as_str().unwrap_or("{}")).ok().filter(Value::is_object).unwrap_or_else(|| json!({}))
}

/// The blocks Anthropic sent for this assistant message, when they still are that message:
/// the same tool calls in the same order, and something besides thinking.
fn kept(m: &Value, ids: &[&str]) -> Option<Vec<Value>> {
    let blocks = m[NATIVE].as_array()?;
    let kept_ids: Vec<&str> = blocks.iter().filter(|b| b["type"] == "tool_use").filter_map(|b| b["id"].as_str()).collect();
    let said = blocks.iter().any(|b| b["type"] == "tool_use" || b["type"] == "text");
    (said && kept_ids == ids).then(|| blocks.clone())
}

/// The Messages request for a Chat Completions `body` (model, messages, tools,
/// max_completion_tokens, reasoning_effort).
pub fn request(body: &Value) -> Value {
    let mut system: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    // index in `out` of the user message that is collecting a run of tool results
    let mut results: Option<usize> = None;
    for m in body["messages"].as_array().into_iter().flatten() {
        let role = m["role"].as_str().unwrap_or("");
        if role == "tool" {
            let block = json!({
                "type": "tool_result",
                "tool_use_id": m["tool_call_id"].as_str().unwrap_or(""),
                "content": or_text(text_of(&m["content"]), "(no output)"),
            });
            match results {
                Some(i) => out[i]["content"].as_array_mut().unwrap().push(block),
                None => {
                    out.push(json!({"role": "user", "content": [block]}));
                    results = Some(out.len() - 1);
                }
            }
            continue;
        }
        results = None;
        match role {
            // The loop sends one, first. A later one goes in as user text, where it sits in the history.
            "system" | "developer" if out.is_empty() => system.push(text_of(&m["content"])),
            "system" | "developer" => {
                out.push(json!({"role": "user", "content": [{"type": "text", "text": or_text(text_of(&m["content"]), "(empty)")}]}))
            }
            "user" => out.push(json!({"role": "user", "content": user_blocks(&m["content"])})),
            "assistant" => {
                let calls: Vec<&Value> = m["tool_calls"].as_array().into_iter().flatten().collect();
                let ids: Vec<&str> = calls.iter().map(|c| c["id"].as_str().unwrap_or("")).collect();
                if let Some(blocks) = kept(m, &ids) {
                    out.push(json!({"role": "assistant", "content": blocks}));
                    continue;
                }
                let mut blocks = Vec::new();
                let text = text_of(&m["content"]);
                if !text.trim().is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for c in calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": c["id"].as_str().unwrap_or(""),
                        "name": c["function"]["name"].as_str().unwrap_or(""),
                        "input": parse_args(&c["function"]["arguments"]),
                    }));
                }
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            _ => {}
        }
    }

    let tools: Vec<Value> = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| if t["function"].is_object() { &t["function"] } else { t })
        .filter(|f| f["name"].as_str().is_some_and(|n| !n.is_empty()))
        .map(|f| {
            let schema = if f["parameters"].is_object() { f["parameters"].clone() } else { json!({"type": "object", "properties": {}}) };
            json!({"name": f["name"], "description": f["description"].as_str().unwrap_or(""), "input_schema": schema})
        })
        .collect();

    let max = body["max_completion_tokens"].as_u64().or(body["max_tokens"].as_u64());
    let mut params = json!({
        "model": body["model"],
        "max_tokens": max.filter(|m| *m > 0).map_or(64_000, |m| m.clamp(1024, 128_000)),
        "messages": out,
        "stream": true,
        // The growing conversation is cached automatically; the marker on the system
        // prompt keeps tools + system readable whatever happens later in the messages.
        "cache_control": {"type": "ephemeral"},
        "thinking": {"type": "adaptive", "display": "summarized", "block_binding": {"prefix_mismatch_behavior": "drop_block"}},
        "output_config": {"effort": body["reasoning_effort"].as_str().unwrap_or("medium")},
    });
    let system = system.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
    if !system.is_empty() {
        params["system"] = json!([{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}]);
    }
    if !tools.is_empty() {
        params["tools"] = json!(tools);
    }
    params
}

// ---- reply: Messages stream -> Chat Completions chunks -------------------------------

fn chunk(delta: Value, finish: Option<&str>) -> String {
    json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}).to_string()
}

fn append(block: &mut Value, key: &str, more: &str) {
    let had = block[key].as_str().unwrap_or("").to_string();
    block[key] = json!(had + more);
}

/// Usage in the Chat Completions shape; `prompt_tokens` counts everything that went in, and
/// what was written to the cache (priced apart) is named beside it.
pub fn usage_of(u: &Map<String, Value>) -> Value {
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let (read, write) = (n("cache_read_input_tokens"), n("cache_creation_input_tokens"));
    let prompt = n("input_tokens") + read + write;
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": n("output_tokens"),
        "total_tokens": prompt + n("output_tokens"),
        "prompt_tokens_details": {"cached_tokens": read},
        "cache_creation_input_tokens": write,
    })
}

/// One reply's event stream, read event by event.
#[derive(Default)]
pub struct Stream {
    /// The content blocks as they fill up, by their index.
    blocks: BTreeMap<u64, Value>,
    /// A tool call's input, as the JSON text that streams in.
    inputs: BTreeMap<u64, String>,
    /// tool_use blocks, by their content index, numbered the way Chat Completions numbers calls.
    calls: BTreeMap<u64, u64>,
    usage: Map<String, Value>,
    stop: String,
    declined: Option<String>,
    /// Thinking blocks of the history the API left out of this request.
    pub dropped: usize,
    ended: bool,
}

impl Stream {
    /// Takes one `data:` payload and gives the Chat Completions payloads it amounts to
    /// (`[DONE]` last); an error event comes back as `Err`.
    pub fn take(&mut self, data: &str) -> Result<Vec<String>, String> {
        let Ok(ev) = serde_json::from_str::<Value>(data) else {
            return Ok(Vec::new());
        };
        let index = ev["index"].as_u64().unwrap_or(0);
        let mut out = Vec::new();
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                if let Some(u) = ev["message"]["usage"].as_object() {
                    self.usage = u.clone();
                }
                self.dropped = ev["message"]["input_transformations"]
                    .as_array()
                    .map_or(0, |t| t.iter().filter(|t| t["type"] == "thinking_dropped").count());
            }
            "content_block_start" => {
                let block = ev["content_block"].clone();
                if block["type"] == "tool_use" {
                    let n = self.calls.len() as u64;
                    self.calls.insert(index, n);
                    out.push(chunk(
                        json!({"tool_calls": [{"index": n, "id": block["id"], "type": "function", "function": {"name": block["name"], "arguments": ""}}]}),
                        None,
                    ));
                } else if let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) {
                    out.push(chunk(json!({"content": text}), None));
                }
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let d = &ev["delta"];
                let block = self.blocks.entry(index).or_insert_with(|| json!({}));
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let text = d["text"].as_str().unwrap_or("");
                        append(block, "text", text);
                        if !text.is_empty() {
                            out.push(chunk(json!({"content": text}), None));
                        }
                    }
                    "thinking_delta" => {
                        let text = d["thinking"].as_str().unwrap_or("");
                        append(block, "thinking", text);
                        if !text.is_empty() {
                            out.push(chunk(json!({"reasoning_content": text}), None));
                        }
                    }
                    "signature_delta" => append(block, "signature", d["signature"].as_str().unwrap_or("")),
                    "input_json_delta" => {
                        let part = d["partial_json"].as_str().unwrap_or("");
                        self.inputs.entry(index).or_default().push_str(part);
                        if let Some(n) = self.calls.get(&index).filter(|_| !part.is_empty()) {
                            out.push(chunk(json!({"tool_calls": [{"index": n, "function": {"arguments": part}}]}), None));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(raw) = self.inputs.get(&index)
                    && let Some(block) = self.blocks.get_mut(&index)
                {
                    block["input"] = parse_args(&json!(if raw.trim().is_empty() { "{}" } else { raw.as_str() }));
                }
            }
            "message_delta" => {
                if let Some(u) = ev["usage"].as_object() {
                    self.usage.extend(u.clone());
                }
                if let Some(stop) = ev["delta"]["stop_reason"].as_str() {
                    self.stop = stop.to_string();
                }
                if self.stop == "refusal" {
                    self.declined = Some(ev["delta"]["stop_details"]["category"].as_str().unwrap_or("no category").to_string());
                }
            }
            "message_stop" => {
                self.ended = true;
                if let Some(why) = &self.declined {
                    out.push(chunk(json!({"content": format!("(Claude declined to answer: {why})")}), None));
                }
                let finish = match self.stop.as_str() {
                    "tool_use" => "tool_calls",
                    "max_tokens" | "model_context_window_exceeded" => "length",
                    _ => "stop",
                };
                out.push(chunk(json!({}), Some(finish)));
                out.push(json!({"choices": [], "usage": usage_of(&self.usage)}).to_string());
                out.push("[DONE]".to_string());
            }
            "error" => {
                let e = &ev["error"];
                return Err(e["message"].as_str().map_or_else(|| e.to_string(), str::to_string));
            }
            _ => {}
        }
        Ok(out)
    }

    /// The reply's content blocks, to keep in its assistant message: none for a reply that
    /// was declined or never ended, and no empty text (the API refuses it back).
    pub fn blocks(&self) -> Option<Value> {
        if !self.ended || self.declined.is_some() {
            return None;
        }
        let blocks: Vec<Value> = self
            .blocks
            .values()
            .filter(|b| b["type"] != "text" || b["text"].as_str().is_some_and(|t| !t.trim().is_empty()))
            .cloned()
            .collect();
        (!blocks.is_empty()).then(|| json!(blocks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(messages: Value) -> Value {
        json!({
            "model": "claude-sonnet-5-5",
            "messages": messages,
            "tools": [{"type": "function", "function": {"name": "bash", "description": "run", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}}],
            "max_completion_tokens": 65_536,
            "reasoning_effort": "medium",
        })
    }

    #[test]
    fn a_tool_loop_becomes_a_messages_request() {
        let r = request(&body(json!([
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": "list"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "a", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}},
                {"id": "b", "type": "function", "function": {"name": "bash", "arguments": "not json"}},
            ]},
            {"role": "tool", "tool_call_id": "a", "content": "x.txt"},
            {"role": "tool", "tool_call_id": "b", "content": ""},
            {"role": "user", "content": "thanks"},
        ])));
        assert_eq!(r["system"][0]["text"], "be brief");
        assert_eq!(r["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(r["output_config"]["effort"], "medium");
        assert_eq!(r["thinking"]["type"], "adaptive");
        assert_eq!(r["max_tokens"], 65_536);
        assert_eq!(r["tools"][0]["input_schema"]["properties"]["command"]["type"], "string");
        let m = r["messages"].as_array().unwrap();
        assert_eq!(m.iter().map(|m| m["role"].as_str().unwrap()).collect::<Vec<_>>(), ["user", "assistant", "user", "user"]);
        assert_eq!(m[1]["content"][0]["input"]["command"], "ls");
        assert_eq!(m[1]["content"][1]["input"], json!({}));
        // both answers travel in one user message, an empty one with a placeholder
        assert_eq!(m[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(m[2]["content"][1]["content"], "(no output)");
    }

    #[test]
    fn kept_blocks_go_back_only_while_they_match_the_message() {
        let blocks = json!([
            {"type": "thinking", "thinking": "hm", "signature": "sig"},
            {"type": "tool_use", "id": "a", "name": "bash", "input": {"command": "ls"}},
        ]);
        let call = |id: &str| json!([{"id": id, "type": "function", "function": {"name": "bash", "arguments": "{}"}}]);
        let r = request(&body(json!([
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": null, "tool_calls": call("a"), NATIVE: blocks},
            {"role": "tool", "tool_call_id": "a", "content": "ok"},
            {"role": "assistant", "content": null, "tool_calls": call("z"), NATIVE: blocks},
            {"role": "tool", "tool_call_id": "z", "content": "ok"},
            // thinking alone is not a message the API takes back
            {"role": "assistant", "content": null, NATIVE: [{"type": "thinking", "thinking": "hm", "signature": "s"}]},
        ])));
        let m = r["messages"].as_array().unwrap();
        assert_eq!(m.len(), 5);
        assert_eq!(m[1]["content"][0]["signature"], "sig");
        assert_eq!(m[3]["content"][0]["type"], "tool_use");
        assert_eq!(m[3]["content"][0]["id"], "z");
    }

    #[test]
    fn a_stream_becomes_chunks_and_keeps_its_blocks() {
        let events = [
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 5, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 20, "output_tokens": 1}, "input_transformations": [{"type": "thinking_dropped"}]}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "look"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "t1", "name": "bash", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"command\":"}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"ls\"}"}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "ping"}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 40}}),
            json!({"type": "message_stop"}),
        ];
        let mut s = Stream::default();
        let mut reply = crate::agent::workers_ai::Reply::default();
        for ev in &events {
            for data in s.take(&ev.to_string()).unwrap() {
                reply.take(&data).unwrap();
            }
        }
        assert!(reply.done);
        assert_eq!(reply.finish.as_deref(), Some("tool_calls"));
        assert_eq!(reply.calls[&0].id, "t1");
        assert_eq!(reply.calls[&0].arguments, "{\"command\":\"ls\"}");
        assert_eq!(s.dropped, 1);
        let u = usage_of(&s.usage);
        assert_eq!((u["prompt_tokens"].as_u64(), u["completion_tokens"].as_u64()), (Some(125), Some(40)));
        assert_eq!(u["prompt_tokens_details"]["cached_tokens"], 100);
        assert_eq!(u["cache_creation_input_tokens"], 20);
        // the empty text block is not kept; the thinking block keeps its signature
        let blocks = s.blocks().unwrap();
        assert_eq!(blocks.as_array().unwrap().len(), 2);
        assert_eq!(blocks[0]["signature"], "sig");
        assert_eq!(blocks[1]["input"]["command"], "ls");
    }

    #[test]
    fn errors_and_refusals_end_the_reply() {
        let mut s = Stream::default();
        let e = s.take(&json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}).to_string());
        assert_eq!(e, Err("Overloaded".to_string()));
        let mut s = Stream::default();
        s.take(&json!({"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_details": {"category": "cyber"}}}).to_string()).unwrap();
        let out = s.take(&json!({"type": "message_stop"}).to_string()).unwrap();
        assert!(out[0].contains("declined to answer: cyber"));
        assert_eq!(out.last().map(String::as_str), Some("[DONE]"));
        assert!(s.blocks().is_none());
    }
}
