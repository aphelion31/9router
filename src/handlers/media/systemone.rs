// Port of systemoneCore.js — native decision payload pass-through.

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use crate::handlers::Credentials;
use crate::handlers::media::common::{
    RequestJsonExt, http_client, media_error, read_json, resolve_credentials, split_provider_model,
};
use crate::server::AppState;

pub fn build_systemone_url(override_url: Option<&str>, base_url: Option<&str>, path: Option<&str>) -> Option<String> {
    if let Some(ov) = override_url.map(str::trim).filter(|s| !s.is_empty()) {
        let Some(p) = path.map(|s| s.trim_end_matches('/')).filter(|s| !s.is_empty()) else {
            return Some(ov.to_owned());
        };
        let host = ov.trim_end_matches('/');
        if host.ends_with(p) {
            return Some(host.to_owned());
        }
        if let Some(idx) = p.rfind('/') {
            let prefix = &p[..idx];
            if !prefix.is_empty() && host.ends_with(prefix) {
                return Some(format!("{host}{}", &p[prefix.len()..]));
            }
        }
        return Some(format!("{host}{p}"));
    }
    base_url.map(str::to_owned)
}

fn default_systemone_cfg(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider {
        "cloudflare-ai" => Some((
            "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/run/{model}",
            "",
        )),
        "openrouter" => Some(("https://openrouter.ai/api/v1/systemone", "/v1/systemone")),
        "openai" => Some(("https://api.openai.com/v1/systemone", "/v1/systemone")),
        "opencode" | "opencode-zen" => Some(("https://opencode.ai/zen/v1/systemone", "/v1/systemone")),
        _ => None,
    }
}

pub async fn handle_systemone(State(state): State<AppState>, Json(mut body): Json<Value>) -> Response {
    if body["state"].is_null() {
        return media_error(StatusCode::BAD_REQUEST, "Missing required field: state");
    }
    if !body["questions"].is_object() {
        return media_error(StatusCode::BAD_REQUEST, "Missing required field: questions");
    }

    let raw_model = body["model"].as_str().unwrap_or("openai/systemone");
    let (provider, model) = split_provider_model(raw_model, "openai");
    let creds: Option<Credentials> = resolve_credentials(&state, &provider);
    let override_url = creds
        .as_ref()
        .and_then(|c| c.provider_specific_data.get("baseUrl").and_then(Value::as_str));

    let (cfg_base, cfg_path) = default_systemone_cfg(&provider)
        .map(|(b, p)| (Some(b), if p.is_empty() { None } else { Some(p) }))
        .unwrap_or((None, Some("/v1/systemone")));

    let Some(mut target_url) = build_systemone_url(override_url, cfg_base, cfg_path) else {
        return media_error(
            StatusCode::BAD_REQUEST,
            format!("Provider '{provider}' does not support System One."),
        );
    };

    if target_url.contains("{accountId}") {
        let Some(acct) = creds
            .as_ref()
            .and_then(|c| c.provider_specific_data.get("accountId").and_then(Value::as_str))
        else {
            return media_error(
                StatusCode::BAD_REQUEST,
                format!("Provider '{provider}' requires accountId in providerSpecificData."),
            );
        };
        target_url = target_url.replace("{accountId}", acct);
    }
    if target_url.contains("{model}") {
        target_url = target_url.replace("{model}", &model);
    }

    body["model"] = Value::String(model);
    let token = creds
        .as_ref()
        .and_then(|c| c.api_key.as_deref().or(c.access_token.as_deref()));

    let mut req = http_client().post(target_url).json_body(&body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    if provider == "opencode" || provider == "opencode-zen" {
        req = req.header("x-opencode-session", format!("ses_{}", std::process::id()));
    }

    match req.send().await {
        Ok(res) if res.status().is_success() => match read_json(res).await {
            Ok(data) => (
                StatusCode::OK,
                [("Access-Control-Allow-Origin", "*")],
                Json(data),
            )
                .into_response(),
            Err(e) => media_error(StatusCode::BAD_GATEWAY, e),
        },
        Ok(res) => {
            let st = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            media_error(st, res.text().await.unwrap_or_default())
        }
        Err(e) => media_error(StatusCode::BAD_GATEWAY, e.to_string()),
    }
}
