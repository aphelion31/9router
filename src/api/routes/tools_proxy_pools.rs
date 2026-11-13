use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::OptionalExtension;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::server::AppState;

const VALID_TYPES: &[&str] = &["http", "vercel", "cloudflare", "deno", "netlify"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proxy-pools", get(list_pools).post(create_pool))
        .route(
            "/api/proxy-pools/test-all",
            get(test_all_status).post(test_all_run),
        )
        .route("/api/proxy-pools/delete-failed", post(delete_failed))
        .route("/api/proxy-pools/cloudflare-deploy", post(deploy_not_available))
        .route("/api/proxy-pools/deno-deploy", post(deploy_not_available))
        .route("/api/proxy-pools/netlify-deploy", post(deploy_not_available))
        .route("/api/proxy-pools/vercel-deploy", post(deploy_not_available))
        .route(
            "/api/proxy-pools/{id}",
            get(get_pool).put(update_pool).delete(delete_pool),
        )
        .route("/api/proxy-pools/{id}/test", post(test_pool))
}

// ponytail: vendor edge relay deployments (cloudflare/deno/netlify/vercel) omitted; return 501 until needed
async fn deploy_not_available() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "not available" })),
    )
        .into_response()
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("pp_{nanos:x}")
}

fn row_to_pool(
    id: String,
    is_active: i64,
    test_status: Option<String>,
    data_str: String,
    created_at: String,
    updated_at: String,
) -> Value {
    let mut obj = match serde_json::from_str::<Value>(&data_str) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    obj.insert("id".into(), Value::String(id));
    obj.insert("isActive".into(), Value::Bool(is_active != 0));
    if let Some(ts) = test_status {
        obj.insert("testStatus".into(), Value::String(ts));
    }
    obj.insert("createdAt".into(), Value::String(created_at));
    obj.insert("updatedAt".into(), Value::String(updated_at));
    Value::Object(obj)
}

fn load_all_pools(db: &rusqlite::Connection) -> Vec<Value> {
    let Ok(mut stmt) = db.prepare(
        "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools ORDER BY createdAt DESC",
    ) else {
        return vec![];
    };
    stmt.query_map([], |r| {
        Ok(row_to_pool(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })
    .ok()
    .into_iter()
    .flatten()
    .filter_map(Result::ok)
    .collect()
}

fn load_pool_by_id(db: &rusqlite::Connection, id: &str) -> Option<Value> {
    db.query_row(
        "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools WHERE id=?1",
        [id],
        |r| {
            Ok(row_to_pool(
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )
    .optional()
    .ok()
    .flatten()
}

fn usage_map(db: &rusqlite::Connection) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    let Ok(mut stmt) = db.prepare("SELECT data FROM providerConnections") else {
        return map;
    };
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .ok()
        .into_iter()
        .flatten();
    for raw in rows.flatten() {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if let Some(pp_id) = v["providerSpecificData"]["proxyPoolId"].as_str() {
                if !pp_id.is_empty() {
                    *map.entry(pp_id.to_owned()).or_insert(0) += 1;
                }
            }
        }
    }
    map
}

async fn list_pools(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let is_active_filter = match params.get("isActive").map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    let include_usage = params.get("includeUsage").map(String::as_str) == Some("true");
    let ids_only = params.get("idsOnly").map(String::as_str) == Some("true");
    let search = params
        .get("search")
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let has_page = params.contains_key("page") || params.contains_key("pageSize");
    let page = params
        .get("page")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let page_size = params
        .get("pageSize")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| matches!(n, 10 | 20 | 50))
        .unwrap_or(50);

    let db = s.db.lock().unwrap();
    let mut pools: Vec<Value> = load_all_pools(&db)
        .into_iter()
        .filter(|p| {
            if let Some(active) = is_active_filter {
                if p["isActive"].as_bool() != Some(active) {
                    return false;
                }
            }
            if !search.is_empty() {
                let name = p["name"].as_str().unwrap_or("").to_ascii_lowercase();
                if !name.contains(&search) {
                    return false;
                }
            }
            true
        })
        .collect();

    if ids_only {
        let ids: Vec<String> = pools
            .iter()
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect();
        let total = ids.len();
        return Json(json!({ "ids": ids, "total": total })).into_response();
    }

    let umap = if include_usage {
        Some(usage_map(&db))
    } else {
        None
    };

    let enrich = |mut p: Value| -> Value {
        if let Some(ref m) = umap {
            let count = p["id"]
                .as_str()
                .and_then(|id| m.get(id))
                .copied()
                .unwrap_or(0);
            if let Some(obj) = p.as_object_mut() {
                obj.insert("boundConnectionCount".into(), json!(count));
            }
        }
        p
    };

    if !has_page {
        let out: Vec<Value> = pools.into_iter().map(enrich).collect();
        return Json(json!({ "proxyPools": out })).into_response();
    }

    let total = pools.len();
    let active_count = pools
        .iter()
        .filter(|p| p["isActive"].as_bool() == Some(true))
        .count();
    let failed_count = pools
        .iter()
        .filter(|p| p["testStatus"].as_str() == Some("error"))
        .count();
    let start = (page - 1) * page_size;
    let page_pools: Vec<Value> = pools
        .drain(..)
        .skip(start)
        .take(page_size)
        .map(enrich)
        .collect();

    Json(json!({
        "proxyPools": page_pools,
        "total": total,
        "activeCount": active_count,
        "failedCount": failed_count,
        "page": page,
        "pageSize": page_size,
    }))
    .into_response()
}

async fn create_pool(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().unwrap_or("").trim().to_owned();
    let proxy_url = body["proxyUrl"].as_str().unwrap_or("").trim().to_owned();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    }
    if proxy_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Proxy URL is required" })),
        )
            .into_response();
    }
    let no_proxy = body["noProxy"].as_str().unwrap_or("").trim().to_owned();
    let is_active = body.get("isActive").and_then(|v| v.as_bool()).unwrap_or(true);
    let strict_proxy = body["strictProxy"].as_bool().unwrap_or(false);
    let pool_type = body["type"]
        .as_str()
        .filter(|t| VALID_TYPES.contains(t))
        .unwrap_or("http")
        .to_owned();

    let id = new_id();
    let now = now_iso();
    let data = json!({
        "name": name,
        "proxyUrl": proxy_url,
        "noProxy": no_proxy,
        "strictProxy": strict_proxy,
        "type": pool_type,
    });
    let db = s.db.lock().unwrap();
    if let Err(e) = db.execute(
        "INSERT INTO proxyPools (id, isActive, testStatus, data, createdAt, updatedAt) VALUES (?1, ?2, NULL, ?3, ?4, ?4)",
        rusqlite::params![id, if is_active { 1 } else { 0 }, data.to_string(), now],
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let pool = load_pool_by_id(&db, &id).unwrap_or(data);
    (StatusCode::CREATED, Json(json!({ "proxyPool": pool }))).into_response()
}

async fn get_pool(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let db = s.db.lock().unwrap();
    match load_pool_by_id(&db, &id) {
        Some(pool) => Json(json!({ "proxyPool": pool })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response(),
    }
}

async fn update_pool(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let db = s.db.lock().unwrap();
    let Some(mut existing) = load_pool_by_id(&db, &id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response();
    };

    let obj = existing.as_object_mut().unwrap();
    if let Some(v) = body.get("name") {
        let name = v.as_str().unwrap_or("").trim();
        if name.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Name is required" })),
            )
                .into_response();
        }
        obj.insert("name".into(), Value::String(name.to_owned()));
    }
    if let Some(v) = body.get("proxyUrl") {
        let pu = v.as_str().unwrap_or("").trim();
        if pu.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Proxy URL is required" })),
            )
                .into_response();
        }
        obj.insert("proxyUrl".into(), Value::String(pu.to_owned()));
    }
    if let Some(v) = body.get("noProxy") {
        obj.insert(
            "noProxy".into(),
            Value::String(v.as_str().unwrap_or("").trim().to_owned()),
        );
    }
    if let Some(v) = body.get("isActive") {
        obj.insert("isActive".into(), Value::Bool(v.as_bool() == Some(true)));
    }
    if let Some(v) = body.get("strictProxy") {
        obj.insert("strictProxy".into(), Value::Bool(v.as_bool() == Some(true)));
    }
    if let Some(v) = body.get("type") {
        let t = v
            .as_str()
            .filter(|t| VALID_TYPES.contains(t))
            .unwrap_or("http");
        obj.insert("type".into(), Value::String(t.to_owned()));
    }

    let is_active = obj.get("isActive").and_then(|v| v.as_bool()).unwrap_or(true);
    let now = now_iso();
    let mut data_obj = obj.clone();
    for k in ["id", "isActive", "testStatus", "createdAt", "updatedAt"] {
        data_obj.remove(k);
    }

    let _ = db.execute(
        "UPDATE proxyPools SET isActive=?1, data=?2, updatedAt=?3 WHERE id=?4",
        rusqlite::params![
            if is_active { 1 } else { 0 },
            Value::Object(data_obj).to_string(),
            now,
            id
        ],
    );
    let updated = load_pool_by_id(&db, &id).unwrap_or(existing);
    Json(json!({ "proxyPool": updated })).into_response()
}

async fn delete_pool(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let db = s.db.lock().unwrap();
    if load_pool_by_id(&db, &id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response();
    }
    let bound = usage_map(&db).get(&id).copied().unwrap_or(0);
    if bound > 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Proxy pool is currently in use",
                "boundConnectionCount": bound,
            })),
        )
            .into_response();
    }
    let _ = db.execute("DELETE FROM proxyPools WHERE id=?1", [&id]);
    Json(json!({ "success": true })).into_response()
}

async fn probe_url(client: &reqwest::Client, url: &str) -> (bool, u16, Option<String>, u128) {
    let t0 = Instant::now();
    match client
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(r) => {
            let code = r.status().as_u16();
            let ok = r.status().is_success();
            let err = if ok {
                None
            } else {
                Some(format!("Proxy test failed with status {code}"))
            };
            (ok, code, err, t0.elapsed().as_millis())
        }
        Err(e) => (false, 0, Some(e.to_string()), t0.elapsed().as_millis()),
    }
}

async fn test_pool(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let pool = {
        let db = s.db.lock().unwrap();
        load_pool_by_id(&db, &id)
    };
    let Some(mut pool) = pool else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response();
    };
    let proxy_url = pool["proxyUrl"].as_str().unwrap_or("").to_owned();
    let (ok, status_code, error_msg, elapsed_ms) = probe_url(&s.executor.0, &proxy_url).await;
    let now = now_iso();

    {
        let db = s.db.lock().unwrap();
        if let Some(obj) = pool.as_object_mut() {
            obj.insert("lastTestedAt".into(), Value::String(now.clone()));
            obj.insert(
                "lastError".into(),
                error_msg.clone().map(Value::String).unwrap_or(Value::Null),
            );
            for k in ["id", "isActive", "testStatus", "createdAt", "updatedAt"] {
                obj.remove(k);
            }
        }
        let test_status = if ok { "active" } else { "error" };
        let _ = db.execute(
            "UPDATE proxyPools SET testStatus=?1, data=?2, updatedAt=?3 WHERE id=?4",
            rusqlite::params![test_status, pool.to_string(), now, id],
        );
    }

    Json(json!({
        "ok": ok,
        "status": status_code,
        "statusText": Value::Null,
        "error": error_msg,
        "elapsedMs": elapsed_ms,
        "testedAt": now,
    }))
    .into_response()
}

async fn test_all_status(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let state = crate::db::kv_get(&db, "proxy_pools", "test_all_state")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| {
            json!({
                "running": false,
                "total": 0,
                "completed": 0,
                "passed": 0,
                "failed": 0,
            })
        });
    Json(state).into_response()
}

async fn test_all_run(State(s): State<AppState>, body: Option<Json<Value>>) -> Response {
    let ids_filter: Option<HashSet<String>> = body
        .and_then(|Json(v)| v["ids"].as_array().cloned())
        .map(|arr| {
            arr.into_iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .filter(|set: &HashSet<String>| !set.is_empty());

    let pools: Vec<Value> = {
        let db = s.db.lock().unwrap();
        load_all_pools(&db)
            .into_iter()
            .filter(|p| {
                ids_filter
                    .as_ref()
                    .is_none_or(|set| p["id"].as_str().is_some_and(|id| set.contains(id)))
            })
            .collect()
    };

    let total = pools.len();
    let mut passed = 0usize;
    let mut failed = 0usize;
    for mut p in pools {
        let id = p["id"].as_str().unwrap_or("").to_owned();
        let url = p["proxyUrl"].as_str().unwrap_or("").to_owned();
        let (ok, _, err, _) = probe_url(&s.executor.0, &url).await;
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
        let now = now_iso();
        if let Some(obj) = p.as_object_mut() {
            obj.insert("lastTestedAt".into(), Value::String(now.clone()));
            obj.insert("lastError".into(), err.map(Value::String).unwrap_or(Value::Null));
            for k in ["id", "isActive", "testStatus", "createdAt", "updatedAt"] {
                obj.remove(k);
            }
        }
        let db = s.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE proxyPools SET testStatus=?1, data=?2, updatedAt=?3 WHERE id=?4",
            rusqlite::params![if ok { "active" } else { "error" }, p.to_string(), now, id],
        );
    }

    let state = json!({
        "running": false,
        "total": total,
        "completed": total,
        "passed": passed,
        "failed": failed,
    });
    let db = s.db.lock().unwrap();
    let _ = crate::db::kv_set(&db, "proxy_pools", "test_all_state", &state.to_string());
    Json(state).into_response()
}

async fn delete_failed(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let failed: Vec<String> = load_all_pools(&db)
        .into_iter()
        .filter(|p| p["testStatus"].as_str() == Some("error"))
        .filter_map(|p| p["id"].as_str().map(str::to_owned))
        .collect();
    if failed.is_empty() {
        return Json(json!({ "deleted": 0, "skippedBound": 0 })).into_response();
    }
    let umap = usage_map(&db);
    let mut deleted = 0usize;
    let mut skipped_bound = 0usize;
    for id in failed {
        if umap.get(&id).copied().unwrap_or(0) > 0 {
            skipped_bound += 1;
        } else if db
            .execute("DELETE FROM proxyPools WHERE id=?1", [&id])
            .is_ok()
        {
            deleted += 1;
        }
    }
    Json(json!({ "deleted": deleted, "skippedBound": skipped_bound })).into_response()
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
    async fn proxy_pools_crud_and_deploys_work() {
        let app = test_app();

        // Create
        let r = app
            .clone()
            .oneshot(
                Request::post("/api/proxy-pools")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "name": "us-east", "proxyUrl": "http://127.0.0.1:8888" })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 201);
        let created: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        let id = created["proxyPool"]["id"].as_str().unwrap().to_owned();

        // List
        let r = app
            .clone()
            .oneshot(Request::get("/api/proxy-pools").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let list: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(list["proxyPools"].as_array().unwrap().len(), 1);

        // Update
        let r = app
            .clone()
            .oneshot(
                Request::put(&format!("/api/proxy-pools/{id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "name": "us-west" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // Delete
        let r = app
            .clone()
            .oneshot(
                Request::delete(&format!("/api/proxy-pools/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // Deploy 501
        for path in [
            "/api/proxy-pools/cloudflare-deploy",
            "/api/proxy-pools/deno-deploy",
            "/api/proxy-pools/netlify-deploy",
            "/api/proxy-pools/vercel-deploy",
        ] {
            let r = app
                .clone()
                .oneshot(Request::post(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(r.status(), 501);
        }
    }
}
