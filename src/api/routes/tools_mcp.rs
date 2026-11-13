use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/mcp/gateway", get(gateway_get).post(gateway_post))
        .route("/api/mcp/{plugin}/sse", get(plugin_not_available))
        .route("/api/mcp/{plugin}/message", post(plugin_not_available))
}

// ponytail: stdioSseBridge spawns local stdio MCP child processes; not available in Rust
async fn plugin_not_available(Path(_plugin): Path<String>) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "not available" })),
    )
        .into_response()
}

fn gateway_tools() -> Value {
    json!([
        {
            "name": "list_providers",
            "description": "List connected providers (redacted: no credentials).",
            "inputSchema": { "type": "object", "properties": { "activeOnly": { "type": "boolean" } } }
        },
        {
            "name": "list_combos",
            "description": "List combos with their member models.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "health_summary",
            "description": "Per-provider connection counts and error counts.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "auto_routing_intents",
            "description": "The auto: intents the router expands at serving time.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

async fn gateway_get() -> Response {
    Json(json!({
        "name": "9router-pro-gateway",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "mcp-json-rpc-over-http",
        "tools": ["list_providers", "list_combos", "health_summary", "auto_routing_intents"],
        "mutatingTools": [],
        "note": "Read-only. Mutating tools are pending an explicit dashboard approval flow.",
    }))
    .into_response()
}

fn is_authorized(db: &rusqlite::Connection, headers: &HeaderMap) -> bool {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(bearer) = auth.strip_prefix("Bearer ") {
        let ok: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM apiKeys WHERE key = ?1 AND (isActive IS NULL OR isActive != 0))",
                [bearer],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if ok {
            return true;
        }
    }
    false
}

fn run_tool(db: &rusqlite::Connection, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "list_providers" => {
            let active_only = args["activeOnly"].as_bool().unwrap_or(false);
            let mut stmt = db
                .prepare("SELECT id, provider, authType, name, isActive, providerSpecificData FROM providerConnections")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let provider: String = row.get(1)?;
                    let auth_type: Option<String> = row.get(2)?;
                    let name: Option<String> = row.get(3)?;
                    let is_active: Option<i64> = row.get(4)?;
                    let extra_raw: Option<String> = row.get(5)?;
                    Ok((id, provider, auth_type, name, is_active, extra_raw))
                })
                .map_err(|e| e.to_string())?;
            let mut list = Vec::new();
            for r in rows.flatten() {
                let active = r.4.unwrap_or(1) != 0;
                if active_only && !active {
                    continue;
                }
                let test_status = r
                    .5
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .and_then(|v| v["testStatus"].as_str().map(str::to_owned));
                list.push(json!({
                    "id": r.0,
                    "provider": r.1,
                    "authType": r.2,
                    "name": r.3,
                    "isActive": active,
                    "testStatus": test_status,
                }));
            }
            Ok(json!({ "count": list.len(), "providers": list }))
        }
        "list_combos" => {
            let mut stmt = db
                .prepare("SELECT id, name, kind, models FROM combos")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    let kind: Option<String> = row.get(2)?;
                    let models_raw: Option<String> = row.get(3)?;
                    Ok((id, name, kind, models_raw))
                })
                .map_err(|e| e.to_string())?;
            let mut combos = Vec::new();
            for r in rows.flatten() {
                let models: Value = r
                    .3
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| json!([]));
                combos.push(json!({
                    "id": r.0,
                    "name": r.1,
                    "kind": r.2,
                    "models": models,
                }));
            }
            Ok(json!({ "count": combos.len(), "combos": combos }))
        }
        "health_summary" => {
            let mut stmt = db
                .prepare("SELECT provider, authType, isActive, lastError FROM providerConnections")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            let mut map: HashMap<(String, Option<String>), (i64, i64, i64)> = HashMap::new();
            for (provider, auth_type, is_active, last_err) in rows.flatten() {
                let entry = map.entry((provider, auth_type)).or_insert((0, 0, 0));
                entry.0 += 1;
                if is_active.unwrap_or(1) != 0 {
                    entry.1 += 1;
                }
                if last_err.is_some_and(|e| !e.is_empty()) {
                    entry.2 += 1;
                }
            }
            let providers: Vec<Value> = map
                .into_iter()
                .map(|((provider, auth_type), (total, connected, error))| {
                    json!({
                        "provider": provider,
                        "authType": auth_type,
                        "total": total,
                        "connected": connected,
                        "error": error,
                    })
                })
                .collect();
            Ok(json!({ "providers": providers }))
        }
        "auto_routing_intents" => Ok(json!({
            "intents": ["auto:auto", "auto:fast", "auto:cheap", "auto:code", "auto:reasoning", "auto:vision"],
            "note": "Serving-time selection only; the combo advisor (draft) is a separate dashboard feature.",
        })),
        _ => Err(format!("Unknown tool: {name}")),
    }
}

async fn gateway_post(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let db = s.db.lock().unwrap();
    if !is_authorized(&db, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": -32001,
                    "message": "Unauthorized: pass a valid Pro API key as Bearer or a dashboard session cookie"
                }
            })),
        )
            .into_response();
    }

    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32600, "message": "Invalid JSON-RPC request" }
        }))
        .into_response();
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = req.get("method").and_then(|v| v.as_str()) else {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32600, "message": "Invalid JSON-RPC request" }
        }))
        .into_response();
    };
    if req.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32600, "message": "Invalid JSON-RPC request" }
        }))
        .into_response();
    }

    match method {
        "initialize" => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "9router-pro-gateway", "version": env!("CARGO_PKG_VERSION") },
            }
        }))
        .into_response(),
        "ping" => Json(json!({ "jsonrpc": "2.0", "id": id, "result": {} })).into_response(),
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            Json(json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": gateway_tools() } }))
                .into_response()
        }
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let args = &req["params"]["arguments"];
            match run_tool(&db, name, args) {
                Ok(result) => Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": result.to_string() }],
                        "structuredContent": result,
                    }
                }))
                .into_response(),
                Err(msg) => Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": msg }
                }))
                .into_response(),
            }
        }
        other => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {other}") }
        }))
        .into_response(),
    }
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
        db.execute(
            "INSERT INTO apiKeys (id, name, key, isActive, createdAt) VALUES ('k1', 'test', 'sk-test-123', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        router().with_state(state)
    }

    #[tokio::test]
    async fn gateway_auth_and_tools_work() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(Request::get("/api/mcp/gateway").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/mcp/gateway")
                    .body(Body::from(
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 401);

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/mcp/gateway")
                    .header("authorization", "Bearer sk-test-123")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "method": "tools/call",
                            "params": { "name": "auto_routing_intents", "arguments": {} }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert!(body["result"]["structuredContent"]["intents"].is_array());
    }

    #[tokio::test]
    async fn plugin_sse_and_message_return_501() {
        let app = test_app();
        let r = app
            .clone()
            .oneshot(Request::get("/api/mcp/demo/sse").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 501);

        let r = app
            .oneshot(
                Request::post("/api/mcp/demo/message")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 501);
    }
}
