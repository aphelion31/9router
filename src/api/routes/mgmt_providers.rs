use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

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
fn lite_connection(c: &Value) -> Value {
    let psd = &c["providerSpecificData"];
    let mut trimmed_psd = Map::new();
    for k in [
        "nodeName",
        "prefix",
        "proxyPoolId",
        "enabledModels",
        "customModels",
        "defaultModel",
        "uid",
        "mimoUserId",
    ] {
        if let Some(v) = psd.get(k).filter(|v| !v.is_null()) {
            trimmed_psd.insert(k.into(), v.clone());
        }
    }
    let mut out = json!({
        "id": c["id"],
        "provider": c["provider"],
        "authType": c["authType"],
        "name": c["name"],
        "email": c["email"],
        "isActive": c["isActive"],
        "priority": c["priority"],
        "testStatus": c["testStatus"],
        "createdAt": c["createdAt"],
        "updatedAt": c["updatedAt"],
        "providerSpecificData": trimmed_psd,
    });
    if !c["defaultModel"].is_null() {
        out["defaultModel"] = c["defaultModel"].clone();
    }
    out
}

async fn list_connections(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
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
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    if q.get("summary").is_some_and(|v| v == "1") {
        let result = (|| -> rusqlite::Result<Map<String, Value>> {
            let mut st = db.prepare(
                "SELECT provider, COUNT(*), SUM(CASE WHEN isActive=1 THEN 1 ELSE 0 END) FROM providerConnections GROUP BY provider",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            })?;
            let mut stats = Map::new();
            for row in rows {
                let (provider, total, active) = row?;
                stats.insert(
                    provider,
                    json!({ "total": total, "active": active, "inactive": total - active }),
                );
            }
            Ok(stats)
        })();
        return match result {
            Ok(stats) => {
                let mut breakers = crate::services::provider_breaker::global().lock().map(|mut b| b.snapshot(crate::routing::classify::now_ms())).unwrap_or_default();
                if let Some(map) = breakers.as_object_mut() { for b in map.values_mut() { b["preTrip"] = json!(b["state"] == "closed" && b["failuresInWindow"].as_u64().unwrap_or(0) > 0); } }
                Json(json!({ "stats": stats, "breakers": breakers })).into_response()
            },
            Err(_) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch provider summary",
            ),
        };
    }

    let lite = q.get("lite").is_some_and(|v| v == "1");
    let provider_filter = q.get("provider").filter(|s| !s.is_empty());

    if q.contains_key("page") {
        let page = q
            .get("page")
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|&p| p > 0)
            .unwrap_or(1);
        let page_size = q
            .get("pageSize")
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|&p| p > 0)
            .unwrap_or(100)
            .min(500);
        let search = q.get("search").map(String::as_str).unwrap_or("").trim();
        let search_like = (!search.is_empty()).then(|| format!("%{search}%"));
        let offset = (page - 1) * page_size;
        let result = (|| -> rusqlite::Result<(Vec<Value>, i64)> {
            let total: i64 = db.query_row(
                "SELECT COUNT(*) FROM providerConnections WHERE (?1 IS NULL OR provider=?1) AND (?2 IS NULL OR name LIKE ?2 OR email LIKE ?2 OR provider LIKE ?2)",
                params![provider_filter, search_like],
                |r| r.get(0),
            )?;
            let mut st = db.prepare(
                "SELECT id FROM providerConnections WHERE (?1 IS NULL OR provider=?1) AND (?2 IS NULL OR name LIKE ?2 OR email LIKE ?2 OR provider LIKE ?2) ORDER BY priority,createdAt LIMIT ?3 OFFSET ?4",
            )?;
            let ids = st
                .query_map(params![provider_filter, search_like, page_size, offset], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let conns = ids
                .iter()
                .map(|id| {
                    connection(&db, id)
                        .and_then(|v| v.ok_or(rusqlite::Error::QueryReturnedNoRows))
                        .map(redact)
                        .map(|v| if lite { lite_connection(&v) } else { v })
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((conns, total))
        })();
        return match result {
            Ok((connections, total)) => Json(json!({
                "connections": connections,
                "total": total,
                "page": page,
                "pageSize": page_size,
            }))
            .into_response(),
            Err(_) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch providers",
            ),
        };
    }

    let result = (|| -> rusqlite::Result<Vec<Value>> {
        let mut st = db.prepare(
            "SELECT id FROM providerConnections WHERE (?1 IS NULL OR provider=?1) ORDER BY priority,createdAt",
        )?;
        let ids = st
            .query_map([provider_filter], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| {
                connection(&db, id)
                    .and_then(|v| v.ok_or(rusqlite::Error::QueryReturnedNoRows))
                    .map(redact)
                    .map(|v| if lite { lite_connection(&v) } else { v })
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

async fn bulk_toggle_connections(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(provider) = body["provider"].as_str().filter(|s| !s.is_empty()) else {
        return err(
            StatusCode::BAD_REQUEST,
            "provider and authTypes are required",
        );
    };
    let Some(auth_types) = body["authTypes"].as_array().filter(|a| !a.is_empty()) else {
        return err(
            StatusCode::BAD_REQUEST,
            "provider and authTypes are required",
        );
    };
    let is_active = if body["isActive"].as_bool().unwrap_or(false) {
        1
    } else {
        0
    };
    let types: Vec<&str> = auth_types.iter().filter_map(Value::as_str).collect();
    if types.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "provider and authTypes are required",
        );
    }
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let mut updated = 0usize;
    for at in types {
        match db.execute(
            "UPDATE providerConnections SET isActive=?1,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE provider=?2 AND authType=?3",
            params![is_active, provider, at],
        ) {
            Ok(n) => updated += n,
            Err(_) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to update connections",
                );
            }
        }
    }
    Json(json!({ "updated": updated })).into_response()
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
    let name = if let Some(n) = body["name"]
        .as_str()
        .or_else(|| body["displayName"].as_str())
        .filter(|s| !s.trim().is_empty())
    {
        n.to_owned()
    } else if body["autoAllocateName"] == true {
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM providerConnections WHERE provider=?1",
                params![provider],
                |r| r.get(0),
            )
            .unwrap_or(0);
        format!("Account {}", count + 1)
    } else {
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
    let mut specific = body["providerSpecificData"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(pid) = body["proxyPoolId"]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty() && *v != "__none__")
    {
        let exists = db
            .query_row("SELECT 1 FROM proxyPools WHERE id=?1", params![pid], |_| Ok(()))
            .is_ok();
        if !exists {
            return err(StatusCode::BAD_REQUEST, "Proxy pool not found");
        }
        specific.insert("proxyPoolId".into(), json!(pid));
    }
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
    let mut data = json!({"apiKey":key,"defaultModel":body["defaultModel"],"globalPriority":body["globalPriority"],"testStatus":body["testStatus"].as_str().unwrap_or("unknown"),"providerSpecificData":specific});
    crate::crypto::encrypt_fields(&mut data);
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
    let mut spec_updates = serde_json::Map::new();
    if let Some(pid_val) = body.get("proxyPoolId") {
        let pid = pid_val.as_str().map(str::trim).unwrap_or("");
        if pid.is_empty() || pid == "__none__" {
            spec_updates.insert("proxyPoolId".into(), Value::Null);
        } else {
            let exists = db
                .query_row("SELECT 1 FROM proxyPools WHERE id=?1", params![pid], |_| Ok(()))
                .is_ok();
            if !exists {
                return err(StatusCode::BAD_REQUEST, "Proxy pool not found");
            }
            spec_updates.insert("proxyPoolId".into(), json!(pid));
        }
    }
    if let Some(en) = body.get("connectionProxyEnabled") {
        spec_updates.insert("connectionProxyEnabled".into(), json!(en == &json!(true)));
    }
    if let Some(u) = body.get("connectionProxyUrl") {
        spec_updates.insert(
            "connectionProxyUrl".into(),
            json!(u.as_str().unwrap_or("").trim()),
        );
    }
    if let Some(np) = body.get("connectionNoProxy") {
        spec_updates.insert(
            "connectionNoProxy".into(),
            json!(np.as_str().unwrap_or("").trim()),
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
    if !spec_updates.is_empty() {
        if !v["providerSpecificData"].is_object() {
            v["providerSpecificData"] = json!({})
        }
        if let Some(obj) = v["providerSpecificData"].as_object_mut() {
            for (k, val) in spec_updates {
                if val.is_null() {
                    obj.remove(&k);
                } else {
                    obj.insert(k, val);
                }
            }
        }
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
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/providers",
            get(list_connections)
                .post(create_connection)
                .patch(bulk_toggle_connections),
        )
        .route(
            "/api/providers/{id}",
            get(get_connection)
                .put(update_connection)
                .delete(delete_connection),
        )
        .route("/api/providers/{id}/models", get(provider_models))
        .route(
            "/api/providers/{id}/overrides",
            get(get_overrides).put(put_overrides),
        )
        .route("/api/providers/{id}/test", post(test_connection))
        .route("/api/providers/{id}/test-models", post(test_provider_models))
        .route("/api/providers/bulk-proxy", post(bulk_proxy))
        .route("/api/providers/client", get(client_connections))
        .route("/api/providers/kilo/free-models", get(kilo_free_models))
        .route("/api/providers/suggested-models", get(suggested_models))
        .route("/api/providers/test-batch", post(test_batch))
        .route("/api/providers/test-batch/stop", post(test_batch_stop))
        .route("/api/providers/validate", post(validate_provider))
        .route(
            "/api/providers/web-login",
            get(web_login_get)
                .post(web_login_post)
                .delete(web_login_delete),
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
    if addrs.is_empty() || addrs.iter().any(|a| !crate::ssrf::public_address(a.ip())) {
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

fn canonical_provider_id(id: &str) -> String {
    crate::providers::lookup(id)
        .map(|p| p.id.clone())
        .unwrap_or_else(|| id.to_string())
}

async fn provider_models(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let conn = {
        let Ok(db) = s.db.lock() else {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
        };
        match connection(&db, &id) {
            Ok(Some(c)) => c,
            Ok(None) => return err(StatusCode::NOT_FOUND, "Connection not found"),
            Err(_) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch connection",
                );
            }
        }
    };
    let provider = conn["provider"].as_str().unwrap_or("");
    let psd = &conn["providerSpecificData"];
    if provider.starts_with("openai-compatible-") || provider.starts_with("anthropic-compatible-")
    {
        if let Some(base) = psd["baseUrl"].as_str().filter(|s| !s.is_empty()) {
            let url = format!("{}/models", base.trim_end_matches('/'));
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            let mut req = client.get(&url);
            if let Some(key) = conn["apiKey"].as_str().filter(|s| !s.is_empty()) {
                req = if provider.starts_with("anthropic-compatible-") {
                    req.header("x-api-key", key)
                        .header("anthropic-version", "2023-06-01")
                } else {
                    req.bearer_auth(key)
                };
            }
            if let Ok(resp) = req.send().await
                && resp.status().is_success()
                && let Ok(text) = resp.text().await
                && let Ok(parsed) = serde_json::from_str::<Value>(&text)
            {
                let list = parsed["data"]
                    .as_array()
                    .or_else(|| parsed["models"].as_array())
                    .or_else(|| parsed.as_array())
                    .cloned()
                    .unwrap_or_default();
                return Json(json!({ "provider": provider, "models": list })).into_response();
            }
        }
        return Json(json!({ "provider": provider, "models": [] })).into_response();
    }

    let live = live_models(provider, &conn, true).await;
    if let Some(models) = live.filter(|m| !m.is_empty()) {
        return Json(json!({ "provider": provider, "models": models })).into_response();
    }
    let models: Vec<Value> = crate::providers::lookup(provider)
        .map(|p| {
            p.models
                .iter()
                .map(|m| {
                    json!({
                        "id": m.id,
                        "name": m.name.as_deref().unwrap_or(&m.id),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Json(json!({ "provider": provider, "models": models })).into_response()
}

pub(crate) async fn live_models(provider: &str, conn: &Value, force: bool) -> Option<Vec<Value>> {
    use crate::services::{clinepass_models, copilot_models, cursor_models, grok_cli_models, kimchi_models, kiro_models, qoder_models};
    let client = reqwest::Client::new();
    let psd = &conn["providerSpecificData"];
    let access = conn["accessToken"].as_str().filter(|s| !s.is_empty()).unwrap_or("");
    let api_key = conn["apiKey"].as_str().filter(|s| !s.is_empty()).unwrap_or("");
    let token = if access.is_empty() { api_key } else { access };
    match provider {
        "nous" => {
            let response = client.get(crate::services::nous::NOUS_MODELS_URL).send().await.ok()?;
            if !response.status().is_success() { return None; }
            let payload = serde_json::from_str::<Value>(&response.text().await.ok()?).ok()?;
            Some(crate::services::nous::normalize_nous_models(&payload))
        }
        "qoder" | "qoder-cn" => {
            let catalog = qoder_models::resolve_qoder_models(conn, force, Some(qoder_models::qoder_region_of(Some(provider)))).await?;
            Some(if force { catalog.models } else { qoder_models::routable_qoder_models_from_catalog(&catalog) })
        }
        "cursor" => cursor_models::resolve_cursor_models(&client, conn, force).await?["models"].as_array().cloned(),
        "cline" | "clinepass" => clinepass_models::resolve_cline_models(&client, token, access.is_empty(), provider == "clinepass").await,
        "kiro" => kiro_models::resolve_kiro_models(&client, psd["profileArn"].as_str().unwrap_or(""), access, conn["id"].as_str().unwrap_or(access), None).await,
        "grok-cli" => grok_cli_models::resolve_grok_cli_models(&client, access, None, conn["email"].as_str(), psd["userId"].as_str()).await,
        "github" => copilot_models::resolve_copilot_models(&client, psd["copilotToken"].as_str().unwrap_or(token), None).await,
        "kimchi" => kimchi_models::resolve_kimchi_models(&client, psd["endpoint"].as_str(), token).await,
        _ => None,
    }
}

#[cfg(test)]
mod live_catalog_tests {
    use super::*;

    #[tokio::test]
    async fn missing_credentials_fall_back_for_each_catalog() {
        for provider in ["qoder", "qoder-cn", "cursor", "cline", "clinepass", "kiro", "grok-cli", "github", "kimchi"] {
            let conn = json!({"provider": provider});
            assert!(live_models(provider, &conn, false).await.is_none(), "{provider}");
            assert!(live_models(provider, &conn, true).await.is_none(), "{provider}");
        }
    }
}

fn read_provider_overrides(db: &rusqlite::Connection) -> Map<String, Value> {
    db.query_row(
        "SELECT value FROM kv WHERE scope='settings' AND key='providerOverrides'",
        [],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|raw| serde_json::from_str::<Map<String, Value>>(&raw).ok())
    .unwrap_or_default()
}

fn normalize_override(body: &Value) -> Result<Option<Value>, &'static str> {
    let Some(raw_headers) = body.get("headers") else {
        return Ok(None);
    };
    if raw_headers.is_null() {
        return Ok(None);
    }
    let Some(obj) = raw_headers.as_object() else {
        return Err("headers must be an object");
    };
    let blocked = [
        "host",
        "content-length",
        "content-type",
        "connection",
        "transfer-encoding",
        "authorization",
        "cookie",
    ];
    let mut clean = Map::new();
    for (name, val) in obj {
        let Some(s) = val.as_str() else {
            if val.is_null() {
                continue;
            }
            return Err("Invalid header value");
        };
        if s.is_empty() {
            continue;
        }
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || name.is_empty()
        {
            return Err("Invalid header name");
        }
        if s.contains('\r') || s.contains('\n') || s.len() > 8192 {
            return Err("Invalid header value");
        }
        if blocked.contains(&name.to_ascii_lowercase().as_str()) {
            return Err("Header cannot be overridden");
        }
        clean.insert(name.clone(), json!(s));
    }
    if clean.len() > 20 {
        return Err("Too many headers (max 20)");
    }
    Ok((!clean.is_empty()).then(|| json!({ "headers": clean })))
}

async fn get_overrides(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let canonical = canonical_provider_id(&id);
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let all = read_provider_overrides(&db);
    let headers = all
        .get(&canonical)
        .and_then(|v| v.get("headers"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    Json(json!({ "headers": headers, "builtinHeaders": {} })).into_response()
}

async fn put_overrides(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let canonical = canonical_provider_id(&id);
    let normalized = match normalize_override(&body) {
        Ok(v) => v,
        Err(msg) => return err(StatusCode::BAD_REQUEST, msg),
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let mut all = read_provider_overrides(&db);
    if let Some(ref ov) = normalized {
        all.insert(canonical.clone(), ov.clone());
    } else {
        all.remove(&canonical);
    }
    match crate::db::kv_set(
        &db,
        "settings",
        "providerOverrides",
        &Value::Object(all).to_string(),
    ) {
        Ok(()) => Json(json!({
            "headers": normalized.and_then(|v| v.get("headers").cloned()).unwrap_or_else(|| json!({})),
            "builtinHeaders": {}
        }))
        .into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save overrides"),
    }
}

async fn run_connection_test(conn: &Value, model_override: Option<&str>) -> (bool, Option<String>, &'static str) {
    let provider = conn["provider"].as_str().unwrap_or("");
    let model = model_override
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| conn["defaultModel"].as_str().filter(|s| !s.is_empty()).map(str::to_string))
        .or_else(|| {
            crate::providers::lookup(provider)
                .and_then(|p| p.models.first().map(|m| m.id.clone()))
        })
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    let api_key = conn["apiKey"].as_str().map(str::to_owned);
    let access_token = conn["accessToken"].as_str().map(str::to_owned);
    let psd = conn["providerSpecificData"].clone();
    let base_url = psd["baseUrl"].as_str().map(str::to_owned);
    if api_key.as_deref().unwrap_or("").is_empty()
        && access_token.as_deref().unwrap_or("").is_empty()
        && provider != "ollama-local"
    {
        return (false, Some("Missing credentials".to_string()), "auth");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();
    if provider == "nous" && model_override.is_none() {
        let key = api_key.as_deref().filter(|s| !s.is_empty()).or(access_token.as_deref()).unwrap_or("");
        let (url, probe) = crate::services::nous::nous_api_key_probe(key);
        return match client.post(url).bearer_auth(key).header("Content-Type", "application/json").body(probe["body"].to_string()).send().await {
            Ok(response) if crate::services::nous::is_nous_api_key_accepted(response.status().as_u16()) => (true, None, "ok"),
            Ok(response) => (false, Some(crate::services::nous::nous_api_key_validation_error(response.status().as_u16())), "auth"),
            Err(e) => (false, Some(e.to_string()), "network"),
        };
    }
    if let Some(identity) = crate::services::web_identity::probe(
        &client, provider, api_key.as_deref().or(access_token.as_deref()).unwrap_or("")
    ).await {
        return (identity.ok, identity.error, match identity.status {
            "valid" => "ok", "invalid" => "auth", _ => "unknown",
        });
    }
    let req = crate::executors::ExecutorRequest {
        provider: provider.to_string(),
        model,
        body: json!({
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
            "stream": false
        }),
        stream: false,
        api_key,
        access_token,
        base_url,
        provider_specific_data: Some(psd),
    };
    match crate::executors::execute(&client, req).await {
        Ok(resp) if resp.status().is_success() => (true, None, "ok"),
        Ok(resp) => {
            let status = resp.status().as_u16();
            let stage = match status {
                401 | 403 => "auth",
                400 | 404 | 422 => "model",
                429 => "rate_limit",
                _ => "upstream",
            };
            let text = resp.text().await.unwrap_or_default();
            let msg: String = text.chars().take(200).collect();
            (false, Some(if msg.is_empty() { format!("HTTP {status}") } else { msg }), stage)
        }
        Err(e) => (false, Some(e.to_string()), "network"),
    }
}

async fn test_connection(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let conn = {
        let Ok(db) = s.db.lock() else {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
        };
        match connection(&db, &id) {
            Ok(Some(c)) => c,
            Ok(None) => return err(StatusCode::NOT_FOUND, "Connection not found"),
            Err(_) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch connection",
                );
            }
        }
    };
    let (valid, error_msg, stage) = run_connection_test(&conn, None).await;
    if stage != "unknown"
        && let Ok(db) = s.db.lock()
        && let Ok(raw) = db.query_row(
            "SELECT data FROM providerConnections WHERE id=?1",
            [&id],
            |r| r.get::<_, String>(0),
        )
        && let Ok(mut data) = serde_json::from_str::<Value>(&raw)
    {
        data["testStatus"] = json!(if valid { "active" } else { "error" });
        data["lastError"] = error_msg.as_ref().map(|e| json!(e)).unwrap_or(Value::Null);
        let _ = db.execute(
            "UPDATE providerConnections SET data=?1,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2",
            params![data.to_string(), id],
        );
    }
    Json(json!({
        "valid": valid,
        "error": error_msg,
        "stage": stage,
        "refreshed": false,
    }))
    .into_response()
}

async fn test_provider_models(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let conn = {
        let Ok(db) = s.db.lock() else {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
        };
        match connection(&db, &id) {
            Ok(Some(c)) => c,
            Ok(None) => return err(StatusCode::NOT_FOUND, "Connection not found"),
            Err(_) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch connection",
                );
            }
        }
    };
    let provider = conn["provider"].as_str().unwrap_or("");
    let models: Vec<(String, String)> = crate::providers::lookup(provider)
        .map(|p| {
            p.models
                .iter()
                .map(|m| (m.id.clone(), m.name.clone().unwrap_or_else(|| m.id.clone())))
                .collect()
        })
        .unwrap_or_default();
    if models.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "No models configured for this provider",
        );
    }
    let mut results = Vec::new();
    for (model_id, model_name) in models.into_iter().take(10) {
        let (ok, error_msg, stage) = run_connection_test(&conn, Some(&model_id)).await;
        results.push(json!({
            "modelId": model_id,
            "name": model_name,
            "ok": ok,
            "stage": stage,
            "error": error_msg,
        }));
    }
    Json(json!({
        "provider": provider,
        "connectionId": id,
        "results": results,
    }))
    .into_response()
}

async fn bulk_proxy(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(provider) = body["provider"].as_str().map(str::trim).filter(|s| !s.is_empty())
    else {
        return err(StatusCode::BAD_REQUEST, "provider is required");
    };
    let mode = body["mode"].as_str().unwrap_or("");
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let pool_ids: Option<Vec<String>> = match mode {
        "none" => None,
        "single" => {
            let Some(pid) = body["proxyPoolId"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return err(StatusCode::BAD_REQUEST, "proxyPoolId is required");
            };
            let exists: bool = db
                .query_row("SELECT 1 FROM proxyPools WHERE id=?1", [pid], |_| Ok(true))
                .optional()
                .ok()
                .flatten()
                .unwrap_or(false);
            if !exists {
                return err(StatusCode::NOT_FOUND, "Proxy pool not found");
            }
            Some(vec![pid.to_string()])
        }
        "rotate" => {
            let Ok(mut st) = db.prepare("SELECT id FROM proxyPools WHERE isActive=1") else {
                return err(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
            };
            let ids = st
                .query_map([], |r| r.get::<_, String>(0))
                .ok()
                .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
                .unwrap_or_default();
            if ids.is_empty() {
                return err(StatusCode::BAD_REQUEST, "No active proxy pools available");
            }
            Some(ids)
        }
        _ => return err(StatusCode::BAD_REQUEST, "mode must be single, rotate or none"),
    };

    let result = (|| -> rusqlite::Result<usize> {
        let mut st = db.prepare("SELECT id,data FROM providerConnections WHERE provider=?1")?;
        let rows = st
            .query_map([provider], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(st);
        let mut updated = 0usize;
        for (cid, raw) in rows {
            let mut data: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            if data.get("providerSpecificData").and_then(Value::as_object).is_none() {
                data["providerSpecificData"] = json!({});
            }
            if let Some(ref ids) = pool_ids {
                data["providerSpecificData"]["proxyPoolId"] = json!(ids);
            } else if let Some(obj) = data["providerSpecificData"].as_object_mut() {
                obj.remove("proxyPoolId");
            }
            updated += db.execute(
                "UPDATE providerConnections SET data=?1,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2",
                params![data.to_string(), cid],
            )?;
        }
        Ok(updated)
    })();
    match result {
        Ok(updated) => Json(json!({ "updated": updated })).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to bulk assign proxy pool",
        ),
    }
}

fn sanitize_client_connection(c: &Value) -> Value {
    let mut safe = Map::new();
    for f in [
        "id",
        "provider",
        "authType",
        "name",
        "email",
        "displayName",
        "priority",
        "globalPriority",
        "isActive",
        "defaultModel",
        "testStatus",
        "lastError",
        "lastErrorAt",
        "errorCode",
        "expiresAt",
        "lastUsedAt",
        "consecutiveUseCount",
        "createdAt",
        "updatedAt",
    ] {
        if let Some(v) = c.get(f) {
            safe.insert(f.into(), v.clone());
        }
    }
    if let Some(psd) = c.get("providerSpecificData").and_then(Value::as_object) {
        let mut safe_psd = Map::new();
        for f in [
            "baseUrl",
            "azureEndpoint",
            "deployment",
            "apiVersion",
            "accountId",
            "region",
            "projectId",
            "resourceUrl",
            "proxyPoolId",
            "connectionProxyEnabled",
            "connectionProxyUrl",
            "connectionNoProxy",
            "githubLogin",
            "githubName",
            "githubEmail",
            "githubUserId",
            "username",
            "firstName",
            "lastName",
            "authMethod",
            "authKind",
            "profileArn",
        ] {
            if let Some(v) = psd.get(f) {
                safe_psd.insert(f.into(), v.clone());
            }
        }
        safe.insert("providerSpecificData".into(), Value::Object(safe_psd));
    }
    Value::Object(safe)
}

async fn client_connections(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let provider = q.get("provider").map(String::as_str).unwrap_or("all");
    let account_status = q.get("accountStatus").map(String::as_str).unwrap_or("all");
    let page = q
        .get("page")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&p| p > 0)
        .unwrap_or(1);
    let page_size = q
        .get("pageSize")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&p| p > 0)
        .unwrap_or(20)
        .min(500);
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let Ok(mut st) = db.prepare("SELECT id FROM providerConnections ORDER BY priority,provider")
    else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
    };
    let ids = st
        .query_map([], |r| r.get::<_, String>(0))
        .ok()
        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    let all_conns: Vec<Value> = ids
        .iter()
        .filter_map(|id| connection(&db, id).ok().flatten())
        .collect();
    let mut provider_options: Vec<String> = all_conns
        .iter()
        .filter_map(|c| c["provider"].as_str().map(str::to_string))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    provider_options.sort();
    let filtered: Vec<Value> = all_conns
        .into_iter()
        .filter(|c| provider == "all" || c["provider"].as_str() == Some(provider))
        .filter(|c| match account_status {
            "active" => c["isActive"].as_bool().unwrap_or(true),
            "inactive" => !c["isActive"].as_bool().unwrap_or(true),
            _ => true,
        })
        .map(|c| sanitize_client_connection(&c))
        .collect();
    let total = filtered.len();
    let total_pages = (total / page_size + usize::from(total % page_size != 0)).max(1);
    let current_page = page.min(total_pages);
    let offset = (current_page - 1) * page_size;
    let page_connections: Vec<Value> = filtered.into_iter().skip(offset).take(page_size).collect();
    Json(json!({
        "connections": page_connections,
        "providerOptions": provider_options,
        "pagination": {
            "page": current_page,
            "pageSize": page_size,
            "total": total,
            "totalPages": total_pages,
        }
    }))
    .into_response()
}

fn kilo_cache() -> &'static Mutex<(Option<Vec<Value>>, Option<std::time::Instant>)> {
    static CACHE: OnceLock<Mutex<(Option<Vec<Value>>, Option<std::time::Instant>)>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new((None, None)))
}

async fn kilo_free_models() -> Response {
    {
        let guard = kilo_cache().lock().unwrap();
        if let (Some(models), Some(at)) = (&guard.0, guard.1)
            && at.elapsed() < std::time::Duration::from_secs(3600)
        {
            return Json(json!({ "models": models, "cached": true })).into_response();
        }
    }
    let url = std::env::var("KILO_MODELS_URL")
        .unwrap_or_else(|_| "https://api.kilo.ai/api/gateway/models".to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    match client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(parsed) = serde_json::from_str::<Value>(&text)
                && let Some(arr) = parsed["data"].as_array()
            {
                let free: Vec<Value> = arr
                    .iter()
                    .filter(|m| m["isFree"].as_bool() == Some(true))
                    .map(|m| {
                        json!({
                            "id": m["id"],
                            "name": m["name"],
                            "isFree": true,
                            "context_length": m["context_length"].as_u64().unwrap_or(0),
                        })
                    })
                    .collect();
                let mut guard = kilo_cache().lock().unwrap();
                *guard = (Some(free.clone()), Some(std::time::Instant::now()));
                return Json(json!({ "models": free, "cached": false })).into_response();
            }
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "models": [], "error": "Invalid Kilo API response" })),
            )
                .into_response()
        }
        Ok(resp) => {
            let msg = format!("Failed to fetch Kilo models: Kilo API returned {}", resp.status().as_u16());
            let guard = kilo_cache().lock().unwrap();
            if let Some(ref cached) = guard.0 {
                return Json(json!({ "models": cached, "cached": true, "warning": msg }))
                    .into_response();
            }
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "models": [], "error": msg })),
            )
                .into_response()
        }
        Err(e) => {
            let msg = format!("Failed to fetch Kilo models: {e}");
            let guard = kilo_cache().lock().unwrap();
            if let Some(ref cached) = guard.0 {
                return Json(json!({ "models": cached, "cached": true, "warning": msg }))
                    .into_response();
            }
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "models": [], "error": msg })),
            )
                .into_response()
        }
    }
}

async fn suggested_models(Query(q): Query<HashMap<String, String>>) -> Response {
    let (Some(url), Some(filter_type)) = (
        q.get("url").filter(|s| !s.is_empty()),
        q.get("type").filter(|s| !s.is_empty()),
    ) else {
        return err(StatusCode::BAD_REQUEST, "Missing url or type");
    };
    let supported = [
        "openrouter-free",
        "opencode-free",
        "opencode-go",
        "mimo-free",
        "minimax-code",
        "airforce-free",
        "nous-free",
        "pricing",
    ];
    if !supported.contains(&filter_type.as_str()) {
        return err(StatusCode::BAD_REQUEST, "Unknown filter type");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let Ok(resp) = client.get(url).send().await else {
        return Json(json!({ "data": [] })).into_response();
    };
    if !resp.status().is_success() {
        return Json(json!({ "data": [] })).into_response();
    }
    let Ok(text) = resp.text().await else {
        return Json(json!({ "data": [] })).into_response();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return Json(json!({ "data": [] })).into_response();
    };
    let raw_arr: Vec<Value> = parsed
        .get("data")
        .or_else(|| parsed.get("models"))
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| parsed.as_array().cloned())
        .unwrap_or_else(|| {
            if parsed.is_object() {
                vec![parsed.clone()]
            } else {
                Vec::new()
            }
        });
    let data: Vec<Value> = match filter_type.as_str() {
        "nous-free" => crate::services::nous::normalize_nous_free_models(&parsed),
        "openrouter-free" => raw_arr
            .into_iter()
            .filter(|m| {
                m["pricing"]["prompt"].as_str() == Some("0")
                    && m["pricing"]["completion"].as_str() == Some("0")
                    && m["context_length"].as_u64().unwrap_or(0) >= 200_000
            })
            .map(|m| json!({ "id": m["id"], "name": m["name"], "contextLength": m["context_length"] }))
            .collect(),
        "opencode-free" => raw_arr
            .into_iter()
            .filter(|m| {
                let id = m["id"].as_str().unwrap_or("");
                (id.ends_with("-free") || id == "big-pickle") && id != "deepseek-v4-flash-free"
            })
            .map(|m| json!({ "id": m["id"], "name": m["id"] }))
            .collect(),
        "opencode-go" => raw_arr
            .into_iter()
            .filter(|m| m["id"].is_string())
            .map(|m| json!({ "id": m["id"], "name": m["id"] }))
            .collect(),
        "mimo-free" => raw_arr
            .into_iter()
            .filter(|m| {
                let id = m["id"].as_str().unwrap_or("");
                let name = m["name"].as_str().unwrap_or("").to_ascii_lowercase();
                id.starts_with("mimo") || name.contains("mimo")
            })
            .map(|m| json!({ "id": m["id"], "name": m["name"].as_str().or(m["id"].as_str()).unwrap_or("") }))
            .collect(),
        "pricing" => raw_arr
            .into_iter()
            .filter(|m| m["model_ratio"].as_f64() == Some(0.0) || m["model_ratio"].as_i64() == Some(0))
            .filter_map(|m| {
                let id = m["model_name"].as_str().or_else(|| m["id"].as_str())?;
                Some(json!({ "id": id, "name": id }))
            })
            .collect(),
        _ => raw_arr
            .into_iter()
            .filter_map(|m| {
                let id = m["id"].as_str()?;
                Some(json!({ "id": id, "name": m["name"].as_str().unwrap_or(id) }))
            })
            .collect(),
    };
    Json(json!({ "data": data })).into_response()
}

fn batch_jobs() -> &'static Mutex<HashMap<String, bool>> {
    static JOBS: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn test_batch_stop(Json(body): Json<Value>) -> Response {
    let Some(job_id) = body["jobId"].as_str().filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "jobId is required");
    };
    let mut jobs = batch_jobs().lock().unwrap();
    let found = if let Some(stopped) = jobs.get_mut(job_id) {
        *stopped = true;
        true
    } else {
        false
    };
    Json(json!({ "ok": true, "found": found })).into_response()
}

async fn test_batch(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(mode) = body["mode"].as_str().filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "mode is required");
    };
    if !matches!(
        mode,
        "provider" | "oauth" | "free" | "apikey" | "compatible" | "all"
    ) {
        return err(
            StatusCode::BAD_REQUEST,
            "Invalid mode. Use: provider, oauth, free, apikey, compatible, all",
        );
    }
    let provider_id = body["providerId"].as_str();
    let job_id = body["jobId"].as_str().map(str::to_string);
    let connections = {
        let Ok(db) = s.db.lock() else {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
        };
        let Ok(mut st) =
            db.prepare("SELECT id FROM providerConnections WHERE isActive=1 ORDER BY priority")
        else {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        };
        let ids = st
            .query_map([], |r| r.get::<_, String>(0))
            .ok()
            .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
            .unwrap_or_default();
        ids.into_iter()
            .filter_map(|id| connection(&db, &id).ok().flatten())
            .filter(|c| {
                let p = c["provider"].as_str().unwrap_or("");
                let auth = c["authType"].as_str().unwrap_or("apikey");
                match mode {
                    "provider" => provider_id.is_some_and(|pid| pid == p),
                    "oauth" => auth == "oauth",
                    "apikey" => auth == "apikey",
                    "compatible" => {
                        p.starts_with("openai-compatible-")
                            || p.starts_with("anthropic-compatible-")
                    }
                    "free" => matches!(p, "kilo" | "opencode" | "openrouter"),
                    "all" => true,
                    _ => false,
                }
            })
            .collect::<Vec<_>>()
    };

    if let Some(ref jid) = job_id {
        batch_jobs().lock().unwrap().insert(jid.clone(), false);
    }
    let mut results = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut stopped = false;
    for conn in &connections {
        if let Some(ref jid) = job_id
            && batch_jobs().lock().unwrap().get(jid).copied().unwrap_or(false)
        {
            stopped = true;
            break;
        }
        let (valid, error_msg, stage) = run_connection_test(conn, None).await;
        if valid {
            passed += 1;
        } else {
            failed += 1;
        }
        results.push(json!({
            "connectionId": conn["id"],
            "provider": conn["provider"],
            "valid": valid,
            "stage": stage,
            "error": error_msg,
        }));
    }
    if let Some(ref jid) = job_id {
        batch_jobs().lock().unwrap().remove(jid);
    }
    Json(json!({
        "mode": mode,
        "providerId": provider_id,
        "results": results,
        "summary": { "total": results.len(), "passed": passed, "failed": failed },
        "stopped": stopped,
    }))
    .into_response()
}

async fn validate_provider(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(provider_raw) = body["provider"].as_str().filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "Provider is required");
    };
    let provider = canonical_provider_id(provider_raw);
    let api_key = body["apiKey"].as_str().unwrap_or("");
    let psd = &body["providerSpecificData"];
    let is_compatible = provider.starts_with("openai-compatible-")
        || provider.starts_with("anthropic-compatible-");
    if !is_compatible && crate::providers::lookup(&provider).is_none() {
        return err(StatusCode::BAD_REQUEST, "Invalid provider");
    }
    if api_key.is_empty() && provider != "ollama-local" {
        return err(StatusCode::BAD_REQUEST, "API Key is required");
    }
    let base_url = if is_compatible {
        let Ok(db) = s.db.lock() else {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
        };
        node(&db, &provider)
            .ok()
            .flatten()
            .and_then(|n| n["baseUrl"].as_str().map(str::to_owned))
    } else {
        psd["baseUrl"].as_str().map(str::to_owned)
    };
    let model = crate::providers::lookup(&provider)
        .and_then(|p| p.models.first().map(|m| m.id.clone()))
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .unwrap_or_default();
    if provider == "nous" {
        let (url, probe) = crate::services::nous::nous_api_key_probe(api_key);
        return match client.post(url).bearer_auth(api_key).header("Content-Type", "application/json").body(probe["body"].to_string()).send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let valid = crate::services::nous::is_nous_api_key_accepted(status);
                if valid { Json(json!({"valid": true, "status": status})).into_response() }
                else { Json(json!({"valid": false, "status": status, "error": crate::services::nous::nous_api_key_validation_error(status)})).into_response() }
            }
            Err(e) => Json(json!({"valid": false, "error": e.to_string()})).into_response(),
        };
    }
    let req = crate::executors::ExecutorRequest {
        provider,
        model,
        body: json!({
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
            "stream": false
        }),
        stream: false,
        api_key: Some(api_key.to_string()),
        access_token: None,
        base_url,
        provider_specific_data: Some(psd.clone()),
    };
    match crate::executors::execute(&client, req).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let valid = status != 401 && status != 403;
            Json(json!({ "valid": valid, "status": status })).into_response()
        }
        Err(e) => Json(json!({ "valid": false, "error": e.to_string() })).into_response(),
    }
}

async fn web_login_get(Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(_provider) = q.get("provider").filter(|s| !s.is_empty()) else {
        return Json(json!({ "providers": [], "profiles": [] })).into_response();
    };
    err(StatusCode::NOT_FOUND, "No login session for this provider")
}

async fn web_login_post() -> Response {
    err(
        StatusCode::NOT_IMPLEMENTED,
        "Interactive browser web-login is not supported in this runtime",
    )
}

async fn web_login_delete(Query(q): Query<HashMap<String, String>>) -> Response {
    if q.get("provider").is_none_or(String::is_empty) {
        return err(StatusCode::BAD_REQUEST, "Provider is required");
    }
    Json(json!({ "ok": true, "cancelled": false })).into_response()
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

    #[tokio::test]
    async fn provider_nodes_crud_lifecycle() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        let app = routes().with_state(state);

        let res = app
            .clone()
            .oneshot(
                Request::post("/api/provider-nodes")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"My Node","prefix":"my","type":"openai-compatible","apiType":"chat","baseUrl":"https://api.example.com/v1"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let created: Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 10000).await.unwrap(),
        )
        .unwrap();
        let id = created["node"]["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("openai-compatible-chat-"));

        let res = app
            .clone()
            .oneshot(Request::get("/api/provider-nodes").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let listed: Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 10000).await.unwrap(),
        )
        .unwrap();
        assert_eq!(listed["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(listed["nodes"][0]["id"], id);

        let res = app
            .clone()
            .oneshot(
                Request::put(format!("/api/provider-nodes/{id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"My Node 2","prefix":"my2","apiType":"responses","baseUrl":"https://api2.example.com/v1"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let updated: Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 10000).await.unwrap(),
        )
        .unwrap();
        assert_eq!(updated["node"]["name"], "My Node 2");
        assert_eq!(updated["node"]["prefix"], "my2");

        let res = app
            .oneshot(
                Request::delete(format!("/api/provider-nodes/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let deleted: Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 10000).await.unwrap(),
        )
        .unwrap();
        assert_eq!(deleted["success"], true);
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

#[cfg(test)]
mod providers_parity_tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    fn test_app() -> (axum::Router, AppState) {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        (routes().with_state(state.clone()), state)
    }

    async fn json_body(res: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn providers_listing_bulk_toggle_client_and_proxy() {
        let (app, state) = test_app();

        // Create two connections
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"provider":"openai","apiKey":"sk-secret-1","name":"Primary OpenAI","email":"a@example.com"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let c1 = json_body(res).await;
        let id1 = c1["connection"]["id"].as_str().unwrap().to_string();

        // summary=1
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/providers?summary=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let sum = json_body(res).await;
        assert_eq!(sum["stats"]["openai"]["total"], 1);
        assert_eq!(sum["stats"]["openai"]["active"], 1);

        // page=1&lite=1
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/providers?page=1&pageSize=10&lite=1&search=Primary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let paged = json_body(res).await;
        assert_eq!(paged["total"], 1);
        assert_eq!(paged["connections"][0]["id"], id1);

        // PATCH /api/providers bulk toggle
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/providers")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"provider":"openai","authTypes":["apikey"],"isActive":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json_body(res).await["updated"], 1);

        // GET /api/providers/client (redacted, paginated)
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/providers/client?provider=openai&accountStatus=inactive&page=1&pageSize=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let client_res = json_body(res).await;
        assert_eq!(client_res["pagination"]["total"], 1);
        assert!(client_res["connections"][0].get("apiKey").is_none());

        // bulk-proxy: create proxyPool then assign single & none
        {
            let db = state.db.lock().unwrap();
            db.execute(
                "INSERT INTO proxyPools (id,isActive,testStatus,data,createdAt,updatedAt) VALUES ('pool-1',1,NULL,'{\"name\":\"Test Pool\"}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/bulk-proxy")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"provider":"openai","mode":"single","proxyPoolId":"pool-1"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json_body(res).await["updated"], 1);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/bulk-proxy")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"provider":"openai","mode":"none"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json_body(res).await["updated"], 1);
    }

    #[tokio::test]
    async fn providers_models_overrides_validation_and_web_login() {
        let (app, _state) = test_app();

        // Create connection
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"provider":"openai","apiKey":"sk-test","name":"Test OpenAI"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let id = json_body(res).await["connection"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // GET /api/providers/{id}/models
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&format!("/api/providers/{id}/models"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let models_res = json_body(res).await;
        assert_eq!(models_res["provider"], "openai");
        assert!(!models_res["models"].as_array().unwrap().is_empty());

        // PUT & GET /api/providers/{id}/overrides
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/providers/openai/overrides")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"headers":{"X-Custom":"val"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json_body(res).await["headers"]["X-Custom"], "val");

        // Blocked header rejected
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/providers/openai/overrides")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"headers":{"Authorization":"Bearer x"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/providers/openai/overrides")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json_body(res).await["headers"]["X-Custom"], "val");

        // suggested-models validation
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/providers/suggested-models?url=https://example.com&type=unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        // validate endpoint validation
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"provider":"openai","apiKey":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        // test-batch & test-batch/stop
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/test-batch/stop")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"jobId":"job-1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json_body(res).await["ok"], true);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/test-batch")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"mode":"oauth"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json_body(res).await["summary"]["total"], 0);

        // web-login
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/providers/web-login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/web-login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"provider":"claude"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/providers/web-login?provider=claude")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // 404 on missing connection test
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/nonexistent/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/nonexistent/test-models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
