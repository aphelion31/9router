use crate::server::AppState;
use serde_json::{Value, json};

pub fn estimate_tokens_from_text(text: &str) -> i64 { (text.encode_utf16().count() as i64 + 3) / 4 }

pub fn record_modality_usage(state: &AppState, provider: &str, model: &str, connection_id: Option<&str>, api_key: Option<&str>, endpoint: &str, prompt: i64, completion: i64, accrual: &str) {
    let Ok(db) = state.db.lock() else { return };
    let result = db.execute("INSERT INTO usageHistory(timestamp,provider,model,connectionId,apiKey,endpoint,promptTokens,completionTokens,status,meta,requestId) VALUES(strftime('%Y-%m-%dT%H:%M:%fZ','now'),?1,?2,?3,?4,?5,?6,?7,'success',?8,lower(hex(randomblob(16))))", rusqlite::params![provider,model,connection_id,api_key,endpoint,prompt,completion,json!({"accrual":accrual}).to_string()]);
    if let Err(err) = result { tracing::warn!("modality usage insert failed: {err}"); }
}

pub fn reported_tokens(usage: &Value) -> (i64, i64) {
    (usage["prompt_tokens"].as_i64().or_else(|| usage["input_tokens"].as_i64()).unwrap_or(0), usage["completion_tokens"].as_i64().or_else(|| usage["output_tokens"].as_i64()).unwrap_or(0))
}

pub fn record_search_usage(state: &AppState, provider: &str, connection: Option<&str>, api_key: Option<&str>, cost: Option<f64>) {
    let Ok(db) = state.db.lock() else { return };
    let meta = if cost.is_some() { json!({"accrual":"cost-only","queries":1}) } else { json!({"accrual":"request-only","reason":"no costPerQuery configured"}) };
    if let Err(err) = db.execute("INSERT INTO usageHistory(timestamp,provider,model,connectionId,apiKey,endpoint,promptTokens,completionTokens,cost,status,meta,requestId) VALUES(strftime('%Y-%m-%dT%H:%M:%fZ','now'),?1,'search',?2,?3,'/v1/search',0,0,?4,'success',?5,lower(hex(randomblob(16))))", rusqlite::params![provider,connection,api_key,cost.unwrap_or(0.0),meta.to_string()]) { tracing::warn!("search usage insert failed: {err}"); }
}
