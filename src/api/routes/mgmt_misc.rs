use crate::server::AppState;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Map, Value, json};

const DEFAULT_PRICING_JSON: &str = include_str!("../../providers/pricing.json");

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn cors_headers() -> [(header::HeaderName, &'static str); 3] {
    [
        (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
        (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
    ]
}

pub fn public_routes() -> Router<AppState> {
    Router::new().route(
        "/api/health",
        get(health).options(|| async { (StatusCode::NO_CONTENT, cors_headers(), "") }),
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/tags",
            get(tags).options(|| async { (cors_headers(), "") }),
        )
        .route(
            "/api/pricing",
            get(get_pricing).patch(patch_pricing).delete(delete_pricing),
        )
}

async fn health(State(s): State<AppState>) -> Response {
    let mut db_ok = false;
    let mut total: i64 = 0;
    let mut active: i64 = 0;

    if let Ok(conn) = s.db.lock() {
        db_ok = conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)).is_ok();
        if db_ok {
            if let Ok((t, a)) = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN isActive IS NULL OR isActive != 0 THEN 1 ELSE 0 END), 0) FROM providerConnections",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            ) {
                total = t;
                active = a;
            }
        }
    }

    let degraded = !db_ok;
    let status = if degraded {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (
        status,
        cors_headers(),
        Json(json!({
            "ok": !degraded,
            "degraded": degraded,
            "checks": {
                "db": if db_ok { "ok" } else { "error" },
                "connections": { "total": total, "active": active },
                "breakers": { "total": 0, "open": 0, "halfOpen": 0, "preTrip": 0 }
            }
        })),
    )
        .into_response()
}

async fn tags() -> Response {
    (
        cors_headers(),
        Json(json!({
            "models": [
                {
                    "name": "llama3.2",
                    "modified_at": "2025-12-26T00:00:00Z",
                    "size": 2000000000i64,
                    "digest": "abc123def456",
                    "details": {
                        "format": "gguf",
                        "family": "llama",
                        "parameter_size": "3B",
                        "quantization_level": "Q4_K_M"
                    }
                },
                {
                    "name": "qwen2.5",
                    "modified_at": "2025-12-26T00:00:00Z",
                    "size": 4000000000i64,
                    "digest": "def456abc123",
                    "details": {
                        "format": "gguf",
                        "family": "qwen",
                        "parameter_size": "7B",
                        "quantization_level": "Q4_K_M"
                    }
                }
            ]
        })),
    )
        .into_response()
}

fn load_user_pricing(conn: &rusqlite::Connection) -> rusqlite::Result<Map<String, Value>> {
    let mut stmt = conn.prepare("SELECT key, value FROM kv WHERE scope = 'pricing'")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Map::new();
    for row in rows {
        let (provider, raw) = row?;
        if let Ok(Value::Object(models)) = serde_json::from_str::<Value>(&raw) {
            out.insert(provider, Value::Object(models));
        }
    }
    Ok(out)
}

fn build_merged_pricing(conn: &rusqlite::Connection) -> Result<Value, String> {
    let defaults: Map<String, Value> =
        serde_json::from_str(DEFAULT_PRICING_JSON).map_err(|e| e.to_string())?;
    let user = load_user_pricing(conn).map_err(|e| e.to_string())?;
    let mut merged = Map::new();

    for (provider, def_models_val) in defaults {
        let mut models_map = def_models_val.as_object().cloned().unwrap_or_default();
        if let Some(Value::Object(user_models)) = user.get(&provider) {
            for (model, user_price) in user_models {
                if let (Some(Value::Object(base_price)), Value::Object(up)) =
                    (models_map.get(model), user_price)
                {
                    let mut next = base_price.clone();
                    for (k, v) in up {
                        next.insert(k.clone(), v.clone());
                    }
                    models_map.insert(model.clone(), Value::Object(next));
                } else {
                    models_map.insert(model.clone(), user_price.clone());
                }
            }
        }
        merged.insert(provider, Value::Object(models_map));
    }

    for (provider, user_models_val) in user {
        if !merged.contains_key(&provider) {
            merged.insert(provider, user_models_val);
        }
    }

    Ok(Value::Object(merged))
}

async fn get_pricing(State(s): State<AppState>) -> Response {
    let Ok(conn) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch pricing");
    };
    match build_merged_pricing(&conn) {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch pricing"),
    }
}

async fn patch_pricing(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(providers) = body.as_object() else {
        return err(StatusCode::BAD_REQUEST, "Invalid pricing data format");
    };
    const VALID_FIELDS: &[&str] = &["input", "output", "cached", "reasoning", "cache_creation"];
    for (provider, models_val) in providers {
        let Some(models) = models_val.as_object() else {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("Invalid pricing for provider: {provider}"),
            );
        };
        for (model, price_val) in models {
            let Some(price) = price_val.as_object() else {
                return err(
                    StatusCode::BAD_REQUEST,
                    &format!("Invalid pricing for model: {provider}/{model}"),
                );
            };
            for (k, v) in price {
                if !VALID_FIELDS.contains(&k.as_str()) {
                    return err(
                        StatusCode::BAD_REQUEST,
                        &format!("Invalid pricing field: {k} for {provider}/{model}"),
                    );
                }
                let Some(n) = v.as_f64() else {
                    return err(
                        StatusCode::BAD_REQUEST,
                        &format!(
                            "Invalid pricing value for {k} in {provider}/{model}: must be non-negative number"
                        ),
                    );
                };
                if !n.is_finite() || n < 0.0 {
                    return err(
                        StatusCode::BAD_REQUEST,
                        &format!(
                            "Invalid pricing value for {k} in {provider}/{model}: must be non-negative number"
                        ),
                    );
                }
            }
        }
    }

    let Ok(mut conn) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update pricing");
    };
    let tx_res = (|| -> rusqlite::Result<Map<String, Value>> {
        let tx = conn.transaction()?;
        for (provider, models_val) in providers {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT value FROM kv WHERE scope = 'pricing' AND key = ?1",
                    params![provider],
                    |r| r.get(0),
                )
                .ok();
            let mut merged = existing
                .and_then(|s| serde_json::from_str::<Map<String, Value>>(&s).ok())
                .unwrap_or_default();
            if let Some(models) = models_val.as_object() {
                for (model, price) in models {
                    merged.insert(model.clone(), price.clone());
                }
            }
            let encoded = serde_json::to_string(&merged).unwrap_or_else(|_| "{}".into());
            tx.execute(
                "INSERT INTO kv(scope, key, value) VALUES('pricing', ?1, ?2)
                 ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
                params![provider, encoded],
            )?;
        }
        tx.commit()?;
        load_user_pricing(&conn)
    })();

    match tx_res {
        Ok(user) => Json(Value::Object(user)).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update pricing"),
    }
}

#[derive(Deserialize)]
struct DeletePricingQuery {
    provider: Option<String>,
    model: Option<String>,
}

async fn delete_pricing(
    State(s): State<AppState>,
    Query(q): Query<DeletePricingQuery>,
) -> Response {
    let Ok(mut conn) = s.db.lock() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to reset pricing");
    };
    let tx_res = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        match (q.provider.as_deref(), q.model.as_deref()) {
            (Some(provider), Some(model)) => {
                if let Ok(raw) = tx.query_row(
                    "SELECT value FROM kv WHERE scope = 'pricing' AND key = ?1",
                    params![provider],
                    |r| r.get::<_, String>(0),
                ) {
                    let mut current = serde_json::from_str::<Map<String, Value>>(&raw)
                        .unwrap_or_default();
                    current.remove(model);
                    if current.is_empty() {
                        tx.execute(
                            "DELETE FROM kv WHERE scope = 'pricing' AND key = ?1",
                            params![provider],
                        )?;
                    } else {
                        let encoded =
                            serde_json::to_string(&current).unwrap_or_else(|_| "{}".into());
                        tx.execute(
                            "INSERT INTO kv(scope, key, value) VALUES('pricing', ?1, ?2)
                             ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
                            params![provider, encoded],
                        )?;
                    }
                }
            }
            (Some(provider), None) => {
                tx.execute(
                    "DELETE FROM kv WHERE scope = 'pricing' AND key = ?1",
                    params![provider],
                )?;
            }
            (None, _) => {
                tx.execute("DELETE FROM kv WHERE scope = 'pricing'", [])?;
            }
        }
        tx.commit()?;
        Ok(())
    })();

    if tx_res.is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to reset pricing");
    }
    match build_merged_pricing(&conn) {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to reset pricing"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::test_support::call;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn mgmt_app() -> (Router, Arc<Mutex<Connection>>) {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let db = Arc::new(Mutex::new(db));
        let state = AppState {
            db: Arc::clone(&db),
            executor: Arc::new(crate::api::executor::HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        let app = Router::new()
            .merge(public_routes())
            .merge(routes())
            .with_state(state);
        (app, db)
    }

    #[tokio::test]
    async fn health_and_tags_work() {
        let (app, db) = mgmt_app();
        db.lock().unwrap().execute(
            "INSERT INTO providerConnections(id,provider,authType,isActive,createdAt,updatedAt,data) VALUES('c1','openai','apikey',1,'2026-01-01','2026-01-01','{}'),('c2','claude','oauth',0,'2026-01-01','2026-01-01','{}')",
            [],
        ).unwrap();
        let (st, body) = call(app.clone(), Request::get("/api/health").body(Body::empty()).unwrap()).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["checks"]["db"], "ok");
        assert_eq!(body["checks"]["connections"]["total"], 2);
        assert_eq!(body["checks"]["connections"]["active"], 1);

        let (st, body) = call(app, Request::get("/api/tags").body(Body::empty()).unwrap()).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["models"][0]["name"], "llama3.2");
    }

    #[tokio::test]
    async fn pricing_get_patch_delete_roundtrip() {
        let (app, _) = mgmt_app();
        let (st, body) = call(app.clone(), Request::get("/api/pricing").body(Body::empty()).unwrap()).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.get("openai").is_some() || body.get("anthropic").is_some() || body.is_object());

        let (st, _) = call(
            app.clone(),
            Request::builder()
                .method(Method::PATCH)
                .uri("/api/pricing")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"openai":{"gpt-4o":{"input":-1}}}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        let (st, body) = call(
            app.clone(),
            Request::builder()
                .method(Method::PATCH)
                .uri("/api/pricing")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"custom":{"my-model":{"input":1.5,"output":3.0}}}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["custom"]["my-model"]["input"], 1.5);

        let (st, body) = call(
            app,
            Request::builder()
                .method(Method::DELETE)
                .uri("/api/pricing?provider=custom&model=my-model")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.get("custom").is_none());
    }
}
