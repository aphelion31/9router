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
// ponytail: provider-specific discovery, validation and connection tests require unported services.
async fn unavailable() -> Response {
    err(StatusCode::NOT_IMPLEMENTED, "Service not implemented")
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/providers/{id}",
            get(unavailable).put(unavailable).delete(unavailable),
        )
        .route("/api/providers/{id}/models", get(unavailable))
        .route(
            "/api/providers/{id}/overrides",
            get(unavailable).put(unavailable),
        )
        .route("/api/providers/{id}/test", post(unavailable))
        .route("/api/providers/{id}/test-models", post(unavailable))
        .route("/api/providers/bulk-proxy", post(unavailable))
        .route("/api/providers/client", get(unavailable))
        .route("/api/providers/kilo/free-models", get(unavailable))
        .route("/api/providers/suggested-models", get(unavailable))
        .route("/api/providers/test-batch", post(unavailable))
        .route("/api/providers/test-batch/stop", post(unavailable))
        .route("/api/providers/validate", post(unavailable))
        .route(
            "/api/providers/web-login",
            get(unavailable).post(unavailable).delete(unavailable),
        )
        .route("/api/provider-nodes", get(list_nodes).post(create_node))
        .route(
            "/api/provider-nodes/{id}",
            axum::routing::put(update_node).delete(delete_node),
        )
        .route("/api/provider-nodes/validate", post(unavailable))
}

fn node(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    db.query_row(
        "SELECT id,type,name,data,createdAt,updatedAt FROM providerNodes WHERE id=?1",
        [id],
        |r| {
            let raw: String = r.get(3)?;
            let mut v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), json!(r.get::<_, String>(0)?));
                o.insert("type".into(), json!(r.get::<_, String>(1)?));
                o.insert("name".into(), json!(r.get::<_, String>(2)?));
                o.insert("createdAt".into(), json!(r.get::<_, String>(4)?));
                o.insert("updatedAt".into(), json!(r.get::<_, String>(5)?));
            }
            Ok(v)
        },
    )
    .optional()
}

fn node_fields(body: &Value, kind: &str, creating: bool) -> Result<Value, &'static str> {
    let name = body["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("Name is required")?;
    let prefix = body["prefix"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("Prefix is required")?;
    if ![
        "openai-compatible",
        "anthropic-compatible",
        "custom-embedding",
    ]
    .contains(&kind)
    {
        return Err("Invalid provider node type");
    }
    if kind == "openai-compatible"
        && !body["apiType"]
            .as_str()
            .is_some_and(|s| ["chat", "responses"].contains(&s))
    {
        return Err("Invalid OpenAI compatible API type");
    }
    let default = if kind == "anthropic-compatible" {
        "https://api.anthropic.com/v1"
    } else {
        "https://api.openai.com/v1"
    };
    let base = body["baseUrl"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if !creating && base.is_none() {
        return Err("Base URL is required");
    }
    let mut base = base.unwrap_or(default).to_string();
    if kind != "openai-compatible" {
        base = base.trim_end_matches('/').to_string();
        let suffix = if kind == "anthropic-compatible" {
            "/messages"
        } else {
            "/embeddings"
        };
        if let Some(v) = base.strip_suffix(suffix) {
            base = v.to_string()
        }
    }
    let mut data = json!({"name":name,"prefix":prefix,"baseUrl":base});
    if kind == "openai-compatible" {
        data["apiType"] = body["apiType"].clone();
    }
    Ok(data)
}

async fn create_node(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let kind = body["type"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("openai-compatible");
    let data = match node_fields(&body, kind, true) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let Ok(db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let name = data["name"].as_str().unwrap();
    match db
        .query_row(
            "SELECT 1 FROM providerNodes WHERE lower(trim(name))=lower(?1)",
            [name],
            |_| Ok(()),
        )
        .optional()
    {
        Ok(Some(())) => return err(StatusCode::BAD_REQUEST, "Provider name already exists"),
        Err(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create provider node",
            );
        }
        _ => {}
    }
    let prefix = match kind {
        "openai-compatible" => format!("openai-compatible-{}-", data["apiType"].as_str().unwrap()),
        "anthropic-compatible" => "anthropic-compatible-".into(),
        _ => "custom-embedding-".into(),
    };
    let id = format!("{}{}", prefix, crate::server::uuid_like());
    if db.execute("INSERT INTO providerNodes(id,type,name,data,createdAt,updatedAt) VALUES(?1,?2,?3,?4,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![id,kind,name,data.to_string()]).is_err() { return err(StatusCode::INTERNAL_SERVER_ERROR,"Failed to create provider node") }
    match node(&db, &id) {
        Ok(Some(v)) => (StatusCode::CREATED, Json(json!({"node":v}))).into_response(),
        _ => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create provider node",
        ),
    }
}
async fn update_node(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Ok(mut db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let existing = match node(&db, &id) {
        Ok(Some(v)) => v,
        Ok(None) => return err(StatusCode::NOT_FOUND, "Provider node not found"),
        Err(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to update provider node",
            );
        }
    };
    let kind = existing["type"].as_str().unwrap_or("");
    let data = match node_fields(&body, kind, false) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let Ok(tx) = db.transaction() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update provider node",
        );
    };
    let result = (|| -> rusqlite::Result<()> {
        tx.execute("UPDATE providerNodes SET name=?1,data=?2,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?3",params![data["name"].as_str(),data.to_string(),id])?;
        let mut st = tx.prepare("SELECT id,data FROM providerConnections WHERE provider=?1")?;
        let rows = st
            .query_map([&id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(st);
        for (cid, raw) in rows {
            let mut v: Value = serde_json::from_str(&raw).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            if !v.is_object() {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if !v["providerSpecificData"].is_object() {
                v["providerSpecificData"] = json!({});
            }
            for key in ["prefix", "baseUrl", "apiType"] {
                if let Some(x) = data.get(key) {
                    v["providerSpecificData"][key] = x.clone();
                }
            }
            v["providerSpecificData"]["nodeName"] = data["name"].clone();
            tx.execute("UPDATE providerConnections SET data=?1,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2",params![v.to_string(),cid])?;
        }
        Ok(())
    })();
    if result.is_err() || tx.commit().is_err() {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update provider node",
        );
    }
    match node(&db, &id) {
        Ok(Some(v)) => Json(json!({"node":v})).into_response(),
        _ => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update provider node",
        ),
    }
}
async fn delete_node(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(mut db) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Database unavailable");
    };
    let Ok(tx) = db.transaction() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete provider node",
        );
    };
    let result = (|| -> rusqlite::Result<usize> {
        let n = tx.execute("DELETE FROM providerNodes WHERE id=?1", [&id])?;
        if n > 0 {
            tx.execute("DELETE FROM providerConnections WHERE provider=?1", [&id])?;
            tx.execute(
                "DELETE FROM kv WHERE scope='customModels' AND substr(key,1,length(?1)+1)=?1||'|'",
                [&id],
            )?;
        }
        Ok(n)
    })();
    match result {
        Ok(0) => err(StatusCode::NOT_FOUND, "Provider node not found"),
        Ok(_) => {
            if tx.commit().is_ok() {
                Json(json!({"success":true})).into_response()
            } else {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to delete provider node",
                )
            }
        }
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete provider node",
        ),
    }
}
async fn list_nodes(State(s): State<AppState>) -> Response {
    let Ok(db) = s.db.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch provider nodes",
        );
    };
    let result = (|| -> rusqlite::Result<Vec<Value>> {
        let mut st = db.prepare(
            "SELECT id,type,name,data,createdAt,updatedAt FROM providerNodes ORDER BY createdAt",
        )?;
        let rows = st.query_map([], |r| {
            let raw: String = r.get(3)?;
            let mut v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), json!(r.get::<_, String>(0)?));
                o.insert("type".into(), json!(r.get::<_, Option<String>>(1)?));
                o.insert("name".into(), json!(r.get::<_, Option<String>>(2)?));
                o.insert("createdAt".into(), json!(r.get::<_, String>(4)?));
                o.insert("updatedAt".into(), json!(r.get::<_, String>(5)?));
            }
            Ok(v)
        })?;
        rows.collect()
    })();
    match result {
        Ok(nodes) => Json(json!({"nodes":nodes})).into_response(),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to fetch provider nodes",
        ),
    }
}
