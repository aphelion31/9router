//! Streaming upstream transport. Request and response translation are handled separately.
use anyhow::{Context, bail};
use serde_json::Value;
mod azure;
mod chatgpt_web;
mod gemini_web;
mod iflow;
mod muse_web;
mod vertex;

#[derive(Debug, Clone)]
pub struct ExecutorRequest {
    pub provider: String,
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub api_key: Option<String>,
    pub access_token: Option<String>,
    /// Overrides the registry endpoint. For Gemini this is the models endpoint.
    pub base_url: Option<String>,
}

/// Sends a translated request and leaves the upstream body unbuffered.
/// The response exposes both `status()` and `bytes_stream()`, including on errors.
pub async fn execute(
    client: &reqwest::Client,
    mut request: ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    let provider = crate::providers::lookup(&request.provider)
        .with_context(|| format!("unknown provider: {}", request.provider))?;
    match provider.id.as_str() {
        "chatgpt-web" => return Err(chatgpt_web::unsupported()),
        "gemini-web" => return Err(gemini_web::unsupported()),
        "muse-web" => return Err(muse_web::unsupported()),
        _ => {}
    }
    let azure_endpoint =
        std::env::var("AZURE_ENDPOINT").unwrap_or_else(|_| "https://api.openai.com".into());
    let vertex_endpoint = "https://aiplatform.googleapis.com";
    let endpoint = request
        .base_url
        .as_deref()
        .or_else(|| (provider.id == "azure").then_some(azure_endpoint.as_str()))
        .or_else(|| (provider.id == "vertex").then_some(vertex_endpoint))
        .or(provider.base_url.as_deref())
        .with_context(|| format!("provider {} has no endpoint", provider.id))?;
    let mut url = if provider.id == "azure" {
        azure::url(endpoint, &request.model)?
    } else if provider.id == "vertex" {
        vertex::url(
            endpoint,
            &request.model,
            request.stream,
            request.api_key.as_deref(),
            request.access_token.as_deref(),
        )?
    } else {
        reqwest::Url::parse(endpoint).context("invalid upstream URL")?
    };
    if !matches!(url.scheme(), "http" | "https") {
        bail!("upstream URL must use HTTP or HTTPS");
    }
    let format = provider.format.as_deref().unwrap_or("openai");
    if format == "gemini" {
        let action = if request.stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid Gemini endpoint"))?
            .pop_if_empty()
            .push(&format!("{}:{}", request.model, action));
        if request.stream {
            url.query_pairs_mut().append_pair("alt", "sse");
        }
    }
    if provider.id == "codex" {
        let body = request
            .body
            .as_object_mut()
            .context("Codex request body must be an object")?;
        body.insert("stream".into(), true.into());
        body.insert("store".into(), false.into());
        body.entry("instructions")
            .or_insert_with(|| "You are a helpful assistant.".into());
    }
    let mut builder = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if provider.id == "iflow" {
        for (name, value) in iflow::prepare(
            &mut request.body,
            request.stream,
            request.api_key.as_deref(),
            request.access_token.as_deref(),
        )? {
            builder = builder.header(name, value);
        }
    }
    if request.stream || provider.id == "codex" {
        builder = builder.header(reqwest::header::ACCEPT, "text/event-stream");
    }
    if provider.id == "azure" {
        if let Some(key) = request
            .api_key
            .as_deref()
            .or(request.access_token.as_deref())
            .map(str::to_owned)
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        {
            builder = builder.header("api-key", key);
        }
        if let Ok(organization) = std::env::var("AZURE_ORGANIZATION") {
            builder = builder.header("openai-organization", organization);
        }
    } else if provider.id == "vertex" {
        if let Some(token) = request.access_token.as_deref() {
            builder = builder.bearer_auth(token);
        }
    } else if format == "claude" {
        builder = builder.header("anthropic-version", "2023-06-01");
        if let Some(key) = request.api_key.as_deref() {
            builder = builder.header("x-api-key", key);
        } else if let Some(token) = request.access_token.as_deref() {
            builder = builder.bearer_auth(token);
        }
    } else if format == "gemini" {
        if let Some(key) = request.api_key.as_deref() {
            builder = builder.header("x-goog-api-key", key);
        } else if let Some(token) = request.access_token.as_deref() {
            builder = builder.bearer_auth(token);
        }
    } else if provider.id == "iflow" {
        if let Some(key) = request.api_key.as_deref() {
            builder = builder.bearer_auth(key);
        }
    } else if let Some(token) = request
        .api_key
        .as_deref()
        .or(request.access_token.as_deref())
    {
        builder = builder.bearer_auth(token);
    }
    Ok(builder
        .body(serde_json::to_vec(&request.body)?)
        .send()
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Bytes,
        extract::OriginalUri,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use serde_json::json;

    async fn mock(
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        body: Bytes,
    ) -> (StatusCode, String) {
        let value: Value = serde_json::from_slice(&body).unwrap();
        (StatusCode::TOO_MANY_REQUESTS, json!({"uri":uri.to_string(), "authorization":headers.get("authorization").and_then(|v|v.to_str().ok()), "key":headers.get("x-api-key").or_else(|| headers.get("x-goog-api-key")).or_else(|| headers.get("api-key")).and_then(|v|v.to_str().ok()), "version":headers.get("anthropic-version").and_then(|v|v.to_str().ok()), "session":headers.get("session-id").and_then(|v|v.to_str().ok()), "timestamp":headers.get("x-iflow-timestamp").and_then(|v|v.to_str().ok()), "signature":headers.get("x-iflow-signature").and_then(|v|v.to_str().ok()), "body":value}).to_string())
    }

    #[tokio::test]
    async fn browser_executors_fail_explicitly() {
        for provider in ["chatgpt-web", "gemini-web", "muse-web"] {
            let error = execute(
                &reqwest::Client::new(),
                ExecutorRequest {
                    provider: provider.into(),
                    model: "model".into(),
                    body: json!({}),
                    stream: false,
                    api_key: None,
                    access_token: None,
                    base_url: None,
                },
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("Unsupported"));
        }
    }

    #[tokio::test]
    async fn protocol_requests_preserve_status_and_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let client = reqwest::Client::new();
        for provider in ["openai", "anthropic", "gemini", "codex"] {
            let response = execute(
                &client,
                ExecutorRequest {
                    provider: provider.into(),
                    model: "model".into(),
                    body: json!({"input":"hi"}),
                    stream: true,
                    api_key: Some("secret".into()),
                    access_token: None,
                    base_url: Some(format!("{base}/v1")),
                },
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            match provider {
                "anthropic" => {
                    assert_eq!(result["key"], "secret");
                    assert_eq!(result["version"], "2023-06-01");
                }
                "gemini" => {
                    assert_eq!(result["key"], "secret");
                    assert_eq!(result["uri"], "/v1/model:streamGenerateContent?alt=sse");
                }
                "codex" => {
                    assert_eq!(result["authorization"], "Bearer secret");
                    assert_eq!(result["body"]["store"], false);
                }
                _ => assert_eq!(result["authorization"], "Bearer secret"),
            }
        }
        task.abort();
    }

    #[tokio::test]
    async fn ollama_local_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/chat", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "ollama-local".into(),
                model: "model".into(),
                body: json!({"model": "model", "messages": []}),
                stream: false,
                api_key: None,
                access_token: None,
                base_url: Some(url),
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["uri"], "/api/chat");
        assert_eq!(result["body"]["model"], "model");
        task.abort();
    }

    #[tokio::test]
    async fn vertex_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "vertex".into(),
                model: "model".into(),
                body: json!({"contents": []}),
                stream: true,
                api_key: Some("secret".into()),
                access_token: None,
                base_url: Some(url),
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert!(
            result["uri"].as_str().unwrap().contains(
                "/publishers/google/models/model:streamGenerateContent?alt=sse&key=secret"
            )
        );
        assert_eq!(result["authorization"], Value::Null);
        assert_eq!(result["body"]["contents"], json!([]));
        task.abort();
    }

    #[tokio::test]
    async fn azure_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "azure".into(),
                model: "deployment".into(),
                body: json!({"messages": []}),
                stream: true,
                api_key: Some("secret".into()),
                access_token: None,
                base_url: Some(url),
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert!(
            result["uri"]
                .as_str()
                .unwrap()
                .starts_with("/openai/deployments/deployment/chat/completions?api-version=")
        );
        assert_eq!(result["key"], "secret");
        assert_eq!(result["authorization"], Value::Null);
        assert_eq!(result["body"]["messages"], json!([]));
        task.abort();
    }

    #[tokio::test]
    async fn iflow_mock_signed_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "iflow".into(),
                model: "model".into(),
                body: json!({"messages": []}),
                stream: true,
                api_key: Some("secret".into()),
                access_token: None,
                base_url: Some(url),
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["uri"], "/v1/chat/completions");
        assert_eq!(result["authorization"], "Bearer secret");
        assert_eq!(result["body"]["stream_options"]["include_usage"], true);
        let payload = format!(
            "iFlow-Cli:{}:{}",
            result["session"].as_str().unwrap(),
            result["timestamp"].as_str().unwrap()
        );
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"secret");
        let expected: String = ring::hmac::sign(&key, payload.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(result["signature"], expected);
        task.abort();
    }
}
