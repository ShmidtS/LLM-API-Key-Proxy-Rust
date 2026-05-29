use crate::provider_normalization::normalize_provider_id;
use crate::provider_registry::AuthType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicProviderConfig {
    pub provider_id: String,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub models: Vec<String>,
    pub auth_type: AuthType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicProviderEnvNames {
    pub proxy_url: String,
    pub api_base: String,
    pub models: String,
    pub api_key: String,
}

impl DynamicProviderEnvNames {
    pub fn new(provider_key: &str) -> Self {
        Self {
            proxy_url: format!("PROXY_{provider_key}_URL"),
            api_base: format!("{provider_key}_API_BASE"),
            models: format!("{provider_key}_MODELS"),
            api_key: format!("{provider_key}_API_KEY"),
        }
    }
}

pub fn dynamic_provider_id(provider_key: &str) -> String {
    normalize_provider_id(provider_key)
}

pub fn parse_models_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}
