use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}
