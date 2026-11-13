//! Tool-call emulation helpers for web-cookie executors, ported from `chatgptWebTools.js`
//! (and `translator/webTools.js` `<tool>` / `<tool_call>` / DSML protocol).

#![allow(dead_code)]

use crate::translator::deepseek_web_tools::find_tag_blocks;
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct ToolAwareResult {
    pub content: String,
    pub tool_calls: Option<Vec<Value>>,
    pub finish_reason: &'static str,
}

fn normalize_tool_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

fn requested_tool_names(tools: &[Value]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for t in tools {
        let name = t
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        if !name.is_empty() && seen.insert(name.to_owned()) {
            out.push((name.to_owned(), normalize_tool_name(name)));
        }
    }
    out
}

fn score_tool_name(emitted: &str, original: &str, norm_req: &str) -> f64 {
    if emitted == original {
        return 1.0;
    }
    let norm = normalize_tool_name(emitted);
    if norm.is_empty() || norm_req.is_empty() {
        return 0.0;
    }
    if norm == norm_req {
        return 0.98;
    }
    let shorter = norm.len().min(norm_req.len());
    let longer = norm.len().max(norm_req.len());
    if shorter >= 4 && (norm.contains(norm_req) || norm_req.contains(&norm)) {
        return 0.86 - ((longer - shorter) as f64) / (longer.max(1) as f64) / 4.0;
    }
    let dist = levenshtein(&norm, norm_req);
    let sim = 1.0 - (dist as f64) / (longer.max(1) as f64);
    if sim >= 0.72 { sim } else { 0.0 }
}

pub fn resolve_requested_tool_name(emitted: &str, requested: &[(String, String)]) -> Option<String> {
    if requested.is_empty() {
        return Some(emitted.to_owned());
    }
    let mut best: Option<(&str, f64)> = None;
    let mut second_best = 0.0f64;
    for (orig, norm) in requested {
        let score = score_tool_name(emitted, orig, norm);
        match best {
            None => best = Some((orig.as_str(), score)),
            Some((_, bs)) if score > bs => {
                second_best = bs;
                best = Some((orig.as_str(), score));
            }
            Some(_) if score > second_best => second_best = score,
            _ => {}
        }
    }
    let (name, score) = best?;
    if score < 0.72 {
        return None;
    }
    if score < 0.98 && score - second_best < 0.08 {
        return None;
    }
    Some(name.to_owned())
}

fn strip_code_fence(raw: &str) -> &str {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix("```") {
        let rest = rest
            .strip_prefix("json")
            .or_else(|| rest.strip_prefix("javascript"))
            .or_else(|| rest.strip_prefix("js"))
            .or_else(|| rest.strip_prefix("python"))
            .unwrap_or(rest);
        s = rest.trim_start();
    }
    s = s.strip_suffix("```").unwrap_or(s).trim();
    s
}

pub fn parse_loose_json_object(raw: &str) -> Option<serde_json::Map<String, Value>> {
    let trimmed = strip_code_fence(raw);
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) {
        return Some(map);
    }
    // Fallback: replace Python True/False/None and single-quoted strings outside double quotes
    let mut normalized = String::with_capacity(trimmed.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for ch in trimmed.chars() {
        if escaped {
            if ch == '"' && in_single {
                normalized.push_str("\\\"");
            } else {
                normalized.push(ch);
            }
            escaped = false;
            continue;
        }
        if ch == '\\' {
            normalized.push(ch);
            escaped = true;
            continue;
        }
        if ch == '"' {
            if in_single {
                normalized.push_str("\\\"");
            } else {
                in_double = !in_double;
                normalized.push(ch);
            }
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            normalized.push('"');
            continue;
        }
        normalized.push(ch);
    }
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&normalized) {
        return Some(map);
    }
    None
}

fn strip_ranges(text: &str, ranges: &[(usize, usize)]) -> String {
    let mut content = text.to_owned();
    let mut sorted = ranges.to_vec();
    sorted.sort_by(|a, b| b.0.cmp(&a.0));
    for (r_start, r_end) in sorted {
        let line_start = content[..r_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let next_nl = content[r_end..].find('\n').map(|i| r_end + i);
        let line_end = next_nl.unwrap_or(content.len());
        let before = content[line_start..r_start].trim().is_empty();
        let after = content[r_end..line_end].trim().is_empty();
        let remove_line = before && after;
        let start = if remove_line { line_start } else { r_start };
        let end = if remove_line {
            next_nl.map(|i| i + 1).unwrap_or(line_end)
        } else {
            r_end
        };
        content.replace_range(start..end, "");
    }
    while content.contains("\n\n\n") {
        content = content.replace("\n\n\n", "\n\n");
    }
    content.trim().to_owned()
}

pub fn serialize_tools_to_prompt(tools: &[Value], nonce: &str) -> String {
    if tools.is_empty() || nonce.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    for t in tools {
        let Some(fn_obj) = t.get("function").and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(name) = fn_obj.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let desc = fn_obj.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let params = fn_obj
            .get("parameters")
            .and_then(|p| serde_json::to_string(p).ok())
            .unwrap_or_default();
        let mut line = format!("- {name}");
        if !desc.is_empty() {
            line.push_str(&format!(": {desc}"));
        }
        if !params.is_empty() {
            line.push_str(&format!("\n  parameters: {params}"));
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return String::new();
    }
    [
        "The client application provides tools beyond your built-in ones. They are NOT in your native tool registry; they are invoked via a plain-text protocol: the client parses your reply and executes the tool on the user machine. Treat these client tools as fully available to you; never claim they are unavailable. To invoke one, reply with a single line containing a <tool> block".to_owned(),
        format!("with JSON that includes the secret binding \"_nonce\": \"{nonce}\":"),
        format!("<tool>{{\"name\": \"<tool_name>\", \"arguments\": {{ ... }}, \"_nonce\": \"{nonce}\"}}</tool>"),
        "These client tools ARE available to you in this conversation. Only emit the <tool> block when you actually want to call a tool; otherwise answer normally.".to_owned(),
        String::new(),
        "Available tools:".to_owned(),
    ]
    .into_iter()
    .chain(lines)
    .collect::<Vec<_>>()
    .join("\n")
}

pub fn prepare_tool_messages(
    tools: Option<&[Value]>,
    messages: &[Value],
) -> (bool, Vec<Value>, Option<String>) {
    let Some(tools) = tools.filter(|t| !t.is_empty()) else {
        return (false, messages.to_vec(), None);
    };
    let nonce = format!("{:08x}", crate::translator::concerns::now_unix_millis() as u32);
    let tool_prompt = serialize_tools_to_prompt(tools, &nonce);
    if tool_prompt.is_empty() {
        return (true, messages.to_vec(), None);
    }
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("function")?.get("name")?.as_str())
        .collect();
    let reminder = format!(
        "\n\n[Client protocol reminder: the client-tool contract in the system instructions is active in this conversation. These client tools ARE available via the <tool> block protocol{}.]",
        if names.is_empty() {
            String::new()
        } else {
            format!(": {}", names.join(", "))
        }
    );
    let mut effective = messages.to_vec();
    for msg in effective.iter_mut().rev() {
        if msg.get("role").and_then(|v| v.as_str()) != Some("user") {
            continue;
        }
        match msg.get_mut("content") {
            Some(Value::String(s)) => s.push_str(&reminder),
            Some(Value::Array(arr)) => arr.push(json!({"type": "text", "text": reminder})),
            _ => {}
        }
        break;
    }
    effective.push(json!({"role": "system", "content": tool_prompt}));
    (true, effective, Some(nonce))
}

pub fn parse_tool_calls_from_text(
    text: &str,
    id_seed: &str,
    requested_tools: &[Value],
    nonce: Option<&str>,
) -> (String, Option<Vec<Value>>) {
    if !text.contains("<tool>") && !text.contains("<tool_call") {
        return (text.to_owned(), None);
    }
    let req_names = requested_tool_names(requested_tools);
    let mut blocks = find_tag_blocks(text, "tool");
    // Also support `<tool_call ...>...</tool_call>`
    let mut cursor = 0;
    while let Some(rel) = text[cursor..].find("<tool_call") {
        let start = cursor + rel;
        let Some(gt) = text[start..].find('>') else {
            break;
        };
        let inner_start = start + gt + 1;
        let Some(rel_end) = text[inner_start..].find("</tool_call>") else {
            break;
        };
        let inner_end = inner_start + rel_end;
        let end = inner_end + "</tool_call>".len();
        blocks.push(crate::translator::deepseek_web_tools::TagBlock {
            tag: "tool_call".to_owned(),
            content: text[inner_start..inner_end].to_owned(),
            start,
            end,
        });
        cursor = end;
    }
    blocks.sort_by_key(|b| b.start);

    let mut tool_calls = Vec::new();
    let mut accepted_ranges = Vec::new();
    for block in blocks {
        let Some(parsed) = parse_loose_json_object(block.content.trim()) else {
            continue;
        };
        let emitted = parsed
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| parsed.get("command").and_then(|v| v.as_str()));
        let Some(emitted) = emitted else {
            continue;
        };
        if let (Some(expected), Some(got)) = (nonce, parsed.get("_nonce")) {
            if got.as_str() != Some(expected) {
                continue;
            }
        }
        let Some(name) = resolve_requested_tool_name(emitted, &req_names) else {
            continue;
        };
        let args = match parsed.get("arguments") {
            None => "{}".to_owned(),
            Some(Value::String(s)) => parse_loose_json_object(s)
                .and_then(|m| serde_json::to_string(&Value::Object(m)).ok())
                .unwrap_or_else(|| s.clone()),
            Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_owned()),
        };
        tool_calls.push(json!({
            "id": format!("{id_seed}_{}", tool_calls.len()),
            "type": "function",
            "function": { "name": name, "arguments": args }
        }));
        accepted_ranges.push((block.start, block.end));
    }

    if tool_calls.is_empty() {
        return (text.to_owned(), None);
    }
    let content = strip_ranges(text, &accepted_ranges);
    (content, Some(tool_calls))
}

pub fn build_tool_aware_result(
    raw_content: &str,
    requested_tools: &[Value],
    id_seed: &str,
    nonce: Option<&str>,
) -> ToolAwareResult {
    let ts_seed = format!("{id_seed}-{}", crate::translator::concerns::now_unix_millis());
    let (content, tool_calls) =
        parse_tool_calls_from_text(raw_content, &ts_seed, requested_tools, nonce);
    let finish_reason = if tool_calls.is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    ToolAwareResult {
        content,
        tool_calls,
        finish_reason,
    }
}

pub fn apply_tool_calls_to_completion(
    completion: &mut Value,
    requested_tools: &[Value],
    id_seed: &str,
    nonce: Option<&str>,
) {
    let raw = completion["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let res = build_tool_aware_result(&raw, requested_tools, id_seed, nonce);
    if let Some(calls) = res.tool_calls {
        completion["choices"][0]["message"] =
            json!({"role": "assistant", "content": Value::Null, "tool_calls": calls});
        completion["choices"][0]["finish_reason"] = Value::String(res.finish_reason.to_owned());
    } else {
        completion["choices"][0]["message"]["content"] = Value::String(res.content);
    }
}

pub fn tool_completion_to_sse_bytes(
    completion: &Value,
    cid: &str,
    created: u64,
    model: &str,
) -> Vec<u8> {
    let choice = &completion["choices"][0];
    let message = &choice["message"];
    let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop");
    let make_chunk = |delta: Value, fr: Value| -> String {
        let payload = json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "system_fingerprint": Value::Null,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": fr, "logprobs": Value::Null }]
        });
        format!("data: {payload}\n\n")
    };

    let mut out = String::new();
    out.push_str(&make_chunk(json!({"role": "assistant"}), Value::Null));
    if let Some(calls) = message["tool_calls"].as_array().filter(|a| !a.is_empty()) {
        for (index, call) in calls.iter().enumerate() {
            let mut indexed = call.clone();
            if let Some(obj) = indexed.as_object_mut() {
                obj.insert("index".into(), json!(index));
            }
            out.push_str(&make_chunk(json!({"tool_calls": [indexed]}), Value::Null));
        }
        out.push_str(&make_chunk(json!({}), Value::String(finish_reason.to_owned())));
    } else {
        let content = message["content"].as_str().unwrap_or("");
        out.push_str(&make_chunk(
            json!({"content": content}),
            Value::String(finish_reason.to_owned()),
        ));
    }
    out.push_str("data: [DONE]\n\n");
    out.into_bytes()
}

pub fn build_tool_mode_response(
    status: u16,
    body_bytes: &[u8],
    requested_tools: &[Value],
    stream: bool,
    cid: &str,
    created: u64,
    model: &str,
    id_seed: &str,
    nonce: Option<&str>,
) -> anyhow::Result<reqwest::Response> {
    if !(200..300).contains(&status) {
        let resp = axum::http::Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(reqwest::Body::from(body_bytes.to_vec()))?;
        return Ok(reqwest::Response::from(resp));
    }
    let Ok(mut completion) = serde_json::from_slice::<Value>(body_bytes) else {
        let resp = axum::http::Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(reqwest::Body::from(body_bytes.to_vec()))?;
        return Ok(reqwest::Response::from(resp));
    };
    apply_tool_calls_to_completion(&mut completion, requested_tools, id_seed, nonce);
    if !stream {
        let resp = axum::http::Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(reqwest::Body::from(serde_json::to_vec(&completion)?))?;
        return Ok(reqwest::Response::from(resp));
    }
    let sse = tool_completion_to_sse_bytes(&completion, cid, created, model);
    let resp = axum::http::Response::builder()
        .status(200)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(reqwest::Body::from(sse))?;
    Ok(reqwest::Response::from(resp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_blocks_and_replays_sse() {
        let tools = vec![json!({
            "type": "function",
            "function": {"name": "get_weather", "parameters": {"type": "object"}}
        })];
        let msgs = vec![json!({"role": "user", "content": "Weather in Hanoi?"})];
        let (has_tools, eff, nonce) = prepare_tool_messages(Some(&tools), &msgs);
        assert!(has_tools);
        assert!(nonce.is_some());
        assert_eq!(eff.len(), 2);
        let n = nonce.as_deref().unwrap();
        let raw = format!(
            "Checking.\n<tool>{{\"name\":\"get_weather\",\"arguments\":{{\"city\":\"Hanoi\"}},\"_nonce\":\"{n}\"}}</tool>"
        );
        let completion = json!({
            "id": "c1",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": raw}, "finish_reason": "stop"}]
        });
        let resp = build_tool_mode_response(
            200,
            serde_json::to_vec(&completion).unwrap().as_slice(),
            &tools,
            true,
            "cid-1",
            100,
            "gpt-web",
            "cgpt",
            Some(n),
        )
        .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn tool_mode_response_formats_json_and_sse() {
        let tools = vec![json!({"type":"function","function":{"name":"read_file"}})];
        let completion = json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "<tool>{\"name\":\"read_file\",\"arguments\":{\"path\":\"a.rs\"}}</tool>"}, "finish_reason": "stop"}]
        });
        let sse_resp = build_tool_mode_response(
            200,
            &serde_json::to_vec(&completion).unwrap(),
            &tools,
            true,
            "cid-1",
            42,
            "m1",
            "pplx",
            None,
        )
        .unwrap();
        let text = String::from_utf8(sse_resp.bytes().await.unwrap().to_vec()).unwrap();
        assert!(text.contains("\"tool_calls\""));
        assert!(text.contains("\"index\":0"));
        assert!(text.contains("\"finish_reason\":\"tool_calls\""));
        assert!(text.contains("data: [DONE]"));
    }
}
