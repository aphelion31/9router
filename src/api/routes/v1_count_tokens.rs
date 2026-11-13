//! POST /v1/messages/count_tokens. Port of `messages/count_tokens/route.js`.
//! Mock estimator: ceil(chars / 4) over system, tools and message content.
//! Char counts use UTF-16 length to match JS `String.length`.
use axum::{
    Json, Router,
    body::Bytes,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};

use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/messages/count_tokens", post(count_tokens))
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn count_value(v: &Value) -> usize {
    match v {
        Value::Null => 0,
        Value::String(s) => utf16_len(s),
        Value::Bool(b) => if *b { 4 } else { 5 },
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string().len(),
            (_, Some(u), _) => u.to_string().len(),
            (_, _, Some(f)) => format!("{f}").len(),
            _ => 0,
        },
        Value::Array(a) => a.iter().map(count_value).sum(),
        Value::Object(o) => o.iter().map(|(k, v)| utf16_len(k) + count_value(v)).sum(),
    }
}

fn count_block(block: &Value) -> usize {
    let Some(obj) = block.as_object() else {
        return match block {
            Value::String(s) => utf16_len(s),
            other => count_value(other),
        };
    };
    let field = |k: &str| obj.get(k).map_or(0, count_value);
    match obj.get("type").and_then(Value::as_str) {
        Some("text") => field("text"),
        Some("tool_use") => field("name") + field("input"),
        Some("tool_result") => field("content"),
        Some("thinking") => field("thinking"),
        _ => count_value(block),
    }
}

fn count_message(msg: &Value) -> usize {
    let Some(obj) = msg.as_object() else {
        return 0;
    };
    match obj.get("content") {
        Some(Value::String(s)) => utf16_len(s),
        Some(Value::Array(blocks)) => blocks.iter().map(count_block).sum(),
        Some(other) => count_value(other),
        None => 0,
    }
}

pub fn estimate_input_tokens(body: &Value) -> u64 {
    let system = body.get("system").map_or(0, count_value);
    let tools = body.get("tools").map_or(0, count_value);
    let messages: usize = body
        .get("messages")
        .and_then(Value::as_array)
        .map_or(0, |m| m.iter().map(count_message).sum());
    (system + tools + messages).div_ceil(4) as u64
}

async fn count_tokens(body: Bytes) -> Response {
    match serde_json::from_slice::<Value>(&body) {
        Ok(v) => Json(json!({"input_tokens": estimate_input_tokens(&v)})).into_response(),
        Err(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Invalid JSON body"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::test_support::{app, call};
    use axum::{body::Body, http::Request};

    #[test]
    fn estimates_chars_div_four_ceiling() {
        let body = json!({
            "system": "abcd",                       // 4
            "messages": [
                {"role":"user","content":"abcde"},  // 5
                {"role":"user","content":[
                    {"type":"text","text":"xyz"},   // 3
                    {"type":"tool_result","content":"qq"} // 2
                ]}
            ]
        });
        assert_eq!(estimate_input_tokens(&body), 14u64.div_ceil(4));
        assert_eq!(estimate_input_tokens(&json!({})), 0);
    }

    #[test]
    fn counts_utf16_units() {
        assert_eq!(estimate_input_tokens(&json!({"system":"😀"})), 1);
    }

    #[tokio::test]
    async fn invalid_json_is_400() {
        let (router, _) = app();
        let (status, v) = call(
            router,
            Request::post("/v1/messages/count_tokens")
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v, json!({"error":"Invalid JSON body"}));
    }

    #[tokio::test]
    async fn returns_input_tokens() {
        let (router, _) = app();
        let (status, v) = call(
            router,
            Request::post("/v1/messages/count_tokens")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"abcdefgh"}]}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v, json!({"input_tokens": 2}));
    }
}
