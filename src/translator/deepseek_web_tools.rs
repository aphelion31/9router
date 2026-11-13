#![allow(dead_code)]

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagBlock {
    pub tag: String,
    pub content: String,
    pub start: usize,
    pub end: usize,
}

/// Find `<tag>...</tag>` blocks in `text`.
pub fn find_tag_blocks(text: &str, tag: &str) -> Vec<TagBlock> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(rel_start) = text[cursor..].find(&open) {
        let start = cursor + rel_start;
        let inner_start = start + open.len();
        if let Some(rel_end) = text[inner_start..].find(&close) {
            let inner_end = inner_start + rel_end;
            let end = inner_end + close.len();
            blocks.push(TagBlock {
                tag: tag.to_string(),
                content: text[inner_start..inner_end].to_string(),
                start,
                end,
            });
            cursor = end;
        } else {
            break;
        }
    }
    blocks
}

/// Remove all `<tag>...</tag>` blocks from `text`.
pub fn strip_tag_blocks(text: &str, tag: &str) -> String {
    let blocks = find_tag_blocks(text, tag);
    if blocks.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for b in blocks {
        out.push_str(&text[last..b.start]);
        last = b.end;
    }
    out.push_str(&text[last..]);
    out
}

/// Build DeepSeek tool-use prompt instructions from OpenAI `tools` array.
pub fn build_deepseek_tools_prompt(tools: &[Value]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let mut defs = Vec::new();
    for t in tools {
        let f = t.get("function").unwrap_or(t);
        let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let desc = f
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let params = f
            .get("parameters")
            .or_else(|| t.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        defs.push(json!({
            "name": name,
            "description": desc,
            "parameters": params,
        }));
    }
    if defs.is_empty() {
        return String::new();
    }
    let defs_json = serde_json::to_string_pretty(&defs).unwrap_or_else(|_| "[]".to_string());
    format!(
        "You have access to the following tools. To call a tool, respond with a <tool_call> block containing JSON {{\"name\": \"...\", \"arguments\": {{...}}}}:\n\n{}",
        defs_json
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDeepSeekToolCall {
    pub name: String,
    pub arguments: Value,
}

/// Parse `<tool_call>...</tool_call>` blocks from model text output.
pub fn parse_deepseek_tool_calls(text: &str) -> (String, Vec<ParsedDeepSeekToolCall>) {
    let blocks = find_tag_blocks(text, "tool_call");
    if blocks.is_empty() {
        return (text.to_string(), Vec::new());
    }
    let mut calls = Vec::new();
    for b in &blocks {
        let trimmed = b.content.trim();
        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
            if let Some(name) = val.get("name").and_then(|v| v.as_str()) {
                let args = val
                    .get("arguments")
                    .or_else(|| val.get("input"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                calls.push(ParsedDeepSeekToolCall {
                    name: name.to_string(),
                    arguments: args,
                });
            }
        }
    }
    let remaining = strip_tag_blocks(text, "tool_call").trim().to_string();
    (remaining, calls)
}

/// Inject OpenAI `tools` into DeepSeek web prompt (system message) and strip `tools` field.
pub fn bind_deepseek_web_tools(body: &mut Value) {
    let tools = body
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if tools.is_empty() {
        return;
    }
    let prompt = build_deepseek_tools_prompt(&tools);
    if prompt.is_empty() {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        obj.remove("tools");
        obj.remove("tool_choice");
    }
    if let Some(msgs) = body.get_mut("messages").and_then(|v| v.as_array_mut()) {
        if let Some(sys) = msgs
            .iter_mut()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        {
            if let Some(s) = sys.get_mut("content").and_then(|c| c.as_str()) {
                let merged = format!("{}\n\n{}", s, prompt);
                if let Some(obj) = sys.as_object_mut() {
                    obj.insert("content".to_string(), Value::String(merged));
                }
            }
        } else {
            msgs.insert(0, json!({ "role": "system", "content": prompt }));
        }
    }
}
