use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::SecureRandom;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

const MAX_BULK_ITEMS: usize = 50;
const QUOTA_WINDOWS: &[i64] = &[24, 168, 720];

fn limits_usage(id: &str, tpm_used: i64) -> Value {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let usage = crate::services::key_rate_limiter::limiter().usage(id, now);
    json!({"rpmUsed":usage.rpm_used,"rphUsed":usage.rph_used,"rpdUsed":usage.rpd_used,"inFlight":usage.in_flight,"tpmUsed":tpm_used})
}

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/keys", get(list_keys).post(create_key).patch(bulk_keys))
        .route(
            "/api/keys/{id}",
            get(get_key).put(update_key).delete(delete_key),
        )
}

fn is_valid_prefix(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 20
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn validate_custom_key(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Custom key cannot be empty");
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("Custom key cannot contain spaces");
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Custom key may only contain letters, digits, - and _");
    }
    if trimmed.len() < 12 {
        return Err("Custom key must be at least 12 characters");
    }
    if trimmed.len() > 128 {
        return Err("Custom key must be at most 128 characters");
    }
    Ok(trimmed.to_string())
}

fn normalize_allow_list(list: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in list {
        if let Some(s) = item.as_str() {
            let entry = s.trim();
            if entry.is_empty() {
                continue;
            }
            let lower = entry.to_lowercase();
            if seen.insert(lower) {
                out.push(entry.to_string());
            }
        }
    }
    out
}

fn validate_key_access(input: &Value) -> Result<(bool, Vec<String>), &'static str> {
    let Some(obj) = input.as_object() else {
        return Err("access must be an object");
    };
    let Some(restricted) = obj.get("restricted").and_then(Value::as_bool) else {
        return Err("access.restricted must be a boolean");
    };
    if !restricted {
        return Ok((false, vec![]));
    }
    let allow = match obj.get("allow") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(arr)) => {
            if arr.len() > 500 {
                return Err("access.allow exceeds maximum of 500 entries");
            }
            for item in arr {
                let Some(s) = item.as_str() else {
                    return Err("access.allow entries must be strings");
                };
                if s.trim().len() > 256 {
                    return Err("access.allow entry exceeds 256 characters");
                }
            }
            normalize_allow_list(arr)
        }
        _ => return Err("access.allow must be an array"),
    };
    Ok((true, allow))
}

fn clean_quota(raw: &Value) -> Option<Value> {
    let obj = raw.as_object()?;
    let limit = obj
        .get("limit")
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()))
        .map(|n| n.floor() as i64)?;
    if limit <= 0 {
        return None;
    }
    let wh = obj
        .get("windowHours")
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()))
        .filter(|w| QUOTA_WINDOWS.contains(w))
        .unwrap_or(24);
    let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let mut out = Map::new();
    out.insert("enabled".into(), json!(enabled));
    out.insert("limit".into(), json!(limit));
    out.insert("windowHours".into(), json!(wh));
    if let Some(cl) = obj
        .get("costLimit")
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()))
        .filter(|n| n.is_finite() && *n > 0.0)
    {
        out.insert("costLimit".into(), json!(cl));
    }
    Some(Value::Object(out))
}

fn clean_limits(raw: &Value) -> Option<Value> {
    let obj = raw.as_object()?;
    let mut out = Map::new();
    for k in ["rpm", "rph", "rpd", "tpm", "maxConcurrent"] {
        if let Some(v) = obj.get(k) {
            if v.is_null() {
                continue;
            }
            if let Some(n) = v
                .as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                .filter(|n| n.is_finite() && *n > 0.0)
            {
                out.insert(k.into(), json!(n.floor() as i64));
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Object(out))
    }
}

fn load_kv_scope(conn: &Connection, scope: &str) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM kv WHERE scope = ?1") {
        if let Ok(rows) =
            stmt.query_map([scope], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            for row in rows.flatten() {
                if let Ok(val) = serde_json::from_str::<Value>(&row.1) {
                    map.insert(row.0, val);
                }
            }
        }
    }
    map
}

fn get_kv_json(conn: &Connection, scope: &str, key: &str) -> Option<Value> {
    crate::db::kv_get(conn, scope, key)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
}

fn set_kv_json(conn: &Connection, scope: &str, key: &str, val: Option<&Value>) -> rusqlite::Result<()> {
    match val {
        Some(v) => {
            let encoded = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
            conn.execute(
                "INSERT INTO kv(scope, key, value) VALUES(?1, ?2, ?3)
                 ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
                params![scope, key, encoded],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM kv WHERE scope = ?1 AND key = ?2",
                params![scope, key],
            )?;
        }
    }
    Ok(())
}

fn key_usage_in_window(conn: &Connection, raw_key: &str, window_secs: i64) -> i64 {
    let modifier = format!("-{window_secs} seconds");
    conn.query_row(
        "SELECT COALESCE(SUM(COALESCE(promptTokens, 0) + COALESCE(completionTokens, 0)), 0)
         FROM usageHistory WHERE apiKey = ?1 AND timestamp >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2)",
        params![raw_key, modifier],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

fn read_key_rows(conn: &Connection, id: Option<&str>) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, name, machineId, isActive, createdAt, expiresAt, accessRestricted, accessAllow
         FROM apiKeys WHERE (?1 IS NULL OR id = ?1) ORDER BY createdAt",
    )?;
    let rows = stmt.query_map([id], |r| {
        let restricted = r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0;
        let raw_allow: Option<String> = r.get(8)?;
        let allow = if restricted {
            raw_allow
                .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
                .map(|v| normalize_allow_list(&v))
                .unwrap_or_default()
        } else {
            vec![]
        };
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "key": r.get::<_, String>(1)?,
            "name": r.get::<_, Option<String>>(2)?,
            "machineId": r.get::<_, Option<String>>(3)?,
            "isActive": r.get::<_, Option<i64>>(4)?.unwrap_or(0) != 0,
            "createdAt": r.get::<_, Option<String>>(5)?,
            "expiresAt": r.get::<_, Option<String>>(6)?,
            "access": { "restricted": restricted, "allow": allow }
        }))
    })?;
    rows.collect()
}

fn random_token(len: usize) -> Result<String, ()> {
    let mut bytes = vec![0u8; len];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn is_pos_num(v: &Value) -> bool {
    v.as_f64().is_some_and(|n| n.is_finite() && n > 0.0)
        || v.as_str()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .is_some_and(|n| n.is_finite() && n > 0.0)
}

fn is_valid_iso_date(s: &str) -> bool {
    let s = s.trim();
    let date_part = s.split(['T', ' ']).next().unwrap_or("");
    let mut parts = date_part.split('-');
    let (Some(y), Some(m), Some(d), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    y.len() == 4
        && y.bytes().all(|b| b.is_ascii_digit())
        && m.parse::<u8>().is_ok_and(|v| (1..=12).contains(&v))
        && d.parse::<u8>().is_ok_and(|v| (1..=31).contains(&v))
}

async fn list_keys(State(s): State<AppState>) -> Response {
    let Ok(conn) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch keys");
    };
    let Ok(mut keys) = read_key_rows(&conn, None) else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch keys");
    };
    let quotas = load_kv_scope(&conn, "keyQuotas");
    let limits_map = load_kv_scope(&conn, "keyLimits");
    for k in &mut keys {
        let id = k["id"].as_str().unwrap_or_default().to_string();
        let raw_key = k["key"].as_str().unwrap_or_default().to_string();
        let quota = quotas.get(&id).and_then(clean_quota);
        let quota_used = quota
            .as_ref()
            .and_then(|q| q["windowHours"].as_i64())
            .map(|wh| key_usage_in_window(&conn, &raw_key, wh * 3600))
            .unwrap_or(0);
        let limits = limits_map.get(&id).and_then(clean_limits);
        let tpm_used = if limits.as_ref().and_then(|l| l["tpm"].as_i64()).is_some() {
            key_usage_in_window(&conn, &raw_key, 60)
        } else {
            0
        };
        if let Some(obj) = k.as_object_mut() {
            obj.insert("quota".into(), quota.unwrap_or(Value::Null));
            obj.insert("quotaUsed".into(), json!(quota_used));
            obj.insert("limits".into(), limits.unwrap_or(Value::Null));
            obj.insert(
                "limitsUsage".into(),
                limits_usage(&id, tpm_used),
            );
        }
    }
    Json(json!({ "keys": keys })).into_response()
}

async fn create_key(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(name) = body["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return err(StatusCode::BAD_REQUEST, "Name is required");
    };

    let raw_custom = body["customKey"].as_str().map(str::trim).unwrap_or("");
    let custom_value = if !raw_custom.is_empty() {
        match validate_custom_key(raw_custom) {
            Ok(v) => Some(v),
            Err(msg) => return err(StatusCode::BAD_REQUEST, msg),
        }
    } else {
        None
    };

    let prefix = body["prefix"].as_str().map(str::trim).unwrap_or("");
    let key_prefix = if prefix.is_empty() { "sk" } else { prefix };
    if custom_value.is_none() && !is_valid_prefix(key_prefix) {
        return err(
            StatusCode::BAD_REQUEST,
            "Invalid prefix: only letters, digits, - and _ allowed, max 20 chars",
        );
    }

    let Ok(conn) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create key");
    };
    if let Ok(existing) = read_key_rows(&conn, None) {
        if existing.iter().any(|k| {
            k["name"]
                .as_str()
                .is_some_and(|n| n.trim().eq_ignore_ascii_case(name))
        }) {
            return err(StatusCode::BAD_REQUEST, "Key name already exists");
        }
    }
    if let Some(ref cv) = custom_value {
        let dup: Option<String> = conn
            .query_row("SELECT id FROM apiKeys WHERE key = ?1", [cv], |r| r.get(0))
            .optional()
            .ok()
            .flatten();
        if dup.is_some() {
            return err(StatusCode::BAD_REQUEST, "Key already exists");
        }
    }

    let Ok(id) = random_token(16) else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate ID");
    };
    let key = match custom_value {
        Some(v) => v,
        None => match random_token(32) {
            Ok(tok) => format!("{key_prefix}-{tok}"),
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate key"),
        },
    };
    let machine_id: Option<String> = crate::db::kv_get(&conn, "app", "machineId")
        .ok()
        .flatten();

    let res = conn.execute(
        "INSERT INTO apiKeys(id, key, name, machineId, isActive, createdAt, accessRestricted, accessAllow)
         VALUES(?1, ?2, ?3, ?4, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 0, '[]')",
        params![id, key, name, machine_id],
    );
    if res.is_err() {
        return err(StatusCode::BAD_REQUEST, "Key name or value already exists");
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "key": key,
            "name": name,
            "id": id,
            "machineId": machine_id
        })),
    )
        .into_response()
}

async fn bulk_keys(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let ids: Vec<String> = body["ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let updates_obj = if let Some(u) = body.get("updates").and_then(Value::as_object) {
        u.clone()
    } else if let Some(active) = body.get("isActive").and_then(Value::as_bool) {
        let mut m = Map::new();
        m.insert("isActive".into(), json!(active));
        m
    } else {
        return err(StatusCode::BAD_REQUEST, "ids[] and updates are required");
    };

    if ids.is_empty() {
        return err(StatusCode::BAD_REQUEST, "ids[] and updates are required");
    }
    if ids.len() > MAX_BULK_ITEMS {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("Bulk edit is limited to {MAX_BULK_ITEMS} keys per request"),
        );
    }
    if let Some(exp) = updates_obj.get("expiresAt") {
        if !exp.is_null() && exp.as_str().is_none_or(|s| !is_valid_iso_date(s)) {
            return err(StatusCode::BAD_REQUEST, "expiresAt must be an ISO date or null");
        }
    }
    if let Some(q) = updates_obj.get("quota") {
        if !q.is_null() {
            let Some(q_obj) = q.as_object() else {
                return err(StatusCode::BAD_REQUEST, "Bulk quota must be an object");
            };
            if let Some(l) = q_obj.get("limit").filter(|v| !v.is_null()) {
                if !is_pos_num(l) {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "Bulk quota limit must be a positive number",
                    );
                }
            }
            if let Some(cl) = q_obj.get("costLimit").filter(|v| !v.is_null()) {
                if !is_pos_num(cl) {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "Bulk quota Max spend must be a positive number",
                    );
                }
            }
        }
    }
    if let Some(l) = updates_obj.get("limits") {
        if !l.is_null() {
            let Some(l_obj) = l.as_object() else {
                return err(StatusCode::BAD_REQUEST, "Bulk limits must be an object");
            };
            for (f, v) in l_obj {
                if !v.is_null() && !is_pos_num(v) {
                    return err(
                        StatusCode::BAD_REQUEST,
                        &format!("Bulk limit {f} must be a positive number"),
                    );
                }
            }
        }
    }

    let Ok(conn) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to bulk-update keys",
        );
    };

    let mut updated = Vec::new();
    let mut failed = Vec::new();

    for id in ids {
        let Ok(mut rows) = read_key_rows(&conn, Some(&id)) else {
            failed.push(json!({ "id": id, "error": "Database error" }));
            continue;
        };
        let Some(existing) = rows.pop() else {
            failed.push(json!({ "id": id, "error": "Key not found" }));
            continue;
        };

        let mut active = existing["isActive"].as_bool().unwrap_or(true);
        if let Some(v) = updates_obj.get("isActive").and_then(Value::as_bool) {
            active = v;
        }
        let mut expires_at = existing["expiresAt"].as_str().map(str::to_string);
        if let Some(v) = updates_obj.get("expiresAt") {
            expires_at = v.as_str().map(str::to_string);
        }
        let mut restricted = existing["access"]["restricted"].as_bool().unwrap_or(false);
        let mut allow_json = serde_json::to_string(&existing["access"]["allow"]).unwrap_or_else(|_| "[]".into());
        if let Some(acc) = updates_obj.get("access") {
            match validate_key_access(acc) {
                Ok((r, a)) => {
                    restricted = r;
                    allow_json = serde_json::to_string(&a).unwrap_or_else(|_| "[]".into());
                }
                Err(msg) => {
                    failed.push(json!({ "id": id, "error": msg }));
                    continue;
                }
            }
        }

        if conn
            .execute(
                "UPDATE apiKeys SET isActive = ?1, expiresAt = ?2, accessRestricted = ?3, accessAllow = ?4 WHERE id = ?5",
                params![active, expires_at, restricted, allow_json, id],
            )
            .is_err()
        {
            failed.push(json!({ "id": id, "error": "Failed to update key" }));
            continue;
        }

        if let Some(q) = updates_obj.get("quota") {
            if q.is_null() {
                let _ = set_kv_json(&conn, "keyQuotas", &id, None);
            } else if let Some(q_obj) = q.as_object() {
                let mut merged = get_kv_json(&conn, "keyQuotas", &id)
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                for (k, v) in q_obj {
                    merged.insert(k.clone(), v.clone());
                }
                let cleaned = clean_quota(&Value::Object(merged));
                let _ = set_kv_json(&conn, "keyQuotas", &id, cleaned.as_ref());
            }
        }
        if let Some(l) = updates_obj.get("limits") {
            if l.is_null() {
                let _ = set_kv_json(&conn, "keyLimits", &id, None);
            } else if let Some(l_obj) = l.as_object() {
                let mut merged = get_kv_json(&conn, "keyLimits", &id)
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                for (k, v) in l_obj {
                    merged.insert(k.clone(), v.clone());
                }
                let cleaned = clean_limits(&Value::Object(merged));
                let _ = set_kv_json(&conn, "keyLimits", &id, cleaned.as_ref());
            }
        }

        updated.push(id);
    }

    Json(json!({ "updated": updated, "failed": failed })).into_response()
}

async fn get_key(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(conn) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch key");
    };
    let Some(mut key) = read_key_rows(&conn, Some(&id)).ok().and_then(|mut v| v.pop()) else {
        return err(StatusCode::NOT_FOUND, "Key not found");
    };
    let raw_key = key["key"].as_str().unwrap_or_default().to_string();
    let limits = get_kv_json(&conn, "keyLimits", &id).and_then(|v| clean_limits(&v));
    let tpm_used = if limits.as_ref().and_then(|l| l["tpm"].as_i64()).is_some() {
        key_usage_in_window(&conn, &raw_key, 60)
    } else {
        0
    };
    if let Some(obj) = key.as_object_mut() {
        obj.insert(
            "limitsUsage".into(),
            limits_usage(&id, tpm_used),
        );
    }
    Json(json!({ "key": key })).into_response()
}

async fn update_key(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Ok(conn) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update key");
    };
    let Some(existing) = read_key_rows(&conn, Some(&id)).ok().and_then(|mut v| v.pop()) else {
        return err(StatusCode::NOT_FOUND, "Key not found");
    };

    let mut name = existing["name"].as_str().map(str::to_string);
    if let Some(n) = body.get("name") {
        let Some(trimmed) = n.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            return err(StatusCode::BAD_REQUEST, "Key name cannot be empty");
        };
        if let Ok(others) = read_key_rows(&conn, None) {
            if others.iter().any(|k| {
                k["id"].as_str() != Some(&id)
                    && k["name"]
                        .as_str()
                        .is_some_and(|kn| kn.trim().eq_ignore_ascii_case(trimmed))
            }) {
                return err(StatusCode::BAD_REQUEST, "Key name already exists");
            }
        }
        name = Some(trimmed.to_string());
    }

    let regenerate = body.get("regenerate").and_then(Value::as_bool) == Some(true);
    let raw_custom = body
        .get("customKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if regenerate && !raw_custom.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "Choose one: regenerate or custom key",
        );
    }
    let mut key_val = existing["key"].as_str().unwrap_or_default().to_string();
    if regenerate {
        let Ok(tok) = random_token(32) else {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to regenerate key",
            );
        };
        key_val = format!("sk-{tok}");
    } else if !raw_custom.is_empty() {
        let checked = match validate_custom_key(raw_custom) {
            Ok(v) => v,
            Err(msg) => return err(StatusCode::BAD_REQUEST, msg),
        };
        let clash: Option<String> = conn
            .query_row("SELECT id FROM apiKeys WHERE key = ?1", [&checked], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten();
        if clash.is_some_and(|cid| cid != id) {
            return err(StatusCode::BAD_REQUEST, "Key already exists");
        }
        key_val = checked;
    }

    let mut is_active = existing["isActive"].as_bool().unwrap_or(true);
    if let Some(a) = body.get("isActive") {
        let Some(b) = a.as_bool() else {
            return err(StatusCode::BAD_REQUEST, "isActive must be a boolean");
        };
        is_active = b;
    }

    let mut expires_at = existing["expiresAt"].as_str().map(str::to_string);
    if let Some(exp) = body.get("expiresAt") {
        if exp.is_null() {
            expires_at = None;
        } else if let Some(s) = exp.as_str().filter(|s| is_valid_iso_date(s)) {
            expires_at = Some(s.to_string());
        } else {
            return err(StatusCode::BAD_REQUEST, "expiresAt must be an ISO date or null");
        }
    }

    let mut restricted = existing["access"]["restricted"].as_bool().unwrap_or(false);
    let mut allow_json =
        serde_json::to_string(&existing["access"]["allow"]).unwrap_or_else(|_| "[]".into());
    if let Some(acc) = body.get("access") {
        match validate_key_access(acc) {
            Ok((r, a)) => {
                restricted = r;
                allow_json = serde_json::to_string(&a).unwrap_or_else(|_| "[]".into());
            }
            Err(msg) => return err(StatusCode::BAD_REQUEST, msg),
        }
    }

    if conn
        .execute(
            "UPDATE apiKeys SET name = ?1, key = ?2, isActive = ?3, expiresAt = ?4, accessRestricted = ?5, accessAllow = ?6 WHERE id = ?7",
            params![name, key_val, is_active, expires_at, restricted, allow_json, id],
        )
        .is_err()
    {
        return err(StatusCode::BAD_REQUEST, "Key name or value already exists");
    }

    let mut saved_quota = get_kv_json(&conn, "keyQuotas", &id).and_then(|v| clean_quota(&v));
    if let Some(q) = body.get("quota") {
        if q.is_null() {
            let _ = set_kv_json(&conn, "keyQuotas", &id, None);
            saved_quota = None;
        } else if let Some(q_obj) = q.as_object() {
            let mut merged = saved_quota
                .as_ref()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            for (k, v) in q_obj {
                merged.insert(k.clone(), v.clone());
            }
            saved_quota = clean_quota(&Value::Object(merged));
            let _ = set_kv_json(&conn, "keyQuotas", &id, saved_quota.as_ref());
        }
    }

    let mut saved_limits = get_kv_json(&conn, "keyLimits", &id).and_then(|v| clean_limits(&v));
    if let Some(l) = body.get("limits") {
        if l.is_null() {
            let _ = set_kv_json(&conn, "keyLimits", &id, None);
            saved_limits = None;
        } else if let Some(l_obj) = l.as_object() {
            let mut merged = saved_limits
                .as_ref()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            for (k, v) in l_obj {
                merged.insert(k.clone(), v.clone());
            }
            saved_limits = clean_limits(&Value::Object(merged));
            let _ = set_kv_json(&conn, "keyLimits", &id, saved_limits.as_ref());
        }
    }

    let mut updated = read_key_rows(&conn, Some(&id))
        .ok()
        .and_then(|mut v| v.pop())
        .unwrap_or(existing);
    let quota_used = saved_quota
        .as_ref()
        .and_then(|q| q["windowHours"].as_i64())
        .map(|wh| key_usage_in_window(&conn, &key_val, wh * 3600))
        .unwrap_or(0);
    let tpm_used = if saved_limits.as_ref().and_then(|l| l["tpm"].as_i64()).is_some() {
        key_usage_in_window(&conn, &key_val, 60)
    } else {
        0
    };
    if let Some(obj) = updated.as_object_mut() {
        obj.insert("quota".into(), saved_quota.unwrap_or(Value::Null));
        obj.insert("quotaUsed".into(), json!(quota_used));
        obj.insert("limits".into(), saved_limits.unwrap_or(Value::Null));
        obj.insert(
            "limitsUsage".into(),
            limits_usage(&id, tpm_used),
        );
    }

    Json(json!({ "key": updated })).into_response()
}

async fn delete_key(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(conn) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete key");
    };
    match conn.execute("DELETE FROM apiKeys WHERE id = ?1", [&id]) {
        Ok(0) => err(StatusCode::NOT_FOUND, "Key not found"),
        Ok(_) => {
            let _ = set_kv_json(&conn, "keyQuotas", &id, None);
            let _ = set_kv_json(&conn, "keyLimits", &id, None);
            Json(json!({ "message": "Key deleted successfully" })).into_response()
        }
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete key"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::test_support::call;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use std::sync::{Arc, Mutex};

    #[test]
    fn reports_live_limit_usage() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let slot = crate::services::key_rate_limiter::limiter().try_admit("limits-usage-test", Default::default(), now).unwrap();
        let usage = limits_usage("limits-usage-test", 17);
        assert_eq!(usage["rpmUsed"], 1);
        assert_eq!(usage["inFlight"], 1);
        assert_eq!(usage["tpmUsed"], 17);
        drop(slot);
        assert_eq!(limits_usage("limits-usage-test", 0)["inFlight"], 0);
    }

    fn keys_app() -> (Router, Arc<Mutex<Connection>>) {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let db = Arc::new(Mutex::new(db));
        let state = AppState {
            db: Arc::clone(&db),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        (routes().with_state(state), db)
    }

    #[tokio::test]
    async fn keys_crud_quota_limits_and_bulk_patch() {
        let (app, _) = keys_app();
        let (st, created) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/keys")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"Primary","prefix":"nr"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED);
        let id = created["id"].as_str().unwrap().to_string();
        assert!(created["key"].as_str().unwrap().starts_with("nr-"));

        let (st, put_res) = call(
            app.clone(),
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/api/keys/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"quota":{"limit":10000,"windowHours":168},"limits":{"rpm":60,"tpm":5000}}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(put_res["key"]["quota"]["limit"], 10000);
        assert_eq!(put_res["key"]["limits"]["rpm"], 60);

        let (st, list_res) = call(app.clone(), Request::get("/api/keys").body(Body::empty()).unwrap()).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list_res["keys"][0]["quota"]["windowHours"], 168);

        let (st, bulk_res) = call(
            app.clone(),
            Request::builder()
                .method(Method::PATCH)
                .uri("/api/keys")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"ids":["{id}","missing"],"updates":{{"isActive":false,"quota":{{"costLimit":5.5}}}}}}"#
                )))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(bulk_res["updated"][0], id);
        assert_eq!(bulk_res["failed"][0]["id"], "missing");

        let (st, get_res) = call(
            app.clone(),
            Request::get(format!("/api/keys/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(get_res["key"]["isActive"], false);

        let (st, _) = call(
            app,
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/keys/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
}
