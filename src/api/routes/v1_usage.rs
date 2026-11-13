//! GET /v1/usage. Port of `usage/route.js`: scoped usage for the caller's own key.
//! Auth is handled by the v1 middleware; the key is re-read from headers to scope rows.
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/usage", get(usage))
}

#[derive(Deserialize)]
struct Page {
    limit: Option<String>,
    offset: Option<String>,
}

/// Mirrors the JS clamp: limit in 1..=200 (default 50), offset >= 0 (default 0).
fn clamp_page(p: &Page) -> (i64, i64) {
    let limit = p
        .limit
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.floor())
        .filter(|v| *v >= 1.0)
        .map_or(50, |v| v.min(200.0) as i64);
    let offset = p
        .offset
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.floor())
        .filter(|v| *v >= 0.0)
        .map_or(0, |v| v as i64);
    (limit, offset)
}

/// Same extraction order as JS `extractClientApiKey` (no query fallback: v1 auth already needs a header).
fn header_key(h: &HeaderMap) -> Option<String> {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| h.get("x-api-key").and_then(|v| v.to_str().ok()))
        .or_else(|| h.get("x-goog-api-key").and_then(|v| v.to_str().ok()))
        .map(str::to_owned)
}

fn scoped_usage(db: &Connection, key: &str, limit: i64, offset: i64) -> rusqlite::Result<Value> {
    let (count, prompt, completion, cost): (i64, i64, i64, f64) = db.query_row(
        "SELECT COUNT(*), COALESCE(SUM(COALESCE(promptTokens,0)),0),
                COALESCE(SUM(COALESCE(completionTokens,0)),0), COALESCE(SUM(COALESCE(cost,0)),0)
         FROM usageHistory WHERE apiKey = ?1",
        [key],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut st = db.prepare(
        "SELECT timestamp, provider, model, connectionId, endpoint,
                promptTokens, completionTokens, cost, status
         FROM usageHistory WHERE apiKey = ?1 ORDER BY id DESC LIMIT ?2 OFFSET ?3",
    )?;
    let rows = st
        .query_map(params![key, limit, offset], |r| {
            Ok(json!({
                "timestamp": r.get::<_, Option<String>>(0)?,
                "provider": r.get::<_, Option<String>>(1)?,
                "model": r.get::<_, Option<String>>(2)?,
                "connectionId": r.get::<_, Option<String>>(3)?,
                "endpoint": r.get::<_, Option<String>>(4)?,
                "promptTokens": r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                "completionTokens": r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                "cost": r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                "status": r.get::<_, Option<String>>(8)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({
        "totals": {"count": count, "promptTokens": prompt, "completionTokens": completion, "cost": cost},
        "rows": rows,
        "limit": limit,
        "offset": offset,
    }))
}

async fn usage(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> Response {
    let Some(key) = header_key(&headers) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"API key required"}))).into_response();
    };
    let (limit, offset) = clamp_page(&page);
    let Ok(db) = s.db.lock() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let name: Option<Option<String>> = db
        .query_row(
            "SELECT COALESCE(name,id) FROM apiKeys WHERE key=?1 AND isActive=1
             AND (expiresAt IS NULL OR expiresAt > strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            [&key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .map(Some);
    let Some(Some(name)) = name else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Invalid or expired API key"}))).into_response();
    };
    match scoped_usage(&db, &key, limit, offset) {
        Ok(mut page) => {
            page["key"] = json!(name);
            Json(page).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"Failed to fetch usage"}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::test_support::{app, call};
    use axum::{body::Body, http::Request};

    #[test]
    fn clamps_page() {
        let p = |l: Option<&str>, o: Option<&str>| Page { limit: l.map(Into::into), offset: o.map(Into::into) };
        assert_eq!(clamp_page(&p(None, None)), (50, 0));
        assert_eq!(clamp_page(&p(Some("0"), Some("-3"))), (50, 0));
        assert_eq!(clamp_page(&p(Some("999"), Some("7"))), (200, 7));
        assert_eq!(clamp_page(&p(Some("abc"), None)), (50, 0));
    }

    #[tokio::test]
    async fn scoped_rows_only_for_caller() {
        let (router, db) = app();
        {
            let db = db.lock().unwrap();
            db.execute("INSERT INTO usageHistory(timestamp,apiKey,promptTokens,completionTokens,cost,status,requestId) VALUES('2026-01-01T00:00:00.000Z','sk-test',3,4,0.5,'200','r1')", []).unwrap();
            db.execute("INSERT INTO usageHistory(timestamp,apiKey,promptTokens,requestId) VALUES('2026-01-02T00:00:00.000Z','other',9,'r2')", []).unwrap();
        }
        let (status, v) = call(
            router,
            Request::get("/v1/usage").header("authorization", "Bearer sk-test").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["key"], "Test key");
        assert_eq!(v["totals"]["count"], 1);
        assert_eq!(v["rows"].as_array().unwrap().len(), 1);
        assert_eq!(v["rows"][0]["promptTokens"], 3);
        assert!(!v.to_string().contains("sk-test"));
    }

    #[tokio::test]
    async fn rejects_unknown_key() {
        let (router, _) = app();
        let (status, _) = call(
            router,
            Request::get("/v1/usage").header("x-api-key", "nope").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
