use crate::{api::routes::obs_events::percentile, server::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_core::Stream;
use rusqlite::{Connection, OptionalExtension, types::Value as SqlValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    pin::Pin,
    sync::{Mutex, OnceLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/usage/stats", get(usage_stats))
        .route("/api/usage/chart", get(usage_chart))
        .route("/api/usage/providers", get(usage_providers))
        .route("/api/usage/by-key", get(usage_by_key))
        .route("/api/usage/latency", get(usage_latency))
        .route("/api/usage/logs", get(usage_logs))
        .route("/api/usage/request-details", get(usage_request_details))
        .route("/api/usage/stream", get(usage_stream))
        .route("/api/usage/{connectionId}", get(usage_connection))
        .route("/api/usage/{connectionId}/claude-reset", post(claude_reset))
        .route(
            "/api/usage/{connectionId}/codex-reset-credits",
            get(codex_reset_credits_get).post(codex_reset_credits_post),
        )
}

fn no_store_json(status: StatusCode, body: Value) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (status, headers, Json(body))
}

fn is_valid_date_key(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn mask_api_key(raw: &str) -> String {
    if raw.len() <= 12 {
        return "****".to_string();
    }
    format!("{}***{}", &raw[..8], &raw[raw.len() - 4..])
}

fn is_observability_enabled(db: &Connection) -> bool {
    let raw: Option<String> = db
        .query_row("SELECT data FROM settings WHERE id = 1", [], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    if let Some(s) = raw
        && let Ok(v) = serde_json::from_str::<Value>(&s)
    {
        if let Some(b) = v.get("observabilityEnabled").and_then(Value::as_bool) {
            return b;
        }
        if let Some(b) = v.get("enableRequestDetails").and_then(Value::as_bool) {
            return b;
        }
    }
    true
}

fn period_bounds(
    db: &Connection,
    period: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Option<(Option<String>, Option<String>)> {
    match period {
        "all" => Some((None, None)),
        "today" => {
            let s: String = db
                .query_row(
                    "SELECT strftime('%Y-%m-%dT00:00:00.000Z','now','localtime')",
                    [],
                    |r| r.get(0),
                )
                .ok()?;
            Some((Some(s), None))
        }
        "24h" => {
            let s: String = db
                .query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 day')",
                    [],
                    |r| r.get(0),
                )
                .ok()?;
            Some((Some(s), None))
        }
        "7d" => {
            let s: String = db
                .query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','-7 days')",
                    [],
                    |r| r.get(0),
                )
                .ok()?;
            Some((Some(s), None))
        }
        "30d" => {
            let s: String = db
                .query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','-30 days')",
                    [],
                    |r| r.get(0),
                )
                .ok()?;
            Some((Some(s), None))
        }
        "60d" => {
            let s: String = db
                .query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','-60 days')",
                    [],
                    |r| r.get(0),
                )
                .ok()?;
            Some((Some(s), None))
        }
        "custom" => {
            let st = start?;
            let en = end?;
            Some((
                Some(format!("{st}T00:00:00.000Z")),
                Some(format!("{en}T23:59:59.999Z")),
            ))
        }
        _ => None,
    }
}

fn build_usage_stats(db: &Connection, start: Option<&str>, end: Option<&str>) -> Value {
    let mut conds = Vec::new();
    let mut sql_params: Vec<String> = Vec::new();
    if let Some(s) = start {
        conds.push("timestamp >= ?".to_string());
        sql_params.push(s.to_string());
    }
    if let Some(e) = end {
        conds.push("timestamp <= ?".to_string());
        sql_params.push(e.to_string());
    }
    let where_clause = if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    };

    let mut conn_map: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) = db.prepare("SELECT id, name, email FROM providerConnections")
        && let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
    {
        for (id, name, email) in rows.flatten() {
            if let Some(n) = name.or(email) {
                conn_map.insert(id, n);
            }
        }
    }

    let mut node_map: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) = db.prepare("SELECT id, name FROM providerNodes WHERE name IS NOT NULL")
        && let Ok(rows) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    {
        for (id, name) in rows.flatten() {
            node_map.insert(id, name);
        }
    }

    let mut key_map: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) = db.prepare("SELECT key, name FROM apiKeys WHERE name IS NOT NULL")
        && let Ok(rows) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    {
        for (k, name) in rows.flatten() {
            key_map.insert(k, name);
        }
    }

    let mut total_requests: i64 = 0;
    let mut total_prompt: i64 = 0;
    let mut total_completion: i64 = 0;
    let mut total_cached: i64 = 0;
    let mut total_cost: f64 = 0.0;
    let mut by_provider: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut by_model: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut by_account: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut by_api_key: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut by_endpoint: serde_json::Map<String, Value> = serde_json::Map::new();

    let sql = format!(
        "SELECT timestamp, provider, model, connectionId, apiKey, endpoint, promptTokens, completionTokens, cost, tokens FROM usageHistory {where_clause}"
    );
    if let Ok(mut stmt) = db.prepare(&sql) {
        let params_refs: Vec<&dyn rusqlite::ToSql> = sql_params
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        if let Ok(rows) = stmt.query_map(params_refs.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                r.get::<_, Option<String>>(9)?,
            ))
        }) {
            for (ts, prov_opt, model_opt, conn_id, api_key, endpoint_opt, pt, ct, cost, tk_str) in
                rows.flatten()
            {
                let tk: Value = tk_str
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null);
                let cached = tk
                    .get("cached_tokens")
                    .or_else(|| tk.get("cache_read_input_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);

                total_requests += 1;
                total_prompt += pt;
                total_completion += ct;
                total_cached += cached;
                total_cost += cost;

                let prov = prov_opt.unwrap_or_else(|| "unknown".to_string());
                let prov_disp = node_map.get(&prov).cloned().unwrap_or_else(|| prov.clone());
                let model = model_opt.unwrap_or_else(|| "unknown".to_string());

                let p_entry = by_provider.entry(prov.clone()).or_insert_with(|| {
                    json!({"requests":0,"promptTokens":0,"completionTokens":0,"cachedTokens":0,"cost":0.0})
                });
                p_entry["requests"] = json!(p_entry["requests"].as_i64().unwrap_or(0) + 1);
                p_entry["promptTokens"] = json!(p_entry["promptTokens"].as_i64().unwrap_or(0) + pt);
                p_entry["completionTokens"] =
                    json!(p_entry["completionTokens"].as_i64().unwrap_or(0) + ct);
                p_entry["cachedTokens"] =
                    json!(p_entry["cachedTokens"].as_i64().unwrap_or(0) + cached);
                p_entry["cost"] = json!(p_entry["cost"].as_f64().unwrap_or(0.0) + cost);

                let m_key = format!("{model} ({prov})");
                let m_entry = by_model.entry(m_key).or_insert_with(|| {
                    json!({"requests":0,"promptTokens":0,"completionTokens":0,"cachedTokens":0,"cost":0.0,"rawModel":model,"provider":prov_disp,"providerRaw":prov,"lastUsed":ts})
                });
                m_entry["requests"] = json!(m_entry["requests"].as_i64().unwrap_or(0) + 1);
                m_entry["promptTokens"] = json!(m_entry["promptTokens"].as_i64().unwrap_or(0) + pt);
                m_entry["completionTokens"] =
                    json!(m_entry["completionTokens"].as_i64().unwrap_or(0) + ct);
                m_entry["cachedTokens"] =
                    json!(m_entry["cachedTokens"].as_i64().unwrap_or(0) + cached);
                m_entry["cost"] = json!(m_entry["cost"].as_f64().unwrap_or(0.0) + cost);

                if let Some(cid) = conn_id {
                    let acc_name = conn_map
                        .get(&cid)
                        .cloned()
                        .unwrap_or_else(|| format!("Account {cid}..."));
                    let a_key = format!("{model} ({prov} - {acc_name})");
                    let a_entry = by_account.entry(a_key).or_insert_with(|| {
                        json!({"requests":0,"promptTokens":0,"completionTokens":0,"cachedTokens":0,"cost":0.0,"rawModel":model,"provider":prov_disp,"providerRaw":prov,"connectionId":cid,"accountName":acc_name,"lastUsed":ts})
                    });
                    a_entry["requests"] = json!(a_entry["requests"].as_i64().unwrap_or(0) + 1);
                    a_entry["promptTokens"] =
                        json!(a_entry["promptTokens"].as_i64().unwrap_or(0) + pt);
                    a_entry["completionTokens"] =
                        json!(a_entry["completionTokens"].as_i64().unwrap_or(0) + ct);
                    a_entry["cachedTokens"] =
                        json!(a_entry["cachedTokens"].as_i64().unwrap_or(0) + cached);
                    a_entry["cost"] = json!(a_entry["cost"].as_f64().unwrap_or(0.0) + cost);
                }

                let (ak_bucket, ak_masked, key_name, ak_key_id) = match api_key.as_deref() {
                    Some(k) if !k.trim().is_empty() => {
                        let k = k.trim();
                        let masked = mask_api_key(k);
                        let name = key_map.get(k).cloned().unwrap_or_else(|| masked.clone());
                        (
                            format!("{masked}|{model}|{prov}"),
                            Value::String(masked.clone()),
                            name,
                            masked,
                        )
                    }
                    _ => (
                        format!("local-no-key|{model}|{prov}"),
                        Value::Null,
                        "Local (No API Key)".to_string(),
                        "local-no-key".to_string(),
                    ),
                };
                let ak_entry = by_api_key.entry(ak_bucket).or_insert_with(|| {
                    json!({"requests":0,"promptTokens":0,"completionTokens":0,"cachedTokens":0,"cost":0.0,"rawModel":model,"provider":prov_disp,"providerRaw":prov,"apiKeyMasked":ak_masked,"keyName":key_name,"apiKeyKey":ak_key_id,"lastUsed":ts})
                });
                ak_entry["requests"] = json!(ak_entry["requests"].as_i64().unwrap_or(0) + 1);
                ak_entry["promptTokens"] =
                    json!(ak_entry["promptTokens"].as_i64().unwrap_or(0) + pt);
                ak_entry["completionTokens"] =
                    json!(ak_entry["completionTokens"].as_i64().unwrap_or(0) + ct);
                ak_entry["cachedTokens"] =
                    json!(ak_entry["cachedTokens"].as_i64().unwrap_or(0) + cached);
                ak_entry["cost"] = json!(ak_entry["cost"].as_f64().unwrap_or(0.0) + cost);

                let ep = endpoint_opt.unwrap_or_else(|| "Unknown".to_string());
                let ep_key = format!("{ep}|{model}|{prov}");
                let ep_entry = by_endpoint.entry(ep_key).or_insert_with(|| {
                    json!({"requests":0,"promptTokens":0,"completionTokens":0,"cachedTokens":0,"cost":0.0,"endpoint":ep,"rawModel":model,"provider":prov_disp,"providerRaw":prov,"lastUsed":ts})
                });
                ep_entry["requests"] = json!(ep_entry["requests"].as_i64().unwrap_or(0) + 1);
                ep_entry["promptTokens"] =
                    json!(ep_entry["promptTokens"].as_i64().unwrap_or(0) + pt);
                ep_entry["completionTokens"] =
                    json!(ep_entry["completionTokens"].as_i64().unwrap_or(0) + ct);
                ep_entry["cachedTokens"] =
                    json!(ep_entry["cachedTokens"].as_i64().unwrap_or(0) + cached);
                ep_entry["cost"] = json!(ep_entry["cost"].as_f64().unwrap_or(0.0) + cost);
            }
        }
    }

    json!({
        "totalRequests": total_requests,
        "totalPromptTokens": total_prompt,
        "totalCompletionTokens": total_completion,
        "totalCachedTokens": total_cached,
        "totalCost": total_cost,
        "byProvider": by_provider,
        "byModel": by_model,
        "byAccount": by_account,
        "byApiKey": by_api_key,
        "byEndpoint": by_endpoint,
        "activeRequests": [],
        "recentRequests": [],
        "errorProvider": Value::Null,
        "pending": {}
    })
}

// ---- /api/usage/stats ----

#[derive(Deserialize)]
struct PeriodQuery {
    period: Option<String>,
    start: Option<String>,
    end: Option<String>,
}

static STATS_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Value)>>> = OnceLock::new();

async fn usage_stats(
    State(state): State<AppState>,
    Query(q): Query<PeriodQuery>,
) -> impl IntoResponse {
    let period = q.period.as_deref().unwrap_or("7d");
    if !matches!(
        period,
        "today" | "24h" | "7d" | "30d" | "60d" | "all" | "custom"
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid period" })),
        );
    }
    if period == "custom" {
        let start = q.start.as_deref().unwrap_or("");
        let end = q.end.as_deref().unwrap_or("");
        if !is_valid_date_key(start) || !is_valid_date_key(end) || start > end {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({ "error": "Custom period needs start and end dates (YYYY-MM-DD, start <= end)" }),
                ),
            );
        }
    }
    let cache_key = format!(
        "{}|{}|{}",
        period,
        q.start.as_deref().unwrap_or(""),
        q.end.as_deref().unwrap_or("")
    );
    let cache = STATS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some((ts, data)) = guard.get(&cache_key)
        && ts.elapsed() < Duration::from_secs(5)
    {
        return (StatusCode::OK, Json(data.clone()));
    }
    let db = state.db.lock().unwrap();
    let Some((start, end)) = period_bounds(&db, period, q.start.as_deref(), q.end.as_deref())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid period" })),
        );
    };
    let stats = build_usage_stats(&db, start.as_deref(), end.as_deref());
    if let Ok(mut guard) = cache.lock() {
        guard.insert(cache_key, (Instant::now(), stats.clone()));
    }
    (StatusCode::OK, Json(stats))
}

// ---- /api/usage/chart ----

async fn usage_chart(
    State(state): State<AppState>,
    Query(q): Query<PeriodQuery>,
) -> impl IntoResponse {
    let period = q.period.as_deref().unwrap_or("7d");
    if !matches!(period, "today" | "24h" | "7d" | "30d" | "60d" | "all") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid period" })),
        );
    }
    let db = state.db.lock().unwrap();
    if matches!(period, "today" | "24h") {
        let mut buckets: Vec<Value> = (0..24)
            .map(|h| {
                json!({
                    "label": format!("{h:02}:00"),
                    "tokens": 0,
                    "cost": 0.0,
                    "requests": 0,
                    "cached": 0,
                    "completion": 0
                })
            })
            .collect();
        let since = if period == "today" {
            "SELECT strftime('%Y-%m-%dT00:00:00.000Z','now')"
        } else {
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 day')"
        };
        let cutoff: String = db.query_row(since, [], |r| r.get(0)).unwrap_or_default();
        if let Ok(mut stmt) = db.prepare(
            "SELECT CAST(strftime('%H', timestamp) AS INTEGER), promptTokens, completionTokens, cost, tokens FROM usageHistory WHERE timestamp >= ?1",
        ) && let Ok(rows) = stmt.query_map([&cutoff], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?.unwrap_or(0) as usize,
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                r.get::<_, Option<String>>(4)?,
            ))
        }) {
            for (hr, pt, ct, cost, tk_str) in rows.flatten() {
                if hr < 24 {
                    let cached = tk_str
                        .as_deref()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .and_then(|v| {
                            v.get("cached_tokens")
                                .or_else(|| v.get("cache_read_input_tokens"))
                                .and_then(Value::as_i64)
                        })
                        .unwrap_or(0);
                    let b = &mut buckets[hr];
                    b["tokens"] = json!(b["tokens"].as_i64().unwrap_or(0) + pt + ct);
                    b["cost"] = json!(b["cost"].as_f64().unwrap_or(0.0) + cost);
                    b["requests"] = json!(b["requests"].as_i64().unwrap_or(0) + 1);
                    b["cached"] = json!(b["cached"].as_i64().unwrap_or(0) + cached);
                    b["completion"] = json!(b["completion"].as_i64().unwrap_or(0) + ct);
                }
            }
        }
        return (StatusCode::OK, Json(Value::Array(buckets)));
    }

    let days: i64 = match period {
        "7d" => 7,
        "30d" => 30,
        "60d" => 60,
        _ => 30,
    };
    let mut day_map: HashMap<String, Value> = HashMap::new();
    if let Ok(mut stmt) = db.prepare("SELECT dateKey, data FROM usageDaily ORDER BY dateKey ASC")
        && let Ok(rows) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    {
        for (dk, ds) in rows.flatten() {
            if let Ok(v) = serde_json::from_str::<Value>(&ds) {
                day_map.insert(dk, v);
            }
        }
    }
    let mut out = Vec::new();
    for offset in (0..days).rev() {
        let dk: String = db
            .query_row(
                "SELECT date('now', printf('-%d days', ?1))",
                [offset],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let d = day_map.get(&dk);
        let pt = d
            .and_then(|v| v.get("promptTokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let ct = d
            .and_then(|v| v.get("completionTokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cost = d
            .and_then(|v| v.get("cost"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let reqs = d
            .and_then(|v| v.get("requests"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cached = d
            .and_then(|v| v.get("cachedTokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        out.push(json!({
            "label": dk,
            "tokens": pt + ct,
            "cost": cost,
            "requests": reqs,
            "cached": cached,
            "completion": ct,
        }));
    }
    (StatusCode::OK, Json(Value::Array(out)))
}

// ---- /api/usage/providers ----

async fn usage_providers(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let mut node_map: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) = db.prepare("SELECT id, name FROM providerNodes WHERE name IS NOT NULL")
        && let Ok(rows) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    {
        for (id, name) in rows.flatten() {
            node_map.insert(id, name);
        }
    }
    let mut providers = Vec::new();
    if let Ok(mut stmt) =
        db.prepare("SELECT DISTINCT provider FROM requestDetails WHERE provider IS NOT NULL ORDER BY provider ASC")
        && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0))
    {
        for id in rows.flatten() {
            let name = node_map.get(&id).cloned().unwrap_or_else(|| id.clone());
            providers.push(json!({ "id": id, "name": name }));
        }
    }
    Json(json!({ "providers": providers }))
}

// ---- /api/usage/by-key ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ByKeyQuery {
    key_id: Option<String>,
    period: Option<String>,
    start: Option<String>,
    end: Option<String>,
}

async fn usage_by_key(
    State(state): State<AppState>,
    Query(q): Query<ByKeyQuery>,
) -> impl IntoResponse {
    let period = q.period.as_deref().unwrap_or("7d");
    if !matches!(
        period,
        "today" | "24h" | "7d" | "30d" | "60d" | "all" | "custom"
    ) {
        return no_store_json(
            StatusCode::BAD_REQUEST,
            json!({ "error": "Invalid period" }),
        );
    }
    if period == "custom" {
        let start = q.start.as_deref().unwrap_or("");
        let end = q.end.as_deref().unwrap_or("");
        if !is_valid_date_key(start) || !is_valid_date_key(end) || start > end {
            return no_store_json(
                StatusCode::BAD_REQUEST,
                json!({ "error": "Custom period needs start and end dates (YYYY-MM-DD, start <= end)" }),
            );
        }
    }
    let db = state.db.lock().unwrap();
    let mut key_obj = Value::Null;
    let mut raw_key: Option<String> = None;
    if let Some(kid) = q.key_id.as_deref() {
        let row: Option<(String, Option<String>, String)> = db
            .query_row(
                "SELECT id, name, key FROM apiKeys WHERE id = ?1",
                [kid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((id, name, raw)) = row else {
            return no_store_json(StatusCode::NOT_FOUND, json!({ "error": "Unknown keyId" }));
        };
        key_obj = json!({
            "id": id,
            "name": name,
            "apiKeyMasked": mask_api_key(&raw),
        });
        raw_key = Some(raw);
    }

    let (start, end) =
        period_bounds(&db, period, q.start.as_deref(), q.end.as_deref()).unwrap_or((None, None));
    let mut conds = Vec::new();
    let mut params_vec: Vec<String> = Vec::new();
    if let Some(rk) = raw_key {
        conds.push("apiKey = ?".to_string());
        params_vec.push(rk);
    }
    if let Some(s) = &start {
        conds.push("timestamp >= ?".to_string());
        params_vec.push(s.clone());
    }
    if let Some(e) = &end {
        conds.push("timestamp <= ?".to_string());
        params_vec.push(e.clone());
    }
    let where_clause = if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    };
    let p_refs: Vec<&dyn rusqlite::ToSql> = params_vec
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();

    let totals = db
        .query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(COALESCE(promptTokens,0)),0), COALESCE(SUM(COALESCE(completionTokens,0)),0), COALESCE(SUM(COALESCE(cost,0)),0) FROM usageHistory {where_clause}"
            ),
            p_refs.as_slice(),
            |r| {
                Ok(json!({
                    "requests": r.get::<_, i64>(0)?,
                    "promptTokens": r.get::<_, i64>(1)?,
                    "completionTokens": r.get::<_, i64>(2)?,
                    "cost": r.get::<_, f64>(3)?,
                }))
            },
        )
        .unwrap_or_else(|_| json!({"requests":0,"promptTokens":0,"completionTokens":0,"cost":0.0}));

    let mut by_model = Vec::new();
    if let Ok(mut stmt) = db.prepare(&format!(
        "SELECT provider, model, COUNT(*) AS requests, COALESCE(SUM(COALESCE(promptTokens,0)),0), COALESCE(SUM(COALESCE(completionTokens,0)),0), COALESCE(SUM(COALESCE(cost,0)),0) FROM usageHistory {where_clause} GROUP BY provider, model ORDER BY requests DESC"
    )) && let Ok(rows) = stmt.query_map(p_refs.as_slice(), |r| {
        Ok(json!({
            "provider": r.get::<_, Option<String>>(0)?,
            "model": r.get::<_, Option<String>>(1)?,
            "requests": r.get::<_, i64>(2)?,
            "promptTokens": r.get::<_, i64>(3)?,
            "completionTokens": r.get::<_, i64>(4)?,
            "cost": r.get::<_, f64>(5)?,
        }))
    }) {
        by_model = rows.flatten().collect();
    }

    let mut by_provider = Vec::new();
    if let Ok(mut stmt) = db.prepare(&format!(
        "SELECT provider, COUNT(*) AS requests, COALESCE(SUM(COALESCE(promptTokens,0)),0), COALESCE(SUM(COALESCE(completionTokens,0)),0), COALESCE(SUM(COALESCE(cost,0)),0) FROM usageHistory {where_clause} GROUP BY provider ORDER BY requests DESC"
    )) && let Ok(rows) = stmt.query_map(p_refs.as_slice(), |r| {
        Ok(json!({
            "provider": r.get::<_, Option<String>>(0)?,
            "requests": r.get::<_, i64>(1)?,
            "promptTokens": r.get::<_, i64>(2)?,
            "completionTokens": r.get::<_, i64>(3)?,
            "cost": r.get::<_, f64>(4)?,
        }))
    }) {
        by_provider = rows.flatten().collect();
    }

    let mut by_endpoint = Vec::new();
    if let Ok(mut stmt) = db.prepare(&format!(
        "SELECT endpoint, COUNT(*) AS requests, COALESCE(SUM(COALESCE(promptTokens,0)),0), COALESCE(SUM(COALESCE(completionTokens,0)),0), COALESCE(SUM(COALESCE(cost,0)),0) FROM usageHistory {where_clause} GROUP BY endpoint ORDER BY requests DESC"
    )) && let Ok(rows) = stmt.query_map(p_refs.as_slice(), |r| {
        Ok(json!({
            "endpoint": r.get::<_, Option<String>>(0)?,
            "requests": r.get::<_, i64>(1)?,
            "promptTokens": r.get::<_, i64>(2)?,
            "completionTokens": r.get::<_, i64>(3)?,
            "cost": r.get::<_, f64>(4)?,
        }))
    }) {
        by_endpoint = rows.flatten().collect();
    }

    let mut out = json!({
        "period": period,
        "key": key_obj,
        "totals": totals,
        "byModel": by_model,
        "byProvider": by_provider,
        "byEndpoint": by_endpoint,
    });
    if let Some(s) = start {
        out["start"] = json!(s);
    }
    if let Some(e) = end {
        out["end"] = json!(e);
    }
    no_store_json(StatusCode::OK, out)
}

// ---- /api/usage/latency ----

async fn usage_latency(
    State(state): State<AppState>,
    Query(q): Query<PeriodQuery>,
) -> impl IntoResponse {
    let period = q.period.as_deref().unwrap_or("7d");
    let db = state.db.lock().unwrap();
    let since: Option<String> = match period {
        "24h" => db
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 day')",
                [],
                |r| r.get(0),
            )
            .ok(),
        "7d" => db
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','-7 days')",
                [],
                |r| r.get(0),
            )
            .ok(),
        "30d" => db
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','-30 days')",
                [],
                |r| r.get(0),
            )
            .ok(),
        _ => None,
    };
    let mut totals: Vec<f64> = Vec::new();
    let mut ttfts: Vec<f64> = Vec::new();
    let mut by_provider: HashMap<String, (Vec<f64>, Vec<f64>)> = HashMap::new();

    let (sql, param): (&str, Vec<String>) = if let Some(s) = since {
        (
            "SELECT data FROM requestDetails WHERE timestamp >= ?1 ORDER BY timestamp DESC LIMIT 5000",
            vec![s],
        )
    } else {
        (
            "SELECT data FROM requestDetails ORDER BY timestamp DESC LIMIT 5000",
            vec![],
        )
    };
    let p_refs: Vec<&dyn rusqlite::ToSql> =
        param.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    if let Ok(mut stmt) = db.prepare(sql)
        && let Ok(rows) = stmt.query_map(p_refs.as_slice(), |r| r.get::<_, String>(0))
    {
        for ds in rows.flatten() {
            if let Ok(d) = serde_json::from_str::<Value>(&ds)
                && let Some(total) = d.pointer("/latency/total").and_then(Value::as_f64)
                && total > 0.0
            {
                let ttft = d
                    .pointer("/latency/ttft")
                    .and_then(Value::as_f64)
                    .filter(|&v| v > 0.0);
                totals.push(total);
                if let Some(t) = ttft {
                    ttfts.push(t);
                }
                let p = d
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let entry = by_provider.entry(p).or_default();
                entry.0.push(total);
                if let Some(t) = ttft {
                    entry.1.push(t);
                }
            }
        }
    }
    totals.sort_by(|a, b| a.total_cmp(b));
    ttfts.sort_by(|a, b| a.total_cmp(b));
    let mut bp_out = serde_json::Map::new();
    for (p, (mut tots, mut ts)) in by_provider {
        tots.sort_by(|a, b| a.total_cmp(b));
        ts.sort_by(|a, b| a.total_cmp(b));
        bp_out.insert(
            p,
            json!({
                "samples": tots.len(),
                "totalP50": percentile(&tots, 50.0),
                "totalP95": percentile(&tots, 95.0),
                "ttftP50": percentile(&ts, 50.0),
            }),
        );
    }
    let obs = is_observability_enabled(&db);
    no_store_json(
        StatusCode::OK,
        json!({
            "period": period,
            "samples": totals.len(),
            "ttft": { "samples": ttfts.len(), "p50": percentile(&ttfts, 50.0), "p95": percentile(&ttfts, 95.0) },
            "total": { "samples": totals.len(), "p50": percentile(&totals, 50.0), "p95": percentile(&totals, 95.0) },
            "byProvider": bp_out,
            "observabilityEnabled": obs,
        }),
    )
}

// ---- /api/usage/logs ----

async fn usage_logs(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let mut conn_map: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) = db.prepare("SELECT id, name, email FROM providerConnections")
        && let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
    {
        for (id, name, email) in rows.flatten() {
            if let Some(n) = name.or(email) {
                conn_map.insert(id, n);
            }
        }
    }
    let mut logs = Vec::new();
    if let Ok(mut stmt) = db.prepare(
        "SELECT timestamp, provider, model, connectionId, promptTokens, completionTokens, status FROM usageHistory ORDER BY id DESC LIMIT 200",
    ) && let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    }) {
        for (ts, p, m, cid, pt, ct, status) in rows.flatten() {
            let p_s = p.as_deref().unwrap_or("-");
            let m_s = m.as_deref().unwrap_or("-");
            let acc = cid
                .as_deref()
                .map(|c| conn_map.get(c).map(String::as_str).unwrap_or(c))
                .unwrap_or("-");
            let sent = pt.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
            let recv = ct.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
            let st = status.as_deref().unwrap_or("-");
            logs.push(format!("{ts} | {m_s} | {p_s} | {acc} | {sent} | {recv} | {st}"));
        }
    }
    Json(Value::Array(logs.into_iter().map(Value::String).collect()))
}

// ---- /api/usage/request-details ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestDetailsQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    provider: Option<String>,
    model: Option<String>,
    request_id: Option<String>,
    connection_id: Option<String>,
    status: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
}

async fn usage_request_details(
    State(state): State<AppState>,
    Query(q): Query<RequestDetailsQuery>,
) -> impl IntoResponse {
    let page = q.page.unwrap_or(1);
    let page_size = q.page_size.unwrap_or(20);
    if page < 1 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Page must be >= 1" })),
        );
    }
    if !(1..=100).contains(&page_size) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "PageSize must be between 1 and 100" })),
        );
    }
    if let (Some(s), Some(e)) = (&q.start_date, &q.end_date)
        && s > e
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Start date must be before end date." })),
        );
    }

    let db = state.db.lock().unwrap();
    let mut conds: Vec<String> = Vec::new();
    let mut params_vec: Vec<SqlValue> = Vec::new();
    if let Some(v) = q.provider.filter(|s| !s.is_empty()) {
        conds.push("provider = ?".to_string());
        params_vec.push(SqlValue::Text(v));
    }
    if let Some(v) = q.model.filter(|s| !s.is_empty()) {
        let escaped = v
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        conds.push("model LIKE ? ESCAPE '\\'".to_string());
        params_vec.push(SqlValue::Text(format!("%{escaped}%")));
    }
    if let Some(v) = q.request_id.filter(|s| !s.is_empty()) {
        conds.push("id = ?".to_string());
        params_vec.push(SqlValue::Text(v));
    }
    if let Some(v) = q.connection_id.filter(|s| !s.is_empty()) {
        conds.push("connectionId = ?".to_string());
        params_vec.push(SqlValue::Text(v));
    }
    if let Some(v) = q.status.filter(|s| !s.is_empty()) {
        conds.push("status = ?".to_string());
        params_vec.push(SqlValue::Text(v));
    }
    if let Some(v) = q.start_date.filter(|s| !s.is_empty()) {
        conds.push("timestamp >= ?".to_string());
        params_vec.push(SqlValue::Text(v));
    }
    if let Some(v) = q.end_date.filter(|s| !s.is_empty()) {
        conds.push("timestamp <= ?".to_string());
        params_vec.push(SqlValue::Text(v));
    }

    let where_clause = if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    };
    let p_refs: Vec<&dyn rusqlite::ToSql> = params_vec
        .iter()
        .map(|v| v as &dyn rusqlite::ToSql)
        .collect();
    let total_items: i64 = db
        .query_row(
            &format!("SELECT COUNT(*) FROM requestDetails {where_clause}"),
            p_refs.as_slice(),
            |r| r.get(0),
        )
        .unwrap_or(0);

    let total_pages = if total_items == 0 {
        0
    } else {
        (total_items + page_size - 1) / page_size
    };
    let offset = (page - 1) * page_size;
    let mut query_params = params_vec.clone();
    query_params.push(SqlValue::Integer(page_size));
    query_params.push(SqlValue::Integer(offset));
    let qp_refs: Vec<&dyn rusqlite::ToSql> = query_params
        .iter()
        .map(|v| v as &dyn rusqlite::ToSql)
        .collect();

    let mut details = Vec::new();
    if let Ok(mut stmt) = db.prepare(&format!(
        "SELECT data FROM requestDetails {where_clause} ORDER BY timestamp DESC LIMIT ? OFFSET ?"
    )) && let Ok(rows) = stmt.query_map(qp_refs.as_slice(), |r| r.get::<_, String>(0))
    {
        for ds in rows.flatten() {
            if let Ok(mut obj) = serde_json::from_str::<Value>(&ds) {
                if let Some(map) = obj.as_object_mut() {
                    for key in ["request", "providerRequest", "providerResponse", "response"] {
                        if map.contains_key(key) {
                            map.insert(key.to_string(), json!({ "redacted": true }));
                        }
                    }
                }
                details.push(obj);
            }
        }
    }
    let obs = is_observability_enabled(&db);
    (
        StatusCode::OK,
        Json(json!({
            "details": details,
            "pagination": {
                "page": page,
                "pageSize": page_size,
                "totalItems": total_items,
                "totalPages": total_pages,
                "hasNext": page < total_pages,
                "hasPrev": page > 1,
            },
            "observabilityEnabled": obs,
        })),
    )
}

// ---- /api/usage/stream ----

struct OnceStream(Option<Result<Event, Infallible>>);

impl Stream for OnceStream {
    type Item = Result<Event, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take())
    }
}

async fn usage_stream(State(state): State<AppState>) -> impl IntoResponse {
    let stats = {
        let db = state.db.lock().unwrap();
        build_usage_stats(&db, None, None)
    };
    let ev = Event::default().data(stats.to_string());
    Sse::new(OnceStream(Some(Ok(ev)))).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(25))
            .text("ping"),
    )
}

// ---- /api/usage/{connectionId} ----

fn load_conn(db: &Connection, id: &str) -> Option<(String, String, i64, Value)> {
    db.query_row(
        "SELECT provider, authType, COALESCE(isActive, 1), data FROM providerConnections WHERE id = ?1",
        [id],
        |r| {
            let ds: String = r.get(3)?;
            let data: Value = serde_json::from_str(&ds).unwrap_or(json!({}));
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, data))
        },
    )
    .optional()
    .ok()
    .flatten()
}

fn reset_client(data: &Value) -> Result<reqwest::Client, String> {
    let psd = &data["providerSpecificData"];
    if psd["proxyPoolId"]
        .as_str()
        .is_some_and(|s| !s.is_empty() && s != "__none__")
    {
        return Err("Reset actions do not support proxy pools".into());
    }
    crate::services::usage::client_for_proxy(psd)
}

fn is_usage_apikey_provider(p: &str) -> bool {
    matches!(
        p,
        "glm"
            | "glm-cn"
            | "glmt"
            | "minimax"
            | "minimax-cn"
            | "kimi"
            | "deepseek"
            | "kiro"
            | "synthetic"
            | "opencode-go"
            | "opencode-zen"
            | "groq"
            | "vercel-ai-gateway"
            | "commandcode"
            | "orcarouter"
            | "ollama"
            | "codebuddy-cn"
            | "codebuddy-intl"
    )
}

async fn usage_connection(
    State(state): State<AppState>,
    Path(connection_id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (conn, proxy) = {
        let db = state.db.lock().unwrap();
        let conn = load_conn(&db, &connection_id);
        let psd = conn
            .as_ref()
            .map(|c| &c.3["providerSpecificData"])
            .unwrap_or(&Value::Null);
        let pool_id = psd["proxyPoolId"].as_str().unwrap_or("");
        let pool = if pool_id.is_empty() || pool_id == "__none__" {
            Ok(None)
        } else {
            db.query_row(
                "SELECT data, isActive FROM proxyPools WHERE id = ?1",
                [pool_id],
                |r| {
                    let raw: String = r.get(0)?;
                    let active: i64 = r.get(1)?;
                    let mut value: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
                    value["isActive"] = json!(active != 0);
                    Ok(value)
                },
            )
            .optional()
            .map_err(|_| ())
        };
        let proxy = crate::services::connection_proxy::resolve_connection_proxy_config(
            psd,
            pool.as_ref().map(|p| p.as_ref()).map_err(|_| ()),
        );
        (conn, proxy)
    };
    let Some((provider, auth_type, is_active, data)) = conn else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response();
    };
    if is_active == 0 {
        return Json(json!({ "message": "Usage not available for this connection" }))
            .into_response();
    }
    let is_oauth = auth_type == "oauth";
    let is_apikey_eligible =
        matches!(auth_type.as_str(), "apikey" | "api_key") && is_usage_apikey_provider(&provider);
    let is_lease = auth_type == "mcode-lease"
        && matches!(provider.as_str(), "minimax-code" | "minimax-code-global");
    if !is_oauth && !is_apikey_eligible && !is_lease {
        return Json(json!({ "message": "Usage not available for this connection" }))
            .into_response();
    }

    let access_token = data
        .get("accessToken")
        .or_else(|| data.get("apiKey"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if access_token.is_empty() && !is_lease {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Credential refresh failed: access token missing" })),
        )
            .into_response();
    }
    let mut data = data;
    data["provider"] = json!(provider);
    data["authType"] = json!(auth_type);
    Json(
        crate::services::usage::get_usage_for_provider(
            &data,
            &proxy,
            params.get("force").is_some_and(|s| s == "1"),
        )
        .await,
    )
    .into_response()
}

async fn claude_reset(
    State(state): State<AppState>,
    Path(connection_id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let conn = {
        let db = state.db.lock().unwrap();
        load_conn(&db, &connection_id)
    };
    let Some((provider, auth_type, is_active, data)) = conn else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response();
    };
    if is_active == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Connection is turned off." })),
        )
            .into_response();
    }
    if provider != "claude" || auth_type != "oauth" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Limit reset is only available for Claude OAuth connections." })),
        )
            .into_response();
    }
    let client = match reset_client(&data) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error":e}))).into_response(),
    };
    let token = data["accessToken"].as_str().unwrap_or("");
    let grant = body["grantId"].as_str().unwrap_or("");
    match crate::services::usage::claude::consume_reset_grant(
        &client,
        "https://api.anthropic.com/api/oauth/profile",
        "https://api.anthropic.com/api/organizations/{org_id}/reset_rate_limits",
        token,
        grant,
    )
    .await
    {
        Ok(mut result) => {
            let status = if result["ok"] == true {
                StatusCode::OK
            } else {
                let code = result["status"].as_u64().unwrap_or(409);
                StatusCode::from_u16(if (400..500).contains(&code) {
                    code as u16
                } else {
                    409
                })
                .unwrap_or(StatusCode::CONFLICT)
            };
            if result["ok"] != true && result["message"].is_null() {
                result["message"] = json!(format!(
                    "Reset not applied: {}",
                    result["reason"]
                        .as_str()
                        .or_else(|| result["result"].as_str())
                        .unwrap_or("unknown")
                ));
            }
            (status, Json(result)).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error":e}))).into_response(),
    }
}

fn validate_codex_conn(db: &Connection, connection_id: &str) -> Result<Value, Response> {
    let Some((provider, auth_type, is_active, data)) = load_conn(db, connection_id) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response());
    };
    if is_active == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Connection is turned off." })),
        )
            .into_response());
    }
    if provider != "codex" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "error": "Codex reset credits are only available for Codex connections." }),
            ),
        )
            .into_response());
    }
    if auth_type != "oauth" && auth_type != "access_token" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Codex reset credits require an OAuth or access-token connection." })),
        )
            .into_response());
    }
    Ok(data)
}

async fn codex_reset_credits_get(
    State(state): State<AppState>,
    Path(connection_id): Path<String>,
) -> Response {
    let data = {
        let db = state.db.lock().unwrap();
        match validate_codex_conn(&db, &connection_id) {
            Ok(d) => d,
            Err(r) => return r,
        }
    };
    let token = data
        .get("accessToken")
        .and_then(Value::as_str)
        .unwrap_or("");
    if token.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Credential refresh failed: access token missing" })),
        )
            .into_response();
    }
    let client = match reset_client(&data) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error":e}))).into_response(),
    };
    match crate::services::usage::codex::get_reset_credits(
        &client,
        "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits",
        token,
        &data["providerSpecificData"],
    )
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error":e}))).into_response(),
    }
}

async fn codex_reset_credits_post(
    State(state): State<AppState>,
    Path(connection_id): Path<String>,
) -> Response {
    let data = {
        let db = state.db.lock().unwrap();
        match validate_codex_conn(&db, &connection_id) {
            Ok(d) => d,
            Err(r) => return r,
        }
    };
    let token = data
        .get("accessToken")
        .and_then(Value::as_str)
        .unwrap_or("");
    if token.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Credential refresh failed: access token missing" })),
        )
            .into_response();
    }
    let client = match reset_client(&data) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error":e}))).into_response(),
    };
    let id = match crate::services::usage::request_id() {
        Ok(id) => id,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":e}))).into_response();
        }
    };
    match crate::services::usage::codex::consume_reset_credit(&client,"https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume",token,&id).await {
        Ok(result) if result["ok"] == true => Json(json!({"code":result["code"],"reset":true,"windows_reset":result["windowsReset"],"redeemRequestId":id,"credit":result["raw"]["credit"]})).into_response(),
        Ok(result) if result["noCredit"] == true => (StatusCode::CONFLICT,Json(json!({"code":"no_credit","reset":false,"windows_reset":result["windowsReset"],"message":"No Codex reset credits available."}))).into_response(),
        Ok(result) => { let code=result["status"].as_u64().unwrap_or(502); let status=StatusCode::from_u16(if (400..500).contains(&code) {code as u16} else {502}).unwrap_or(StatusCode::BAD_GATEWAY); (status,Json(json!({"code":result["code"].as_str().unwrap_or("unknown_response"),"reset":false,"windows_reset":result["windowsReset"],"message":result["message"].as_str().unwrap_or("Codex reset credit consume returned an unexpected response.")}))).into_response() },
        Err(e) => (StatusCode::BAD_GATEWAY,Json(json!({"error":e}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::executor::HttpExecutor;
    use axum::{body::{Body, to_bytes}, http::Request};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    fn test_app() -> (Router, Arc<Mutex<Connection>>) {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let state = AppState {
            db: Arc::clone(&db),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        (router().with_state(state), db)
    }

    #[tokio::test]
    async fn usage_stats_and_chart_validate_period() {
        let (app, db) = test_app();
        {
            let conn = db.lock().unwrap();
            crate::db::usage_insert(
                &conn,
                &crate::db::UsageRow {
                    provider: "openai".into(),
                    model: "gpt-4o".into(),
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    status: 200,
                    request_id: "r1".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/usage/stats?period=bad")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let res = app
            .clone()
            .oneshot(
                Request::get("/api/usage/stats?period=all")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["totalRequests"], 1);

        let res = app
            .oneshot(
                Request::get("/api/usage/chart?period=7d")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let arr: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 7);
    }

    #[tokio::test]
    async fn usage_by_key_masks_secret_and_rejects_unknown_id() {
        let (app, db) = test_app();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO apiKeys(id,key,name,createdAt) VALUES('k1','sk-super-secret-value-1234','dev','now')",
                [],
            )
            .unwrap();
        }
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/usage/by-key?keyId=missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let res = app
            .oneshot(
                Request::get("/api/usage/by-key?keyId=k1&period=all")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), 65536).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("sk-super-secret-value-1234"));
    }

    #[tokio::test]
    async fn usage_request_details_redacts_payloads() {
        let (app, db) = test_app();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO requestDetails(id,timestamp,provider,model,status,data) VALUES('d1','2026-01-01T00:00:00Z','openai','gpt-4o','200',?1)",
                [r#"{"id":"d1","provider":"openai","latency":{"total":120,"ttft":40},"request":{"secret":"prompt"}}"#],
            )
            .unwrap();
        }
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/usage/request-details?page=1&pageSize=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["details"][0]["request"]["redacted"], true);

        let res = app
            .clone()
            .oneshot(
                Request::get("/api/usage/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .oneshot(
                Request::get("/api/usage/latency?period=all")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let lv: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(lv["samples"], 1);
    }

    #[tokio::test]
    async fn usage_logs_stream_and_connection_guards() {
        let (app, db) = test_app();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO providerConnections(id,provider,authType,isActive,data,createdAt,updatedAt) VALUES('c-off','codex','oauth',0,'{}','now','now')",
                [],
            )
            .unwrap();
        }
        let res = app
            .clone()
            .oneshot(Request::get("/api/usage/logs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(
                Request::get("/api/usage/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(
                Request::get("/api/usage/c-off")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["message"], "Usage not available for this connection");

        let res = app
            .clone()
            .oneshot(
                Request::post("/api/usage/c-off/claude-reset")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let res = app
            .oneshot(
                Request::get("/api/usage/c-off/codex-reset-credits")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn provider_usage_route_dispatches_real_handler() {
        let (app, db) = test_app();
        db.lock().unwrap().execute("INSERT INTO providerConnections(id,provider,authType,isActive,data,createdAt,updatedAt) VALUES('orca','orcarouter','apikey',1,?1,'now','now')", [r#"{"apiKey":"example"}"#]).unwrap();
        let response = app
            .oneshot(
                Request::get("/api/usage/orca?force=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(value["plan"], "OrcaRouter");
        assert!(
            value["message"]
                .as_str()
                .unwrap()
                .contains("does not expose")
        );
    }
}
