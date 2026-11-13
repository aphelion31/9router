use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use ring::digest::{Context, SHA256};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    db::{kv_get, kv_set},
    server::AppState,
};

const SCOPE_INJECT: &str = "promptInjections";
const SCOPE_TEMPLATES: &str = "promptPresets";
const SCOPE_STATS: &str = "promptInjectStats";
const MAX_TURNS: usize = 50;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/prompt-injection",
            get(get_injections)
                .put(put_injection)
                .delete(delete_injection),
        )
        .route("/api/prompt-injection/stats", get(get_stats))
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
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

fn sha256_hex(s: &str) -> String {
    let mut ctx = Context::new(&SHA256);
    ctx.update(s.as_bytes());
    let digest = ctx.finish();
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn clean_turns(raw: Option<&Value>) -> Vec<Value> {
    let Some(arr) = raw.and_then(|v| v.as_array()) else {
        return vec![];
    };
    let mut out = Vec::new();
    for t in arr {
        let Some(obj) = t.as_object() else { continue };
        let role = match obj.get("role").and_then(|v| v.as_str()) {
            Some("assistant") => "assistant",
            Some("user") => "user",
            _ => continue,
        };
        let content = obj.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if !content.trim().is_empty() {
            out.push(json!({ "role": role, "content": content }));
            if out.len() >= MAX_TURNS {
                break;
            }
        }
    }
    out
}

fn clean_entry(raw: &Value, stamp_now: bool) -> Option<Value> {
    let obj = raw.as_object()?;
    let position = if obj.get("position").and_then(|v| v.as_str()) == Some("prepend") {
        "prepend"
    } else {
        "append"
    };
    let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let updated_at = if stamp_now {
        Value::String(now_iso())
    } else {
        obj.get("updatedAt").cloned().unwrap_or(Value::Null)
    };
    let template_name = obj
        .get("templateName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.chars().take(80).collect()))
        .unwrap_or(Value::Null);
    let name = obj
        .get("name")
        .or_else(|| obj.get("label"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.chars().take(80).collect()))
        .unwrap_or(Value::Null);

    if obj.get("mode").and_then(|v| v.as_str()) == Some("chat") {
        let turns = clean_turns(obj.get("turns"));
        if turns.is_empty() {
            return None;
        }
        return Some(json!({
            "mode": "chat",
            "content": "",
            "turns": turns,
            "position": position,
            "enabled": enabled,
            "updatedAt": updated_at,
            "templateName": template_name,
            "name": name,
        }));
    }

    let content = obj.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.trim().is_empty() {
        return None;
    }
    Some(json!({
        "mode": "single",
        "content": content,
        "turns": [],
        "position": position,
        "enabled": enabled,
        "updatedAt": updated_at,
        "templateName": template_name,
        "name": name,
    }))
}

fn clean_map(raw: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(obj) = raw.and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, v) in obj {
        if !k.is_empty() {
            if let Some(c) = clean_entry(v, false) {
                out.insert(k.clone(), c);
            }
        }
    }
    out
}

fn clean_variant_entry(raw: &Value, stamp_now: bool) -> Option<Value> {
    let obj = raw.as_object()?;
    let backing = obj.get("model").and_then(|v| v.as_str()).map(str::trim)?;
    if !backing.contains('/') {
        return None;
    }
    let mut entry = clean_entry(raw, stamp_now)?;
    let map = entry.as_object_mut()?;
    map.insert("model".into(), Value::String(backing.to_owned()));
    let combo_id = obj
        .get("comboId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_owned()))
        .unwrap_or(Value::Null);
    map.insert("comboId".into(), combo_id);
    if let Some(s) = obj.get("sampling").and_then(|v| v.as_object()) {
        let mut sout = Map::new();
        if let Some(t) = s.get("temperature").and_then(|v| v.as_f64()) {
            if t.is_finite() && (0.0..=2.0).contains(&t) {
                sout.insert("temperature".into(), json!(t));
            }
        }
        if let Some(p) = s.get("top_p").and_then(|v| v.as_f64()) {
            if p.is_finite() && p > 0.0 && p <= 1.0 {
                sout.insert("top_p".into(), json!(p));
            }
        }
        if let Some(m) = s.get("max_tokens").and_then(|v| v.as_i64()) {
            if (1..=1_000_000).contains(&m) {
                sout.insert("max_tokens".into(), json!(m));
            }
        }
        if !sout.is_empty() {
            map.insert("sampling".into(), Value::Object(sout));
        }
    }
    Some(entry)
}

fn clean_variant_map(raw: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(obj) = raw.and_then(|v| v.as_object()) else {
        return out;
    };
    for (name, v) in obj {
        let trimmed = name.trim();
        if !trimmed.is_empty() && trimmed.len() <= 80 && !trimmed.contains('/') {
            if let Some(c) = clean_variant_entry(v, false) {
                out.insert(trimmed.to_owned(), c);
            }
        }
    }
    out
}

fn read_json_kv(conn: &Connection, scope: &str, key: &str) -> Option<Value> {
    kv_get(conn, scope, key)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
}

fn read_key_model_map(conn: &Connection) -> Map<String, Value> {
    let Some(obj) = read_json_kv(conn, SCOPE_INJECT, "byKeyModel").and_then(|v| v.as_object().cloned()) else {
        return Map::new();
    };
    let mut out = Map::new();
    for (k, models) in obj {
        let cleaned = clean_map(Some(&models));
        if !k.is_empty() && !cleaned.is_empty() {
            out.insert(k, Value::Object(cleaned));
        }
    }
    out
}

fn read_templates(conn: &Connection) -> Vec<Value> {
    let Some(arr) = read_json_kv(conn, SCOPE_TEMPLATES, "list").and_then(|v| v.as_array().cloned()) else {
        return vec![];
    };
    let mut out = Vec::new();
    for t in arr {
        let Some(obj) = t.as_object() else { continue };
        let id = obj.get("id").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        if id.is_empty() || name.is_empty() {
            continue;
        }
        if let Some(mut entry) = clean_entry(&t, false) {
            if let Some(m) = entry.as_object_mut() {
                m.insert("id".into(), Value::String(id.to_owned()));
                m.insert("name".into(), Value::String(name.chars().take(80).collect()));
            }
            out.push(entry);
        }
    }
    out
}

fn builtin_templates() -> Vec<Value> {
    // ponytail: minimal built-in metadata matching JS; full prompt bodies live in JS constants
    vec![
        json!({
            "id": "builtin-glm-5-3",
            "name": "GLM 5.3",
            "mode": "single",
            "content": "You are a creative and technical partner.",
            "turns": [],
            "position": "prepend",
            "enabled": true,
            "builtin": true
        }),
        json!({
            "id": "builtin-deepseek-v2",
            "name": "DeepSeek V2",
            "mode": "single",
            "content": "Terse, precise, hands-on senior systems engineer voice.",
            "turns": [],
            "position": "append",
            "enabled": true,
            "builtin": true
        }),
    ]
}

fn select_stack(
    global: &Option<Value>,
    by_key: &Map<String, Value>,
    by_model: &Map<String, Value>,
    by_key_model: &Map<String, Value>,
    by_variant: &Map<String, Value>,
    requested_model: &str,
    key_id: Option<&str>,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut push = |name: &str, e: &Value| {
        if !e.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
            return;
        }
        if e.get("mode").and_then(|v| v.as_str()) == Some("chat") {
            out.push(json!({
                "name": name,
                "mode": "chat",
                "turns": e["turns"],
                "position": e["position"],
            }));
        } else {
            out.push(json!({
                "name": name,
                "mode": "single",
                "text": e["content"],
                "position": e["position"],
            }));
        }
    };

    if let Some(g) = global {
        push("global", g);
    }
    if let Some(kid) = key_id.filter(|s| !s.is_empty()) {
        if let Some(e) = by_key.get(kid) {
            push("user", e);
        }
    }

    let mut variant_hit = false;
    if !requested_model.is_empty() && !requested_model.contains('/') {
        if let Some(e) = by_variant.get(requested_model) {
            if e.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
                push(requested_model, e);
                variant_hit = true;
            }
        }
    }

    if !variant_hit && !requested_model.is_empty() {
        if let Some(e) = by_model.get(requested_model) {
            push(requested_model, e);
        }
        if let Some(kid) = key_id.filter(|s| !s.is_empty()) {
            if let Some(scoped) = by_key_model.get(kid).and_then(|v| v.as_object()) {
                if let Some(e) = scoped.get(requested_model) {
                    push(requested_model, e);
                }
            }
        }
    }
    out
}

async fn get_injections(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let db = s.db.lock().unwrap();
    let global = read_json_kv(&db, SCOPE_INJECT, "global").and_then(|v| clean_entry(&v, false));
    let user_prompts = clean_map(read_json_kv(&db, SCOPE_INJECT, "byKey").as_ref());
    let prompts = clean_map(read_json_kv(&db, SCOPE_INJECT, "prompts").as_ref());
    let key_model_prompts = read_key_model_map(&db);
    let variants = clean_variant_map(read_json_kv(&db, SCOPE_INJECT, "variants").as_ref());
    let templates = read_templates(&db);

    if params.get("preview").is_some_and(|v| !v.is_empty()) {
        let model = params.get("model").map(String::as_str).unwrap_or("");
        let key_id = params.get("keyId").map(String::as_str);
        let stack = select_stack(
            &global,
            &user_prompts,
            &prompts,
            &key_model_prompts,
            &variants,
            model,
            key_id,
        );
        return Json(json!({ "stack": stack })).into_response();
    }

    let stored_hashes: HashSet<String> = templates
        .iter()
        .map(|t| sha256_hex(t["content"].as_str().unwrap_or("")))
        .collect();
    let mut merged_templates: Vec<Value> = builtin_templates()
        .into_iter()
        .filter(|b| !stored_hashes.contains(&sha256_hex(b["content"].as_str().unwrap_or(""))))
        .collect();
    merged_templates.extend(templates);

    Json(json!({
        "global": global,
        "userPrompts": Value::Object(user_prompts),
        "prompts": Value::Object(prompts),
        "keyModelPrompts": Value::Object(key_model_prompts),
        "variants": Value::Object(variants),
        "templates": merged_templates,
    }))
    .into_response()
}

async fn put_injection(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(obj) = body.as_object() else {
        return err(
            StatusCode::BAD_REQUEST,
            "Provide global, template, keyId, or variant",
        );
    };
    let db = s.db.lock().unwrap();

    if let Some(g) = obj.get("global") {
        let entry = clean_entry(g, true);
        let val = entry.clone().unwrap_or(Value::Null);
        if kv_set(&db, SCOPE_INJECT, "global", &val.to_string()).is_err() {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save global");
        }
        return Json(json!({ "global": entry })).into_response();
    }

    if let Some(tpl) = obj.get("template").and_then(|v| v.as_object()) {
        let name = tpl.get("name").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
        if name.is_empty() {
            return err(
                StatusCode::BAD_REQUEST,
                "Template needs a name and a non-empty prompt",
            );
        }
        let id = tpl
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("tpl-{}", now_iso()));
        let Some(mut entry) = clean_entry(&Value::Object(tpl.clone()), true) else {
            return err(
                StatusCode::BAD_REQUEST,
                "Template needs a name and a non-empty prompt",
            );
        };
        if let Some(m) = entry.as_object_mut() {
            m.insert("id".into(), Value::String(id.clone()));
            m.insert("name".into(), Value::String(name.chars().take(80).collect()));
        }
        let mut list = read_templates(&db);
        if let Some(pos) = list.iter().position(|t| t["id"].as_str() == Some(&id)) {
            list[pos] = entry.clone();
        } else {
            list.push(entry.clone());
        }
        if kv_set(&db, SCOPE_TEMPLATES, "list", &Value::Array(list).to_string()).is_err() {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save template");
        }
        return Json(json!({ "template": entry })).into_response();
    }

    if let Some(v) = obj.get("variant").and_then(|x| x.as_object()) {
        let name = v.get("name").and_then(|x| x.as_str()).map(str::trim).unwrap_or("");
        if name.is_empty() || name.len() > 80 || name.contains('/') {
            return err(
                StatusCode::BAD_REQUEST,
                "Variant needs a custom name without \"/\" and at most 80 characters",
            );
        }
        if name == "global" || name == "user" {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("Variant name \"{name}\" is reserved — pick another name"),
            );
        }
        let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("");
        if !model.contains('/') {
            return err(
                StatusCode::BAD_REQUEST,
                "Variant needs a backing provider/model",
            );
        }
        let mut variants = clean_variant_map(read_json_kv(&db, SCOPE_INJECT, "variants").as_ref());
        let is_new = !variants.contains_key(name);
        if is_new {
            let combo_exists: Option<i64> = db
                .query_row("SELECT 1 FROM combos WHERE name = ?1", [name], |r| r.get(0))
                .optional()
                .ok()
                .flatten();
            if combo_exists.is_some() {
                return err(
                    StatusCode::BAD_REQUEST,
                    &format!("Name \"{name}\" is already used by a combo — pick another name"),
                );
            }
            if let Some(aliases) = read_json_kv(&db, "modelAliases", "aliases").and_then(|a| a.as_object().cloned()) {
                if aliases.contains_key(name) {
                    return err(
                        StatusCode::BAD_REQUEST,
                        &format!("Name \"{name}\" is already used by a model alias — pick another name"),
                    );
                }
            }
        }
        let Some(saved) = clean_variant_entry(&Value::Object(v.clone()), true) else {
            return err(StatusCode::BAD_REQUEST, "Failed to save variant");
        };
        variants.insert(name.to_owned(), saved.clone());
        if kv_set(&db, SCOPE_INJECT, "variants", &Value::Object(variants).to_string()).is_err() {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save variant");
        }
        return Json(json!({ "variant": saved })).into_response();
    }

    let key_id = obj.get("keyId").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    let model = obj.get("model").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());

    if model.is_some() && key_id.is_none() {
        return err(
            StatusCode::BAD_REQUEST,
            "Model targets are now variants — give the injection a custom name (variant) instead",
        );
    }
    if let (Some(kid), Some(m)) = (key_id, model) {
        let mut all = read_key_model_map(&db);
        let saved = clean_entry(&body, true);
        let mut scoped = all
            .get(kid)
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if let Some(ref e) = saved {
            scoped.insert(m.to_owned(), e.clone());
        } else {
            scoped.remove(m);
        }
        if scoped.is_empty() {
            all.remove(kid);
        } else {
            all.insert(kid.to_owned(), Value::Object(scoped));
        }
        if kv_set(&db, SCOPE_INJECT, "byKeyModel", &Value::Object(all).to_string()).is_err() {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save prompt injection");
        }
        return Json(json!({ "keyId": kid, "model": m, "entry": saved })).into_response();
    }
    if let Some(kid) = key_id {
        let mut map = clean_map(read_json_kv(&db, SCOPE_INJECT, "byKey").as_ref());
        let saved = clean_entry(&body, true);
        if let Some(ref e) = saved {
            map.insert(kid.to_owned(), e.clone());
        } else {
            map.remove(kid);
        }
        if kv_set(&db, SCOPE_INJECT, "byKey", &Value::Object(map).to_string()).is_err() {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save prompt injection");
        }
        return Json(json!({ "keyId": kid, "entry": saved })).into_response();
    }

    err(
        StatusCode::BAD_REQUEST,
        "Provide global, template, keyId, or variant",
    )
}

async fn delete_injection(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let db = s.db.lock().unwrap();
    if let Some(tid) = params.get("templateId").filter(|s| !s.is_empty()) {
        let list = read_templates(&db);
        let next: Vec<Value> = list
            .into_iter()
            .filter(|t| t["id"].as_str() != Some(tid.as_str()))
            .collect();
        let deleted = kv_set(&db, SCOPE_TEMPLATES, "list", &Value::Array(next).to_string()).is_ok();
        return Json(json!({ "deleted": deleted })).into_response();
    }
    if let Some(v) = params.get("variant").filter(|s| !s.is_empty()) {
        let mut map = clean_variant_map(read_json_kv(&db, SCOPE_INJECT, "variants").as_ref());
        let deleted = map.remove(v).is_some()
            && kv_set(&db, SCOPE_INJECT, "variants", &Value::Object(map).to_string()).is_ok();
        return Json(json!({ "deleted": deleted })).into_response();
    }
    if let (Some(m), Some(kid)) = (
        params.get("model").filter(|s| !s.is_empty()),
        params.get("keyId").filter(|s| !s.is_empty()),
    ) {
        let mut all = read_key_model_map(&db);
        let mut deleted = false;
        if let Some(scoped) = all.get_mut(kid).and_then(|v| v.as_object_mut()) {
            deleted = scoped.remove(m).is_some();
            if scoped.is_empty() {
                all.remove(kid);
            }
        }
        if deleted {
            let _ = kv_set(&db, SCOPE_INJECT, "byKeyModel", &Value::Object(all).to_string());
        }
        return Json(json!({ "deleted": deleted })).into_response();
    }
    if let Some(m) = params.get("model").filter(|s| !s.is_empty()) {
        let mut map = clean_map(read_json_kv(&db, SCOPE_INJECT, "prompts").as_ref());
        let deleted = map.remove(m).is_some()
            && kv_set(&db, SCOPE_INJECT, "prompts", &Value::Object(map).to_string()).is_ok();
        return Json(json!({ "deleted": deleted })).into_response();
    }
    if let Some(kid) = params.get("keyId").filter(|s| !s.is_empty()) {
        let mut map = clean_map(read_json_kv(&db, SCOPE_INJECT, "byKey").as_ref());
        let deleted = map.remove(kid).is_some()
            && kv_set(&db, SCOPE_INJECT, "byKey", &Value::Object(map).to_string()).is_ok();
        return Json(json!({ "deleted": deleted })).into_response();
    }
    err(
        StatusCode::BAD_REQUEST,
        "Provide variant, model, keyId, or templateId",
    )
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
    let mut by_scope: HashMap<String, i64> = HashMap::new();
    let mut by_name: HashMap<String, (String, i64)> = HashMap::new();

    for i in (0..days).rev() {
        let key = utc_day_key(now_secs - i * 86_400);
        let day = read_json_kv(&db, SCOPE_STATS, &key)
            .filter(|v| v.is_object())
            .unwrap_or_else(|| json!({ "requests": 0, "byScope": {}, "byName": {} }));
        total_requests += day["requests"].as_i64().unwrap_or(0);
        if let Some(scopes) = day["byScope"].as_object() {
            for (sc, n) in scopes {
                *by_scope.entry(sc.clone()).or_insert(0) += n.as_i64().unwrap_or(0);
            }
        }
        if let Some(names) = day["byName"].as_object() {
            for (nm, e) in names {
                let scope = e["scope"].as_str().unwrap_or("unknown").to_owned();
                let fires = e["fires"].as_i64().unwrap_or(0);
                let entry = by_name.entry(nm.clone()).or_insert((scope, 0));
                entry.1 += fires;
            }
        }
        let mut obj = day.as_object().cloned().unwrap_or_default();
        obj.insert("date".into(), Value::String(key));
        out_days.push(Value::Object(obj));
    }

    let by_name_json: Map<String, Value> = by_name
        .into_iter()
        .map(|(k, (scope, fires))| (k, json!({ "scope": scope, "fires": fires })))
        .collect();

    Json(json!({
        "days": out_days,
        "totals": {
            "requests": total_requests,
            "byScope": by_scope,
            "byName": by_name_json,
        }
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::executor::HttpExecutor;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
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
    async fn prompt_injection_variant_preview_delete() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::put("/api/prompt-injection")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"variant":{"name":"terse-dev","model":"openai/gpt-4o","content":"Be terse."}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .clone()
            .oneshot(
                Request::get("/api/prompt-injection?preview=1&model=terse-dev")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = serde_json::from_slice(&to_bytes(r.into_body(), 10_000).await.unwrap()).unwrap();
        assert_eq!(body["stack"].as_array().unwrap().len(), 1);
        assert_eq!(body["stack"][0]["name"], "terse-dev");

        let r = app
            .clone()
            .oneshot(
                Request::delete("/api/prompt-injection?variant=terse-dev")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = serde_json::from_slice(&to_bytes(r.into_body(), 10_000).await.unwrap()).unwrap();
        assert_eq!(body["deleted"], true);
    }

    #[tokio::test]
    async fn prompt_injection_stats_returns_days() {
        let app = test_app();
        let r = app
            .oneshot(
                Request::get("/api/prompt-injection/stats?days=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = serde_json::from_slice(&to_bytes(r.into_body(), 10_000).await.unwrap()).unwrap();
        assert_eq!(body["days"].as_array().unwrap().len(), 5);
        assert_eq!(body["totals"]["requests"], 0);
    }
}
