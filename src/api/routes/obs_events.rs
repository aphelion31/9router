use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/monitoring", get(monitoring))
        .route("/api/security-log", get(security_log))
        .route("/api/notifications", get(notifications_get).post(notifications_post))
}

fn no_store_json(status: StatusCode, body: Value) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (status, headers, Json(body))
}

fn now_iso(db: &Connection) -> String {
    db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get(0))
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_string())
}

// ---- /api/monitoring ----

static START_TIME: OnceLock<Instant> = OnceLock::new();
static MONITORING_CACHE: OnceLock<Mutex<(Instant, Value)>> = OnceLock::new();

fn app_uptime_sec() -> u64 {
    START_TIME.get_or_init(Instant::now).elapsed().as_secs()
}

fn get_disk() -> Value {
    let Ok(out) = std::process::Command::new("df").args(["-k", "/"]).output() else {
        return Value::Null;
    };
    if !out.status.success() {
        return Value::Null;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(last) = text.trim().lines().last() else {
        return Value::Null;
    };
    let parts: Vec<&str> = last.split_whitespace().collect();
    if parts.len() < 4 {
        return Value::Null;
    }
    let (Ok(total), Ok(used), Ok(avail)) = (
        parts[1].parse::<u64>(),
        parts[2].parse::<u64>(),
        parts[3].parse::<u64>(),
    ) else {
        return Value::Null;
    };
    if total == 0 {
        return Value::Null;
    }
    json!({
        "totalBytes": total * 1024,
        "usedBytes": used * 1024,
        "availBytes": avail * 1024,
    })
}

fn mask_key(raw: &str) -> String {
    if raw.len() <= 8 {
        return "****".to_string();
    }
    format!("{}...{}", &raw[..4], &raw[raw.len() - 4..])
}

pub(crate) fn percentile(sorted_asc: &[f64], p: f64) -> Option<f64> {
    if sorted_asc.is_empty() {
        return None;
    }
    let idx = ((p / 100.0) * sorted_asc.len() as f64).ceil() as usize;
    let idx = idx.saturating_sub(1).min(sorted_asc.len() - 1);
    Some(sorted_asc[idx])
}

async fn monitoring(State(state): State<AppState>) -> impl IntoResponse {
    let cache = MONITORING_CACHE.get_or_init(|| {
        Mutex::new((Instant::now() - Duration::from_secs(10), Value::Null))
    });
    if let Ok(guard) = cache.lock()
        && !guard.1.is_null()
        && guard.0.elapsed() < Duration::from_secs(5)
    {
        return no_store_json(StatusCode::OK, guard.1.clone());
    }

    let db = state.db.lock().unwrap();
    let db_ok = db.query_row("SELECT 1", [], |_| Ok(())).is_ok();

    let since_7d: String = db
        .query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')",
            [],
            |r| r.get(0),
        )
        .unwrap_or_default();

    // providerConnections
    let mut total_conns: i64 = 0;
    let mut active_conns: i64 = 0;
    let mut conns_by_provider: HashMap<String, (i64, i64)> = HashMap::new();
    if let Ok(mut stmt) = db.prepare("SELECT provider, isActive FROM providerConnections")
        && let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?.unwrap_or(1),
            ))
        })
    {
        for row in rows.flatten() {
            total_conns += 1;
            let is_act = row.1 != 0;
            if is_act {
                active_conns += 1;
            }
            let entry = conns_by_provider.entry(row.0).or_insert((0, 0));
            entry.0 += 1;
            if is_act {
                entry.1 += 1;
            }
        }
    }

    // proxyPools
    let mut pools_total: i64 = 0;
    let mut pools_active: i64 = 0;
    let mut total_proxies: usize = 0;
    if let Ok(mut stmt) = db.prepare("SELECT isActive, data FROM proxyPools")
        && let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                r.get::<_, String>(1)?,
            ))
        })
    {
        for (is_act, data_str) in rows.flatten() {
            pools_total += 1;
            if is_act == 1 {
                pools_active += 1;
            }
            if let Ok(v) = serde_json::from_str::<Value>(&data_str)
                && let Some(url) = v.get("proxyUrl").and_then(Value::as_str)
            {
                total_proxies += url
                    .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                    .filter(|s| !s.is_empty())
                    .count();
            }
        }
    }

    let combos_total: i64 = db
        .query_row("SELECT COUNT(*) FROM combos", [], |r| r.get(0))
        .unwrap_or(0);
    let keys_total: i64 = db
        .query_row("SELECT COUNT(*) FROM apiKeys", [], |r| r.get(0))
        .unwrap_or(0);

    let mut provider_names: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) = db.prepare("SELECT id, name FROM providerNodes WHERE name IS NOT NULL")
        && let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    {
        for (id, name) in rows.flatten() {
            provider_names.insert(id, name);
        }
    }
    let resolve_name = |id: &str| -> String {
        provider_names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    };

    // fastest provider from requestDetails
    let mut lat_by_provider: HashMap<String, Vec<f64>> = HashMap::new();
    if let Ok(mut stmt) = db.prepare(
        "SELECT data FROM requestDetails WHERE timestamp >= ?1 ORDER BY timestamp DESC LIMIT 5000",
    ) && let Ok(rows) = stmt.query_map([&since_7d], |r| r.get::<_, String>(0))
    {
        for data_str in rows.flatten() {
            if let Ok(d) = serde_json::from_str::<Value>(&data_str)
                && let Some(total) = d.pointer("/latency/total").and_then(Value::as_f64)
                && total > 0.0
            {
                let p = d
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                lat_by_provider.entry(p).or_default().push(total);
            }
        }
    }
    let mut lat_entries: Vec<(String, usize, f64)> = lat_by_provider
        .into_iter()
        .filter_map(|(id, mut vals)| {
            if vals.is_empty() {
                return None;
            }
            vals.sort_by(|a, b| a.total_cmp(b));
            let p50 = percentile(&vals, 50.0)?;
            Some((id, vals.len(), p50))
        })
        .collect();
    let has_reliable = lat_entries.iter().any(|e| e.1 >= 5);
    if has_reliable {
        lat_entries.retain(|e| e.1 >= 5);
    }
    lat_entries.sort_by(|a, b| a.2.total_cmp(&b.2));
    let fastest = lat_entries.first().map(|(id, samples, p50)| {
        json!({
            "id": id,
            "name": resolve_name(id),
            "p50Ms": p50.round() as i64,
            "samples": samples,
        })
    });

    // mostUsed & mostUsedKey from usageHistory
    let total_requests_7d: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM usageHistory WHERE timestamp >= ?1",
            [&since_7d],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let most_used = db
        .query_row(
            "SELECT provider, COUNT(*) as c, COALESCE(SUM(promptTokens),0)+COALESCE(SUM(completionTokens),0)
             FROM usageHistory WHERE timestamp >= ?1 AND provider IS NOT NULL
             GROUP BY provider ORDER BY c DESC LIMIT 1",
            [&since_7d],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        )
        .optional()
        .ok()
        .flatten()
        .map(|(id, reqs, toks)| {
            let share = if total_requests_7d > 0 {
                Some(((reqs as f64 / total_requests_7d as f64) * 100.0).round() as i64)
            } else {
                None
            };
            json!({
                "id": id,
                "name": resolve_name(&id),
                "requests": reqs,
                "tokens": toks,
                "sharePct": share,
            })
        });

    let most_used_key = db
        .query_row(
            "SELECT u.apiKey, ak.name, MAX(u.provider), COUNT(*) as c, COALESCE(SUM(u.promptTokens),0)+COALESCE(SUM(u.completionTokens),0)
             FROM usageHistory u
             LEFT JOIN apiKeys ak ON ak.key = u.apiKey
             WHERE u.timestamp >= ?1 AND u.apiKey IS NOT NULL AND u.apiKey != '' AND u.apiKey != 'local-no-key'
             GROUP BY u.apiKey ORDER BY c DESC LIMIT 1",
            [&since_7d],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
        .map(|(raw_key, key_name, provider, reqs, toks)| {
            let name = key_name.unwrap_or_else(|| mask_key(&raw_key));
            let share = if total_requests_7d > 0 {
                Some(((reqs as f64 / total_requests_7d as f64) * 100.0).round() as i64)
            } else {
                None
            };
            json!({
                "name": name,
                "provider": provider,
                "requests": reqs,
                "tokens": toks,
                "sharePct": share,
            })
        });

    let mut ranked_keys: Vec<(String, i64, i64)> = conns_by_provider
        .into_iter()
        .map(|(id, (tot, act))| (id, tot, act))
        .collect();
    ranked_keys.sort_by(|a, b| b.1.cmp(&a.1));
    let most_keys = ranked_keys.first().filter(|e| e.1 > 0).map(|(id, tot, act)| {
        let share = if total_conns > 0 {
            Some(((*tot as f64 / total_conns as f64) * 100.0).round() as i64)
        } else {
            None
        };
        json!({
            "id": id,
            "name": resolve_name(id),
            "total": tot,
            "active": act,
            "sharePct": share,
        })
    });

    let cpu_cores = std::thread::available_parallelism().map(|n| n.get()).ok();
    let hostname = std::env::var("HOSTNAME").ok();
    let system = json!({
        "cpuModel": Value::Null,
        "cpuCores": cpu_cores,
        "load": [0.0, 0.0, 0.0],
        "memTotalBytes": Value::Null,
        "memFreeBytes": Value::Null,
        "disk": get_disk(),
        "uptimeSec": Value::Null,
        "appUptimeSec": app_uptime_sec(),
        "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "hostname": hostname,
    });

    let data = json!({
        "system": system,
        "providers": { "total": total_conns, "active": active_conns },
        "proxies": { "pools": pools_total, "activePools": pools_active, "totalProxies": total_proxies },
        "combos": { "total": combos_total },
        "keys": { "total": keys_total },
        "fastest": fastest,
        "mostUsed": most_used,
        "mostUsedKey": most_used_key,
        "mostKeys": most_keys,
        "providerNames": provider_names,
        "health": { "db": if db_ok { "ok" } else { "error" }, "breakersOpen": crate::services::provider_breaker::global().lock().map(|mut b| b.snapshot(crate::routing::classify::now_ms()).as_object().map(|m| m.values().filter(|v| v["state"] == "open").count()).unwrap_or(0)).unwrap_or(0) },
    });

    if let Ok(mut guard) = cache.lock() {
        *guard = (Instant::now(), data.clone());
    }
    no_store_json(StatusCode::OK, data)
}

// ---- /api/security-log ----

async fn security_log(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let Ok(mut stmt) = db.prepare(
        "SELECT id, event, ip, userAgent, detail, breach, createdAt FROM securityLog ORDER BY createdAt DESC, id DESC LIMIT 100",
    ) else {
        return no_store_json(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "failed"}));
    };
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "event": r.get::<_, String>(1)?,
                "ip": r.get::<_, Option<String>>(2)?,
                "userAgent": r.get::<_, Option<String>>(3)?,
                "detail": r.get::<_, Option<String>>(4)?,
                "breach": r.get::<_, i64>(5)? == 1,
                "createdAt": r.get::<_, String>(6)?,
            }))
        })
        .map(|it| it.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    no_store_json(StatusCode::OK, json!({ "events": rows }))
}

// ---- /api/notifications ----

#[derive(Deserialize)]
struct NotificationsQuery {
    unread: Option<String>,
}

async fn notifications_get(
    State(state): State<AppState>,
    Query(q): Query<NotificationsQuery>,
) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let unread_only = q.unread.as_deref() == Some("1");
    let sql = if unread_only {
        "SELECT id, kind, level, subject, title, detail, createdAt, read FROM trustEvents WHERE read = 0 ORDER BY createdAt DESC, id DESC LIMIT 100"
    } else {
        "SELECT id, kind, level, subject, title, detail, createdAt, read FROM trustEvents ORDER BY createdAt DESC, id DESC LIMIT 100"
    };
    let events = db
        .prepare(sql)
        .and_then(|mut stmt| {
            let mapped = stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "level": r.get::<_, String>(2)?,
                    "subject": r.get::<_, Option<String>>(3)?,
                    "title": r.get::<_, String>(4)?,
                    "detail": r.get::<_, Option<String>>(5)?,
                    "createdAt": r.get::<_, String>(6)?,
                    "read": r.get::<_, i64>(7)? == 1,
                }))
            })?;
            Ok(mapped.flatten().collect::<Vec<_>>())
        })
        .unwrap_or_default();
    let unread: i64 = db
        .query_row("SELECT COUNT(*) FROM trustEvents WHERE read = 0", [], |r| r.get(0))
        .unwrap_or(0);
    no_store_json(StatusCode::OK, json!({ "events": events, "unread": unread }))
}

async fn notifications_post(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let body: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let action = body.get("action").and_then(Value::as_str).unwrap_or("");
    let db = state.db.lock().unwrap();

    match action {
        "read" => {
            let Some(id) = body.get("id").and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
            }) else {
                return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Unknown action" })));
            };
            let changes = db
                .execute("UPDATE trustEvents SET read = 1 WHERE id = ?1", [id])
                .unwrap_or(0);
            if changes == 0 {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "Notification not found" })),
                );
            }
            (StatusCode::OK, Json(json!({ "ok": true })))
        }
        "read-all" => {
            let cleared = db
                .execute("UPDATE trustEvents SET read = 1 WHERE read = 0", [])
                .unwrap_or(0);
            (StatusCode::OK, Json(json!({ "ok": true, "cleared": cleared })))
        }
        "clear-subject" => {
            let kind = body.get("kind").and_then(Value::as_str).unwrap_or("");
            let subject = body.get("subject").and_then(Value::as_str).unwrap_or("");
            if kind.is_empty() || subject.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Unknown action" })));
            }
            let cleared = db
                .execute(
                    "UPDATE trustEvents SET read = 1 WHERE kind = ?1 AND COALESCE(subject, '') = ?2 AND read = 0",
                    params![kind, subject],
                )
                .unwrap_or(0);
            (StatusCode::OK, Json(json!({ "ok": true, "cleared": cleared })))
        }
        "event" => {
            let kind = body.get("kind").and_then(Value::as_str).unwrap_or("");
            let title = body.get("title").and_then(Value::as_str).unwrap_or("");
            if !matches!(kind, "quota" | "failed-request" | "security" | "job") || title.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Unknown action" })));
            }
            let level = body.get("level").and_then(Value::as_str).unwrap_or("info");
            let subject = body.get("subject").and_then(Value::as_str);
            let title_trunc: String = title.chars().take(200).collect();
            let detail_trunc: Option<String> = body
                .get("detail")
                .and_then(Value::as_str)
                .map(|s| s.chars().take(500).collect());

            let existing: Option<i64> = db
                .query_row(
                    "SELECT id FROM trustEvents WHERE kind = ?1 AND COALESCE(subject, '') = ?2 AND level = ?3 AND read = 0 ORDER BY id DESC LIMIT 1",
                    params![kind, subject.unwrap_or(""), level],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten();
            if let Some(id) = existing {
                return (StatusCode::OK, Json(json!({ "ok": true, "id": id })));
            }
            let now = now_iso(&db);
            let ok = db
                .execute(
                    "INSERT INTO trustEvents(kind, level, subject, title, detail, createdAt, read) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 0)",
                    params![kind, level, subject, title_trunc, detail_trunc, now],
                )
                .is_ok();
            let id = if ok { Some(db.last_insert_rowid()) } else { None };
            (StatusCode::OK, Json(json!({ "ok": id.is_some(), "id": id })))
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": "Unknown action" }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::executor::HttpExecutor;
    use axum::{body::to_bytes, http::Request};
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
    async fn monitoring_returns_counts_and_health() {
        let (app, db) = test_app();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO providerConnections(id,provider,authType,isActive,data,createdAt,updatedAt) VALUES('c1','codex','oauth',1,'{}','now','now')",
                [],
            ).unwrap();
        }
        let res = app
            .oneshot(Request::get("/api/monitoring").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["health"]["db"], "ok");
        assert!(v["providers"]["total"].as_i64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn security_log_lists_events() {
        let (app, db) = test_app();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO securityLog(event,ip,userAgent,detail,breach,createdAt) VALUES('login-failed','127.0.0.1','curl','bad pw',1,'2026-01-01T00:00:00Z')",
                [],
            ).unwrap();
        }
        let res = app
            .oneshot(Request::get("/api/security-log").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["events"][0]["event"], "login-failed");
        assert_eq!(v["events"][0]["breach"], true);
    }

    #[tokio::test]
    async fn notifications_create_dedup_and_read() {
        let (app, _) = test_app();
        let req = Request::post("/api/notifications")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                r#"{"action":"event","kind":"quota","level":"warn","subject":"openai","title":"90% used"}"#,
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["ok"], true);
        let id = v["id"].as_i64().unwrap();

        let res = app
            .clone()
            .oneshot(Request::get("/api/notifications?unread=1").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let list: Value = serde_json::from_slice(&to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(list["unread"], 1);

        let res = app
            .clone()
            .oneshot(
                Request::post("/api/notifications")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(format!(r#"{{"action":"read","id":{id}}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .oneshot(
                Request::post("/api/notifications")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"action":"read","id":99999}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
