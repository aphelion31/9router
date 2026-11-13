use serde_json::Value;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResponseSignals {
    pub text: String,
    pub has_tools: bool,
    pub has_reasoning: bool,
}

pub fn resolve_anti_flaky_config(settings: &Value) -> (bool, u8) {
    let retries = settings["antiFlakyMaxRetries"]
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| n.floor().clamp(0.0, 5.0) as u8)
        .unwrap_or(2);
    (settings["antiFlakyEnabled"] != false, retries)
}
fn content_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.into();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().or_else(|| v["text"].as_str()))
        .collect()
}
pub fn extract_response_signals(json: &Value) -> ResponseSignals {
    let mut sig = ResponseSignals::default();
    if let Some(choice) = json["choices"].as_array().and_then(|a| a.first()) {
        if let Some(msg) = choice
            .get("message")
            .filter(|v| !v.is_null())
            .or_else(|| choice.get("delta"))
        {
            sig.text = content_text(&msg["content"]);
            sig.has_tools = msg["tool_calls"].as_array().is_some_and(|a| !a.is_empty());
            sig.has_reasoning = msg["reasoning_content"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty());
            return sig;
        }
    }
    if json["type"] == "message" {
        if let Some(blocks) = json["content"].as_array() {
            for block in blocks {
                match block["type"].as_str() {
                    Some("text") => sig.text.push_str(block["text"].as_str().unwrap_or("")),
                    Some("tool_use") => sig.has_tools = true,
                    Some("thinking") => {
                        sig.has_reasoning |= block["thinking"]
                            .as_str()
                            .is_some_and(|s| !s.trim().is_empty())
                    }
                    _ => {}
                }
            }
            return sig;
        }
    }
    if let Some(items) = json["output"].as_array() {
        for item in items {
            match item["type"].as_str() {
                Some("function_call" | "custom_tool_call") => sig.has_tools = true,
                Some("reasoning") => sig.has_reasoning = true,
                _ if item["content"].is_array() => {
                    sig.text.push_str(&content_text(&item["content"]))
                }
                _ => {}
            }
        }
        return sig;
    }
    if let Some(parts) = json["candidates"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c["content"]["parts"].as_array())
    {
        for part in parts {
            if part["thought"] != true {
                if let Some(text) = part["text"].as_str() {
                    sig.text.push_str(text);
                }
            }
            sig.has_tools |= !part["functionCall"].is_null() || !part["function_call"].is_null();
            if !part["inlineData"].is_null() || !part["inline_data"].is_null() {
                sig.text.push(' ');
            }
        }
    }
    sig
}

pub fn is_degenerate_text(text: &str) -> bool {
    let t: Vec<char> = text.trim().chars().collect();
    if t.len() < 24 {
        return false;
    }
    for unit in 1..=16.min(t.len()) {
        if t[..unit].iter().all(|c| c.is_whitespace()) {
            continue;
        }
        let mut covered = 0;
        while covered + unit <= t.len() && t[covered..covered + unit] == t[..unit] {
            covered += unit;
        }
        let rest = &t[covered..];
        let tail_ok = !rest.is_empty() && rest.len() < unit && t[..unit].starts_with(rest);
        if (covered == t.len() || tail_ok) && covered * 5 >= t.len() * 4 && covered / unit >= 4 {
            return true;
        }
    }
    let words: Vec<_> = text.split_whitespace().collect();
    if words.len() < 12 {
        return false;
    }
    let (mut run, mut max) = (1, 1);
    for pair in words.windows(2) {
        if pair[0] == pair[1] {
            run += 1;
        } else {
            run = 1;
        }
        max = max.max(run);
    }
    max >= 8 && max * 2 >= words.len()
}

pub fn detect_flaky_response(json: &Value) -> Option<&'static str> {
    if !json.is_object() {
        return None;
    }
    let sig = extract_response_signals(json);
    if sig.has_tools || sig.has_reasoning {
        return None;
    }
    if sig.text.trim().is_empty() {
        return Some("empty");
    }
    if is_degenerate_text(&sig.text) {
        return Some("degenerate");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn classifies_empty_repetition_tools_and_reasoning() {
        assert_eq!(
            detect_flaky_response(&json!({"choices":[{"message":{"content":""}}]})),
            Some("empty")
        );
        assert_eq!(
            detect_flaky_response(
                &json!({"choices":[{"message":{"content":"abcabcabcabcabcabcabcabc"}}]})
            ),
            Some("degenerate")
        );
        assert_eq!(
            detect_flaky_response(&json!({"choices":[{"message":{"tool_calls":[{}]}}]})),
            None
        );
        assert_eq!(
            detect_flaky_response(
                &json!({"type":"message","content":[{"type":"thinking","thinking":"yes"}]})
            ),
            None
        );
        assert_eq!(
            resolve_anti_flaky_config(&json!({"antiFlakyMaxRetries":99})),
            (true, 5)
        );
    }
}
