use serde_json::{Value, json};

pub fn get_usage() -> Value {
    json!({"plan":"OrcaRouter","message":"OrcaRouter does not expose an account balance API. Balance is tracked per-request via cost headers (X-OrcaRouter-Include-Cost / GET /v1/generation?id=…); no credits or remaining-$ query is available."})
}
