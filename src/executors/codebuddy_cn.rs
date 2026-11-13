use anyhow::Context;
use serde_json::{Value, json};

const NEUTRAL_PROMPT: &str =
    "You are a helpful AI assistant that helps with software engineering tasks.";

fn flatten_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn has_ordered(haystack: &str, parts: &[&str]) -> bool {
    let mut rest = haystack;
    for (idx, part) in parts.iter().enumerate() {
        let Some(pos) = rest.find(part) else {
            return false;
        };
        if idx > 0 && pos == 0 {
            return false;
        }
        rest = &rest[pos + part.len()..];
    }
    true
}

fn matches_agent_pattern(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let direct = [
        "you are claude code",
        "you are cursor",
        "you are windsurf",
        "you are cline",
        "you are aider",
        "you are continue",
        "you are copilot",
        "you are cody",
        "you are a coding agent",
        "you are an ai agent",
        "you are an ai coding agent",
        "you are an ai code agent",
        "you are a code agent",
        "you are a agent",
        "you are an agent",
        "orchestration capabilities",
        "ohmyopencode",
        "<agent-identity>",
        "<role>",
        "<behavior_instructions>",
    ];
    if direct.iter().any(|p| lower.contains(p)) {
        return true;
    }
    if has_ordered(&lower, &["anthropic", "official", "cli"])
        || has_ordered(&lower, &["anxthxropic", "official", "cli"])
        || has_ordered(&lower, &["claude", "code", "official", "cli"])
        || has_ordered(&lower, &["claude", "code", "issues"])
        || has_ordered(&lower, &["give feedback", "claude", "code"])
    {
        return true;
    }
    if let Some(pos) = lower.find("cc_entrypoint") {
        let after = lower[pos + "cc_entrypoint".len()..].trim_start();
        if let Some(after_eq) = after.strip_prefix('=') {
            let val = after_eq.trim_start();
            if ["cli", "vscode", "jetbrains", "gui"]
                .iter()
                .any(|k| val.starts_with(k))
            {
                return true;
            }
        }
    }
    let mut search = lower.as_str();
    while let Some(pos) = search.find("you are ") {
        let after = &search[pos + "you are ".len()..];
        let window: String = after.chars().take(48).collect();
        if let Some(idx) = window.find("ai agent") {
            if idx <= 30 || window[..idx].ends_with("powerful ") {
                return true;
            }
        }
        search = &search[pos + 8..];
    }
    false
}

pub(super) fn prepare(body: &mut Value) -> anyhow::Result<()> {
    let object = body
        .as_object_mut()
        .context("CodeBuddy CN request body must be an object")?;
    object.insert("stream".into(), Value::Bool(true));

    if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            let Some(msg_obj) = message.as_object_mut() else {
                continue;
            };
            if msg_obj.get("role").and_then(Value::as_str) != Some("system") {
                continue;
            }
            let Some(content) = msg_obj.get("content") else {
                continue;
            };
            let text = flatten_content(content);
            if text.is_empty() {
                continue;
            }
            if text.len() > 2000 || matches_agent_pattern(&text) {
                let replacement = if content.is_string() {
                    json!(NEUTRAL_PROMPT)
                } else {
                    json!([{"type": "text", "text": NEUTRAL_PROMPT}])
                };
                msg_obj.insert("content".into(), replacement);
            }
        }
    }

    if let Some(eff) = object.get("reasoning_effort").and_then(Value::as_str) {
        if matches!(eff, "none" | "off") {
            object.remove("reasoning_effort");
        } else if !eff.is_empty() {
            object.insert("reasoning_summary".into(), json!("auto"));
        }
    }
    Ok(())
}

pub(crate) fn parse_code_6004_reset_ms(body: &Value, now_ms: u64) -> Option<u64> {
    let msg = body
        .pointer("/error/message")
        .or_else(|| body.get("msg"))
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let code = body
        .pointer("/error/code")
        .or_else(|| body.get("code"))
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
    if code != Some(6004) && !msg.contains("frequency limit") && !msg.contains("超出频率限制") && !msg.contains("限额") {
        return None;
    }
    // Upstream text such as "2026-11-14 12:00:00 UTC+8" carries the reset instant (commandcode-style parity with codebuddy-cn.js).
    if let Some(reset) = reset_from_message(msg) {
        return Some(reset.saturating_sub(now_ms)).filter(|d| *d > 0);
    }
    if let Some(ms) = body
        .pointer("/error/retryAfterMs")
        .or_else(|| body.get("retryAfterMs"))
        .and_then(Value::as_u64)
    {
        return Some(ms);
    }
    if let Some(sec) = body
        .pointer("/error/retryAfter")
        .or_else(|| body.get("retryAfter"))
        .and_then(Value::as_u64)
    {
        return Some(sec * 1000);
    }
    Some(60_000)
}

/// Parses `YYYY-MM-DD HH:MM:SS [UTC[+]H[:MM]]` (default +08:00) into unix ms, as codebuddy-cn.js does.
fn reset_from_message(msg: &str) -> Option<u64> {
    let re = regex_lite_match(msg)?;
    let (date, time, tz) = re;
    let tz = tz.map_or_else(
        || "+08:00".to_string(),
        |t| {
            if t.contains(':') {
                if t.starts_with('+') { t.to_string() } else { format!("+{t}") }
            } else {
                format!("+{:0>2}:00", t)
            }
        },
    );
    let dt = chrono_free_parse(&date, &time, &tz)?;
    Some(dt)
}

fn regex_lite_match(msg: &str) -> Option<(String, String, Option<String>)> {
    let lower = msg.trim();
    let idx = lower.find(|c: char| c.is_ascii_digit())?;
    let rest = &lower[idx..];
    let mut parts = rest.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    if date.len() != 10 || time.len() < 8 {
        return None;
    }
    let tz = parts
        .find_map(|p| p.strip_prefix("UTC").or_else(|| p.strip_prefix("utc")))
        .map(|s| s.trim_start_matches('+').to_string());
    Some((date.to_string(), time[..8].to_string(), tz))
}

fn chrono_free_parse(date: &str, time: &str, tz: &str) -> Option<u64> {
    let y: i64 = date.get(0..4)?.parse().ok()?;
    let mo: i64 = date.get(5..7)?.parse().ok()?;
    let d: i64 = date.get(8..10)?.parse().ok()?;
    let h: i64 = time.get(0..2)?.parse().ok()?;
    let mi: i64 = time.get(3..5)?.parse().ok()?;
    let s: i64 = time.get(6..8)?.parse().ok()?;
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let tzb = tz.trim_start_matches(['+', '-']);
    let (th, tm) = tzb.split_once(':')?;
    let off = sign * (th.parse::<i64>().ok()? * 3600 + tm.parse::<i64>().ok()? * 60);
    // Days from civil (Howard Hinnant), no chrono dependency.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + s - off;
    u64::try_from(secs).ok().map(|s| s * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebuddy_cn_6004_reset_parsing_works() {
        assert_eq!(
            parse_code_6004_reset_ms(&json!({"error": {"code": 6004, "retryAfter": 45}}), 0),
            Some(45_000)
        );
        assert_eq!(parse_code_6004_reset_ms(&json!({"error": {"code": 500}}), 0), None);
        // Text reset instant: 2026-11-14 12:00:00 at UTC+8 is 2026-11-14 04:00:00 UTC.
        let msg = json!({"error": {"code": 6004, "message": "frequency limit 2026-11-14 12:00:00 UTC+8"}});
        assert_eq!(parse_code_6004_reset_ms(&msg, 0), Some(1_794_628_800_000));
    }
}
