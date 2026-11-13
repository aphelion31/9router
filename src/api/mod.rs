pub mod executor;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// API key access always contains an allow list, including for legacy records.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KeyAccess {
    pub restricted: bool,
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApiKey {
    pub id: String,
    pub key: String,
    pub name: Option<String>,
    pub machine_id: Option<String>,
    pub is_active: bool,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub access: KeyAccess,
    pub quota: Option<Value>,
    pub limits: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_access_defaults_allow() {
        let key: ApiKey = serde_json::from_str(r#"{"access":{"restricted":true}}"#).unwrap();
        let key = serde_json::to_value(key).unwrap();
        assert_eq!(
            key["access"],
            serde_json::json!({"restricted": true, "allow": []})
        );
        assert!(key["quota"].is_null());
        assert!(key["limits"].is_null());
    }
}
