#![allow(dead_code)]

use serde_json::{json, Value};

pub const WEB_SEARCH_TOOL_NAMES: &[&str] = &[
    "web_search",
    "web_search_20250305",
    "web_search_20260209",
    "google_search",
    "google_search_retrieval",
    "search_web",
];

pub const WEB_FETCH_TOOL_NAMES: &[&str] = &[
    "web_fetch",
    "web_fetch_20250910",
    "web_fetch_20260209",
    "url_context",
    "fetch_url",
];

pub fn is_web_search_tool(tool: &Value) -> bool {
    let t = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let name = tool
        .get("name")
        .or_else(|| tool.get("function").and_then(|f| f.get("name")))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    WEB_SEARCH_TOOL_NAMES.contains(&t)
        || WEB_SEARCH_TOOL_NAMES.contains(&name)
        || t.starts_with("web_search")
        || name.starts_with("web_search")
}

pub fn is_web_fetch_tool(tool: &Value) -> bool {
    let t = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let name = tool
        .get("name")
        .or_else(|| tool.get("function").and_then(|f| f.get("name")))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    WEB_FETCH_TOOL_NAMES.contains(&t)
        || WEB_FETCH_TOOL_NAMES.contains(&name)
        || t.starts_with("web_fetch")
        || name.starts_with("web_fetch")
}

pub fn is_web_tool(tool: &Value) -> bool {
    is_web_search_tool(tool) || is_web_fetch_tool(tool)
}

pub fn detect_web_tools(tools: Option<&Value>) -> (bool, bool) {
    let Some(arr) = tools.and_then(|v| v.as_array()) else {
        return (false, false);
    };
    let has_search = arr.iter().any(is_web_search_tool);
    let has_fetch = arr.iter().any(is_web_fetch_tool);
    (has_search, has_fetch)
}

/// Strip built-in web tools from `body.tools`, returning `(stripped_count, had_search, had_fetch)`.
pub fn strip_web_tools(body: &mut Value) -> (usize, bool, bool) {
    let Some(arr) = body.get_mut("tools").and_then(|v| v.as_array_mut()) else {
        return (0, false, false);
    };
    let mut had_search = false;
    let mut had_fetch = false;
    let before = arr.len();
    arr.retain(|t| {
        if is_web_search_tool(t) {
            had_search = true;
            false
        } else if is_web_fetch_tool(t) {
            had_fetch = true;
            false
        } else {
            true
        }
    });
    let removed = before - arr.len();
    if arr.is_empty() {
        if let Some(obj) = body.as_object_mut() {
            obj.remove("tools");
        }
    }
    (removed, had_search, had_fetch)
}

pub fn extract_urls_from_text(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for token in text.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| {
            matches!(c, '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';')
        });
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            let clean = trimmed.trim_end_matches('.');
            if !urls.iter().any(|u| u == clean) {
                urls.push(clean.to_string());
            }
        }
    }
    urls
}

/// Inject formatted web search/fetch results into the last user message or system context.
pub fn inject_web_context_into_messages(body: &mut Value, context_text: &str) {
    if context_text.trim().is_empty() {
        return;
    }
    let Some(msgs) = body.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return;
    };
    if let Some(last_user) = msgs
        .iter_mut()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
    {
        match last_user.get_mut("content") {
            Some(Value::String(s)) => {
                *s = format!("{}\n\n{}", context_text, s);
            }
            Some(Value::Array(arr)) => {
                arr.insert(0, json!({ "type": "text", "text": context_text }));
            }
            _ => {
                if let Some(obj) = last_user.as_object_mut() {
                    obj.insert("content".to_string(), Value::String(context_text.to_string()));
                }
            }
        }
    }
}
