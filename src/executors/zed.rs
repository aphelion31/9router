use anyhow::Context;
use serde_json::{Value, json};

pub(crate) fn zed_provider_for_model(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.contains("claude") {
        "anthropic"
    } else if m.contains("gemini") {
        "google"
    } else if m.contains("grok") {
        "x_ai"
    } else {
        "open_ai"
    }
}

pub(super) async fn execute(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
    body: &Value,
    token: Option<&str>,
    credentials: Option<&Value>,
) -> anyhow::Result<reqwest::Response> {
    let provider = zed_provider_for_model(model);
    let access_token = token.context("Zed access token required")?;
    let creds = credentials.context("Zed credential metadata required")?;
    let user = creds.get("userId").and_then(Value::as_str).context("Zed user id required")?;
    let base = endpoint.trim_end_matches("/completions").trim_end_matches('/');
    let auth = format!("{user} {access_token}");
    let system = creds.get("systemId").and_then(Value::as_str).unwrap_or("");
    let mut organization = creds.get("organizationId").or_else(|| creds.get("defaultOrganizationId")).cloned().unwrap_or(Value::Null);
    if organization.is_null() {
        let user_info: Value = serde_json::from_slice(&client.get(format!("{base}/client/users/me")).header("authorization", &auth).header("x-zed-system-id", system).send().await?.error_for_status()?.bytes().await?)?;
        organization = user_info.get("default_organization_id").or_else(|| user_info.get("defaultOrganizationId")).cloned().unwrap_or_else(|| user_info["organizations"].as_array().and_then(|orgs| orgs.iter().find(|o| o["is_personal"] == true).or_else(|| orgs.first())).map(|o| o["id"].clone()).unwrap_or(Value::Null));
    }
    if organization.is_object() { organization = organization["id"].clone(); }
    if organization.is_array() { organization = organization[0].clone(); }
    anyhow::ensure!(organization.as_str().is_some_and(|s| !s.is_empty()), "Zed organization required");
    let exchanged: Value = serde_json::from_slice(&client.post(format!("{base}/client/llm_tokens")).header("authorization", auth).header("x-zed-system-id", system).header("content-type", "application/json").body(json!({"organization_id": organization}).to_string()).send().await?.error_for_status()?.bytes().await?)?;
    let token = exchanged["token"].as_str().or_else(|| exchanged["token"][0].as_str()).or_else(|| exchanged["token"]["value"].as_str()).context("Zed LLM token missing")?;
    let format = match provider { "open_ai" => "openai-responses", "anthropic" => "claude", "google" => "gemini", _ => "openai" };
    let mut provider_request = crate::translator::translate_request("openai", format, model, body, true, None, None);
    if provider == "google" { provider_request.as_object_mut().map(|o| o.remove("safetySettings")); }
    let payload = json!({"thread_id":body.get("thread_id"), "prompt_id":body.get("prompt_id"), "provider":provider, "model":model, "provider_request":provider_request});
    let response = client
        .post(endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/x-ndjson, text/event-stream, */*")
        .header("user-agent", "9router/zed")
        .header("x-zed-version", "0.200.0")
        .header("x-zed-client-supports-status-messages", "true")
        .header("x-zed-client-supports-stream-ended-request-completion-status", "true")
        .bearer_auth(token)
        .body(payload.to_string())
        .send()
        .await?;
    if !response.status().is_success() {
        return Ok(response);
    }
    let bytes = response.bytes().await?;
    let text = std::str::from_utf8(&bytes)?;
    let mut output = String::new();
    let mut state = crate::translator::init_state(format);
    state.model = Some(model.to_owned());
    for line in text.lines() {
        let line = line.trim().strip_prefix("data:").unwrap_or(line).trim();
        if line == "[DONE]" {
            break;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("status").is_some() {
                continue;
            }
            let event = v.get("event").unwrap_or(&v);
            for chunk in crate::translator::translate_response(format, "openai", Some(event), &mut state) {
                output.push_str(&format!("data: {chunk}\n\n"));
            }
        }
    }
    output.push_str("data: [DONE]\n\n");
    Ok(reqwest::Response::from(
        axum::http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(reqwest::Body::from(output))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, http::HeaderMap, routing::post};
    #[tokio::test]
    async fn zed_mock_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/client/llm_tokens", post(|headers: HeaderMap, body: Bytes| async move {
            assert_eq!(headers["authorization"], "1 test");
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["organization_id"], "org");
            "{\"token\":\"llm-test\"}"
        })).fallback(post(|headers: HeaderMap, body: Bytes| async move {
            assert_eq!(headers["authorization"], "Bearer llm-test");
            let v: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["provider"], "x_ai");
            "{\"event\":{\"choices\":[]}}\n[DONE]\n"
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = execute(
            &reqwest::Client::new(),
            &url,
            "grok-test",
            &json!({"messages":[]}),
            Some("test"), Some(&json!({"userId":"1","organizationId":"org"})),
        )
        .await
        .unwrap();
        assert_eq!(
            response.text().await.unwrap(),
            "data: {\"choices\":[]}\n\ndata: [DONE]\n\n"
        );
        task.abort();
    }

    #[tokio::test]
    async fn zed_token_exchange_and_provider_routing_work() {
        assert_eq!(zed_provider_for_model("claude-sonnet-4"), "anthropic");
        assert_eq!(zed_provider_for_model("gemini-2.5-pro"), "google");
        assert_eq!(zed_provider_for_model("grok-3"), "x_ai");
        assert_eq!(zed_provider_for_model("gpt-4.1"), "open_ai");
    }
}
