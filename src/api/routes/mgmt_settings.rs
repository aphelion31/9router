use crate::dashboard;
use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::URL_SAFE_NO_PAD,
};
use ring::rand::SecureRandom;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

const WEBHOOK_EVENT_TYPES: &[&str] = &["usage", "quota", "error"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/settings", get(get_settings).patch(patch_settings))
        .route("/api/settings/require-login", get(require_login))
        .route(
            "/api/settings/backup",
            get(backup_status).post(backup_create),
        )
        .route(
            "/api/settings/backup/{id}",
            get(backup_download).post(backup_restore),
        )
        .route("/api/settings/database", get(db_export).post(db_import))
        .route(
            "/api/settings/encryption",
            get(encryption_status).post(encryption_migrate),
        )
        .route(
            "/api/settings/maintenance",
            get(maintenance_get).post(maintenance_run),
        )
        .route(
            "/api/settings/notifications",
            get(notifications_get).post(notifications_post),
        )
        .route("/api/settings/notifications/test", post(notifications_test))
        .route("/api/settings/proxy-test", post(proxy_test))
        .route(
            "/api/settings/webhook-events",
            get(webhook_events_get).post(webhook_events_post),
        )
}

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn safe_settings_map(conn: &Connection) -> Value {
    let mut map = crate::db::settings_get(conn)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let has_pw = map
        .get("password")
        .and_then(Value::as_str)
        .is_some_and(|p| !p.is_empty())
        || crate::db::kv_get(conn, "settings", "password")
            .ok()
            .flatten()
            .is_some_and(|p| !p.is_empty());
    map.remove("password");
    map.entry("requireLogin".to_string())
        .or_insert(Value::Bool(true));
    map.insert("hasPassword".into(), Value::Bool(has_pw));
    Value::Object(map)
}

async fn get_settings(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    Json(safe_settings_map(&db)).into_response()
}

async fn patch_settings(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(patch) = body.as_object() else {
        return err(StatusCode::BAD_REQUEST, "Invalid settings object");
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let mut map = crate::db::settings_get(&db)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let has_new_pw = patch.contains_key("newPassword");
    for (k, v) in patch {
        if k == "newPassword" {
            let Some(pw) = v.as_str().filter(|p| !p.is_empty()) else {
                return err(StatusCode::BAD_REQUEST, "Password cannot be empty");
            };
            if let Some(cur) = patch.get("currentPassword").and_then(Value::as_str) {
                if !dashboard::verify_password(&db, cur) {
                    return err(StatusCode::UNAUTHORIZED, "Current password is incorrect");
                }
            }
            let Ok(hash) = bcrypt::hash(pw, 10) else {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to hash password",
                );
            };
            let _ = crate::db::kv_set(&db, "settings", "password", &hash);
            map.insert("password".into(), Value::String(hash));
            continue;
        }
        if k == "currentPassword" || k == "hasPassword" || (k == "password" && has_new_pw) {
            continue;
        }
        if k == "password" {
            if v.is_null() {
                let _ = db.execute("DELETE FROM kv WHERE scope = 'settings' AND key = 'password'", []);
                map.remove("password");
            } else if let Some(pw) = v.as_str().filter(|p| !p.is_empty()) {
                let Ok(hash) = bcrypt::hash(pw, 10) else {
                    return err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to hash password",
                    );
                };
                let _ = crate::db::kv_set(&db, "settings", "password", &hash);
                map.insert("password".into(), Value::String(hash));
            }
            continue;
        }
        map.insert(k.clone(), v.clone());
    }
    if crate::db::settings_put(&db, &Value::Object(map)).is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update settings");
    }
    Json(safe_settings_map(&db)).into_response()
}

async fn require_login(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return Json(json!({ "requireLogin": true })).into_response();
    };
    let req = safe_settings_map(&db)["requireLogin"]
        .as_bool()
        .unwrap_or(true);
    Json(json!({ "requireLogin": req })).into_response()
}

fn export_db_payload(
    conn: &Connection,
    sections: Option<&[String]>,
    providers: Option<&[String]>,
) -> Value {
    let want = |name: &str| {
        sections
            .map(|s| s.iter().any(|x| x == name))
            .unwrap_or(true)
    };
    let mut out = Map::new();

    if want("providerConnections") {
        let mut arr = Vec::new();
        if let Ok(mut st) =
            conn.prepare("SELECT id, provider, name, data FROM providerConnections")
        {
            if let Ok(rows) = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            }) {
                for (id, provider, name, raw) in rows.flatten() {
                    if let Some(p_filter) = providers {
                        if !p_filter.iter().any(|p| p == &provider) {
                            continue;
                        }
                    }
                    let mut obj = serde_json::from_str::<Map<String, Value>>(&raw)
                        .unwrap_or_default();
                    obj.entry("id".to_string()).or_insert(json!(id));
                    obj.entry("provider".to_string()).or_insert(json!(provider));
                    if let Some(n) = name {
                        obj.entry("name".to_string()).or_insert(json!(n));
                    }
                    arr.push(Value::Object(obj));
                }
            }
        }
        out.insert("providerConnections".into(), Value::Array(arr));
    }

    if want("providerNodes") {
        let mut arr = Vec::new();
        if let Ok(mut st) = conn.prepare("SELECT id, type, name, data FROM providerNodes") {
            if let Ok(rows) = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            }) {
                for (id, typ, name, raw) in rows.flatten() {
                    let mut obj = serde_json::from_str::<Map<String, Value>>(&raw)
                        .unwrap_or_default();
                    obj.entry("id".to_string()).or_insert(json!(id));
                    obj.entry("type".to_string()).or_insert(json!(typ));
                    if let Some(n) = name {
                        obj.entry("name".to_string()).or_insert(json!(n));
                    }
                    arr.push(Value::Object(obj));
                }
            }
        }
        out.insert("providerNodes".into(), Value::Array(arr));
    }

    if want("combos") {
        let mut arr = Vec::new();
        if let Ok(mut st) = conn.prepare("SELECT id, name, data FROM combos") {
            if let Ok(rows) = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            }) {
                for (id, name, raw) in rows.flatten() {
                    let mut obj = serde_json::from_str::<Map<String, Value>>(&raw)
                        .unwrap_or_default();
                    obj.entry("id".to_string()).or_insert(json!(id));
                    obj.entry("name".to_string()).or_insert(json!(name));
                    arr.push(Value::Object(obj));
                }
            }
        }
        out.insert("combos".into(), Value::Array(arr));
    }

    if want("apiKeys") {
        let mut arr = Vec::new();
        if let Ok(mut st) = conn.prepare(
            "SELECT id, key, name, machineId, isActive, createdAt, expiresAt, accessRestricted, accessAllow FROM apiKeys",
        ) {
            if let Ok(rows) = st.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "key": r.get::<_, String>(1)?,
                    "name": r.get::<_, Option<String>>(2)?,
                    "machineId": r.get::<_, Option<String>>(3)?,
                    "isActive": r.get::<_, Option<i64>>(4)?.unwrap_or(1) != 0,
                    "createdAt": r.get::<_, Option<String>>(5)?,
                    "expiresAt": r.get::<_, Option<String>>(6)?,
                    "accessRestricted": r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                    "accessAllow": r.get::<_, Option<String>>(8)?,
                }))
            }) {
                arr.extend(rows.flatten());
            }
        }
        out.insert("apiKeys".into(), Value::Array(arr));
    }

    if want("settings") {
        out.insert("settings".into(), safe_settings_map(conn));
    }

    Value::Object(out)
}

fn summarize_backup(payload: &Value) -> Value {
    json!({
        "providerConnections": payload["providerConnections"].as_array().map(|a| a.len()).unwrap_or(0),
        "providerNodes": payload["providerNodes"].as_array().map(|a| a.len()).unwrap_or(0),
        "combos": payload["combos"].as_array().map(|a| a.len()).unwrap_or(0),
        "apiKeys": payload["apiKeys"].as_array().map(|a| a.len()).unwrap_or(0),
        "hasSettings": payload.get("settings").is_some_and(Value::is_object),
    })
}

fn import_db_payload(
    conn: &Connection,
    payload: &Value,
    sections: Option<&[String]>,
) -> Result<(), String> {
    let want = |name: &str| {
        sections
            .map(|s| s.iter().any(|x| x == name))
            .unwrap_or(true)
    };
    if want("providerConnections")
        && let Some(arr) = payload["providerConnections"].as_array()
    {
        for item in arr {
            let Some(id) = item["id"].as_str() else {
                continue;
            };
            let provider = item["provider"].as_str().unwrap_or("");
            let name = item["name"].as_str();
            let _ = conn.execute(
                "INSERT INTO providerConnections(id, provider, name, data) VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET provider = excluded.provider, name = excluded.name, data = excluded.data",
                params![id, provider, name, item.to_string()],
            );
        }
    }
    if want("providerNodes")
        && let Some(arr) = payload["providerNodes"].as_array()
    {
        for item in arr {
            let Some(id) = item["id"].as_str() else {
                continue;
            };
            let typ = item["type"].as_str().unwrap_or("");
            let name = item["name"].as_str();
            let _ = conn.execute(
                "INSERT INTO providerNodes(id, type, name, data) VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET type = excluded.type, name = excluded.name, data = excluded.data",
                params![id, typ, name, item.to_string()],
            );
        }
    }
    if want("combos")
        && let Some(arr) = payload["combos"].as_array()
    {
        for item in arr {
            let Some(id) = item["id"].as_str() else {
                continue;
            };
            let name = item["name"].as_str().unwrap_or("");
            let _ = conn.execute(
                "INSERT INTO combos(id, name, data) VALUES(?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, data = excluded.data",
                params![id, name, item.to_string()],
            );
        }
    }
    if want("apiKeys")
        && let Some(arr) = payload["apiKeys"].as_array()
    {
        for item in arr {
            let (Some(id), Some(key)) = (item["id"].as_str(), item["key"].as_str()) else {
                continue;
            };
            let name = item["name"].as_str();
            let machine_id = item["machineId"].as_str();
            let active = item["isActive"].as_bool().unwrap_or(true) as i64;
            let _ = conn.execute(
                "INSERT INTO apiKeys(id, key, name, machineId, isActive, createdAt)
                 VALUES(?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 ON CONFLICT(id) DO UPDATE SET key = excluded.key, name = excluded.name, isActive = excluded.isActive",
                params![id, key, name, machine_id, active],
            );
        }
    }
    if want("settings")
        && let Some(obj) = payload["settings"].as_object()
    {
        let mut cur = crate::db::kv_get(conn, "settings", "data")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Map<String, Value>>(&s).ok())
            .unwrap_or_default();
        for (k, v) in obj {
            if k != "hasPassword" {
                cur.insert(k.clone(), v.clone());
            }
        }
        crate::db::kv_set(conn, "settings", "data", &Value::Object(cur).to_string())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn check_db_password(conn: &Connection, password: Option<&str>) -> bool {
    password.is_some_and(|pw| dashboard::verify_password(conn, pw))
}

async fn db_export(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let pw = headers.get("x-9r-password").and_then(|v| v.to_str().ok());
    if !check_db_password(&db, pw) {
        return err(StatusCode::UNAUTHORIZED, "Invalid password");
    }
    let sections: Option<Vec<String>> = q
        .get("sections")
        .map(|s| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect())
        .filter(|v: &Vec<String>| !v.is_empty());
    let providers: Option<Vec<String>> = q
        .get("providers")
        .map(|s| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect())
        .filter(|v: &Vec<String>| !v.is_empty());
    let payload = export_db_payload(&db, sections.as_deref(), providers.as_deref());
    Json(payload).into_response()
}

async fn db_import(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let pw = body["password"].as_str();
    if !check_db_password(&db, pw) {
        return err(StatusCode::UNAUTHORIZED, "Invalid password");
    }
    if body["preview"] == true {
        return Json(json!({ "summary": summarize_backup(&body) })).into_response();
    }
    let sections: Option<Vec<String>> = body["applySections"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
    if let Err(e) = import_db_payload(&db, &body, sections.as_deref()) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &e);
    }
    Json(json!({ "success": true })).into_response()
}

async fn backup_status(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let payload = export_db_payload(&db, None, None);
    let settings = safe_settings_map(&db);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(arr) = payload["providerConnections"].as_array() {
        for c in arr {
            if let Some(p) = c["provider"].as_str() {
                *counts.entry(p.to_string()).or_insert(0) += 1;
            }
        }
    }
    let providers: Vec<Value> = counts
        .into_iter()
        .map(|(id, connections)| json!({ "id": id, "name": Value::Null, "connections": connections }))
        .collect();

    let mut snapshots = Vec::new();
    if let Ok(mut st) = db.prepare("SELECT key, value FROM kv WHERE scope = 'backupSnapshots' ORDER BY key DESC") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
            for (id, raw) in rows.flatten() {
                let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                snapshots.push(json!({
                    "id": id,
                    "createdAt": parsed["createdAt"],
                    "reason": parsed["reason"].as_str().unwrap_or("manual"),
                    "summary": summarize_backup(&parsed["payload"]),
                }));
            }
        }
    }

    Json(json!({
        "summary": summarize_backup(&payload),
        "providers": providers,
        "lastBackupAt": settings["lastBackupAt"],
        "autoBackupEnabled": settings["autoBackupEnabled"].as_bool().unwrap_or(true),
        "autoBackupKeep": settings["autoBackupKeep"].as_i64().unwrap_or(10),
        "snapshots": snapshots,
    }))
    .into_response()
}

async fn backup_create(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let payload = export_db_payload(&db, None, None);
    let mut rnd = [0u8; 6];
    let _ = ring::rand::SystemRandom::new().fill(&mut rnd);
    let id = format!("snap-{}", URL_SAFE_NO_PAD.encode(rnd));
    let created_at = db
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap_or_else(|_| "2026-01-01T00:00:00.000Z".into());
    let snap = json!({
        "id": id,
        "createdAt": created_at,
        "reason": "manual",
        "payload": payload,
    });
    if crate::db::kv_set(&db, "backupSnapshots", &id, &snap.to_string()).is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save snapshot");
    }
    Json(json!({
        "success": true,
        "snapshot": {
            "id": id,
            "createdAt": created_at,
            "reason": "manual",
            "summary": summarize_backup(&snap["payload"]),
        }
    }))
    .into_response()
}

async fn backup_download(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let pw = headers.get("x-9r-password").and_then(|v| v.to_str().ok());
    if !check_db_password(&db, pw) {
        return err(StatusCode::UNAUTHORIZED, "Invalid password");
    }
    let Some(raw) = crate::db::kv_get(&db, "backupSnapshots", &id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "Snapshot not found");
    };
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    Json(parsed["payload"].clone()).into_response()
}

async fn backup_restore(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    if !check_db_password(&db, body["password"].as_str()) {
        return err(StatusCode::UNAUTHORIZED, "Invalid password");
    }
    let Some(raw) = crate::db::kv_get(&db, "backupSnapshots", &id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "Snapshot not found");
    };
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let sections: Option<Vec<String>> = body["applySections"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
    if let Err(e) = import_db_payload(&db, &parsed["payload"], sections.as_deref()) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &e);
    }
    Json(json!({ "success": true })).into_response()
}

fn count_row_encryption(data: &Value) -> (usize, usize) {
    let mut plain = 0;
    let mut enc = 0;
    let spec = &data["providerSpecificData"];
    for f in crate::crypto::SENSITIVE_FIELDS {
        for s in [&data[*f], &spec[*f]].into_iter().filter_map(Value::as_str).filter(|s| !s.is_empty()) {
            if crate::crypto::is_envelope(s) {
                enc += 1;
            } else {
                plain += 1;
            }
        }
    }
    (plain, enc)
}

async fn encryption_status(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let mut connections = 0usize;
    let mut plaintext = 0usize;
    let mut encrypted = 0usize;
    let mut rows_with_plaintext = 0usize;
    if let Ok(mut st) = db.prepare("SELECT data FROM providerConnections") {
        if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) {
            for raw in rows.flatten() {
                connections += 1;
                let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                let (p, e) = count_row_encryption(&v);
                plaintext += p;
                encrypted += e;
                if p > 0 {
                    rows_with_plaintext += 1;
                }
            }
        }
    }
    Json(json!({
        "keyConfigured": crate::crypto::key().is_some(),
        "envVar": "PRO_CREDENTIAL_KEY",
        "connections": connections,
        "plaintextFields": plaintext,
        "encryptedFields": encrypted,
        "rowsWithPlaintext": rows_with_plaintext,
        "note": "Set PRO_CREDENTIAL_KEY (32 bytes, hex or base64) before migrating; keep a copy of the key — encrypted rows need it to decrypt.",
    }))
    .into_response()
}

async fn encryption_migrate(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    if !check_db_password(&db, body["password"].as_str()) {
        return err(StatusCode::UNAUTHORIZED, "Dashboard password required");
    }
    let action = body["action"].as_str().unwrap_or("");
    if action != "dry-run" && action != "migrate" {
        return err(StatusCode::BAD_REQUEST, "Unknown action");
    }
    let dry_run = action == "dry-run";
    if !dry_run && crate::crypto::key().is_none() {
        return err(
            StatusCode::BAD_REQUEST,
            "PRO_CREDENTIAL_KEY is not set — refusing to migrate without a key",
        );
    }

    let mut migrated_rows = 0usize;
    let mut migrated_fields = 0usize;
    let mut rows_data = Vec::new();
    if let Ok(mut st) = db.prepare("SELECT id, data FROM providerConnections") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
            rows_data.extend(rows.flatten());
        }
    }
    for (id, raw) in rows_data {
        let mut data: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
        let (plain, _) = count_row_encryption(&data);
        if plain == 0 {
            continue;
        }
        migrated_rows += 1;
        migrated_fields += plain;
        if !dry_run && crate::crypto::encrypt_fields(&mut data) > 0 {
            let _ = db.execute(
                "UPDATE providerConnections SET data = ?1, updatedAt = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
                params![data.to_string(), id],
            );
        }
    }
    Json(json!({
        "dryRun": dry_run,
        "migratedRows": migrated_rows,
        "migratedFields": migrated_fields,
    }))
    .into_response()
}

async fn maintenance_get(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let settings = safe_settings_map(&db);
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(json!({ "config": settings.get("dbMaintenance").cloned().unwrap_or_else(|| json!({})) })),
    )
        .into_response()
}

async fn maintenance_run(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let start = Instant::now();
    let _ = db.execute_batch("PRAGMA optimize; VACUUM;");
    Json(json!({
        "result": {
            "ok": true,
            "durationMs": start.elapsed().as_millis() as u64,
        }
    }))
    .into_response()
}

fn read_notify_config(conn: &Connection) -> Value {
    crate::db::kv_get(conn, "notify", "config")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| {
            json!({
                "telegram": { "enabled": false, "botToken": "", "chatId": "" },
                "webhook": { "enabled": false, "url": "", "secret": "" },
                "webhookEvents": { "usage": false, "quota": false, "error": false, "maskCredentials": true }
            })
        })
}

fn normalize_webhook_events(raw: &Value) -> Value {
    json!({
        "usage": raw["usage"].as_bool().unwrap_or(false),
        "quota": raw["quota"].as_bool().unwrap_or(false),
        "error": raw["error"].as_bool().unwrap_or(false),
        "maskCredentials": raw["maskCredentials"].as_bool().unwrap_or(true),
    })
}

fn notify_status(cfg: &Value) -> Value {
    let tg = &cfg["telegram"];
    let wh = &cfg["webhook"];
    json!({
        "telegram": {
            "enabled": tg["enabled"].as_bool().unwrap_or(false),
            "chatId": tg["chatId"].as_str().unwrap_or(""),
            "hasBotToken": tg["botToken"].as_str().is_some_and(|s| !s.is_empty()),
        },
        "webhook": {
            "enabled": wh["enabled"].as_bool().unwrap_or(false),
            "url": wh["url"].as_str().unwrap_or(""),
            "hasSecret": wh["secret"].as_str().is_some_and(|s| !s.is_empty()),
        },
        "webhookEvents": normalize_webhook_events(&cfg["webhookEvents"]),
    })
}

async fn notifications_get(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let cfg = read_notify_config(&db);
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(notify_status(&cfg)),
    )
        .into_response()
}

async fn notifications_post(State(s): State<AppState>, Json(raw): Json<Value>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let existing = read_notify_config(&db);
    let tg_enabled = raw["telegram"]["enabled"].as_bool().unwrap_or(false);
    let mut tg_token = raw["telegram"]["botToken"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    if tg_token.is_empty() {
        tg_token = existing["telegram"]["botToken"]
            .as_str()
            .unwrap_or("")
            .to_string();
    }
    let tg_chat = raw["telegram"]["chatId"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    let wh_enabled = raw["webhook"]["enabled"].as_bool().unwrap_or(false);
    let wh_url = raw["webhook"]["url"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    let mut wh_secret = raw["webhook"]["secret"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    if wh_secret.is_empty() {
        wh_secret = existing["webhook"]["secret"]
            .as_str()
            .unwrap_or("")
            .to_string();
    }

    if tg_enabled && (tg_token.is_empty() || tg_chat.is_empty()) {
        return err(
            StatusCode::BAD_REQUEST,
            "Telegram needs a bot token and a chat id",
        );
    }
    if wh_enabled && !(wh_url.starts_with("http://") || wh_url.starts_with("https://")) {
        return err(
            StatusCode::BAD_REQUEST,
            "Webhook needs a valid http(s) URL",
        );
    }

    let events = if raw.get("webhookEvents").is_some() {
        normalize_webhook_events(&raw["webhookEvents"])
    } else {
        normalize_webhook_events(&existing["webhookEvents"])
    };

    let cfg = json!({
        "telegram": { "enabled": tg_enabled, "botToken": tg_token, "chatId": tg_chat },
        "webhook": { "enabled": wh_enabled, "url": wh_url, "secret": wh_secret },
        "webhookEvents": events,
    });
    if crate::db::kv_set(&db, "notify", "config", &cfg.to_string()).is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save config");
    }
    Json(notify_status(&cfg)).into_response()
}

async fn notifications_test(State(s): State<AppState>) -> Response {
    let cfg = {
        let Ok(db) = s.db.lock() else {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
        };
        read_notify_config(&db)
    };
    let mut results = Vec::new();
    if cfg["telegram"]["enabled"] == true {
        let tok = cfg["telegram"]["botToken"].as_str().unwrap_or("");
        let chat = cfg["telegram"]["chatId"].as_str().unwrap_or("");
        let url = format!("https://api.telegram.org/bot{tok}/sendMessage");
        let ok = s
            .executor
            .0
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(
                json!({ "chat_id": chat, "text": "Test notification from 9Router Pro" })
                    .to_string(),
            )
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        results.push(json!({ "channel": "telegram", "ok": ok }));
    }
    if cfg["webhook"]["enabled"] == true {
        let url = cfg["webhook"]["url"].as_str().unwrap_or("");
        let ok = s
            .executor
            .0
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(
                json!({ "event": "test", "title": "Test notification from 9Router Pro" })
                    .to_string(),
            )
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        results.push(json!({ "channel": "webhook", "ok": ok }));
    }
    Json(json!({ "results": results })).into_response()
}

async fn proxy_test(Json(body): Json<Value>) -> Response {
    let proxy_url = body["proxyUrl"].as_str().unwrap_or("").trim();
    let test_url = body["testUrl"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://httpbin.org/ip");
    let timeout_ms = body["timeoutMs"].as_u64().unwrap_or(8000).clamp(500, 30000);

    let mut builder = reqwest::Client::builder().timeout(Duration::from_millis(timeout_ms));
    if !proxy_url.is_empty() {
        let Ok(proxy) = reqwest::Proxy::all(proxy_url) else {
            return err(StatusCode::BAD_REQUEST, "Invalid proxy URL");
        };
        builder = builder.proxy(proxy);
    }
    let Ok(client) = builder.build() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to build client");
    };
    let start = Instant::now();
    match client.get(test_url).send().await {
        Ok(resp) => Json(json!({
            "ok": resp.status().is_success(),
            "status": resp.status().as_u16(),
            "latencyMs": start.elapsed().as_millis() as u64,
            "proxyUrl": proxy_url,
            "testUrl": test_url,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn webhook_events_get(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let cfg = read_notify_config(&db);
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(json!({
            "webhookEnabled": cfg["webhook"]["enabled"] == true,
            "webhookConfigured": cfg["webhook"]["url"].as_str().is_some_and(|u| !u.is_empty()),
            "events": normalize_webhook_events(&cfg["webhookEvents"]),
        })),
    )
        .into_response()
}

async fn webhook_events_post(State(s): State<AppState>, Json(raw): Json<Value>) -> Response {
    let Some(events_obj) = raw.get("events").and_then(Value::as_object) else {
        return err(StatusCode::BAD_REQUEST, "events object required");
    };
    for k in events_obj.keys() {
        if !WEBHOOK_EVENT_TYPES.contains(&k.as_str()) && k != "maskCredentials" {
            return err(StatusCode::BAD_REQUEST, &format!("unknown event flag: {k}"));
        }
    }
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "DB lock failed");
    };
    let mut cfg = read_notify_config(&db);
    let events = normalize_webhook_events(&Value::Object(events_obj.clone()));
    cfg["webhookEvents"] = events.clone();
    if crate::db::kv_set(&db, "notify", "config", &cfg.to_string()).is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save config");
    }
    Json(json!({
        "webhookEnabled": cfg["webhook"]["enabled"] == true,
        "webhookConfigured": cfg["webhook"]["url"].as_str().is_some_and(|u| !u.is_empty()),
        "events": events,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::test_support::call;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use std::sync::{Arc, Mutex};

    fn settings_app() -> (Router, Arc<Mutex<Connection>>) {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        crate::db::kv_set(&db, "settings", "password", "pw").unwrap();
        let db = Arc::new(Mutex::new(db));
        let state = AppState {
            db: Arc::clone(&db),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        (routes().with_state(state), db)
    }

    #[tokio::test]
    async fn settings_backup_db_encryption_maintenance_notify_and_webhooks() {
        let (app, _) = settings_app();

        let (st, enc) = call(
            app.clone(),
            Request::get("/api/settings/encryption")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(enc["envVar"], "PRO_CREDENTIAL_KEY");

        let (st, snap) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/settings/backup")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let snap_id = snap["snapshot"]["id"].as_str().unwrap().to_string();

        let (st, dl) = call(
            app.clone(),
            Request::get(format!("/api/settings/backup/{snap_id}"))
                .header("x-9r-password", "pw")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(dl["settings"].is_object());

        let (st, exp) = call(
            app.clone(),
            Request::get("/api/settings/database")
                .header("x-9r-password", "pw")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(exp["apiKeys"].is_array());

        let (st, maint) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/settings/maintenance")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(maint["result"]["ok"], true);

        let (st, notif) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/settings/notifications")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"webhook":{"enabled":true,"url":"https://example.com/hook","secret":"s"}}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(notif["webhook"]["hasSecret"], true);

        let (st, wh) = call(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/api/settings/webhook-events")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"events":{"usage":true,"error":true}}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(wh["events"]["usage"], true);
        assert_eq!(wh["events"]["error"], true);
    }
}
