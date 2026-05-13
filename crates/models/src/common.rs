use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelList {
    pub object: String,
    pub data: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    pub error: ApiError,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}
