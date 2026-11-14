// Shared system-prompt injector: inserts an instruction into the system message of
// the final request body, dispatching by format so it works for translated and
// native-passthrough flows. Used by rtk_caveman and Prompt Injection.

use serde_json::{Value, json};

use crate::translator::formats::{
    FORMAT_ANTIGRAVITY, FORMAT_CLAUDE, FORMAT_GEMINI, FORMAT_GEMINI_CLI, FORMAT_KIRO,
    FORMAT_VERTEX,
};

const SEP: &str = "\n\n";

pub fn inject_system_prompt(body: &mut Value, format: &str, prompt: &str, position: &str) {
    if prompt.is_empty() || !body.is_object() {
        return;
    }
    let pos = if position == "prepend" { "prepend" } else { "append" };

    if is_kiro_body(body) || format == FORMAT_KIRO {
        inject_kiro_system(body, prompt, pos);
        return;
    }

    if format == FORMAT_CLAUDE {
        inject_claude_system(body, prompt, pos);
        return;
    }

    if matches!(
        format,
        FORMAT_GEMINI | FORMAT_GEMINI_CLI | FORMAT_VERTEX | FORMAT_ANTIGRAVITY
    ) {
        inject_gemini_system(body, prompt, pos);
        return;
    }

    if body.get("instructions").is_some_and(Value::is_string) {
        inject_instructions_system(body, prompt, pos);
        return;
    }
    if body.get("messages").is_some_and(Value::is_array) {
        inject_chat_system(body, prompt, pos);
        return;
    }
    if body.get("input").is_some_and(Value::is_array) {
        inject_responses_input_system(body, prompt, pos);
    }
}

fn is_kiro_body(body: &Value) -> bool {
    let Some(cs) = body.get("conversationState").and_then(Value::as_object) else {
        return false;
    };
    let history_turn = cs
        .get("history")
        .and_then(Value::as_array)
        .is_some_and(|arr| {
            arr.iter().any(|it| {
                it.get("userInputMessage").is_some() || it.get("assistantResponseMessage").is_some()
            })
        });
    let current_turn = cs
        .get("currentMessage")
        .and_then(|c| c.get("userInputMessage"))
        .is_some();
    history_turn || current_turn
}

fn has_prompt(haystack: &str, prompt: &str) -> bool {
    if haystack.is_empty() {
        return false;
    }
    if haystack == prompt {
        return true;
    }
    haystack.split(SEP).any(|seg| seg == prompt)
}

fn dedup_string_append(curr: &str, prompt: &str, position: &str) -> String {
    if curr.is_empty() {
        return prompt.to_owned();
    }
    if has_prompt(curr, prompt) {
        return curr.to_owned();
    }
    if position == "prepend" {
        format!("{prompt}{SEP}{curr}")
    } else {
        format!("{curr}{SEP}{prompt}")
    }
}

fn inject_instructions_system(body: &mut Value, prompt: &str, position: &str) {
    let Some(curr) = body.get("instructions").and_then(Value::as_str) else {
        return;
    };
    if has_prompt(curr, prompt) {
        return;
    }
    let next = if curr.is_empty() {
        prompt.to_owned()
    } else {
        dedup_string_append(curr, prompt, position)
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("instructions".into(), Value::String(next));
    }
}

fn contains_prompt_in_messages(arr: &[Value], prompt: &str) -> bool {
    for m in arr {
        let role = m.get("role").and_then(Value::as_str);
        if role != Some("system") && role != Some("developer") {
            continue;
        }
        match m.get("content") {
            Some(Value::String(s)) if has_prompt(s, prompt) => return true,
            Some(Value::Array(parts)) => {
                if parts
                    .iter()
                    .any(|p| p.get("text").and_then(Value::as_str).is_some_and(|t| has_prompt(t, prompt)))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn inject_chat_system(body: &mut Value, prompt: &str, position: &str) {
    let Some(arr) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if contains_prompt_in_messages(arr, prompt) {
        return;
    }
    let idx = arr.iter().position(|m| {
        let r = m.get("role").and_then(Value::as_str);
        r == Some("system") || r == Some("developer")
    });
    if let Some(i) = idx {
        append_to_chat_message(&mut arr[i], prompt, position);
    } else {
        arr.insert(0, json!({ "role": "system", "content": prompt }));
    }
}

fn append_to_chat_message(msg: &mut Value, prompt: &str, position: &str) {
    let Some(obj) = msg.as_object_mut() else {
        return;
    };
    match obj.get_mut("content") {
        Some(Value::String(c)) => {
            let next = dedup_string_append(c, prompt, position);
            if next != *c {
                *c = next;
            }
        }
        Some(Value::Array(parts)) => {
            if parts
                .iter()
                .any(|b| b.get("text").and_then(Value::as_str) == Some(prompt))
            {
                return;
            }
            let block = json!({ "type": "text", "text": prompt });
            if position == "prepend" {
                parts.insert(0, block);
            } else {
                parts.push(block);
            }
        }
        _ => {
            obj.insert("content".into(), Value::String(prompt.to_owned()));
        }
    }
}

fn contains_prompt_in_responses_input(arr: &[Value], prompt: &str) -> bool {
    for item in arr {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let role = item.get("role").and_then(Value::as_str);
        if role != Some("system") && role != Some("developer") {
            continue;
        }
        match item.get("content") {
            Some(Value::String(s)) if has_prompt(s, prompt) => return true,
            Some(Value::Array(parts)) => {
                if parts
                    .iter()
                    .any(|p| p.get("text").and_then(Value::as_str).is_some_and(|t| has_prompt(t, prompt)))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn inject_responses_input_system(body: &mut Value, prompt: &str, position: &str) {
    let Some(arr) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    if contains_prompt_in_responses_input(arr, prompt) {
        return;
    }
    let idx = arr.iter().position(|m| {
        m.get("type").and_then(Value::as_str) == Some("message")
            && matches!(
                m.get("role").and_then(Value::as_str),
                Some("system") | Some("developer")
            )
    });
    if let Some(i) = idx {
        append_to_responses_message(&mut arr[i], prompt, position);
    } else {
        arr.insert(
            0,
            json!({
                "type": "message",
                "role": "system",
                "content": [{ "type": "input_text", "text": prompt }]
            }),
        );
    }
}

fn append_to_responses_message(msg: &mut Value, prompt: &str, position: &str) {
    let Some(obj) = msg.as_object_mut() else {
        return;
    };
    match obj.get_mut("content") {
        Some(Value::String(c)) => {
            let next = dedup_string_append(c, prompt, position);
            if next != *c {
                *c = next;
            }
        }
        Some(Value::Array(parts)) => {
            if parts
                .iter()
                .any(|b| b.get("text").and_then(Value::as_str) == Some(prompt))
            {
                return;
            }
            let block = json!({ "type": "input_text", "text": prompt });
            if position == "prepend" {
                parts.insert(0, block);
            } else {
                parts.push(block);
            }
        }
        _ => {
            obj.insert(
                "content".into(),
                json!([{ "type": "input_text", "text": prompt }]),
            );
        }
    }
}

fn inject_claude_system(body: &mut Value, prompt: &str, position: &str) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    match obj.get_mut("system") {
        Some(Value::String(sys)) => {
            if has_prompt(sys, prompt) {
                return;
            }
            let next = if sys.is_empty() {
                prompt.to_owned()
            } else {
                dedup_string_append(sys, prompt, position)
            };
            *sys = next;
        }
        Some(Value::Array(sys)) => {
            if sys
                .iter()
                .any(|b| b.get("text").and_then(Value::as_str) == Some(prompt))
            {
                return;
            }
            let block = json!({ "type": "text", "text": prompt });
            if position == "prepend" {
                sys.insert(0, block);
                return;
            }
            let last_cache_idx = sys
                .iter()
                .rposition(|b| b.get("cache_control").is_some_and(|v| !v.is_null() && v != false));
            if let Some(idx) = last_cache_idx {
                sys.insert(idx, block);
            } else {
                sys.push(block);
            }
        }
        _ => {
            obj.insert("system".into(), Value::String(prompt.to_owned()));
        }
    }
}

fn inject_gemini_system(body: &mut Value, prompt: &str, position: &str) {
    let target = if body.get("request").is_some_and(Value::is_object) {
        body.get_mut("request").unwrap()
    } else {
        body
    };
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let key = if obj.contains_key("system_instruction") {
        "system_instruction"
    } else {
        "systemInstruction"
    };
    if let Some(parts) = obj
        .get_mut(key)
        .and_then(|s| s.get_mut("parts"))
        .and_then(Value::as_array_mut)
    {
        if parts
            .iter()
            .any(|p| p.get("text").and_then(Value::as_str) == Some(prompt))
        {
            return;
        }
        let block = json!({ "text": prompt });
        if position == "prepend" {
            parts.insert(0, block);
        } else {
            parts.push(block);
        }
    } else {
        obj.insert(key.into(), json!({ "parts": [{ "text": prompt }] }));
    }
}

fn inject_kiro_system(body: &mut Value, prompt: &str, position: &str) {
    let Some(cs) = body.get_mut("conversationState").and_then(Value::as_object_mut) else {
        return;
    };
    let mut target_msg: Option<&mut Value> = cs
        .get_mut("history")
        .and_then(Value::as_array_mut)
        .and_then(|hist| {
            hist.iter_mut()
                .find_map(|item| item.get_mut("userInputMessage"))
        });

    if target_msg.is_none() {
        target_msg = cs
            .get_mut("currentMessage")
            .and_then(|cm| cm.get_mut("userInputMessage"));
    }

    let Some(msg) = target_msg.and_then(Value::as_object_mut) else {
        return;
    };
    let content = msg
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let next = dedup_string_append(&content, prompt, position);
    if next != content {
        msg.insert("content".into(), Value::String(next));
    }
}

pub fn inject_chat_turns(body: &mut Value, format: &str, turns: &[Value]) {
    if !body.is_object() || is_kiro_body(body) || format == FORMAT_KIRO {
        return;
    }
    let clean: Vec<(String, String)> = turns
        .iter()
        .filter_map(|t| {
            let role = t.get("role").and_then(Value::as_str)?;
            let content = t.get("content").and_then(Value::as_str)?;
            if (role == "user" || role == "assistant") && !content.trim().is_empty() {
                Some((role.to_owned(), content.to_owned()))
            } else {
                None
            }
        })
        .collect();
    if clean.is_empty() {
        return;
    }

    if matches!(
        format,
        FORMAT_GEMINI | FORMAT_GEMINI_CLI | FORMAT_VERTEX | FORMAT_ANTIGRAVITY
    ) {
        if let Some(contents) = body
            .get_mut("request")
            .and_then(|r| r.get_mut("contents"))
            .and_then(Value::as_array_mut)
        {
            insert_turns_into_contents(contents, &clean);
            return;
        }
        if let Some(contents) = body.get_mut("contents").and_then(Value::as_array_mut) {
            insert_turns_into_contents(contents, &clean);
            return;
        }
    }
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        insert_turns_into_messages(messages, &clean);
        return;
    }
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        insert_turns_into_responses_input(input, &clean);
    }
}

fn insert_turns_into_messages(arr: &mut Vec<Value>, turns: &[(String, String)]) {
    let mut idx = 0;
    while idx < arr.len()
        && matches!(
            arr[idx].get("role").and_then(Value::as_str),
            Some("system") | Some("developer")
        )
    {
        idx += 1;
    }
    if turns.len() <= arr.len().saturating_sub(idx)
        && turns.iter().enumerate().all(|(i, (r, c))| {
            arr[idx + i].get("role").and_then(Value::as_str) == Some(r.as_str())
                && arr[idx + i].get("content").and_then(Value::as_str) == Some(c.as_str())
        })
    {
        return;
    }
    let mut block: Vec<Value> = turns
        .iter()
        .map(|(r, c)| json!({ "role": r, "content": c }))
        .collect();
    if let Some(next) = arr.get(idx)
        && let Some(last) = block.last_mut()
        && next.get("role") == last.get("role")
        && let Some(next_c) = next.get("content").and_then(Value::as_str)
        && let Some(last_c) = last.get("content").and_then(Value::as_str)
    {
        last["content"] = Value::String(format!("{last_c}{SEP}{next_c}"));
        arr.remove(idx);
    }
    arr.splice(idx..idx, block);
}

fn insert_turns_into_contents(contents: &mut Vec<Value>, turns: &[(String, String)]) {
    let mut block: Vec<Value> = turns
        .iter()
        .map(|(r, c)| {
            let role = if r == "assistant" { "model" } else { "user" };
            json!({ "role": role, "parts": [{ "text": c }] })
        })
        .collect();
    if block.len() <= contents.len()
        && block.iter().enumerate().all(|(i, b)| {
            contents[i].get("role") == b.get("role")
                && contents[i]
                    .get("parts")
                    .and_then(Value::as_array)
                    .is_some_and(|p| p.len() == 1 && p[0].get("text") == b["parts"][0].get("text"))
        })
    {
        return;
    }
    if let Some(next) = contents.first()
        && let Some(last) = block.last_mut()
        && next.get("role") == last.get("role")
        && let Some(parts) = next.get("parts").and_then(Value::as_array)
        && parts.len() == 1
        && let Some(next_t) = parts[0].get("text").and_then(Value::as_str)
        && let Some(last_t) = last["parts"][0].get("text").and_then(Value::as_str)
    {
        last["parts"] = json!([{ "text": format!("{last_t}{SEP}{next_t}") }]);
        contents.remove(0);
    }
    contents.splice(0..0, block);
}

fn insert_turns_into_responses_input(input: &mut Vec<Value>, turns: &[(String, String)]) {
    let mut idx = 0;
    while idx < input.len()
        && input[idx].get("type").and_then(Value::as_str) == Some("message")
        && matches!(
            input[idx].get("role").and_then(Value::as_str),
            Some("system") | Some("developer")
        )
    {
        idx += 1;
    }
    let mut block: Vec<Value> = turns
        .iter()
        .map(|(r, c)| {
            let text_type = if r == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            json!({
                "type": "message",
                "role": r,
                "content": [{ "type": text_type, "text": c }]
            })
        })
        .collect();
    if let Some(next) = input.get(idx)
        && let Some(last) = block.last_mut()
        && next.get("type").and_then(Value::as_str) == Some("message")
        && next.get("role") == last.get("role")
        && let Some(parts) = next.get("content").and_then(Value::as_array)
        && parts.len() == 1
        && let Some(next_t) = parts[0].get("text").and_then(Value::as_str)
        && let Some(last_t) = last["content"][0].get("text").and_then(Value::as_str)
    {
        let t_type = last["content"][0]["type"].as_str().unwrap_or("input_text");
        last["content"] = json!([{ "type": t_type, "text": format!("{last_t}{SEP}{next_t}") }]);
        input.remove(idx);
    }
    input.splice(idx..idx, block);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::formats::FORMAT_OPENAI;

    #[test]
    fn injects_and_dedups_openai_messages() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "Base"},
                {"role": "user", "content": "hi"}
            ]
        });
        inject_system_prompt(&mut body, FORMAT_OPENAI, "Be terse", "append");
        assert_eq!(body["messages"][0]["content"], "Base\n\nBe terse");
        inject_system_prompt(&mut body, FORMAT_OPENAI, "Be terse", "append");
        assert_eq!(body["messages"][0]["content"], "Base\n\nBe terse");
    }

    #[test]
    fn injects_claude_array_before_cache_control() {
        let mut body = json!({
            "system": [
                {"type": "text", "text": "Base", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": []
        });
        inject_system_prompt(&mut body, FORMAT_CLAUDE, "Be terse", "append");
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 2);
        assert_eq!(sys[0]["text"], "Be terse");
        assert_eq!(sys[1]["text"], "Base");
    }

    #[test]
    fn injects_gemini_and_kiro() {
        let mut gem = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]});
        inject_system_prompt(&mut gem, FORMAT_GEMINI, "Be terse", "append");
        assert_eq!(gem["systemInstruction"]["parts"][0]["text"], "Be terse");

        let mut kiro = json!({
            "conversationState": {
                "currentMessage": { "userInputMessage": { "content": "hello" } }
            }
        });
        inject_system_prompt(&mut kiro, FORMAT_KIRO, "Be terse", "append");
        assert_eq!(
            kiro["conversationState"]["currentMessage"]["userInputMessage"]["content"],
            "hello\n\nBe terse"
        );
        assert!(kiro.get("systemPrompt").is_none());
    }

    #[test]
    fn antigravity_system_and_chat_turns_match_upstream() {
        let mut body = json!({"request":{"contents":[{"role":"user","parts":[{"text":"pong"}]}]}});
        inject_system_prompt(&mut body, FORMAT_ANTIGRAVITY, "brief", "append");
        inject_system_prompt(&mut body, FORMAT_ANTIGRAVITY, "ponytail", "append");
        inject_system_prompt(&mut body, FORMAT_ANTIGRAVITY, "global", "prepend");
        inject_system_prompt(&mut body, FORMAT_ANTIGRAVITY, "key model", "append");
        inject_chat_turns(&mut body, FORMAT_ANTIGRAVITY, &[
            json!({"role":"user","content":"question"}),
            json!({"role":"assistant","content":"answer"}),
        ]);
        assert_eq!(body["request"], json!({
            "systemInstruction":{"parts":[{"text":"global"},{"text":"brief"},{"text":"ponytail"},{"text":"key model"}]},
            "contents":[{"role":"user","parts":[{"text":"question"}]},{"role":"model","parts":[{"text":"answer"}]},{"role":"user","parts":[{"text":"pong"}]}]
        }));
    }
}
