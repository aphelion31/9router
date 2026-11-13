//! Streaming upstream transport. Request and response translation are handled separately.
use anyhow::{Context, bail};
use serde_json::Value;
mod antigravity;
mod azure;
mod browser_executable_check;
mod deepseek_web_done_terminator;
pub(crate) mod bedrock;
mod chatgpt_web;
mod devin_cli;
mod codebuddy_cn;
mod codebuddy_intl;
pub(crate) mod commandcode;
mod gemini_cli;
mod gemini_web;
mod github;
mod grok_cli;
mod iflow;
mod kimchi;
mod mimo_free;
mod minimax_code;
mod muse_web;
mod opencode;
mod opencode_go;
mod opencode_zen;
mod vertex;
mod xiaomi_mimo;
mod xiaomi_tokenplan;
mod zed;
pub(crate) mod trae;
pub(crate) mod windsurf;

#[derive(Debug, Clone, Default)]
pub struct ExecutorRequest {
    pub provider: String,
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub api_key: Option<String>,
    pub access_token: Option<String>,
    /// Overrides the registry endpoint. For Gemini this is the models endpoint.
    pub base_url: Option<String>,
    pub provider_specific_data: Option<Value>,
}

fn retry_policy(status: u16) -> (usize, std::time::Duration) {
    match status {
        502 => (3, std::time::Duration::from_secs(3)),
        503 => (3, std::time::Duration::from_secs(2)),
        504 => (2, std::time::Duration::from_secs(3)),
        _ => (0, std::time::Duration::ZERO),
    }
}

/// Sends a translated request and leaves the upstream body unbuffered.
/// The response exposes both `status()` and `bytes_stream()`, including on errors.
pub async fn execute(
    client: &reqwest::Client,
    mut request: ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    if request.provider == "devin-cli" {
        return Err(devin_cli::unsupported());
    }
    if request.provider == "trae" {
        return trae::execute(client, &request).await;
    }
    if request.provider == "windsurf" {
        return windsurf::execute(client, &request).await;
    }
    let provider = crate::providers::lookup(&request.provider)
        .with_context(|| format!("unknown provider: {}", request.provider))?;
    match provider.id.as_str() {
        "chatgpt-web" => return Err(chatgpt_web::unsupported()),
        "gemini-web" => return Err(gemini_web::unsupported()),
        "muse-web" => return Err(muse_web::unsupported()),
        _ => {}
    }
    if matches!(provider.id.as_str(), "bedrock" | "bedrock-xai") {
        let (url, headers, body) = bedrock::prepare(
            request.base_url.as_deref(),
            provider.id == "bedrock-xai",
            &request.model,
            &mut request.body,
            request.stream,
        )?;
        // AWS credentials must never be forwarded to a redirect target.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut builder = client.post(url).body(body);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        return Ok(builder.send().await?);
    }
    let mut vertex_project = None;
    if matches!(provider.id.as_str(), "vertex" | "vertex-partner") {
        if let Some((token, project)) = vertex::refresh(client, request.api_key.as_deref()).await? {
            request.access_token = Some(token);
            vertex_project = project;
        }
    }
    let azure_endpoint =
        std::env::var("AZURE_ENDPOINT").unwrap_or_else(|_| "https://api.openai.com".into());
    let vertex_endpoint = "https://aiplatform.googleapis.com";
    let endpoint = request
        .base_url
        .as_deref()
        .or_else(|| (provider.id == "xiaomi-tokenplan").then(|| xiaomi_tokenplan::endpoint(request.provider_specific_data.as_ref())))
        .or_else(|| (provider.id == "azure").then(|| request.provider_specific_data.as_ref()
            .and_then(|v| v.get("azureEndpoint")).and_then(Value::as_str).filter(|s| !s.is_empty())).flatten())
        .or_else(|| (provider.id == "azure").then_some(azure_endpoint.as_str()))
        .or_else(|| {
            matches!(provider.id.as_str(), "vertex" | "vertex-partner").then_some(vertex_endpoint)
        })
        .or_else(|| (provider.id == "antigravity").then_some(antigravity::default_endpoint()))
        .or(provider.base_url.as_deref())
        .with_context(|| format!("provider {} has no endpoint", provider.id))?;
    if provider.id == "mimo-free" {
        return mimo_free::execute(client, endpoint, &mut request.body, request.stream).await;
    }
    if provider.id == "zed" {
        return zed::execute(
            client,
            endpoint,
            &request.model,
            &request.body,
            request
                .access_token
                .as_deref()
                .or(request.api_key.as_deref()),
        )
        .await;
    }
    let responses = matches!(provider.id.as_str(), "opencode-go" | "opencode-zen")
        && opencode_go::is_responses(&provider.id, &request.model);
    let mut url = if provider.id == "ollama-local" && request.base_url.is_none() {
        let host = request.provider_specific_data.as_ref()
            .and_then(|data| data.get("baseUrl"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .unwrap_or("http://localhost:11434")
            .trim_end_matches('/');
        reqwest::Url::parse(&format!("{host}/api/chat")).context("invalid Ollama host")?
    } else if responses {
        opencode_go::url(endpoint)?
    } else if provider.id == "opencode" {
        opencode::url(endpoint, &request.model)?
    } else if provider.id == "xiaomi-tokenplan" {
        xiaomi_tokenplan::url(endpoint, request.model.ends_with("-claude"))?
    } else if provider.id == "azure" {
        azure::url(endpoint, &request.model, request.provider_specific_data.as_ref())?
    } else if provider.id == "antigravity" {
        antigravity::url(endpoint, &request.model, request.stream)?
    } else if provider.id == "gemini-cli" {
        gemini_cli::url(endpoint, request.stream)?
    } else if matches!(provider.id.as_str(), "vertex" | "vertex-partner") {
        vertex::url(
            endpoint,
            provider.id == "vertex-partner",
            vertex_project.as_deref(),
            &request.model,
            request.stream,
            request.api_key.as_deref(),
            request.access_token.as_deref(),
        )?
    } else {
        reqwest::Url::parse(endpoint).context("invalid upstream URL")?
    };
    let github_claude =
        provider.id == "github" && request.model.to_ascii_lowercase().contains("claude");
    let tokenplan_claude = provider.id == "xiaomi-tokenplan" && request.model.ends_with("-claude");
    if github_claude || tokenplan_claude {
        if github_claude {
            url.set_path("/v1/messages");
        }
        let model = request
            .model
            .strip_suffix("-claude")
            .unwrap_or(&request.model);
        request.body = crate::translator::claude::openai_to_claude_request(
            model,
            &request.body,
            request.stream,
            None,
        );
        if let Some(body) = request.body.as_object_mut() {
            body.remove("_toolNameMap");
        }
    }
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
    if provider.id == "kimchi" {
        kimchi::prepare(&request.model, &mut request.body);
    }
    if provider.id == "xiaomi-mimo" {
        xiaomi_mimo::prepare(&request.model, &mut request.body, false);
    }
    if provider.id == "opencode" {
        opencode::prepare(&mut request.body, &request.model);
    }
    if responses {
        opencode_go::prepare(&mut request.body, &request.model);
    }
    if provider.id == "opencode-zen" {
        opencode_zen::prepare(&mut request.body, responses);
    }
    if provider.id == "github" && !github_claude {
        github::prepare(&request.model, &mut request.body)?;
    }
    let mut builder = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if provider.id == "github" {
        builder = builder
            .header("copilot-integration-id", "vscode-chat")
            .header("editor-version", "vscode/1.110.0")
            .header("editor-plugin-version", "copilot-chat/0.38.0")
            .header("user-agent", "GitHubCopilotChat/0.38.0")
            .header("openai-intent", "conversation-panel")
            .header("x-github-api-version", "2025-04-01")
            .header("x-vscode-user-agent-library-version", "electron-fetch")
            .header("x-initiator", "user")
            .header("anthropic-version", "2023-06-01");
    }
    if provider.id == "opencode" {
        let session = opencode::session();
        builder = builder
            .header("user-agent", "opencode/latest/2.2.5/cli")
            .header("x-opencode-client", "cli")
            .header("x-opencode-project", "global")
            .header("x-opencode-session", session)
            .header("x-opencode-session-id", session)
            .header("x-session-affinity", session)
            .header("x-session-id", session)
            .bearer_auth("public");
    }
    if provider.id == "opencode-go" {
        builder = builder.header("x-opencode-session", opencode::session());
    }
    if provider.id == "opencode-zen" {
        builder = builder
            .header("x-opencode-session", opencode::session())
            .header("user-agent", "opencode/latest/2.2.5/cli")
            .header("x-opencode-client", "desktop")
            .header("x-opencode-project", "global");
        if request.model.ends_with("-free") {
            builder = builder.bearer_auth("public");
        }
    }
    if tokenplan_claude {
        builder = builder.header("anthropic-version", "2023-06-01");
    }
    if provider.id == "antigravity" {
        for (name, value) in antigravity::prepare(
            &request.model,
            &mut request.body,
            request.stream,
            request.provider_specific_data.as_ref(),
        )? {
            builder = builder.header(name, value);
        }
    }
    if provider.id == "gemini-cli" {
        for (name, value) in gemini_cli::prepare(
            &request.model,
            &mut request.body,
            request.stream,
            request.provider_specific_data.as_ref(),
        ) {
            builder = builder.header(name, value);
        }
    }
    if provider.id == "commandcode" {
        for (name, value) in commandcode::prepare(&mut request.body)? {
            builder = builder.header(name, value);
        }
    }
    if provider.id == "codebuddy-cn" {
        codebuddy_cn::prepare(&mut request.body)?;
    }
    if provider.id == "codebuddy-intl" {
        codebuddy_intl::prepare(&mut request.body)?;
    }
    if provider.id == "grok-cli" {
        for (name, value) in grok_cli::prepare(
            &request.model,
            &mut request.body,
            request.provider_specific_data.as_ref(),
        )? {
            builder = builder.header(name, value);
        }
    }
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
    if request.stream || matches!(provider.id.as_str(), "codex" | "codebuddy-cn" | "codebuddy-intl") {
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
        if let Some(organization) = request.provider_specific_data.as_ref()
            .and_then(|v| v.get("organization")).and_then(Value::as_str).filter(|s| !s.is_empty())
            .map(str::to_owned).or_else(|| std::env::var("AZURE_ORGANIZATION").ok()) {
            builder = builder.header("openai-organization", organization);
        }
    } else if matches!(provider.id.as_str(), "vertex" | "vertex-partner") {
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
    } else if provider.id != "opencode"
        && !(provider.id == "opencode-zen" && request.model.ends_with("-free"))
        && let Some(token) = request
            .api_key
            .as_deref()
            .or(request.access_token.as_deref())
    {
        builder = builder.bearer_auth(token);
    }
    let outgoing = builder.body(serde_json::to_vec(&request.body)?).build()?;
    let mut attempts = 0;
    let response = loop {
        let cloned = outgoing.try_clone().context("upstream request cannot be retried")?;
        let result = client.execute(cloned).await;
        let status = result.as_ref().map(|r| r.status().as_u16()).unwrap_or(502);
        let (limit, delay) = retry_policy(status);
        if attempts >= limit || result.as_ref().is_err_and(|e| e.is_timeout()) {
            break result?;
        }
        attempts += 1;
        tokio::time::sleep(delay).await;
    };
    if provider.id == "minimax-code" && matches!(response.status().as_u16(), 402 | 403) {
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await?;
        let text = String::from_utf8_lossy(&bytes);
        let quota = minimax_code::is_quota(status.as_u16(), &text);
        let body = if quota {
            serde_json::json!({"type":"error", "error":{"type":"rate_limit_error", "message":format!("usage limit reached: {text}")}}).to_string().into_bytes()
        } else {
            bytes.to_vec()
        };
        let mut rebuilt =
            axum::http::Response::builder().status(if quota { 429 } else { status.as_u16() });
        for (name, value) in &headers {
            if name != reqwest::header::CONTENT_LENGTH && name != reqwest::header::CONTENT_ENCODING
            {
                rebuilt = rebuilt.header(name, value);
            }
        }
        return Ok(reqwest::Response::from(
            rebuilt.body(reqwest::Body::from(body))?,
        ));
    }
    Ok(response)
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

    #[tokio::test]
    async fn minimax_code_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(post(|| async {
                    (StatusCode::FORBIDDEN, "insufficient credit")
                })),
            )
            .await
            .unwrap()
        });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "minimax-code".into(),
                model: "test".into(),
                body: json!({"messages":[]}),
                stream: true,
                api_key: None,
                access_token: Some("test".into()),
                base_url: Some(url), ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            String::from_utf8_lossy(&response.bytes().await.unwrap())
                .contains("insufficient credit")
        );
        task.abort();
    }

    #[tokio::test]
    async fn xiaomi_mimo_mock_request() {
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
                provider: "xiaomi-mimo".into(),
                model: "mimo-v2.6-pro".into(),
                body: json!({"messages":[], "reasoning_effort":"xhigh"}),
                stream: true,
                api_key: Some("test".into()),
                access_token: None,
                base_url: Some(url), ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["authorization"], "Bearer test");
        assert_eq!(result["body"]["reasoning_effort"], "xhigh");
        task.abort();
    }

    #[tokio::test]
    async fn opencode_zen_mock_request() {
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
                provider: "opencode-zen".into(),
                model: "test-free".into(),
                body: json!({"messages":[]}),
                stream: true,
                api_key: Some("test".into()),
                access_token: None,
                base_url: Some(url), ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["authorization"], "Bearer public");
        assert_eq!(result["body"]["tools"].as_array().unwrap().len(), 4);
        task.abort();
    }

    #[tokio::test]
    async fn opencode_go_mock_request() {
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
                provider: "opencode-go".into(),
                model: "muse-spark-test".into(),
                body: json!({"input":[{"type":"reasoning"}],"max_tokens":100}),
                stream: true,
                api_key: Some("test".into()),
                access_token: None,
                base_url: Some(url), ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["uri"], "/v1/responses");
        assert_eq!(result["body"]["store"], false);
        assert_eq!(result["body"]["max_output_tokens"], 100);
        assert_eq!(result["body"]["input"], json!([]));
        task.abort();
    }

    #[tokio::test]
    async fn opencode_mock_request() {
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
                provider: "opencode".into(),
                model: "test".into(),
                body: json!({"messages":[], "reasoning_effort":"auto"}),
                stream: true,
                api_key: None,
                access_token: None,
                base_url: Some(url), ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["uri"], "/inference/openai/v1/chat/completions");
        assert_eq!(result["authorization"], "Bearer public");
        assert_eq!(result["body"]["stream"], true);
        assert!(result["body"]["reasoning_effort"].is_null());
        task.abort();
    }

    #[tokio::test]
    async fn xiaomi_tokenplan_mock_request() {
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
        for (model, path) in [
            ("mimo-v2.5-pro", "/v1/chat/completions"),
            ("mimo-v2.5-pro-claude", "/anthropic/v1/messages"),
        ] {
            let response = execute(
                &reqwest::Client::new(),
                ExecutorRequest {
                    provider: "xiaomi-tokenplan".into(),
                    model: model.into(),
                    body: json!({"messages":[{"role":"user", "content":"hi"}]}),
                    stream: false,
                    api_key: Some("test".into()),
                    access_token: None,
                    base_url: Some(url.clone()), ..Default::default()
                },
            )
            .await
            .unwrap();
            let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            assert_eq!(result["uri"], path);
            assert_eq!(result["authorization"], "Bearer test");
        }
        task.abort();
    }

    #[tokio::test]
    async fn kimchi_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(&reqwest::Client::new(), ExecutorRequest { provider: "kimchi".into(), model: "claude-test".into(), body: json!({"system":"policy", "messages":[{"role":"assistant", "reasoning_content":"private reasoning", "content":[{"type":"text", "text":"hi", "signature":"x"}]}], "thinking":{}, "reasoning_effort":"high"}), stream: false, api_key: Some("test".into()), access_token: None, base_url: Some(url), ..Default::default() }).await.unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["body"]["messages"][0]["content"], "policy");
        assert!(result["body"]["thinking"].is_null());
        assert!(result["body"]["reasoning_effort"].is_null());
        assert!(result["body"]["messages"][1]["content"][0]["signature"].is_null());
        task.abort();
    }

    async fn mock(
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        body: Bytes,
    ) -> (StatusCode, String) {
        let value: Value = serde_json::from_slice(&body).unwrap();
        let all_headers: serde_json::Map<String, Value> = headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), Value::String(s.to_string()))))
            .collect();
        (StatusCode::TOO_MANY_REQUESTS, json!({"uri":uri.to_string(), "authorization":headers.get("authorization").and_then(|v|v.to_str().ok()), "key":headers.get("x-api-key").or_else(|| headers.get("x-goog-api-key")).or_else(|| headers.get("api-key")).and_then(|v|v.to_str().ok()), "version":headers.get("anthropic-version").and_then(|v|v.to_str().ok()), "session":headers.get("session-id").and_then(|v|v.to_str().ok()), "timestamp":headers.get("x-iflow-timestamp").and_then(|v|v.to_str().ok()), "signature":headers.get("x-iflow-signature").and_then(|v|v.to_str().ok()), "headers": all_headers, "body":value}).to_string())
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
                    ..Default::default()
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
                    ..Default::default()
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
    async fn bedrock_mock_request() {
        if std::env::var_os("AWS_ACCESS_KEY_ID").is_none()
            || std::env::var_os("AWS_SECRET_ACCESS_KEY").is_none()
        {
            return;
        }
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
                provider: "bedrock".into(),
                model: "anthropic.claude-v2".into(),
                body: json!({"model":"anthropic.claude-v2", "stream":false, "messages": []}),
                stream: false,
                api_key: None,
                access_token: None,
                base_url: Some(url),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["uri"], "/model/anthropic.claude-v2/invoke");
        assert!(
            result["authorization"]
                .as_str()
                .unwrap()
                .starts_with("AWS4-HMAC-SHA256 ")
        );
        assert_eq!(result["body"]["anthropic_version"], "bedrock-2023-05-31");
        assert!(result["body"].get("model").is_none());
        task.abort();
    }

    #[tokio::test]
    async fn orcarouter_mock_request() {
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
                provider: "orcarouter".into(),
                model: "model".into(),
                body: json!({"messages": []}),
                stream: true,
                api_key: Some("secret".into()),
                access_token: None,
                base_url: Some(url),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["authorization"], "Bearer secret");
        assert_eq!(result["uri"], "/v1/chat/completions");
        task.abort();
    }

    #[tokio::test]
    async fn github_claude_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "github".into(),
                model: "claude-sonnet-4".into(),
                body: json!({"messages": [{"role":"user", "content":"hello"}]}),
                stream: false,
                api_key: None,
                access_token: Some("secret".into()),
                base_url: Some(url),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["uri"], "/v1/messages");
        assert_eq!(result["authorization"], "Bearer secret");
        assert_eq!(result["version"], "2023-06-01");
        assert_eq!(result["body"]["model"], "claude-sonnet-4");
        task.abort();
    }

    #[tokio::test]
    async fn github_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(&reqwest::Client::new(), ExecutorRequest { provider: "github".into(), model: "gpt-5".into(), body: json!({"max_tokens": 123, "reasoning_effort":"none", "messages": [{"role":"user", "content":[{"type":"thinking", "text":"hello"}]}]}), stream: false, api_key: None, access_token: Some("secret".into()), base_url: Some(url), ..Default::default() }).await.unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["authorization"], "Bearer secret");
        assert_eq!(result["body"]["max_completion_tokens"], 123);
        assert_eq!(result["body"]["reasoning_effort"], Value::Null);
        assert_eq!(result["body"]["messages"][0]["content"][0]["type"], "text");
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
                provider_specific_data: Some(json!({"baseUrl": format!(" {}/// ", url.trim_end_matches("/api/chat"))})),
                ..Default::default()
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
    async fn retries_transient_upstream_status() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/chat", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(move || {
                let seen = seen.clone();
                async move {
                    if seen.fetch_add(1, Ordering::SeqCst) == 0 { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::OK }
                }
            }))).await.unwrap()
        });
        let response = execute(&reqwest::Client::new(), ExecutorRequest {
            provider: "ollama-local".into(), model: "model".into(), body: json!({}), base_url: Some(url), ..Default::default()
        }).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(retry_policy(429).0, 0);
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
                ..Default::default()
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
                provider_specific_data: Some(json!({"azureEndpoint": url, "deployment": "override", "apiVersion": "test-version", "organization": "org"})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert!(
            result["uri"]
                .as_str()
                .unwrap()
                .starts_with("/openai/deployments/override/chat/completions?api-version=test-version")
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
                ..Default::default()
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

    #[tokio::test]
    async fn gemini_cli_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1internal", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "gemini-cli".into(),
                model: "gemini-2.5-pro".into(),
                body: json!({"contents": []}),
                stream: true,
                access_token: Some("secret-tok".into()),
                base_url: Some(url),
                provider_specific_data: Some(json!({"projectId": "proj-123"})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["uri"], "/v1internal:streamGenerateContent?alt=sse");
        assert_eq!(result["authorization"], "Bearer secret-tok");
        assert_eq!(result["body"]["project"], "proj-123");
        assert_eq!(result["body"]["model"], "gemini-2.5-pro");
        assert_eq!(result["body"]["request"], json!({"contents": []}));
        assert!(
            result["headers"]["user-agent"]
                .as_str()
                .unwrap()
                .starts_with("GeminiCLI/0.34.0/gemini-2.5-pro ")
        );
        assert_eq!(
            result["headers"]["x-goog-api-client"],
            "google-genai-sdk/1.41.0 gl-node/v22.19.0"
        );
        task.abort();
    }

    #[tokio::test]
    async fn codebuddy_intl_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v2/chat/completions",
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
                provider: "codebuddy-intl".into(),
                model: "claude-sonnet-4".into(),
                body: json!({
                    "reasoning_effort": "high",
                    "messages": [
                        {"role": "system", "content": "custom"},
                        {"role": "developer", "content": "dev"},
                        {"role": "user", "content": "hello"}
                    ]
                }),
                stream: false,
                api_key: Some("cb-key".into()),
                base_url: Some(url),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["authorization"], "Bearer cb-key");
        assert_eq!(result["body"]["stream"], true);
        assert_eq!(result["body"]["reasoning_summary"], "auto");
        assert_eq!(
            result["body"]["messages"],
            json!([
                {"role": "system", "content": "You are CodeBuddy Code."},
                {"role": "user", "content": [{"type": "text", "text": "hello"}]}
            ])
        );
        task.abort();
    }

    #[tokio::test]
    async fn codebuddy_cn_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v2/chat/completions",
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
                provider: "codebuddy-cn".into(),
                model: "deepseek-chat".into(),
                body: json!({
                    "reasoning_effort": "none",
                    "messages": [
                        {"role": "system", "content": "You are Claude Code, Anthropic's official CLI."},
                        {"role": "user", "content": "hi"}
                    ]
                }),
                stream: false,
                api_key: Some("cb-cn-key".into()),
                base_url: Some(url),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["authorization"], "Bearer cb-cn-key");
        assert_eq!(result["body"]["stream"], true);
        assert!(result["body"].get("reasoning_effort").is_none());
        assert_eq!(
            result["body"]["messages"][0]["content"],
            "You are a helpful AI assistant that helps with software engineering tasks."
        );
        task.abort();
    }

    #[tokio::test]
    async fn commandcode_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/alpha/generate", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(post(mock)))
                .await
                .unwrap()
        });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "commandcode".into(),
                model: "deepseek/deepseek-v4-pro".into(),
                body: json!({"threadId": "thread-abc", "messages": []}),
                stream: false,
                api_key: Some("user_secret".into()),
                base_url: Some(url),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(result["authorization"], "Bearer user_secret");
        assert_eq!(result["body"]["stream"], true);
        assert_eq!(result["headers"]["user-agent"], "cli");
        assert_eq!(result["headers"]["x-command-code-version"], "1.88.0");
        assert_eq!(result["headers"]["x-session-id"], "thread-abc");
        assert_eq!(result["headers"]["x-cli-environment"], "production");
        assert_eq!(result["headers"]["x-cli-surface"], "cli");
        assert_eq!(result["headers"]["x-taste-learning"], "false");

        let mut decoder = commandcode::Decoder::new("deepseek/deepseek-v4-pro");
        let sse = decoder
            .push(b"{\"type\":\"text-delta\",\"delta\":\"hi\"}\n")
            .unwrap();
        let text = String::from_utf8(sse).unwrap();
        assert!(text.starts_with("data: "));
        assert!(text.contains("\"content\":\"hi\""));
        assert_eq!(
            String::from_utf8(decoder.finish()).unwrap(),
            "data: [DONE]\n\n"
        );
        task.abort();
    }

    #[tokio::test]
    async fn trae_mock_request() {
        use axum::routing::get;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route(
                "/chat_sessions",
                post(|headers: HeaderMap, body: Bytes| async move {
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(
                        headers.get("authorization").unwrap().to_str().unwrap(),
                        "Cloud-IDE-JWT jwt-tok"
                    );
                    assert_eq!(v["mode"], "code");
                    assert_eq!(v["initial_message"]["model_name"], "claude-sonnet-4");
                    json!({"code": 0, "data": {"chat_session_id": "sess-1", "message_id": "msg-1"}}).to_string()
                }),
            )
            .route(
                "/chat_sessions/sess-1/events",
                get(|OriginalUri(uri): OriginalUri, headers: HeaderMap| async move {
                    assert_eq!(
                        uri.to_string(),
                        "/chat_sessions/sess-1/events?reply_to_message_id=msg-1"
                    );
                    assert_eq!(
                        headers.get("authorization").unwrap().to_str().unwrap(),
                        "Cloud-IDE-JWT jwt-tok"
                    );
                    "event: plan_item\ndata: {\"id\":\"p1\",\"thought\":\"Hello\"}\n\nevent: plan_item\ndata: {\"id\":\"p1\",\"thought\":\"Hello world\"}\n\nevent: token_usage\ndata: {\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}\n\nevent: done\ndata: {}\n\n"
                }),
            );
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "trae".into(),
                model: "claude-sonnet-4".into(),
                body: json!({"messages": [{"role": "user", "content": "hi"}]}),
                stream: true,
                access_token: Some("jwt-tok".into()),
                base_url: Some(base),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let raw = response.bytes().await.unwrap();
        let mut decoder = trae::Decoder::new("claude-sonnet-4");
        let mut sse = String::from_utf8(decoder.push(&raw).unwrap()).unwrap();
        sse.push_str(&String::from_utf8(decoder.finish()).unwrap());
        assert!(sse.contains("\"content\":\"Hello\""));
        assert!(sse.contains("\"content\":\" world\""));
        assert!(sse.contains("\"total_tokens\":5"));
        assert!(sse.contains("data: [DONE]"));

        let non_stream: Value =
            serde_json::from_slice(&trae::decode_non_stream(&raw, "claude-sonnet-4")).unwrap();
        assert_eq!(
            non_stream["choices"][0]["message"]["content"],
            "Hello world"
        );
        assert_eq!(non_stream["usage"]["total_tokens"], 5);
        task.abort();
    }

    #[tokio::test]
    async fn grok_cli_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let app = Router::new().route("/", post(mock));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "grok-cli".into(),
                model: "grok-4.5-high".into(),
                body: json!({
                    "messages": [
                        {"role": "user", "content": "hi"},
                        {"role": "assistant", "content": "hello"},
                        {"role": "user", "content": "next"}
                    ],
                    "tools": [{
                        "type": "function",
                        "function": {"name": "read", "parameters": {"type": "object"}}
                    }],
                    "max_tokens": 100
                }),
                stream: false,
                access_token: Some("grok-tok".into()),
                base_url: Some(base),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let echo: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        let headers = &echo["headers"];
        let body = &echo["body"];
        assert!(
            headers["user-agent"]
                .as_str()
                .unwrap()
                .starts_with("grok-shell/")
        );
        assert_eq!(headers["x-grok-client-identifier"], "grok-shell");
        assert_eq!(headers["x-grok-turn-idx"], "2");
        assert_eq!(headers["x-grok-model-override"], "grok-4.5");
        assert_eq!(body["model"], "grok-4.5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["tools"][0]["name"], "read");
        task.abort();
    }

    #[tokio::test]
    async fn windsurf_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}/GetChatMessage");
        let app = Router::new().route(
            "/GetChatMessage",
            post(|headers: HeaderMap, body: Bytes| async move {
                assert_eq!(
                    headers.get("content-type").and_then(|v| v.to_str().ok()),
                    Some("application/grpc-web+proto")
                );
                assert_eq!(
                    headers.get("x-grpc-web").and_then(|v| v.to_str().ok()),
                    Some("1")
                );
                // 5-byte gRPC-web frame header + protobuf payload containing resolved model name
                assert!(body.len() > 5);
                assert_eq!(body[0], 0x00);
                let payload_str = String::from_utf8_lossy(&body[5..]);
                assert!(payload_str.contains("claude-sonnet-4-6"));
                assert!(payload_str.contains("ws-key"));

                // Encode two CompletionChunk frames: ContentChunk("Hi ") and DoneChunk(10, 4)
                fn varint(mut v: u64, out: &mut Vec<u8>) {
                    while v > 0x7f {
                        out.push(((v & 0x7f) as u8) | 0x80);
                        v >>= 7;
                    }
                    out.push(v as u8);
                }
                let mut content_inner = Vec::new();
                content_inner.push((1 << 3) | 2);
                varint(3, &mut content_inner);
                content_inner.extend_from_slice(b"Hi ");
                let mut chunk1 = Vec::new();
                chunk1.push((1 << 3) | 2);
                varint(content_inner.len() as u64, &mut chunk1);
                chunk1.extend_from_slice(&content_inner);

                let mut usage = Vec::new();
                usage.push(1 << 3);
                varint(10, &mut usage);
                usage.push(2 << 3);
                varint(4, &mut usage);
                let mut done_inner = Vec::new();
                done_inner.push((1 << 3) | 2);
                varint(usage.len() as u64, &mut done_inner);
                done_inner.extend_from_slice(&usage);
                let mut chunk2 = Vec::new();
                chunk2.push((3 << 3) | 2);
                varint(done_inner.len() as u64, &mut chunk2);
                chunk2.extend_from_slice(&done_inner);

                let mut out = windsurf::grpc_web_frame(&chunk1);
                out.extend_from_slice(&windsurf::grpc_web_frame(&chunk2));
                (StatusCode::OK, out)
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "windsurf".into(),
                model: "claude-sonnet-4.6".into(),
                body: json!({"messages": [{"role": "user", "content": "hello"}]}),
                stream: true,
                access_token: Some("ws-key".into()),
                base_url: Some(base),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let raw = response.bytes().await.unwrap();
        let mut decoder = windsurf::Decoder::new("claude-sonnet-4.6");
        let mut sse = String::from_utf8(decoder.push(&raw).unwrap()).unwrap();
        sse.push_str(&String::from_utf8(decoder.finish()).unwrap());
        assert!(sse.contains("\"content\":\"Hi \""));
        assert!(sse.contains("\"total_tokens\":14"));
        assert!(sse.contains("data: [DONE]"));

        let non_stream: Value =
            serde_json::from_slice(&windsurf::decode_non_stream(&raw, "claude-sonnet-4.6"))
                .unwrap();
        assert_eq!(non_stream["choices"][0]["message"]["content"], "Hi ");
        assert_eq!(non_stream["usage"]["total_tokens"], 14);
        task.abort();
    }

    #[tokio::test]
    async fn antigravity_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let app = Router::new().route("/v1internal:streamGenerateContent", post(mock));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = execute(
            &reqwest::Client::new(),
            ExecutorRequest {
                provider: "antigravity".into(),
                model: "claude-sonnet-4-6".into(),
                body: json!({
                    "thinking": {"type": "enabled"},
                    "requestType": "agent",
                    "request": {
                        "thinkingConfig": {"thinkingBudget": 1024},
                        "systemInstruction": {"parts": [{"text": "opencode helper\nx-anthropic-billing-header: foo\n"}]},
                        "contents": [{
                            "role": "model",
                            "parts": [
                                {"thought": true, "text": "hmm"},
                                {"functionCall": {"name": "read", "args": {}}}
                            ]
                        }],
                        "tools": [{
                            "functionDeclarations": [{"name": "my/tool!", "parameters": {"type": "object", "additionalProperties": false}}]
                        }]
                    }
                }),
                stream: true,
                access_token: Some("ag-tok".into()),
                base_url: Some(base),
                provider_specific_data: Some(json!({"projectId": "proj-1"})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let echo: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(echo["uri"], "/v1internal:streamGenerateContent?alt=sse");
        assert!(
            echo["headers"]["user-agent"]
                .as_str()
                .unwrap()
                .starts_with("antigravity/hub/")
        );
        let body = &echo["body"];
        assert_eq!(body["project"], "proj-1");
        assert_eq!(body["userAgent"], "antigravity");
        assert!(body.get("requestType").is_none());
        assert!(body.get("thinking").is_none());
        assert!(body["request"].get("thinkingConfig").is_none());
        let sys = body["request"]["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(sys.contains("antigravity helper"));
        assert!(!sys.contains("x-anthropic-billing-header"));
        let parts = body["request"]["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert!(parts[0]["thoughtSignature"].is_string());
        assert_eq!(
            body["request"]["tools"][0]["functionDeclarations"][0]["name"],
            "my_tool_"
        );
        assert_eq!(
            body["request"]["toolConfig"]["functionCallingConfig"]["mode"],
            "VALIDATED"
        );
        task.abort();
    }
}
