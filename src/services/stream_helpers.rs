use serde_json::Value;

pub fn parse_sse_line(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).ok();
    }
    if !line.starts_with('d') || line.len() < 5 || !line.is_char_boundary(5) {
        return None;
    }
    let data = line[5..].trim();
    if data == "[DONE]" {
        return Some(serde_json::json!({"done": true}));
    }
    serde_json::from_str(data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sse_and_ndjson() {
        assert_eq!(parse_sse_line("data: [DONE]").unwrap()["done"], true);
        assert_eq!(parse_sse_line("data: {\"x\":1}").unwrap()["x"], 1);
        assert_eq!(parse_sse_line(" {\"x\":2} ").unwrap()["x"], 2);
        assert!(parse_sse_line("event: message").is_none());
        assert!(parse_sse_line("data: bad").is_none());
    }
}
