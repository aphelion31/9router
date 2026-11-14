use crate::server::AppState;
use serde_json::{Value, json};

pub fn estimate_tokens_from_text(text: &str) -> i64 { (text.encode_utf16().count() as i64 + 3) / 4 }

/// Port of upstream `recordModalityUsage` -> `saveRequestUsage`: prices the row by its
/// token counts and folds it into usageDaily and the lifetime counter, so key quotas see it.
pub fn record_modality_usage(state: &AppState, provider: &str, model: &str, connection_id: Option<&str>, api_key: Option<&str>, endpoint: &str, prompt: i64, completion: i64, accrual: &str) {
    record_row(state, provider, model, connection_id, api_key, endpoint, prompt, completion, json!({"accrual":accrual}), None);
}

pub fn reported_tokens(usage: &Value) -> (i64, i64) {
    (usage["prompt_tokens"].as_i64().or_else(|| usage["input_tokens"].as_i64()).unwrap_or(0), usage["completion_tokens"].as_i64().or_else(|| usage["output_tokens"].as_i64()).unwrap_or(0))
}

/// Search and fetch spend is USD per call (`costPerQuery`), not tokens. Token quotas stay
/// at zero; cost quotas count `cost` through the same rollup path.
pub fn record_search_usage(state: &AppState, provider: &str, connection: Option<&str>, api_key: Option<&str>, cost: Option<f64>) {
    let meta = if cost.is_some() { json!({"accrual":"cost-only","queries":1}) } else { json!({"accrual":"request-only","reason":"no costPerQuery configured"}) };
    record_row(state, provider, "search", connection, api_key, "/v1/search", 0, 0, meta, cost);
}

#[allow(clippy::too_many_arguments)]
fn record_row(state: &AppState, provider: &str, model: &str, connection_id: Option<&str>, api_key: Option<&str>, endpoint: &str, prompt: i64, completion: i64, meta: Value, cost_override: Option<f64>) {
    let Ok(db) = state.db.lock() else { return };
    let row = crate::db::UsageRow {
        provider: provider.to_owned(),
        model: model.to_owned(),
        connection_id: connection_id.map(str::to_owned),
        api_key: api_key.map(str::to_owned),
        endpoint: Some(endpoint.to_owned()),
        prompt_tokens: prompt,
        completion_tokens: completion,
        status: 200,
        request_id: crate::server::uuid_like(),
        meta,
        ..Default::default()
    };
    let tokens = crate::services::pricing::TokenCounts { prompt_tokens: prompt, completion_tokens: completion, ..Default::default() };
    if let Err(err) = crate::db::usage_record(&db, &row, &tokens, cost_override) { tracing::warn!("modality usage insert failed: {err}"); }
}
