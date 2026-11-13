use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

use crate::server::AppState;

const ALL_STATUS_TOOLS: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "droid",
    "openclaw",
    "hermes",
    "cowork",
    "cline",
    "kilo",
    "deepseek-tui",
    "jcode",
    "grok-build",
    "devin",
    "pi",
    "omp",
    "crush",
    "forge",
    "smelt",
    "codewhale",
];

pub fn router() -> Router<AppState> {
    let mut r = Router::new()
        .route("/api/cli-tools/all-statuses", get(all_statuses))
        .route(
            "/api/cli-tools/antigravity-mitm",
            get(mitm_status)
                .post(mitm_not_available)
                .patch(mitm_not_available)
                .delete(mitm_not_available),
        )
        .route(
            "/api/cli-tools/antigravity-mitm/alias",
            get(mitm_alias_get).put(mitm_alias_put),
        )
        .route(
            "/api/cli-tools/codex-profiles",
            get(codex_profiles_get)
                .post(codex_profiles_post)
                .delete(codex_profiles_delete),
        )
        .route("/api/cli-tools/hermes-profiles", get(hermes_profiles_get))
        .route(
            "/api/cli-tools/cowork-mcp-registry",
            get(cowork_mcp_registry),
        )
        .route("/api/cli-tools/cowork-mcp-tools", post(cowork_mcp_tools))
        .route("/api/cli-tools/devin-settings", get(devin_get))
        .route(
            "/api/cli-tools/opencode-settings",
            get(|s| tool_get(s, "opencode"))
                .post(|s, b| tool_post(s, "opencode", b))
                .patch(|s, b| tool_patch(s, "opencode", b))
                .delete(|s| tool_delete(s, "opencode")),
        );

    for &(route_suffix, tool_id) in &[
        ("claude-settings", "claude"),
        ("cline-settings", "cline"),
        ("codewhale-settings", "codewhale"),
        ("codex-settings", "codex"),
        ("copilot-settings", "copilot"),
        ("cowork-settings", "cowork"),
        ("crush-settings", "crush"),
        ("deepseek-tui-settings", "deepseek-tui"),
        ("droid-settings", "droid"),
        ("forge-settings", "forge"),
        ("grok-build-settings", "grok-build"),
        ("hermes-settings", "hermes"),
        ("jcode-settings", "jcode"),
        ("kilo-settings", "kilo"),
        ("omp-settings", "omp"),
        ("openclaw-settings", "openclaw"),
        ("pi-settings", "pi"),
        ("smelt-settings", "smelt"),
    ] {
        let path = format!("/api/cli-tools/{route_suffix}");
        r = r.route(
            &path,
            get(move |s| tool_get(s, tool_id))
                .post(move |s, b| tool_post(s, tool_id, b))
                .delete(move |s| tool_delete(s, tool_id)),
        );
    }

    r
}

fn read_tool_status(db: &rusqlite::Connection, tool_id: &str) -> Value {
    let stored = crate::db::kv_get(db, "cli_tool", tool_id)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let has_9router = stored
        .as_ref()
        .is_some_and(|v| v.as_object().is_some_and(|m| !m.is_empty()));
    json!({
        "installed": false,
        "has9Router": has_9router,
        "settings": stored,
    })
}

async fn devin_get(State(s): State<AppState>) -> Response {
    tool_get(State(s), "devin").await
}

async fn tool_get(State(s): State<AppState>, tool_id: &'static str) -> Response {
    let db = s.db.lock().unwrap();
    Json(read_tool_status(&db, tool_id)).into_response()
}

async fn tool_post(
    State(s): State<AppState>,
    tool_id: &'static str,
    Json(body): Json<Value>,
) -> Response {
    let db = s.db.lock().unwrap();
    let _ = crate::db::kv_set(&db, "cli_tool", tool_id, &body.to_string());
    Json(json!({ "success": true, "settings": body })).into_response()
}

async fn tool_patch(
    State(s): State<AppState>,
    tool_id: &'static str,
    Json(body): Json<Value>,
) -> Response {
    let db = s.db.lock().unwrap();
    let mut existing = crate::db::kv_get(&db, "cli_tool", tool_id)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    if let (Some(dst), Some(src)) = (existing.as_object_mut(), body.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let _ = crate::db::kv_set(&db, "cli_tool", tool_id, &existing.to_string());
    Json(json!({ "success": true, "settings": existing })).into_response()
}

async fn tool_delete(State(s): State<AppState>, tool_id: &'static str) -> Response {
    let db = s.db.lock().unwrap();
    let _ = crate::db::kv_set(&db, "cli_tool", tool_id, "{}");
    Json(json!({ "success": true })).into_response()
}

async fn all_statuses(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let mut out = Map::new();
    for tool_id in ALL_STATUS_TOOLS {
        out.insert((*tool_id).to_owned(), read_tool_status(&db, tool_id));
    }
    Json(Value::Object(out)).into_response()
}

async fn mitm_status() -> Response {
    Json(json!({
        "running": false,
        "certInstalled": false,
        "dnsStatus": {},
    }))
    .into_response()
}

// ponytail: antigravity-mitm modifies /etc/hosts, root certs, and spawns sudo MITM proxy; not available in Rust
async fn mitm_not_available() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "not available" })),
    )
        .into_response()
}

async fn mitm_alias_get(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let tool = params.get("tool").map(String::as_str).unwrap_or("default");
    let db = s.db.lock().unwrap();
    let aliases = crate::db::kv_get(&db, "mitm_alias", tool)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    Json(json!({ "aliases": aliases })).into_response()
}

async fn mitm_alias_put(Json(body): Json<Value>) -> Response {
    let tool = body["tool"].as_str().unwrap_or("");
    let Some(mappings) = body.get("mappings").and_then(|v| v.as_object()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "tool and mappings required" })),
        )
            .into_response();
    };
    if tool.is_empty() || mappings.is_empty() && body.get("tool").is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "tool and mappings required" })),
        )
            .into_response();
    }
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": format!("DNS must be enabled for {tool} before editing model mappings")
        })),
    )
        .into_response()
}

async fn codex_profiles_get(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let profiles: Value = crate::db::kv_get(&db, "cli_tool", "codex_profiles")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!([]));
    Json(json!({ "profiles": profiles })).into_response()
}

async fn codex_profiles_post(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().unwrap_or("").trim().to_owned();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Valid profile name required" })),
        )
            .into_response();
    }
    let db = s.db.lock().unwrap();
    let mut profiles: Vec<Value> = crate::db::kv_get(&db, "cli_tool", "codex_profiles")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    profiles.retain(|p| p["name"].as_str() != Some(&name));
    profiles.push(json!({ "name": name, "model": body["model"], "config": body["config"] }));
    let _ = crate::db::kv_set(
        &db,
        "cli_tool",
        "codex_profiles",
        &Value::Array(profiles.clone()).to_string(),
    );
    Json(json!({ "success": true, "profiles": profiles })).into_response()
}

async fn codex_profiles_delete(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(name) = params.get("name").filter(|n| !n.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Profile name required" })),
        )
            .into_response();
    };
    let db = s.db.lock().unwrap();
    let mut profiles: Vec<Value> = crate::db::kv_get(&db, "cli_tool", "codex_profiles")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    profiles.retain(|p| p["name"].as_str() != Some(name.as_str()));
    let _ = crate::db::kv_set(
        &db,
        "cli_tool",
        "codex_profiles",
        &Value::Array(profiles).to_string(),
    );
    Json(json!({ "success": true })).into_response()
}

async fn hermes_profiles_get(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let profiles: Value = crate::db::kv_get(&db, "cli_tool", "hermes_profiles")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!([]));
    Json(json!({ "profiles": profiles })).into_response()
}

async fn cowork_mcp_registry(State(s): State<AppState>) -> Response {
    let res = s
        .executor
        .0
        .get("https://registry.modelcontextprotocol.io/v0/servers?limit=50")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    match res {
        Ok(r) if r.status().is_success() => {
            let data: Value = r
                .bytes()
                .await
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_else(|| json!({ "servers": [] }));
            Json(data).into_response()
        }
        _ => Json(json!({ "servers": [] })).into_response(),
    }
}

async fn cowork_mcp_tools(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(url) = body["url"].as_str().filter(|u| !u.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "url required" })),
        )
            .into_response();
    };
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "URL not allowed" })),
        )
            .into_response();
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "URL not allowed" })),
        )
            .into_response();
    }

    let init_res = match s
        .executor
        .0
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .timeout(std::time::Duration::from_secs(8))
        .body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "9router", "version": "1" }
                }
            })
            .to_string(),
        )
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": e.to_string(), "tools": [] })).into_response(),
    };

    if matches!(init_res.status().as_u16(), 401 | 403) {
        return Json(json!({ "requiresAuth": true, "tools": [] })).into_response();
    }
    if !init_res.status().is_success() {
        return Json(json!({
            "error": format!("init {}", init_res.status().as_u16()),
            "tools": []
        }))
        .into_response();
    }
    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let mut list_req = s
        .executor
        .0
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .timeout(std::time::Duration::from_secs(8));
    if let Some(sid) = session_id {
        list_req = list_req.header("mcp-session-id", sid);
    }
    let list_res = match list_req
        .body(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }).to_string())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": e.to_string(), "tools": [] })).into_response(),
    };
    if matches!(list_res.status().as_u16(), 401 | 403) {
        return Json(json!({ "requiresAuth": true, "tools": [] })).into_response();
    }
    let bytes = list_res.bytes().await.unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let tools: Vec<Value> = parsed["result"]["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            json!({
                "name": t["name"],
                "description": t["description"].as_str().unwrap_or(""),
            })
        })
        .collect();
    Json(json!({ "tools": tools })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::executor::HttpExecutor;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    fn test_app() -> Router {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        router().with_state(state)
    }

    #[tokio::test]
    async fn all_statuses_and_tool_settings_persist() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/cli-tools/claude-settings")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "baseUrl": "http://localhost:20128" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .clone()
            .oneshot(
                Request::get("/api/cli-tools/all-statuses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 16384).await.unwrap()).unwrap();
        assert_eq!(body["claude"]["has9Router"], true);
        assert_eq!(body["codex"]["has9Router"], false);
    }

    #[tokio::test]
    async fn codex_profiles_and_mitm_501() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/cli-tools/codex-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "name": "work", "model": "o3" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .clone()
            .oneshot(
                Request::get("/api/cli-tools/codex-profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["profiles"].as_array().unwrap().len(), 1);

        let r = app
            .oneshot(
                Request::post("/api/cli-tools/antigravity-mitm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 501);
    }
}
