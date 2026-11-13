use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    db::{kv_get, kv_set},
    server::AppState,
};

const SCOPE_BRIEF: &str = "briefMode";
const SCOPE_STATS: &str = "tokenSaverStats";
const VALID_LEVELS: &[&str] = &[
    "lite",
    "full",
    "ultra",
    "wenyan-lite",
    "wenyan",
    "wenyan-ultra",
];
const MAX_TEST_TEXT: usize = 2 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/token-saver/brief",
            get(get_brief).put(put_brief).delete(delete_brief),
        )
        .route("/api/token-saver/stats", get(get_stats))
        .route("/api/token-saver/test", post(post_test))
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn is_valid_level(s: &str) -> bool {
    VALID_LEVELS.contains(&s)
}

fn read_brief_modes(conn: &rusqlite::Connection) -> anyhow::Result<Value> {
    let global = kv_get(conn, SCOPE_BRIEF, "global")?
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_str().filter(|l| is_valid_level(l)).map(str::to_owned));
    let by_key = kv_get(conn, SCOPE_BRIEF, "byKey")?
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    let by_model = kv_get(conn, SCOPE_BRIEF, "byModel")?
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    Ok(json!({
        "global": global,
        "byKey": by_key,
        "byModel": by_model,
    }))
}

fn update_brief_map(
    conn: &rusqlite::Connection,
    kv_key: &str,
    entry_key: &str,
    level: Option<&Value>,
) -> Result<(), (StatusCode, String)> {
    let mut map: Map<String, Value> = kv_get(conn, SCOPE_BRIEF, kv_key)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    match level {
        None | Some(Value::Null) => {
            map.remove(entry_key);
        }
        Some(Value::String(s)) if is_valid_level(s) => {
            map.insert(entry_key.to_owned(), Value::String(s.clone()));
        }
        Some(other) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid brief level: {other}"),
            ));
        }
    }
    kv_set(conn, SCOPE_BRIEF, kv_key, &Value::Object(map).to_string())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(())
}

async fn get_brief(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    match read_brief_modes(&db) {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch brief modes",
        ),
    }
}

async fn put_brief(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(obj) = body.as_object() else {
        return err(StatusCode::BAD_REQUEST, "Provide global, keyId, or model");
    };
    let db = s.db.lock().unwrap();
    if let Some(val) = obj.get("global") {
        let res = match val {
            Value::Null => kv_set(&db, SCOPE_BRIEF, "global", "null")
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
            Value::String(l) if is_valid_level(l) => {
                kv_set(&db, SCOPE_BRIEF, "global", &Value::String(l.clone()).to_string())
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
            }
            other => Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid brief level: {other}"),
            )),
        };
        if let Err((st, msg)) = res {
            return err(st, &msg);
        }
    } else if let Some(key_id) = obj.get("keyId").and_then(|v| v.as_str()).filter(|k| !k.is_empty()) {
        if let Err((st, msg)) = update_brief_map(&db, "byKey", key_id, obj.get("level")) {
            return err(st, &msg);
        }
    } else if let Some(model) = obj.get("model").and_then(|v| v.as_str()).filter(|m| !m.is_empty()) {
        if let Err((st, msg)) = update_brief_map(&db, "byModel", model, obj.get("level")) {
            return err(st, &msg);
        }
    } else {
        return err(StatusCode::BAD_REQUEST, "Provide global, keyId, or model");
    }

    match read_brief_modes(&db) {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch brief modes",
        ),
    }
}

async fn delete_brief(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let db = s.db.lock().unwrap();
    if params.contains_key("global") {
        if kv_set(&db, SCOPE_BRIEF, "global", "null").is_err() {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete brief mode");
        }
    } else if let Some(key_id) = params.get("keyId").filter(|k| !k.is_empty()) {
        if let Err((st, msg)) = update_brief_map(&db, "byKey", key_id, None) {
            return err(st, &msg);
        }
    } else if let Some(model) = params.get("model").filter(|m| !m.is_empty()) {
        if let Err((st, msg)) = update_brief_map(&db, "byModel", model, None) {
            return err(st, &msg);
        }
    } else {
        return err(StatusCode::BAD_REQUEST, "Provide global, keyId, or model");
    }

    match read_brief_modes(&db) {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch brief modes",
        ),
    }
}

fn utc_day_key(unix_secs: i64) -> String {
    let z = unix_secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02}")
}

async fn get_stats(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let days = params
        .get("days")
        .and_then(|d| d.parse::<i64>().ok())
        .unwrap_or(14)
        .clamp(1, 90);
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let db = s.db.lock().unwrap();
    let mut out_days = Vec::with_capacity(days as usize);
    let mut total_requests: i64 = 0;
    let mut total_before: i64 = 0;
    let mut total_after: i64 = 0;
    let mut total_hits: i64 = 0;
    let mut total_filters: HashMap<String, (i64, i64)> = HashMap::new();

    for i in (0..days).rev() {
        let key = utc_day_key(now_secs - i * 86_400);
        let stored = kv_get(&db, SCOPE_STATS, &key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .filter(|v| v.is_object());
        let day = stored.unwrap_or_else(|| {
            json!({
                "requests": 0,
                "bytesBefore": 0,
                "bytesAfter": 0,
                "hits": 0,
                "filters": {}
            })
        });
        total_requests += day["requests"].as_i64().unwrap_or(0);
        total_before += day["bytesBefore"].as_i64().unwrap_or(0);
        total_after += day["bytesAfter"].as_i64().unwrap_or(0);
        total_hits += day["hits"].as_i64().unwrap_or(0);
        if let Some(filters) = day["filters"].as_object() {
            for (name, f) in filters {
                let e = total_filters.entry(name.clone()).or_insert((0, 0));
                e.0 += f["hits"].as_i64().unwrap_or(0);
                e.1 += f["saved"].as_i64().unwrap_or(0);
            }
        }
        let mut obj = day.as_object().cloned().unwrap_or_default();
        obj.insert("date".into(), Value::String(key));
        out_days.push(Value::Object(obj));
    }

    let mut top_filters: Vec<(String, i64, i64)> = total_filters
        .iter()
        .map(|(n, (h, sv))| (n.clone(), *h, *sv))
        .collect();
    top_filters.sort_by(|a, b| b.2.cmp(&a.2));
    top_filters.truncate(6);

    let filters_json: Map<String, Value> = total_filters
        .into_iter()
        .map(|(n, (h, sv))| (n, json!({ "hits": h, "saved": sv })))
        .collect();

    Json(json!({
        "days": out_days,
        "totals": {
            "requests": total_requests,
            "bytesBefore": total_before,
            "bytesAfter": total_after,
            "hits": total_hits,
            "filters": filters_json,
        },
        "topFilters": top_filters.into_iter().map(|(name, hits, saved)| json!({
            "name": name,
            "hits": hits,
            "saved": saved,
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}

fn compress_preview(text: &str) -> (String, Vec<&'static str>) {
    // ponytail: full 12-filter RTK suite lives in JS; Rust test bench runs dedup-log + smart-truncate
    if text.len() < 500 {
        return (text.to_owned(), vec![]);
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut filters = Vec::new();

    // Dedup consecutive identical lines
    let mut deduped: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        let mut count = 1usize;
        while i + count < lines.len() && lines[i + count] == cur {
            count += 1;
        }
        deduped.push(cur.to_owned());
        if count > 1 {
            deduped.push(format!("... (repeated {} more times)", count - 1));
        }
        i += count;
    }
    let mut out = deduped.join("\n");
    if out.len() < text.len() {
        filters.push("dedup-log");
    } else {
        out = text.to_owned();
    }

    // Smart truncate if still >= 250 lines
    let out_lines: Vec<&str> = out.lines().collect();
    if out_lines.len() >= 250 {
        let mut truncated = Vec::with_capacity(181);
        truncated.extend_from_slice(&out_lines[..120]);
        truncated.push(&"");
        let omitted = out_lines.len() - 180;
        let marker = format!("... ({omitted} lines omitted)");
        let mut result = truncated[..120].join("\n");
        result.push('\n');
        result.push_str(&marker);
        result.push('\n');
        result.push_str(&out_lines[out_lines.len() - 60..].join("\n"));
        if result.len() < out.len() {
            out = result;
            filters.push("smart-truncate");
        }
    }

    if out.is_empty() || out.len() >= text.len() {
        (text.to_owned(), vec![])
    } else {
        (out, filters)
    }
}

async fn post_test(Json(body): Json<Value>) -> Response {
    let Some(text) = body.get("text").and_then(|v| v.as_str()) else {
        return err(StatusCode::BAD_REQUEST, "Provide text to compress");
    };
    if text.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Provide text to compress");
    }
    if text.len() > MAX_TEST_TEXT {
        return err(StatusCode::BAD_REQUEST, "Text too large (max 2 MB)");
    }

    let (output, filters) = compress_preview(text);
    let before = text.len();
    let after = output.len();
    let est = |n: usize| (n + 3) / 4;
    let saved_pct = if before > 0 {
        (((before - after) as f64 / before as f64) * 1000.0).round() / 10.0
    } else {
        0.0
    };

    Json(json!({
        "applied": !filters.is_empty(),
        "filters": filters,
        "hits": filters.len(),
        "before": { "chars": before, "tokens": est(before) },
        "after": { "chars": after, "tokens": est(after) },
        "saved": { "chars": before - after, "tokens": est(before) - est(after) },
        "savedPct": saved_pct,
        "output": output,
    }))
    .into_response()
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
    async fn brief_roundtrip_put_get_delete() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::put("/api/token-saver/brief")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"global":"ultra"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .clone()
            .oneshot(
                Request::put("/api/token-saver/brief")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"keyId":"k1","level":"lite"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = serde_json::from_slice(&to_bytes(r.into_body(), 10_000).await.unwrap()).unwrap();
        assert_eq!(body["global"], "ultra");
        assert_eq!(body["byKey"]["k1"], "lite");

        let r = app
            .clone()
            .oneshot(
                Request::delete("/api/token-saver/brief?keyId=k1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = serde_json::from_slice(&to_bytes(r.into_body(), 10_000).await.unwrap()).unwrap();
        assert!(body["byKey"].get("k1").is_none());
    }

    #[tokio::test]
    async fn stats_returns_requested_days() {
        let app = test_app();
        let r = app
            .oneshot(
                Request::get("/api/token-saver/stats?days=7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = serde_json::from_slice(&to_bytes(r.into_body(), 20_000).await.unwrap()).unwrap();
        assert_eq!(body["days"].as_array().unwrap().len(), 7);
        assert_eq!(body["totals"]["requests"], 0);
    }

    #[tokio::test]
    async fn test_bench_compresses_repeated_lines() {
        let app = test_app();
        let line = "WARN repeated log line for testing token saver compression\n";
        let big = line.repeat(30);
        let r = app
            .oneshot(
                Request::post("/api/token-saver/test")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "text": big }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = serde_json::from_slice(&to_bytes(r.into_body(), 50_000).await.unwrap()).unwrap();
        assert_eq!(body["applied"], true);
        assert!(body["saved"]["chars"].as_u64().unwrap() > 0);
    }
}
