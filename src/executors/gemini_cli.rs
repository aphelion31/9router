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
