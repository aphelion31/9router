// Port of responsesHandler.js — converts OpenAI Responses API format to Chat Completions.

use serde_json::{Value, json};

pub fn convert_responses_api_format(body: &Value) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(instr) = body["instructions"].as_str().filter(|s| !s.is_empty()) {
        messages.push(json!({ "role": "system", "content": instr }));
    }
    match &body["input"] {
        Value::String(s) => {
            messages.push(json!({ "role": "user", "content": s }));
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    messages.push(json!({ "role": "user", "content": s }));
                } else if item.is_object() {
                    let role = item["role"].as_str().unwrap_or("user");
                    let content = if item["content"].is_null() {
                        item["text"].clone()
                    } else {
                        item["content"].clone()
                    };
                    messages.push(json!({ "role": role, "content": content }));
                }
            }
        }
        _ => {}
    }

    let mut out = json!({
        "model": body["model"].as_str().unwrap_or(""),
        "messages": messages,
        "stream": body["stream"].as_bool().unwrap_or(false),
    });
    if !body["max_output_tokens"].is_null() {
        out["max_tokens"] = body["max_output_tokens"].clone();
    }
    if !body["temperature"].is_null() {
        out["temperature"] = body["temperature"].clone();
    }
    if !body["tools"].is_null() {
        out["tools"] = body["tools"].clone();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_responses_input_to_chat_messages() {
        let req = json!({
            "model": "gpt-4o",
            "instructions": "Be brief",
            "input": "Hello!",
            "max_output_tokens": 100
        });
        let chat = convert_responses_api_format(&req);
        assert_eq!(chat["model"], "gpt-4o");
        assert_eq!(chat["stream"], false);
        assert_eq!(chat["max_tokens"], 100);
        assert_eq!(chat["messages"].as_array().unwrap().len(), 2);
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][1]["content"], "Hello!");
    }
}
