use serde_json::{Value, json};

pub const NOUS_INFERENCE_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";
pub const NOUS_VALIDATION_MODEL: &str = "poolside/laguna-xs-2.1:free";

fn models(payload: &Value) -> &[Value] {
    payload
        .as_array()
        .or_else(|| payload["data"].as_array())
        .or_else(|| payload["models"].as_array())
        .or_else(|| payload["results"].as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .filter(|n: &f64| n.is_finite())
}
fn positive_integer(v: &Value) -> Option<u64> {
    let n = number(v)?;
    if n > 0.0 && n.fract() == 0.0 && n < u64::MAX as f64 {
        Some(n as u64)
    } else {
        None
    }
}
fn context(model: &Value) -> Option<u64> {
    positive_integer(
        model
            .get("context_length")
            .filter(|v| !v.is_null())
            .unwrap_or(&model["top_provider"]["context_length"]),
    )
}
fn text_capable(model: &Value) -> bool {
    model["architecture"]["output_modalities"]
        .as_array()
        .is_none_or(|v| v.is_empty() || v.iter().any(|v| v == "text"))
}

pub fn normalize_nous_models(payload: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    for model in models(payload).iter().filter(|m| text_capable(m)) {
        let Some(id) = model["id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let mut m = json!({"id":id,"name":model.get("name").filter(|v| !v.is_null() && *v != "").cloned().unwrap_or_else(|| json!(id))});
        if let Some(n) = context(model) {
            m["contextLength"] = json!(n);
        }
        if let Some(n) = positive_integer(&model["top_provider"]["max_completion_tokens"]) {
            m["maxOutputTokens"] = json!(n);
        }
        if !model["description"].is_null() && model["description"] != "" {
            m["description"] = model["description"].clone();
        }
        result.push(m);
    }
    result.sort_by_key(|m| !m["id"].as_str().unwrap_or("").starts_with("nousresearch/"));
    result
}

pub fn normalize_nous_free_models(payload: &Value) -> Vec<Value> {
    let free: Vec<_> = models(payload)
        .iter()
        .filter(|m| {
            text_capable(m)
                && !m["pricing"]["prompt"].is_null()
                && number(&m["pricing"]["prompt"]) == Some(0.0)
                && !m["pricing"]["completion"].is_null()
                && number(&m["pricing"]["completion"]) == Some(0.0)
                && context(m).is_some_and(|n| n >= 200000)
        })
        .cloned()
        .collect();
    let mut result = normalize_nous_models(&Value::Array(free));
    result.sort_by_key(|m| std::cmp::Reverse(m["contextLength"].as_u64().unwrap_or(0)));
    result
}

pub fn is_nous_api_key_accepted(status: u16) -> bool {
    (200..300).contains(&status)
}
pub fn nous_api_key_validation_error(status: u16) -> String {
    if status == 401 || status == 403 {
        "Invalid or inactive API key".into()
    } else {
        format!("Unable to verify API key (Nous returned {status})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_filters_and_prioritizes() {
        let payload = json!({"data":[{"id":"third","context_length":200000,"pricing":{"prompt":"0","completion":"0"}},{"id":"nousresearch/first","context_length":300000,"pricing":{"prompt":0,"completion":0}},{"id":"image","architecture":{"output_modalities":["image"]}}]});
        assert_eq!(
            normalize_nous_models(&payload)[0]["id"],
            "nousresearch/first"
        );
        let free = normalize_nous_free_models(&payload);
        assert_eq!(free.len(), 2);
        assert_eq!(free[0]["id"], "nousresearch/first");
        assert!(!is_nous_api_key_accepted(401));
        assert!(is_nous_api_key_accepted(200));
    }
}

pub const NOUS_CHAT_COMPLETIONS_URL: &str = "https://inference-api.nousresearch.com/v1/chat/completions";
pub const NOUS_MODELS_URL: &str = "https://inference-api.nousresearch.com/v1/models";

/// Credential probe: an authenticated one-token inference call. `/models` is public,
/// so it cannot prove a key is accepted.
pub fn nous_api_key_probe(api_key: &str) -> (&'static str, Value) {
    (
        NOUS_CHAT_COMPLETIONS_URL,
        json!({
            "headers": {
                "Authorization": format!("Bearer {api_key}"),
                "Content-Type": "application/json",
            },
            "body": {
                "model": NOUS_VALIDATION_MODEL,
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 1,
                "stream": false,
            },
        }),
    )
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    #[test]
    fn probe_is_authenticated_one_token_inference() {
        let (url, req) = nous_api_key_probe("k");
        assert_eq!(url, NOUS_CHAT_COMPLETIONS_URL);
        assert_eq!(req["headers"]["Authorization"], "Bearer k");
        assert_eq!(req["body"]["max_tokens"], 1);
        assert_eq!(req["body"]["stream"], false);
    }
}
