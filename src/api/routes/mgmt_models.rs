use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({"error":msg}))).into_response()
}
// ponytail: provider catalog and model test services need a separate port.
async fn unavailable() -> Response {
    err(StatusCode::NOT_IMPLEMENTED, "Service not implemented")
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/combos", post(create))
        .route(
            "/api/combos/{id}",
            get(get_combo).put(update).delete(delete),
        )
        .route("/api/combos/{id}/references", get(references))
        .route("/api/models", get(unavailable).put(unavailable))
        .route(
            "/api/models/alias",
            get(alias_list).put(alias_put).delete(alias_delete),
        )
        .route(
            "/api/models/availability",
            get(unavailable).post(unavailable),
        )
        .route(
            "/api/models/catalog-sync",
            get(unavailable).post(unavailable),
        )
        .route(
            "/api/models/custom",
            get(unavailable)
                .post(unavailable)
                .patch(unavailable)
                .delete(unavailable),
        )
        .route(
            "/api/models/disabled",
            get(disabled_list)
                .post(disabled_add)
                .delete(disabled_delete),
        )
        .route("/api/models/test", post(unavailable))
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
    let r=db.execute("INSERT INTO combos(id,name,kind,models,createdAt,updatedAt) VALUES(?1,?2,?3,?4,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![id,name,body["kind"].as_str(),body["models"].to_string()]);
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
