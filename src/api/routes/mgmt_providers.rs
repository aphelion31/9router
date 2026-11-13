use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({"error":msg}))).into_response()
}

fn connection(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    db.query_row("SELECT id,provider,authType,name,email,priority,isActive,data,createdAt,updatedAt FROM providerConnections WHERE id=?1",[id],|r| {
        let raw:String=r.get(7)?;
        let mut v:Value=serde_json::from_str(&raw).map_err(|e|rusqlite::Error::FromSqlConversionFailure(7,rusqlite::types::Type::Text,Box::new(e)))?;
        let Some(o)=v.as_object_mut() else {return Err(rusqlite::Error::InvalidQuery)};
        o.insert("id".into(),json!(r.get::<_,String>(0)?)); o.insert("provider".into(),json!(r.get::<_,String>(1)?)); o.insert("authType".into(),json!(r.get::<_,String>(2)?)); o.insert("name".into(),json!(r.get::<_,Option<String>>(3)?)); o.insert("email".into(),json!(r.get::<_,Option<String>>(4)?)); o.insert("priority".into(),json!(r.get::<_,Option<i64>>(5)?)); o.insert("isActive".into(),json!(r.get::<_,Option<i64>>(6)?.unwrap_or(0)!=0)); o.insert("createdAt".into(),json!(r.get::<_,String>(8)?)); o.insert("updatedAt".into(),json!(r.get::<_,String>(9)?)); Ok(v)
    }).optional()
}
fn redact(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        for key in ["apiKey", "accessToken", "refreshToken", "idToken"] {
            o.remove(key);
        }
        if let Some(p) = o
            .get_mut("providerSpecificData")
            .and_then(Value::as_object_mut)
        {
            p.remove("sessionToken");
        }
    }
    v
}
async fn get_connection(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    match connection(&db, &id) {
        Ok(Some(v)) => Json(json!({"connection":redact(v)})).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "Connection not found"),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch connection",
        ),
    }
}
async fn list_connections(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !q.contains_key("provider")
        && !q.contains_key("page")
        && q.get("lite").is_none_or(|v| v != "1")
        && q.get("summary").is_none_or(|v| v != "1")
    {
        return err(
            StatusCode::BAD_REQUEST,
            "Use ?summary=1, ?lite=1, or ?provider=<id>&page=<n> to list connections.",
        );
    }
    // ponytail: summary and paginated provider statistics require the unported statistics repository.
    if q.contains_key("page") || q.get("summary").is_some_and(|v| v == "1") {
        return unavailable().await;
    }
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let result = (|| -> rusqlite::Result<Vec<Value>> {
        let mut st=db.prepare("SELECT id FROM providerConnections WHERE (?1 IS NULL OR provider=?1) ORDER BY priority,createdAt")?;
        let ids = st
            .query_map([q.get("provider")], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| {
                connection(&db, id)
                    .and_then(|v| v.ok_or(rusqlite::Error::QueryReturnedNoRows))
                    .map(redact)
            })
            .collect()
    })();
    match result {
        Ok(v) => Json(json!({"connections":v})).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch providers",
        ),
    }
}
async fn create_connection(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(provider) = body["provider"].as_str().filter(|v| !v.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "Invalid provider");
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let existing_node = match node(&db, provider) {
        Ok(v) => v,
        Err(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create connection",
            );
        }
    };
    if existing_node.is_none() && !crate::providers::all().iter().any(|p| p.id == provider) {
        return err(StatusCode::BAD_REQUEST, "Invalid provider");
    }
    let key = body["apiKey"].as_str().unwrap_or("");
    if key.is_empty() && provider != "ollama-local" {
        return err(StatusCode::BAD_REQUEST, "API Key is required");
    }
    let Some(name) = body["name"]
        .as_str()
        .or_else(|| body["displayName"].as_str())
        .filter(|s| !s.trim().is_empty())
    else {
        return err(StatusCode::BAD_REQUEST, "Name is required");
    };
    if db
        .query_row(
            "SELECT 1 FROM providerConnections WHERE provider=?1 AND lower(name)=lower(?2)",
            params![provider, name],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    {
        return err(StatusCode::CONFLICT, "Connection name already exists");
    }
    if body["connectionProxyEnabled"] == true
        && body["connectionProxyUrl"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty())
    {
        return err(
            StatusCode::BAD_REQUEST,
            "Connection proxy URL is required when connection proxy is enabled",
        );
    }
    // ponytail: proxy pools and auto-name allocation need the upstream services.
    if body["autoAllocateName"] == true
        || body["proxyPoolId"]
            .as_str()
            .is_some_and(|v| !v.is_empty() && v != "__none__")
    {
        return err(StatusCode::NOT_IMPLEMENTED, "Service not implemented");
    }
    let mut specific = body["providerSpecificData"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(n) = existing_node {
        for k in ["prefix", "apiType", "baseUrl"] {
            if let Some(v) = n.get(k) {
                specific.insert(k.into(), v.clone());
            }
        }
        specific.insert("nodeName".into(), n["name"].clone());
    }
    specific.insert(
        "connectionProxyEnabled".into(),
        json!(body["connectionProxyEnabled"] == true),
    );
    specific.insert(
        "connectionProxyUrl".into(),
        json!(body["connectionProxyUrl"].as_str().unwrap_or("").trim()),
    );
    specific.insert(
        "connectionNoProxy".into(),
        json!(body["connectionNoProxy"].as_str().unwrap_or("").trim()),
    );
    let data = json!({"apiKey":key,"defaultModel":body["defaultModel"],"globalPriority":body["globalPriority"],"testStatus":body["testStatus"].as_str().unwrap_or("unknown"),"providerSpecificData":specific});
    let id = crate::server::uuid_like();
    if db.execute("INSERT INTO providerConnections(id,provider,authType,name,priority,isActive,data,createdAt,updatedAt) VALUES(?1,?2,'apikey',?3,?4,1,?5,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![id,provider,name,body["priority"].as_i64().unwrap_or(1),data.to_string()]).is_err(){return err(StatusCode::INTERNAL_SERVER_ERROR,"Failed to create connection")}
    match connection(&db, &id) {
        Ok(Some(v)) => (StatusCode::CREATED, Json(json!({"connection":redact(v)}))).into_response(),
        _ => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create connection",
        ),
    }
}
async fn update_connection(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let mut v = match connection(&db, &id) {
        Ok(Some(v)) => v,
        Ok(None) => return err(StatusCode::NOT_FOUND, "Connection not found"),
        Err(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to update connection",
            );
        }
    };
    if body["connectionProxyEnabled"] == true
        && body["connectionProxyUrl"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty())
    {
        return err(
            StatusCode::BAD_REQUEST,
            "Connection proxy URL is required when connection proxy is enabled",
        );
    }
    if body.get("proxyPoolId").is_some()
        || [
            "connectionProxyEnabled",
            "connectionProxyUrl",
            "connectionNoProxy",
        ]
        .iter()
        .any(|k| body.get(*k).is_some())
    {
        return err(
            StatusCode::NOT_IMPLEMENTED,
            "Proxy configuration is not implemented",
        );
    }
    for key in [
        "name",
        "priority",
        "isActive",
        "globalPriority",
        "defaultModel",
        "apiKey",
        "testStatus",
        "lastError",
        "lastErrorAt",
    ] {
        if let Some(x) = body.get(key) {
            v[key] = x.clone()
        }
    }
    if let Some(spec) = body.get("providerSpecificData").and_then(Value::as_object) {
        if !v["providerSpecificData"].is_object() {
            v["providerSpecificData"] = json!({})
        }
        v["providerSpecificData"]
            .as_object_mut()
            .unwrap()
            .extend(spec.clone());
    }
    let mut data = v.clone();
    if let Some(o) = data.as_object_mut() {
        for k in [
            "id",
            "provider",
            "authType",
            "name",
            "email",
            "priority",
            "isActive",
            "createdAt",
            "updatedAt",
        ] {
            o.remove(k);
        }
    }
    if db.execute("UPDATE providerConnections SET name=?1,priority=?2,isActive=?3,data=?4,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?5",params![v["name"].as_str(),v["priority"].as_i64(),v["isActive"].as_bool().unwrap_or(true),data.to_string(),id]).is_err(){return err(StatusCode::INTERNAL_SERVER_ERROR,"Failed to update connection")}
    match connection(&db, &id) {
        Ok(Some(v)) => Json(json!({"connection":redact(v)})).into_response(),
        _ => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update connection",
        ),
    }
}
async fn delete_connection(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    match db.execute("DELETE FROM providerConnections WHERE id=?1", [id]) {
        Ok(0) => err(StatusCode::NOT_FOUND, "Connection not found"),
        Ok(_) => Json(json!({"success":true})).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete connection",
        ),
    }
}
// ponytail: provider-specific discovery, validation and connection tests require unported services.
async fn unavailable() -> Response {
    err(StatusCode::NOT_IMPLEMENTED, "Service not implemented")
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/providers",
            get(list_connections)
                .post(create_connection)
                .patch(unavailable),
        )
        .route(
            "/api/providers/{id}",
            get(get_connection)
                .put(update_connection)
                .delete(delete_connection),
        )
        .route("/api/providers/{id}/models", get(unavailable))
        .route(
            "/api/providers/{id}/overrides",
            get(unavailable).put(unavailable),
        )
        .route("/api/providers/{id}/test", post(unavailable))
        .route("/api/providers/{id}/test-models", post(unavailable))
        .route("/api/providers/bulk-proxy", post(unavailable))
        .route("/api/providers/client", get(unavailable))
        .route("/api/providers/kilo/free-models", get(unavailable))
        .route("/api/providers/suggested-models", get(unavailable))
        .route("/api/providers/test-batch", post(unavailable))
        .route("/api/providers/test-batch/stop", post(unavailable))
        .route("/api/providers/validate", post(unavailable))
        .route(
            "/api/providers/web-login",
            get(unavailable).post(unavailable).delete(unavailable),
        )
        .route("/api/provider-nodes", get(list_nodes).post(create_node))
        .route(
            "/api/provider-nodes/{id}",
            axum::routing::put(update_node).delete(delete_node),
        )
        .route("/api/provider-nodes/validate", post(validate_node))
}

async fn validate_node(Json(body): Json<Value>) -> Response {
    let (Some(base), Some(key)) = (
        body["baseUrl"].as_str().filter(|s| !s.is_empty()),
        body["apiKey"].as_str().filter(|s| !s.is_empty()),
    ) else {
        return err(StatusCode::BAD_REQUEST, "Base URL and API key required");
    };
    let Ok(url) = reqwest::Url::parse(base) else {
        return err(StatusCode::BAD_REQUEST, "Invalid URL format");
    };
    let Some(host) = url.host_str() else {
        return err(StatusCode::BAD_REQUEST, "Invalid URL format");
    };
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return err(StatusCode::BAD_REQUEST, "URL not allowed");
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let Ok(addrs) = tokio::net::lookup_host((host, port)).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"valid":false,"error":"DNS lookup failed - invalid domain or network issue"}))).into_response();
    };
    let addrs: Vec<_> = addrs.collect();
    if addrs.is_empty() || addrs.iter().any(|a| !public_address(a.ip())) {
        return err(StatusCode::BAD_REQUEST, "URL not allowed");
    }
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addrs)
        .build()
    else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Network connection failed - check URL and network connectivity",
        );
    };
    let kind = body["type"].as_str().unwrap_or("");
    let model = body["modelId"].as_str().unwrap_or("");
    let mut base = base.trim().trim_end_matches('/').to_string();
    if kind == "anthropic-compatible" {
        base = base.strip_suffix("/messages").unwrap_or(&base).to_string();
    }
    if kind == "custom-embedding" && model.trim().is_empty() {
        return Json(json!({"valid":false,"error":"Model ID required for embedding validation"}))
            .into_response();
    }
    let endpoint = if kind == "custom-embedding" {
        "embeddings"
    } else {
        "models"
    };
    let mut request = if endpoint == "models" {
        client.get(format!("{base}/{endpoint}"))
    } else {
        client
            .post(format!("{base}/{endpoint}"))
            .header("content-type", "application/json")
            .body(json!({"model":model.trim(),"input":"ping"}).to_string())
    };
    request = request.bearer_auth(key);
    if kind == "anthropic-compatible" {
        request = request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01");
    }
    let Ok(res) = request.send().await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"valid":false,"error":"Network connection failed - check URL and network connectivity"}))).into_response();
    };
    let status = res.status().as_u16();
    if res.status().is_success() {
        if endpoint == "embeddings" {
            let data: Value = res
                .text()
                .await
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null);
            return Json(json!({"valid":true,"method":"embeddings","dimensions":data["data"][0]["embedding"].as_array().map(Vec::len)})).into_response();
        }
        return Json(json!({"valid":true})).into_response();
    }
    if status == 401 || status == 403 {
        return Json(json!({"valid":false,"error":"API key unauthorized"})).into_response();
    }
    if endpoint == "embeddings" {
        let text = res.text().await.unwrap_or_default();
        let suffix: String = text.chars().take(200).collect();
        return Json(json!({"valid":false,"method":"embeddings","error":format!("Embeddings request failed ({status}){}", if suffix.is_empty() { String::new() } else { format!(": {suffix}") })})).into_response();
    }
    if !model.is_empty() {
        let mut request = client
            .post(format!("{base}/chat/completions"))
            .bearer_auth(key)
            .header("content-type", "application/json")
            .body(
                json!({"model":model,"messages":[{"role":"user","content":"ping"}],"max_tokens":1})
                    .to_string(),
            );
        if kind == "anthropic-compatible" {
            request = request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01");
        }
        let Ok(res) = request.send().await else {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"valid":false,"error":"Network connection failed - check URL and network connectivity"}))).into_response();
        };
        let status = res.status().as_u16();
        if res.status().is_success() {
            return Json(json!({"valid":true,"method":"chat"})).into_response();
        }
        let message = match status {
            401 | 403 => "API key unauthorized".to_string(),
            400 => "Invalid model or bad request".to_string(),
            404 => "Chat endpoint not found".to_string(),
            500..=599 => "Server error - try again later".to_string(),
            _ => format!("Chat request failed ({status})"),
        };
        return Json(json!({"valid":false,"error":message,"method":"chat"})).into_response();
    }
    let message = match status {
        404 => "/models endpoint not found - try chat validation with model ID".to_string(),
        500..=599 => "Server error - try again later".to_string(),
        _ => format!("Unexpected response ({status})"),
    };
    Json(json!({"valid":false,"error":message})).into_response()
}

fn public_address(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 224
                || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])))
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_address(v4.into());
            }
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

#[cfg(test)]
mod node_validation_tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[tokio::test]
    async fn rejects_missing_and_private_targets() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        let app = routes().with_state(state);
        for body in [
            json!({}),
            json!({"baseUrl":"http://127.0.0.1:1234","apiKey":"test"}),
        ] {
            let request = Request::post("/api/provider-nodes/validate")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
    }
}

fn node(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    db.query_row(
        "SELECT id,type,name,data,createdAt,updatedAt FROM providerNodes WHERE id=?1",
        [id],
        |r| {
            let raw: String = r.get(3)?;
            let mut v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), json!(r.get::<_, String>(0)?));
                o.insert("type".into(), json!(r.get::<_, String>(1)?));
                o.insert("name".into(), json!(r.get::<_, String>(2)?));
                o.insert("createdAt".into(), json!(r.get::<_, String>(4)?));
                o.insert("updatedAt".into(), json!(r.get::<_, String>(5)?));
            }
            Ok(v)
        },
    )
    .optional()
}

fn node_fields(body: &Value, kind: &str, creating: bool) -> Result<Value, &'static str> {
    let name = body["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("Name is required")?;
    let prefix = body["prefix"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("Prefix is required")?;
    if ![
        "openai-compatible",
        "anthropic-compatible",
        "custom-embedding",
    ]
    .contains(&kind)
    {
        return Err("Invalid provider node type");
    }
    if kind == "openai-compatible"
        && !body["apiType"]
            .as_str()
            .is_some_and(|s| ["chat", "responses"].contains(&s))
    {
        return Err("Invalid OpenAI compatible API type");
    }
    let default = if kind == "anthropic-compatible" {
        "https://api.anthropic.com/v1"
    } else {
        "https://api.openai.com/v1"
    };
    let base = body["baseUrl"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if !creating && base.is_none() {
        return Err("Base URL is required");
    }
    let mut base = base.unwrap_or(default).to_string();
    if kind != "openai-compatible" {
        base = base.trim_end_matches('/').to_string();
        let suffix = if kind == "anthropic-compatible" {
            "/messages"
        } else {
            "/embeddings"
        };
        if let Some(v) = base.strip_suffix(suffix) {
            base = v.to_string()
        }
    }
    let mut data = json!({"name":name,"prefix":prefix,"baseUrl":base});
    if kind == "openai-compatible" {
        data["apiType"] = body["apiType"].clone();
    }
    Ok(data)
}

async fn create_node(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let kind = body["type"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("openai-compatible");
    let data = match node_fields(&body, kind, true) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let name = data["name"].as_str().unwrap();
    match db
        .query_row(
            "SELECT 1 FROM providerNodes WHERE lower(trim(name))=lower(?1)",
            [name],
            |_| Ok(()),
        )
        .optional()
    {
        Ok(Some(())) => return err(StatusCode::BAD_REQUEST, "Provider name already exists"),
        Err(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create provider node",
            );
        }
        _ => {}
    }
    let prefix = match kind {
        "openai-compatible" => format!("openai-compatible-{}-", data["apiType"].as_str().unwrap()),
        "anthropic-compatible" => "anthropic-compatible-".into(),
        _ => "custom-embedding-".into(),
    };
    let id = format!("{}{}", prefix, crate::server::uuid_like());
    if db.execute("INSERT INTO providerNodes(id,type,name,data,createdAt,updatedAt) VALUES(?1,?2,?3,?4,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![id,kind,name,data.to_string()]).is_err() { return err(StatusCode::INTERNAL_SERVER_ERROR,"Failed to create provider node") }
    match node(&db, &id) {
        Ok(Some(v)) => (StatusCode::CREATED, Json(json!({"node":v}))).into_response(),
        _ => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create provider node",
        ),
    }
}
async fn update_node(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Ok(mut db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let existing = match node(&db, &id) {
        Ok(Some(v)) => v,
        Ok(None) => return err(StatusCode::NOT_FOUND, "Provider node not found"),
        Err(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to update provider node",
            );
        }
    };
    let kind = existing["type"].as_str().unwrap_or("");
    let data = match node_fields(&body, kind, false) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let Ok(tx) = db.transaction() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update provider node",
        );
    };
    let result = (|| -> rusqlite::Result<()> {
        tx.execute("UPDATE providerNodes SET name=?1,data=?2,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?3",params![data["name"].as_str(),data.to_string(),id])?;
        let mut st = tx.prepare("SELECT id,data FROM providerConnections WHERE provider=?1")?;
        let rows = st
            .query_map([&id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(st);
        for (cid, raw) in rows {
            let mut v: Value = serde_json::from_str(&raw).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            if !v.is_object() {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if !v["providerSpecificData"].is_object() {
                v["providerSpecificData"] = json!({});
            }
            for key in ["prefix", "baseUrl", "apiType"] {
                if let Some(x) = data.get(key) {
                    v["providerSpecificData"][key] = x.clone();
                }
            }
            v["providerSpecificData"]["nodeName"] = data["name"].clone();
            tx.execute("UPDATE providerConnections SET data=?1,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2",params![v.to_string(),cid])?;
        }
        Ok(())
    })();
    if result.is_err() || tx.commit().is_err() {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update provider node",
        );
    }
    match node(&db, &id) {
        Ok(Some(v)) => Json(json!({"node":v})).into_response(),
        _ => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update provider node",
        ),
    }
}
async fn delete_node(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(mut db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let Ok(tx) = db.transaction() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete provider node",
        );
    };
    let result = (|| -> rusqlite::Result<usize> {
        let n = tx.execute("DELETE FROM providerNodes WHERE id=?1", [&id])?;
        if n > 0 {
            tx.execute("DELETE FROM providerConnections WHERE provider=?1", [&id])?;
            tx.execute(
                "DELETE FROM kv WHERE scope='customModels' AND substr(key,1,length(?1)+1)=?1||'|'",
                [&id],
            )?;
        }
        Ok(n)
    })();
    match result {
        Ok(0) => err(StatusCode::NOT_FOUND, "Provider node not found"),
        Ok(_) => {
            if tx.commit().is_ok() {
                Json(json!({"success":true})).into_response()
            } else {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to delete provider node",
                )
            }
        }
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete provider node",
        ),
    }
}
async fn list_nodes(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch provider nodes",
        );
    };
    let result = (|| -> rusqlite::Result<Vec<Value>> {
        let mut st = db.prepare(
            "SELECT id,type,name,data,createdAt,updatedAt FROM providerNodes ORDER BY createdAt",
        )?;
        let rows = st.query_map([], |r| {
            let raw: String = r.get(3)?;
            let mut v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), json!(r.get::<_, String>(0)?));
                o.insert("type".into(), json!(r.get::<_, Option<String>>(1)?));
                o.insert("name".into(), json!(r.get::<_, Option<String>>(2)?));
                o.insert("createdAt".into(), json!(r.get::<_, String>(4)?));
                o.insert("updatedAt".into(), json!(r.get::<_, String>(5)?));
            }
            Ok(v)
        })?;
        rows.collect()
    })();
    match result {
        Ok(nodes) => Json(json!({"nodes":nodes})).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch provider nodes",
        ),
    }
}
