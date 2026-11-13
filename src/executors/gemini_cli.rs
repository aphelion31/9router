use anyhow::Context;
use serde_json::{Value, json};

const GEMINI_CLI_VERSION: &str = "0.34.0";
const GEMINI_CLI_API_CLIENT: &str = "google-genai-sdk/1.41.0 gl-node/v22.19.0";

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

pub(super) fn url(endpoint: &str, stream: bool) -> anyhow::Result<reqwest::Url> {
    let action = if stream {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };
    let base = endpoint.trim_end_matches('/');
    reqwest::Url::parse(&format!("{base}:{action}")).context("invalid Gemini CLI endpoint")
}

pub(super) fn prepare(
    model: &str,
    body: &mut Value,
    stream: bool,
    provider_specific_data: Option<&Value>,
) -> Vec<(&'static str, String)> {
    let already_wrapped = body.get("request").is_some() && body.get("model").is_some();
    if !already_wrapped {
        let project = provider_specific_data
            .and_then(|d| d.get("projectId"))
            .or_else(|| body.get("project"))
            .cloned()
            .unwrap_or(Value::Null);
        let inner = std::mem::replace(body, Value::Null);
        *body = json!({
            "project": project,
            "model": model,
            "request": inner,
        });
    }
    let model_name = if model.is_empty() { "unknown" } else { model };
    let ua = format!(
        "GeminiCLI/{GEMINI_CLI_VERSION}/{model_name} ({}; {}; terminal)",
        platform(),
        arch()
    );
    vec![
        ("user-agent", ua),
        ("x-goog-api-client", GEMINI_CLI_API_CLIENT.to_string()),
        (
            "accept",
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            }
            .to_string(),
        ),
    ]
}

pub(crate) fn parse_retry_info_ms(body: &Value) -> Option<u64> {
    let details = body.pointer("/error/details").and_then(Value::as_array)?;
    for d in details {
        let is_retry = d
            .get("@type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.contains("RetryInfo"));
        if is_retry
            && let Some(delay) = d.get("retryDelay").and_then(Value::as_str)
            && let Some(secs_str) = delay.trim().strip_suffix('s')
            && let Ok(secs) = secs_str.parse::<f64>()
            && secs > 0.0
        {
            return Some((secs * 1000.0).ceil() as u64);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_cli_retry_info_parses_seconds() {
        let body = json!({
            "error": {
                "code": 429,
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": "4.5s"
                }]
            }
        });
        assert_eq!(parse_retry_info_ms(&body), Some(4500));
    }
}
