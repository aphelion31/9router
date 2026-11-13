#![allow(dead_code)]

pub mod chat;
pub mod chat_core;
pub mod media;
pub mod sse;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    pub connection_id: String,
    pub connection_name: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub api_key: Option<String>,
    pub expires_at: Option<String>,
    #[serde(default)]
    pub provider_specific_data: Value,
    #[serde(default)]
    pub raw_headers: HashMap<String, String>,
    #[serde(default)]
    pub runtime_transport: Option<Value>,
}

impl Credentials {
    pub fn to_translator_value(&self) -> Value {
        serde_json::json!({
            "connectionId": self.connection_id,
            "accessToken": self.access_token,
            "apiKey": self.api_key,
            "providerSpecificData": self.provider_specific_data,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ExecutorRequest {
    pub provider: String,
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: Credentials,
    pub provider_session_id: Option<String>,
    pub client_tool: Option<String>,
}

pub enum ExecutorBody {
    Json(Value),
    Bytes(Vec<u8>),
    Stream(mpsc::Receiver<Result<Vec<u8>, String>>),
}

pub struct ExecutorResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ExecutorBody,
    pub url: Option<String>,
    pub response_format: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RefreshedCredentials {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<String>,
    pub copilot_token: Option<String>,
}

/// Call boundary between chat handlers and provider executors.
pub trait Executor: Send + Sync {
    fn no_auth(&self) -> bool {
        false
    }

    fn execute<'a>(
        &'a self,
        req: ExecutorRequest,
    ) -> BoxFuture<'a, Result<ExecutorResponse, String>>;

    fn refresh_credentials<'a>(
        &'a self,
        _credentials: &'a Credentials,
    ) -> BoxFuture<'a, Result<Option<RefreshedCredentials>, String>> {
        Box::pin(async { Ok(None) })
    }
}
