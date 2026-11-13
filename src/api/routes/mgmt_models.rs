use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({"error":msg}))).into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/combos", get(list_combos).post(create))
        .route(
            "/api/combos/{id}",
            get(get_combo).put(update).delete(delete),
        )
        .route("/api/combos/{id}/references", get(references))
        .route("/api/models", get(models_list).put(models_put))
        .route(
            "/api/models/alias",
            get(alias_list).put(alias_put).delete(alias_delete),
        )
        .route(
            "/api/models/availability",
            get(availability_get).post(availability_post),
        )
        .route(
            "/api/models/catalog-sync",
            get(catalog_sync_get).post(catalog_sync_post),
        )
        .route(
            "/api/models/custom",
            get(custom_list)
                .post(custom_post)
                .patch(custom_patch)
                .delete(custom_delete),
        )
        .route(
            "/api/models/disabled",
            get(disabled_list)
                .post(disabled_add)
                .delete(disabled_delete),
        )
        .route("/api/models/test", axum::routing::post(models_test))
}
async fn references(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch combo references",
        );
    };
    let c = match combo(&db, &id) {
        Ok(Some(c)) => c,
        Ok(None) => return err(StatusCode::NOT_FOUND, "Combo not found"),
        Err(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch combo references",
            );
        }
    };
    let name = c["name"].as_str().unwrap_or("");
    let result = (|| -> rusqlite::Result<(Vec<Value>, Vec<String>)> {
        let mut st = db.prepare("SELECT name,accessAllow FROM apiKeys WHERE accessRestricted=1")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })?;
        let mut keys = Vec::new();
        for row in rows {
            let (n, raw) = row?;
            if raw
                .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                .is_some_and(|a| a.iter().any(|x| x == name))
            {
                keys.push(json!(n));
            }
        }
        let mut st = db.prepare("SELECT key,value FROM kv WHERE scope='modelAliases'")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut aliases = Vec::new();
        for row in rows {
            let (k, v) = row?;
            if serde_json::from_str::<String>(&v).ok().as_deref() == Some(name) {
                aliases.push(k)
            }
        }
        Ok((keys, aliases))
    })();
    match result {
        Ok((keys, aliases)) => {
            Json(json!({"name":name,"references":{"keys":keys,"aliases":aliases}})).into_response()
        }
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch combo references",
        ),
    }
}

async fn disabled_list(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch disabled models",
        );
    };
    let result = (|| -> rusqlite::Result<serde_json::Map<String, Value>> {
        let mut st = db.prepare("SELECT key,value FROM kv WHERE scope='disabledModels'")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut map = serde_json::Map::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, serde_json::from_str(&v).unwrap_or(json!([])));
        }
        Ok(map)
    })();
    match result {
        Ok(map) => {
            if let Some(provider) = q.get("providerAlias").filter(|v| !v.is_empty()) {
                Json(json!({"ids":map.get(provider).cloned().unwrap_or(json!([]))})).into_response()
            } else {
                Json(json!({"disabled":map})).into_response()
            }
        }
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch disabled models",
        ),
    }
}
async fn disabled_add(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let (Some(provider), Some(ids)) = (
        body["providerAlias"].as_str().filter(|s| !s.is_empty()),
        body["ids"].as_array(),
    ) else {
        return err(StatusCode::BAD_REQUEST, "providerAlias and ids[] required");
    };
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to disable models",
        );
    };
    let old: Result<Option<String>, _> = db
        .query_row(
            "SELECT value FROM kv WHERE scope='disabledModels' AND key=?1",
            [provider],
            |r| r.get(0),
        )
        .optional();
    let Ok(old) = old else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to disable models",
        );
    };
    let mut list: Vec<Value> = old
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    for id in ids {
        if !list.contains(id) {
            list.push(id.clone());
        }
    }
    match crate::db::kv_set(&db, "disabledModels", provider, &json!(list).to_string()) {
        Ok(()) => Json(json!({"success":true})).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to disable models",
        ),
    }
}
async fn disabled_delete(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(provider) = q.get("providerAlias").filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "providerAlias required");
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to enable models");
    };
    let result = (|| -> anyhow::Result<()> {
        if let Some(id) = q.get("id").filter(|s| !s.is_empty()) {
            let old: Option<String> = db
                .query_row(
                    "SELECT value FROM kv WHERE scope='disabledModels' AND key=?1",
                    [provider],
                    |r| r.get(0),
                )
                .optional()?;
            let mut list: Vec<Value> = old
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            list.retain(|v| v.as_str() != Some(id));
            if !list.is_empty() {
                crate::db::kv_set(&db, "disabledModels", provider, &json!(list).to_string())?;
                return Ok(());
            }
        }
        db.execute(
            "DELETE FROM kv WHERE scope='disabledModels' AND key=?1",
            [provider],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => Json(json!({"success":true})).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to enable models"),
    }
}
fn combo(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    db.query_row("SELECT id,name,kind,models,createdAt,updatedAt FROM combos WHERE id=?1",[id],|r|{let raw:String=r.get(3)?;Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"kind":r.get::<_,Option<String>>(2)?,"models":serde_json::from_str::<Value>(&raw).unwrap_or(json!([])),"createdAt":r.get::<_,String>(4)?,"updatedAt":r.get::<_,String>(5)?}))}).optional()
}
fn normalize_members(db: &rusqlite::Connection, models: &Value) -> Value {
    let nodes = (|| -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = db.prepare("SELECT id,data FROM providerNodes")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect()
    })()
    .unwrap_or_default();
    let mut models = models.clone();
    if let Some(members) = models.as_array_mut() {
        for member in members {
            let Some((prefix, rest)) = member.as_str().and_then(|s| s.split_once('/')) else {
                continue;
            };
            if prefix.is_empty() || nodes.iter().any(|(id, _)| id == prefix) {
                continue;
            }
            let matches: Vec<_> = nodes
                .iter()
                .filter(|(_, data)| {
                    serde_json::from_str::<Value>(data)
                        .ok()
                        .and_then(|v| v["prefix"].as_str().map(str::to_owned))
                        .as_deref()
                        == Some(prefix)
                })
                .collect();
            if let [node] = matches.as_slice() {
                *member = json!(format!("{}/{rest}", node.0));
            }
        }
    }
    models
}
fn validate(v: &Value, create: bool) -> Option<&'static str> {
    if create && v["name"].as_str().is_none_or(|s| s.is_empty()) {
        return Some("Name is required");
    }
    if let Some(name) = v.get("name") {
        if name.as_str().is_none_or(|s| {
            s.is_empty()
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        }) {
            return Some("Name can only contain letters, numbers, -, _ and .");
        }
    }
    if create || v.get("models").is_some() {
        let Some(a) = v["models"].as_array() else {
            return Some("models must be an array");
        };
        if a.is_empty() {
            return Some("At least one member is required");
        }
    }
    if let Some(kind) = v.get("kind") {
        if !kind.is_null()
            && !kind.as_str().is_some_and(|s| {
                [
                    "llm",
                    "embedding",
                    "image",
                    "imageToText",
                    "tts",
                    "stt",
                    "video",
                    "music",
                    "systemone",
                    "webSearch",
                    "webFetch",
                ]
                .contains(&s)
            })
        {
            return Some("Invalid kind");
        }
    }
    None
}
async fn list_combos(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch combos");
    };
    let result = (|| -> rusqlite::Result<Vec<Value>> {
        let mut st = db.prepare(
            "SELECT id,name,kind,models,createdAt,updatedAt FROM combos ORDER BY createdAt",
        )?;
        let rows = st.query_map([], |r| {
            let raw: String = r.get(3)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "kind": r.get::<_, Option<String>>(2)?,
                "models": serde_json::from_str::<Value>(&raw).unwrap_or(json!([])),
                "createdAt": r.get::<_, String>(4)?,
                "updatedAt": r.get::<_, String>(5)?,
            }))
        })?;
        rows.collect()
    })();
    match result {
        Ok(combos) => Json(json!({ "combos": combos })).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch combos"),
    }
}
async fn create(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    if let Some(e) = validate(&body, true) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let name = body["name"].as_str().unwrap();
    let collision = db
        .query_row(
            "SELECT 1 FROM kv WHERE scope IN ('modelAliases','injectVariants') AND key=?1",
            [name],
            |_| Ok(()),
        )
        .optional();
    if matches!(collision, Ok(Some(()))) {
        return err(
            StatusCode::CONFLICT,
            "Combo name is already used by a model alias or variant",
        );
    }
    let id = crate::server::uuid_like();
    let models = normalize_members(&db, &body["models"]);
    let r=db.execute("INSERT INTO combos(id,name,kind,models,createdAt,updatedAt) VALUES(?1,?2,?3,?4,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![id,name,body["kind"].as_str(),models.to_string()]);
    if let Err(e) = r {
        return if matches!(e,rusqlite::Error::SqliteFailure(ref x,_) if x.code==rusqlite::ErrorCode::ConstraintViolation)
        {
            err(StatusCode::BAD_REQUEST, "Combo name already exists")
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create combo")
        };
    }
    match combo(&db, &id) {
        Ok(Some(v)) => (StatusCode::CREATED, Json(v)).into_response(),
        _ => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create combo"),
    }
}
async fn get_combo(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    match combo(&db, &id) {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "Combo not found"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch combo"),
    }
}
async fn update(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(e) = validate(&body, false) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let Ok(Some(mut v)) = combo(&db, &id) else {
        return err(StatusCode::NOT_FOUND, "Combo not found");
    };
    if let Some(name) = body["name"].as_str() {
        if db
            .query_row(
                "SELECT 1 FROM kv WHERE scope IN ('modelAliases','injectVariants') AND key=?1",
                [name],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
        {
            return err(
                StatusCode::CONFLICT,
                "Combo name is already used by a model alias or variant",
            );
        }
    }
    for k in ["name", "kind", "models"] {
        if let Some(x) = body.get(k) {
            v[k] = x.clone()
        }
    }
    if body.get("models").is_some() {
        v["models"] = normalize_members(&db, &v["models"]);
    }
    match db.execute("UPDATE combos SET name=?1,kind=?2,models=?3,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?4",params![v["name"].as_str(),v["kind"].as_str(),v["models"].to_string(),id]){Ok(_)=>match combo(&db,&id){Ok(Some(v))=>Json(v).into_response(),_=>err(StatusCode::INTERNAL_SERVER_ERROR,"Failed to update combo")},Err(rusqlite::Error::SqliteFailure(x,_)) if x.code==rusqlite::ErrorCode::ConstraintViolation=>err(StatusCode::BAD_REQUEST,"Combo name already exists"),Err(_)=>err(StatusCode::INTERNAL_SERVER_ERROR,"Failed to update combo")}
}
async fn delete(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    match db.execute("DELETE FROM combos WHERE id=?1", [id]) {
        Ok(0) => err(StatusCode::NOT_FOUND, "Combo not found"),
        Ok(_) => Json(json!({"success":true})).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete combo"),
    }
}
async fn alias_list(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let result = (|| -> rusqlite::Result<Value> {
        let mut st = db.prepare("SELECT key,value FROM kv WHERE scope='modelAliases'")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut map = serde_json::Map::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, serde_json::from_str(&v).unwrap_or(Value::Null));
        }
        Ok(Value::Object(map))
    })();
    match result {
        Ok(v) => Json(json!({"aliases":v})).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch aliases"),
    }
}
async fn alias_put(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let (Some(model), Some(alias)) = (
        body["model"].as_str().filter(|s| !s.is_empty()),
        body["alias"].as_str().filter(|s| !s.is_empty()),
    ) else {
        return err(StatusCode::BAD_REQUEST, "Model and alias required");
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    match crate::db::kv_set(&db, "modelAliases", alias, &json!(model).to_string()) {
        Ok(()) => Json(json!({"success":true,"model":model,"alias":alias})).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update alias"),
    }
}
async fn alias_delete(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(alias) = q.get("alias").filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "Alias required");
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    match db.execute(
        "DELETE FROM kv WHERE scope='modelAliases' AND key=?1",
        [alias],
    ) {
        Ok(_) => Json(json!({"success":true})).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete alias"),
    }
}

fn sanitize_caps(caps: &Value) -> Option<Map<String, Value>> {
    let obj = caps.as_object()?;
    let mut clean = Map::new();
    for key in ["vision", "search", "reasoning"] {
        if let Some(b) = obj.get(key).and_then(Value::as_bool) {
            clean.insert(key.into(), json!(b));
        }
    }
    (!clean.is_empty()).then_some(clean)
}

fn sanitize_caps_override(val: &Value) -> Option<Map<String, Value>> {
    let obj = val.as_object()?;
    let mut clean = Map::new();
    for key in ["vision", "search", "reasoning"] {
        if let Some(b) = obj.get(key).and_then(Value::as_bool) {
            clean.insert(key.into(), json!(b));
        }
    }
    for key in ["contextWindow", "maxOutput"] {
        if let Some(n) = obj.get(key).and_then(Value::as_u64).filter(|&n| n > 0) {
            clean.insert(key.into(), json!(n));
        }
    }
    Some(clean)
}

fn read_custom_models(db: &rusqlite::Connection) -> rusqlite::Result<Vec<Value>> {
    let mut st = db.prepare("SELECT value FROM kv WHERE scope='customModels'")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok(v) = serde_json::from_str::<Value>(&row?) {
            out.push(v);
        }
    }
    Ok(out)
}

fn read_kv_map(db: &rusqlite::Connection, scope: &str) -> rusqlite::Result<Map<String, Value>> {
    let mut st = db.prepare("SELECT key,value FROM kv WHERE scope=?1")?;
    let rows = st.query_map([scope], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut map = Map::new();
    for row in rows {
        let (k, v) = row?;
        map.insert(k, serde_json::from_str(&v).unwrap_or(Value::Null));
    }
    Ok(map)
}

fn model_caps(m: &crate::providers::Model) -> Value {
    let meta = &m.metadata;
    let caps = meta.get("capabilities").and_then(Value::as_object);
    let bool_cap = |k: &str| -> bool {
        meta.get(k)
            .and_then(Value::as_bool)
            .or_else(|| caps.and_then(|c| c.get(k)).and_then(Value::as_bool))
            .unwrap_or(false)
    };
    let num_cap = |k1: &str, k2: &str| -> Value {
        meta.get(k1)
            .or_else(|| meta.get(k2))
            .or_else(|| caps.and_then(|c| c.get(k1).or_else(|| c.get(k2))))
            .and_then(Value::as_u64)
            .map(|n| json!(n))
            .unwrap_or(Value::Null)
    };
    json!({
        "vision": bool_cap("vision"),
        "search": bool_cap("search"),
        "reasoning": bool_cap("reasoning"),
        "contextWindow": num_cap("contextWindow", "contextLength"),
        "maxOutput": num_cap("maxOutput", "maxTokens"),
    })
}

async fn models_list(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch models");
    };
    let Ok(aliases) = read_kv_map(&db, "modelAliases") else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch models");
    };
    let Ok(disabled) = read_kv_map(&db, "disabledModels") else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch models");
    };
    let Ok(custom_rows) = read_custom_models(&db) else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch models");
    };

    let mut models: Vec<Value> = Vec::new();
    let mut index_by_routed: HashMap<String, usize> = HashMap::new();
    let mut index_by_full: HashMap<String, usize> = HashMap::new();

    for p in crate::providers::all() {
        let disabled_alias = disabled.get(&p.alias).and_then(Value::as_array);
        let disabled_id = disabled.get(&p.id).and_then(Value::as_array);
        for m in &p.models {
            let is_disabled = disabled_alias
                .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&m.id)))
                || disabled_id.is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&m.id)));
            if is_disabled {
                continue;
            }
            let full_model = format!("{}/{}", p.id, m.id);
            let routed_model = format!("{}/{}", p.alias, m.id);
            let alias = aliases
                .get(&full_model)
                .and_then(Value::as_str)
                .unwrap_or(&m.id);
            let idx = models.len();
            index_by_routed.insert(routed_model.clone(), idx);
            index_by_full.insert(full_model.clone(), idx);
            models.push(json!({
                "provider": p.id,
                "model": m.id,
                "name": m.name.as_deref().unwrap_or(&m.id),
                "fullModel": full_model,
                "routedModel": routed_model,
                "alias": alias,
                "caps": model_caps(m),
            }));
        }
    }

    for cm in custom_rows {
        let Some(id) = cm["id"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let kind = cm["kind"]
            .as_str()
            .or_else(|| cm["type"].as_str())
            .unwrap_or("llm");
        if kind != "llm" {
            continue;
        }
        let Some(provider_alias) = cm["providerAlias"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let key = format!("{provider_alias}/{id}");
        if let Some(&idx) = index_by_routed.get(&key).or_else(|| index_by_full.get(&key)) {
            if let Some(ov) = sanitize_caps_override(&cm["capsOverride"]).filter(|m| !m.is_empty())
            {
                if let Some(caps_obj) = models[idx]["caps"].as_object_mut() {
                    for (k, v) in ov {
                        caps_obj.insert(k, v);
                    }
                }
            }
            continue;
        }
        let mut caps = json!({
            "vision": false,
            "search": false,
            "reasoning": false,
            "contextWindow": Value::Null,
            "maxOutput": Value::Null,
        });
        if let Some(caps_obj) = caps.as_object_mut() {
            if let Some(c) = cm["caps"].as_object() {
                for (k, v) in c {
                    caps_obj.insert(k.clone(), v.clone());
                }
            }
            if let Some(ov) = sanitize_caps_override(&cm["capsOverride"]) {
                for (k, v) in ov {
                    caps_obj.insert(k, v);
                }
            }
        }
        let alias = aliases
            .get(&key)
            .and_then(Value::as_str)
            .unwrap_or(id);
        models.push(json!({
            "provider": provider_alias,
            "model": id,
            "name": cm["name"].as_str().unwrap_or(id),
            "fullModel": key,
            "routedModel": key,
            "alias": alias,
            "caps": caps,
        }));
    }

    Json(json!({ "models": models })).into_response()
}

async fn models_put(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let (Some(model), Some(alias)) = (
        body["model"].as_str().filter(|s| !s.is_empty()),
        body["alias"].as_str().filter(|s| !s.is_empty()),
    ) else {
        return err(StatusCode::BAD_REQUEST, "Model and alias required");
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update alias");
    };
    let Ok(aliases) = read_kv_map(&db, "modelAliases") else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update alias");
    };
    if aliases
        .iter()
        .any(|(k, v)| k != model && v.as_str() == Some(alias))
    {
        return err(StatusCode::BAD_REQUEST, "Alias already in use");
    }
    match crate::db::kv_set(&db, "modelAliases", model, &json!(alias).to_string()) {
        Ok(()) => Json(json!({ "success": true, "model": model, "alias": alias })).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update alias"),
    }
}

async fn custom_list(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch custom models",
        );
    };
    match read_custom_models(&db) {
        Ok(models) => Json(json!({ "models": models })).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch custom models",
        ),
    }
}

async fn custom_post(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let (Some(provider_alias), Some(id)) = (
        body["providerAlias"].as_str().filter(|s| !s.is_empty()),
        body["id"].as_str().filter(|s| !s.is_empty()),
    ) else {
        return err(StatusCode::BAD_REQUEST, "providerAlias and id required");
    };
    let model_type = body["type"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("llm");
    let mut entry = json!({
        "providerAlias": provider_alias,
        "id": id,
        "type": model_type,
    });
    if let Some(name) = body["name"].as_str().filter(|s| !s.is_empty()) {
        entry["name"] = json!(name);
    }
    if let Some(caps) = sanitize_caps(&body["caps"]) {
        entry["caps"] = Value::Object(caps);
    }
    if let Some(ov) = sanitize_caps_override(&body["capsOverride"]) {
        entry["capsOverride"] = Value::Object(ov);
    }
    if model_type == "stt"
        && let Some(tr) = body["transport"].as_str().map(str::trim).filter(|s| !s.is_empty())
    {
        entry["transport"] = json!(tr);
    }
    let key = format!("{provider_alias}|{model_type}|{id}");
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to add custom model",
        );
    };
    match crate::db::kv_set(&db, "customModels", &key, &entry.to_string()) {
        Ok(()) => Json(json!({ "success": true, "added": entry })).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to add custom model",
        ),
    }
}

async fn custom_patch(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let (Some(provider_alias), Some(id)) = (
        body["providerAlias"].as_str().filter(|s| !s.is_empty()),
        body["id"].as_str().filter(|s| !s.is_empty()),
    ) else {
        return err(StatusCode::BAD_REQUEST, "providerAlias and id required");
    };
    let has_override = body.get("capsOverride").is_some();
    let has_name = body.get("name").is_some();
    if !has_override && !has_name {
        return err(StatusCode::BAD_REQUEST, "capsOverride or name required");
    }
    let clean_override = if let Some(raw_ov) = body.get("capsOverride") {
        let Some(ov) = sanitize_caps_override(raw_ov) else {
            return err(StatusCode::BAD_REQUEST, "capsOverride must be an object");
        };
        Some(ov)
    } else {
        None
    };
    let model_type = body["type"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("llm");
    let key = format!("{provider_alias}|{model_type}|{id}");
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update custom model",
        );
    };
    let existing: Option<Value> = db
        .query_row(
            "SELECT value FROM kv WHERE scope='customModels' AND key=?1",
            [&key],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let mut entry = existing.unwrap_or_else(|| {
        json!({
            "providerAlias": provider_alias,
            "id": id,
            "type": model_type,
        })
    });
    if let Some(name) = body["name"].as_str().filter(|s| !s.is_empty()) {
        entry["name"] = json!(name);
    }
    if let Some(ov) = clean_override {
        entry["capsOverride"] = Value::Object(ov);
    }
    match crate::db::kv_set(&db, "customModels", &key, &entry.to_string()) {
        Ok(()) => Json(json!({ "success": true, "added": entry })).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update custom model",
        ),
    }
}

async fn custom_delete(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (Some(provider_alias), Some(id)) = (
        q.get("providerAlias").filter(|s| !s.is_empty()),
        q.get("id").filter(|s| !s.is_empty()),
    ) else {
        return err(StatusCode::BAD_REQUEST, "providerAlias and id required");
    };
    let model_type = q
        .get("type")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("llm");
    let key = format!("{provider_alias}|{model_type}|{id}");
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete custom model",
        );
    };
    match db.execute(
        "DELETE FROM kv WHERE scope='customModels' AND key=?1",
        [&key],
    ) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete custom model",
        ),
    }
}

async fn availability_get(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch model availability",
        );
    };
    let result = (|| -> rusqlite::Result<Vec<Value>> {
        let mut st = db.prepare("SELECT id,provider,name,email,data FROM providerConnections")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut models = Vec::new();
        for row in rows {
            let (id, provider, name, email, raw) = row?;
            let data: Value = crate::crypto::parse_data(&raw).unwrap_or(json!({}));
            let conn_name = name
                .filter(|s| !s.is_empty())
                .or(email.filter(|s| !s.is_empty()))
                .unwrap_or_else(|| id.clone());
            let last_error = data["lastError"].clone();
            let mut lock_count = 0usize;
            if let Some(obj) = data.as_object() {
                for (k, v) in obj {
                    let Some(model_suffix) = k.strip_prefix("modelLock_") else {
                        continue;
                    };
                    let Some(until) = v.as_str().filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    let active = db
                        .query_row(
                            "SELECT datetime(?1) > datetime('now')",
                            [until],
                            |r| r.get::<_, bool>(0),
                        )
                        .unwrap_or(false);
                    if active {
                        lock_count += 1;
                        models.push(json!({
                            "provider": provider,
                            "model": if model_suffix.is_empty() { "__all" } else { model_suffix },
                            "status": "cooldown",
                            "until": until,
                            "connectionId": id,
                            "connectionName": conn_name,
                            "lastError": last_error,
                        }));
                    }
                }
            }
            if lock_count == 0 && data["testStatus"].as_str() == Some("unavailable") {
                models.push(json!({
                    "provider": provider,
                    "model": "__all",
                    "status": "unavailable",
                    "connectionId": id,
                    "connectionName": conn_name,
                    "lastError": last_error,
                }));
            }
        }
        Ok(models)
    })();
    match result {
        Ok(models) => {
            let count = models.len();
            Json(json!({ "models": models, "unavailableCount": count })).into_response()
        }
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch model availability",
        ),
    }
}

async fn availability_post(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let (Some(action), Some(provider), Some(model)) = (
        body["action"].as_str(),
        body["provider"].as_str().filter(|s| !s.is_empty()),
        body["model"].as_str().filter(|s| !s.is_empty()),
    ) else {
        return err(StatusCode::BAD_REQUEST, "Invalid request");
    };
    if action != "clearCooldown" {
        return err(StatusCode::BAD_REQUEST, "Invalid request");
    }
    let lock_key = format!("modelLock_{model}");
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to clear cooldown");
    };
    let result = (|| -> rusqlite::Result<()> {
        let mut st = db.prepare("SELECT id,data FROM providerConnections WHERE provider=?1")?;
        let rows = st
            .query_map([provider], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(st);
        for (id, raw) in rows {
            let mut data: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            let Some(obj) = data.as_object_mut() else {
                continue;
            };
            if obj.get(&lock_key).is_none_or(Value::is_null) {
                continue;
            }
            obj.insert(lock_key.clone(), Value::Null);
            if obj.get("testStatus").and_then(Value::as_str) == Some("unavailable") {
                obj.insert("testStatus".into(), json!("active"));
                obj.insert("lastError".into(), Value::Null);
                obj.insert("lastErrorAt".into(), Value::Null);
                obj.insert("backoffLevel".into(), json!(0));
            }
            db.execute(
                "UPDATE providerConnections SET data=?1,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2",
                params![data.to_string(), id],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to clear cooldown"),
    }
}

#[derive(Default, Clone)]
struct CatalogSyncState {
    syncing: bool,
    last_synced_at: Option<String>,
    last_error: Option<String>,
    models: usize,
    providers: usize,
}

fn catalog_sync_state() -> &'static Mutex<CatalogSyncState> {
    static STATE: OnceLock<Mutex<CatalogSyncState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(CatalogSyncState::default()))
}

async fn catalog_sync_get() -> Response {
    let st = catalog_sync_state().lock().unwrap().clone();
    let catalog = st.last_synced_at.as_ref().map(|ts| {
        json!({
            "syncedAt": ts,
            "models": st.models,
            "providers": st.providers,
        })
    });
    Json(json!({
        "syncing": st.syncing,
        "lastSyncedAt": st.last_synced_at,
        "lastError": st.last_error,
        "catalog": catalog,
    }))
    .into_response()
}

async fn catalog_sync_post() -> Response {
    {
        let mut st = catalog_sync_state().lock().unwrap();
        if st.syncing {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                st.last_error.as_deref().unwrap_or("sync in progress"),
            );
        }
        st.syncing = true;
    }
    let url = std::env::var("CATALOG_SYNC_URL")
        .unwrap_or_else(|_| "https://models.dev/api.json".to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();
    let res = client.get(&url).send().await;
    let payload = match res {
        Ok(resp) if resp.status().is_success() => Ok(resp.text().await.unwrap_or_default()),
        Ok(resp) => Err(format!("Catalog fetch failed ({})", resp.status().as_u16())),
        Err(e) => Err(e.to_string()),
    };
    let mut st = catalog_sync_state().lock().unwrap();
    st.syncing = false;
    match payload {
        Ok(text) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text)
                && let Some(obj) = parsed.as_object()
            {
                let providers = obj.len();
                let models: usize = obj
                    .values()
                    .filter_map(|p| p.get("models").and_then(Value::as_object))
                    .map(Map::len)
                    .sum();
                let now = format!(
                    "{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                );
                st.last_synced_at = Some(now.clone());
                st.last_error = None;
                st.models = models;
                st.providers = providers;
                return Json(json!({
                    "success": true,
                    "result": { "syncedAt": now, "models": models, "providers": providers }
                }))
                .into_response();
            }
            st.last_error = Some("Invalid catalog payload".to_string());
            err(StatusCode::SERVICE_UNAVAILABLE, "Invalid catalog payload")
        }
        Err(msg) => {
            st.last_error = Some(msg.clone());
            err(StatusCode::SERVICE_UNAVAILABLE, &msg)
        }
    }
}

fn classify_test_stage(status: Option<u16>, error: Option<&str>) -> &'static str {
    if let Some(code) = status {
        return match code {
            401 | 403 => "auth",
            404 | 400 | 422 => "model",
            429 => "rate_limit",
            500..=599 => "upstream",
            _ => "upstream",
        };
    }
    let msg = error.unwrap_or("").to_ascii_lowercase();
    if msg.contains("timeout") || msg.contains("timed out") {
        "timeout"
    } else if msg.contains("proxy") {
        "proxy"
    } else if msg.contains("dns") || msg.contains("connect") || msg.contains("network") {
        "network"
    } else if msg.contains("unauthorized") || msg.contains("401") || msg.contains("403") {
        "auth"
    } else {
        "upstream"
    }
}

async fn models_test(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(model_raw) = body["model"].as_str().filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "Model required");
    };
    let _kind = body["kind"].as_str().unwrap_or("llm");
    let (provider_part, model_id) = match model_raw.split_once('/') {
        Some((p, m)) if !p.is_empty() && !m.is_empty() => (p.to_string(), m.to_string()),
        _ => {
            return Json(json!({
                "ok": false,
                "stage": "model",
                "error": "Model must be in provider/model format",
            }))
            .into_response();
        }
    };
    let provider_id = crate::providers::lookup(&provider_part)
        .map(|p| p.id.clone())
        .unwrap_or_else(|| provider_part.clone());
    let creds: Option<(Option<String>, Option<String>, Value)> = {
        let Ok(db) = s.db.lock() else {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
        };
        db.query_row(
            "SELECT apiKey,accessToken,data FROM providerConnections WHERE (provider=?1 OR provider=?2) AND isActive=1 ORDER BY priority LIMIT 1",
            params![provider_id, provider_part],
            |r| {
                let raw: String = r.get(2)?;
                let data: Value = crate::crypto::parse_data(&raw).unwrap_or(json!({}));
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    data["providerSpecificData"].clone(),
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
    };
    let Some((api_key, access_token, psd)) = creds else {
        return Json(json!({
            "ok": false,
            "stage": "auth",
            "error": "No active connection for provider",
        }))
        .into_response();
    };
    let base_url = psd["baseUrl"].as_str().map(str::to_owned);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();
    let started = std::time::Instant::now();
    let req = crate::executors::ExecutorRequest {
        provider: provider_id,
        model: model_id,
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
        Ok(resp) => {
            let status = resp.status().as_u16();
            let latency_ms = started.elapsed().as_millis() as u64;
            if resp.status().is_success() {
                Json(json!({
                    "ok": true,
                    "stage": "ok",
                    "status": status,
                    "latencyMs": latency_ms,
                }))
                .into_response()
            } else {
                let text = resp.text().await.unwrap_or_default();
                let snippet: String = text.chars().take(200).collect();
                Json(json!({
                    "ok": false,
                    "stage": classify_test_stage(Some(status), Some(&snippet)),
                    "status": status,
                    "latencyMs": latency_ms,
                    "error": snippet,
                }))
                .into_response()
            }
        }
        Err(e) => {
            let msg = e.to_string();
            Json(json!({
                "ok": false,
                "stage": classify_test_stage(None, Some(&msg)),
                "error": msg,
            }))
            .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[tokio::test]
    async fn combos_normalize_and_list() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO providerNodes(id,type,name,data,createdAt,updatedAt) VALUES('node1','openai-compatible','Node',?1,'now','now')", [json!({"prefix":"custom"}).to_string()]).unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        let app = routes().with_state(state);
        let request = Request::post("/api/combos")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"test","models":["custom/model","other/model"]}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(body["models"], json!(["node1/model", "other/model"]));
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/combos/{}", body["id"].as_str().unwrap()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(body["models"][0], "node1/model");

        let combo_id = body["id"].as_str().unwrap().to_string();

        let response = app
            .clone()
            .oneshot(Request::get("/api/combos").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let list_body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(list_body["combos"].as_array().unwrap().len(), 1);
        assert_eq!(list_body["combos"][0]["id"], combo_id);

        let response = app
            .clone()
            .oneshot(
                Request::put(format!("/api/combos/{combo_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"test2","models":["custom/m2"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let upd: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(upd["name"], "test2");
        assert_eq!(upd["models"], json!(["node1/m2"]));

        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/combos/{combo_id}/references"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let refs: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(refs["name"], "test2");
        assert_eq!(refs["references"]["keys"], json!([]));
        assert_eq!(refs["references"]["aliases"], json!([]));

        let response = app
            .oneshot(
                Request::delete(format!("/api/combos/{combo_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let del: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(del["success"], true);
    }

    #[tokio::test]
    async fn models_catalog_custom_alias_disabled_availability_sync_and_test() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute(
            "INSERT INTO providerConnections(id,provider,authType,name,data,isActive,priority,createdAt,updatedAt) VALUES('c1','openai','apikey','Conn1',?1,1,1,'now','now')",
            [json!({
                "testStatus": "unavailable",
                "lastError": "rate limit",
                "modelLock_gpt-4o": "2099-01-01T00:00:00.000Z"
            })
            .to_string()],
        )
        .unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        let app = routes().with_state(state);

        // /api/models/alias GET, PUT, DELETE
        let res = app
            .clone()
            .oneshot(
                Request::put("/api/models/alias")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"openai/gpt-4o","alias":"fast"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .clone()
            .oneshot(Request::get("/api/models/alias").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(body["aliases"]["fast"], "openai/gpt-4o");
        let res = app
            .clone()
            .oneshot(
                Request::delete("/api/models/alias?alias=fast")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // /api/models/disabled POST, GET, DELETE
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/models/disabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"providerAlias":"oa","ids":["gpt-4o"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/models/disabled?providerAlias=oa")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(body["ids"], json!(["gpt-4o"]));
        let res = app
            .clone()
            .oneshot(
                Request::delete("/api/models/disabled?providerAlias=oa&id=gpt-4o")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // /api/models/custom POST, GET, PATCH, DELETE
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/models/custom")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"providerAlias":"oa","id":"my-custom","name":"My Custom","caps":{"vision":true}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .clone()
            .oneshot(Request::get("/api/models/custom").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(body["models"].as_array().unwrap().len(), 1);
        let res = app
            .clone()
            .oneshot(
                Request::patch("/api/models/custom")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"providerAlias":"oa","id":"my-custom","capsOverride":{"contextWindow":128000}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // /api/models GET & PUT
        let res = app
            .clone()
            .oneshot(Request::get("/api/models").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 2_000_000).await.unwrap()).unwrap();
        assert!(
            body["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["fullModel"] == "oa/my-custom" && m["caps"]["contextWindow"] == 128000)
        );
        let res = app
            .clone()
            .oneshot(
                Request::put("/api/models")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"oa/my-custom","alias":"mc"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(
                Request::delete("/api/models/custom?providerAlias=oa&id=my-custom")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // /api/models/availability GET & POST
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/models/availability")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(body["unavailableCount"], 1);
        assert_eq!(body["models"][0]["status"], "cooldown");
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/models/availability")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"action":"clearCooldown","provider":"openai","model":"gpt-4o"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/models/availability")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(body["unavailableCount"], 0);

        // /api/models/catalog-sync GET
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/models/catalog-sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // /api/models/test POST (missing model -> 400; missing connection -> stage=auth)
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/models/test")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = app
            .oneshot(
                Request::post("/api/models/test")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"anthropic/claude-3-5-sonnet"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 10000).await.unwrap()).unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["stage"], "auth");
    }
}
