use dashmap::DashMap;
use regex::Regex;
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};
use thiserror::Error;
use tokio::time::Instant;

static REGEX_CACHE: OnceLock<DashMap<String, Regex>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMetadata {
    pub model_id: String,
    pub display_name: String,
    pub provider: String,
    pub context_length: u32,
    pub pricing_input_per_1k: f64,
    pub pricing_output_per_1k: f64,
    pub supports_streaming: bool,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub capabilities: Vec<String>,
}

#[derive(Debug)]
pub struct ModelInfoService {
    models: DashMap<String, ModelMetadata>,
    static_model_ids: DashMap<String, ()>,
    refresh_interval: Duration,
    last_refresh: Option<Instant>,
}

impl Default for ModelInfoService {
    fn default() -> Self {
        Self {
            models: DashMap::new(),
            static_model_ids: DashMap::new(),
            refresh_interval: Duration::from_secs(60 * 60),
            last_refresh: None,
        }
    }
}

#[derive(Error, Debug)]
pub enum ModelInfoError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("catalog parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

impl ModelInfoService {
    pub fn new() -> Self {
        let service = Self::default();
        for metadata in default_models() {
            service
                .static_model_ids
                .insert(metadata.model_id.clone(), ());
            service.register_model(metadata);
        }
        service
    }

    pub fn register_model(&self, metadata: ModelMetadata) {
        self.models.insert(metadata.model_id.clone(), metadata);
    }

    pub fn get_model(&self, model_id: &str) -> Option<ModelMetadata> {
        self.models.get(model_id).map(|entry| entry.value().clone())
    }

    pub fn resolve_alias(&self, alias: &str) -> Option<&str> {
        match alias {
            "claude-3.5-sonnet" | "claude-3-5-sonnet" => Some("claude-3-5-sonnet-20241022"),
            "claude-3.5-haiku" | "claude-3-5-haiku" => Some("claude-3-5-haiku-20241022"),
            "claude-3-opus" => Some("claude-3-opus-20240229"),
            "gpt-4" => Some("gpt-4-0613"),
            "gpt-3.5-turbo" => Some("gpt-3.5-turbo-0125"),
            _ => None,
        }
    }

    pub fn merge_external_models(&self, models: Vec<ModelMetadata>) {
        for metadata in models {
            if !self.static_model_ids.contains_key(&metadata.model_id) {
                self.register_model(metadata);
            }
        }
    }

    pub async fn refresh_if_needed(
        &mut self,
        client: &reqwest::Client,
    ) -> Result<(), ModelInfoError> {
        let should_refresh = self
            .last_refresh
            .is_none_or(|last_refresh| last_refresh.elapsed() >= self.refresh_interval);
        if !should_refresh {
            return Ok(());
        }

        self.last_refresh = Some(Instant::now());
        let models = fetch_openrouter_catalog(client).await?;
        self.merge_external_models(models);
        Ok(())
    }

    pub fn find_models(&self, pattern: &str) -> Vec<ModelMetadata> {
        let regex = match cached_regex(pattern) {
            Some(regex) => regex,
            None => return Vec::new(),
        };

        let mut models: Vec<ModelMetadata> = self
            .models
            .iter()
            .filter(|entry| regex.is_match(entry.key()))
            .map(|entry| entry.value().clone())
            .collect();
        sort_models(&mut models);
        models
    }

    pub fn get_models_by_provider(&self, provider: &str) -> Vec<ModelMetadata> {
        let mut models: Vec<ModelMetadata> = self
            .models
            .iter()
            .filter(|entry| entry.value().provider == provider)
            .map(|entry| entry.value().clone())
            .collect();
        sort_models(&mut models);
        models
    }

    pub fn get_all_models(&self) -> Vec<ModelMetadata> {
        let mut models: Vec<ModelMetadata> = self
            .models
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        sort_models(&mut models);
        models
    }
}

/// Fetches and normalizes OpenRouter model catalog.
pub async fn fetch_openrouter_catalog(
    client: &reqwest::Client,
) -> Result<Vec<ModelMetadata>, ModelInfoError> {
    let payload = client
        .get("https://openrouter.ai/api/v1/models")
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    normalize_openrouter_catalog(payload)
}

/// Fetches and normalizes Models.dev catalog.
pub async fn fetch_modelsdev_catalog(
    client: &reqwest::Client,
) -> Result<Vec<ModelMetadata>, ModelInfoError> {
    let payload = client
        .get("https://models.dev/api.json")
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    normalize_modelsdev_catalog(payload)
}

fn normalize_openrouter_catalog(payload: Value) -> Result<Vec<ModelMetadata>, ModelInfoError> {
    let mut models = Vec::new();
    if let Some(entries) = payload.get("data").and_then(Value::as_array) {
        for entry in entries {
            if let Some(model) = normalize_catalog_model(entry) {
                models.push(model);
            }
        }
    }
    sort_models(&mut models);
    Ok(models)
}

fn normalize_modelsdev_catalog(payload: Value) -> Result<Vec<ModelMetadata>, ModelInfoError> {
    let mut models = Vec::new();
    collect_modelsdev_models(&payload, &mut models);
    sort_models(&mut models);
    Ok(models)
}

fn collect_modelsdev_models(value: &Value, models: &mut Vec<ModelMetadata>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_modelsdev_models(item, models);
            }
        }
        Value::Object(map) => {
            if let Some(model) = normalize_catalog_model(value) {
                models.push(model);
            } else {
                for item in map.values() {
                    collect_modelsdev_models(item, models);
                }
            }
        }
        _ => {}
    }
}

fn normalize_catalog_model(entry: &Value) -> Option<ModelMetadata> {
    let model_id = entry
        .get("id")
        .or_else(|| entry.get("model_id"))
        .and_then(Value::as_str)?;
    let display_name = entry
        .get("name")
        .or_else(|| entry.get("display_name"))
        .and_then(Value::as_str)
        .unwrap_or(model_id);
    let context_length = entry
        .get("context_length")
        .or_else(|| entry.get("max_tokens"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default();
    let pricing = entry.get("pricing");
    let pricing_input_per_1k = catalog_price(pricing, "prompt");
    let pricing_output_per_1k = catalog_price(pricing, "completion");
    let supports_tools = entry
        .get("supported_parameters")
        .and_then(Value::as_array)
        .is_some_and(|parameters| {
            parameters
                .iter()
                .any(|value| value.as_str() == Some("tools"))
        });
    let supports_vision = entry
        .get("architecture")
        .and_then(|architecture| architecture.get("modality"))
        .and_then(Value::as_str)
        .is_some_and(|modality| modality.contains("image"));
    let mut capabilities = vec!["chat".to_string()];
    if supports_vision {
        capabilities.push("vision".to_string());
    }
    if supports_tools {
        capabilities.push("tools".to_string());
    }

    Some(ModelMetadata {
        model_id: model_id.to_string(),
        display_name: display_name.to_string(),
        provider: provider_from_model_id(model_id),
        context_length,
        pricing_input_per_1k,
        pricing_output_per_1k,
        supports_streaming: true,
        supports_vision,
        supports_tools,
        capabilities,
    })
}

fn catalog_price(pricing: Option<&Value>, key: &str) -> f64 {
    pricing
        .and_then(|pricing| pricing.get(key))
        .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
        .map(|price| (price * 1000.0 * 1_000_000.0).round() / 1_000_000.0)
        .unwrap_or_default()
}

fn provider_from_model_id(model_id: &str) -> String {
    model_id
        .split_once('/')
        .map(|(provider, _)| provider)
        .unwrap_or_else(|| model_id.split('-').next().unwrap_or("unknown"))
        .to_string()
}

fn cached_regex(pattern: &str) -> Option<Regex> {
    let cache = REGEX_CACHE.get_or_init(DashMap::new);
    if let Some(regex) = cache.get(pattern) {
        return Some(regex.value().clone());
    }

    let regex = Regex::new(pattern).ok()?;
    cache.insert(pattern.to_string(), regex.clone());
    Some(regex)
}

fn sort_models(models: &mut [ModelMetadata]) {
    models.sort_by(|left, right| left.model_id.cmp(&right.model_id));
}

fn default_models() -> Vec<ModelMetadata> {
    vec![
        model(
            "gpt-4o",
            "openai",
            128_000,
            0.005,
            0.015,
            true,
            true,
            true,
            &["chat", "vision", "tools", "json"],
        ),
        model(
            "gpt-4o-mini",
            "openai",
            128_000,
            0.00015,
            0.0006,
            true,
            true,
            true,
            &["chat", "vision", "tools", "json"],
        ),
        model(
            "gpt-4-turbo",
            "openai",
            128_000,
            0.01,
            0.03,
            true,
            true,
            true,
            &["chat", "vision", "tools", "json"],
        ),
        model(
            "gpt-4",
            "openai",
            8_192,
            0.03,
            0.06,
            true,
            false,
            true,
            &["chat", "tools"],
        ),
        model(
            "gpt-3.5-turbo",
            "openai",
            16_385,
            0.0005,
            0.0015,
            true,
            false,
            true,
            &["chat", "tools", "json"],
        ),
        model(
            "o1-preview",
            "openai",
            128_000,
            0.015,
            0.06,
            false,
            false,
            false,
            &["chat", "reasoning"],
        ),
        model(
            "o1-mini",
            "openai",
            128_000,
            0.003,
            0.012,
            false,
            false,
            false,
            &["chat", "reasoning"],
        ),
        model(
            "claude-opus-4-1",
            "anthropic",
            200_000,
            0.015,
            0.075,
            true,
            true,
            true,
            &["chat", "vision", "tools", "reasoning"],
        ),
        model(
            "claude-sonnet-4",
            "anthropic",
            200_000,
            0.003,
            0.015,
            true,
            true,
            true,
            &["chat", "vision", "tools", "reasoning"],
        ),
        model(
            "claude-3-5-sonnet-20241022",
            "anthropic",
            200_000,
            0.003,
            0.015,
            true,
            true,
            true,
            &["chat", "vision", "tools", "json"],
        ),
        model(
            "claude-3-5-haiku-20241022",
            "anthropic",
            200_000,
            0.0008,
            0.004,
            true,
            true,
            true,
            &["chat", "vision", "tools"],
        ),
        model(
            "claude-3-opus-20240229",
            "anthropic",
            200_000,
            0.015,
            0.075,
            true,
            true,
            true,
            &["chat", "vision", "tools"],
        ),
        model(
            "claude-3-haiku-20240307",
            "anthropic",
            200_000,
            0.00025,
            0.00125,
            true,
            true,
            true,
            &["chat", "vision", "tools"],
        ),
        model(
            "gemini-2.5-pro",
            "gemini",
            1_048_576,
            0.00125,
            0.01,
            true,
            true,
            true,
            &["chat", "vision", "tools", "long-context", "reasoning"],
        ),
        model(
            "gemini-2.5-flash",
            "gemini",
            1_048_576,
            0.0003,
            0.0025,
            true,
            true,
            true,
            &["chat", "vision", "tools", "long-context"],
        ),
        model(
            "gemini-2.0-flash",
            "gemini",
            1_048_576,
            0.0001,
            0.0004,
            true,
            true,
            true,
            &["chat", "vision", "tools", "long-context"],
        ),
        model(
            "gemini-1.5-pro",
            "gemini",
            2_097_152,
            0.00125,
            0.005,
            true,
            true,
            true,
            &["chat", "vision", "tools", "long-context"],
        ),
        model(
            "gemini-1.5-flash",
            "gemini",
            1_048_576,
            0.000075,
            0.0003,
            true,
            true,
            true,
            &["chat", "vision", "tools", "long-context"],
        ),
        model(
            "llama-3.1-405b-instruct",
            "meta",
            128_000,
            0.0027,
            0.0027,
            true,
            false,
            true,
            &["chat", "tools", "open-weights"],
        ),
        model(
            "llama-3.1-70b-instruct",
            "meta",
            128_000,
            0.0009,
            0.0009,
            true,
            false,
            true,
            &["chat", "tools", "open-weights"],
        ),
        model(
            "llama-3.1-8b-instruct",
            "meta",
            128_000,
            0.0002,
            0.0002,
            true,
            false,
            true,
            &["chat", "tools", "open-weights"],
        ),
        model(
            "mistral-large-latest",
            "mistral",
            128_000,
            0.002,
            0.006,
            true,
            false,
            true,
            &["chat", "tools", "json"],
        ),
        model(
            "mistral-small-latest",
            "mistral",
            32_000,
            0.0002,
            0.0006,
            true,
            false,
            true,
            &["chat", "tools"],
        ),
        model(
            "codestral-latest",
            "mistral",
            32_000,
            0.0002,
            0.0006,
            true,
            false,
            false,
            &["code", "completion"],
        ),
        model(
            "command-r-plus",
            "cohere",
            128_000,
            0.003,
            0.015,
            true,
            false,
            true,
            &["chat", "tools", "rag"],
        ),
        model(
            "command-r",
            "cohere",
            128_000,
            0.0005,
            0.0015,
            true,
            false,
            true,
            &["chat", "tools", "rag"],
        ),
        model(
            "qwen-max",
            "qwen",
            32_768,
            0.0016,
            0.0064,
            true,
            false,
            true,
            &["chat", "tools"],
        ),
        model(
            "qwen-plus",
            "qwen",
            131_072,
            0.0004,
            0.0012,
            true,
            false,
            true,
            &["chat", "tools", "long-context"],
        ),
        model(
            "qwen-turbo",
            "qwen",
            1_000_000,
            0.00005,
            0.0002,
            true,
            false,
            true,
            &["chat", "tools", "long-context"],
        ),
        model(
            "deepseek-chat",
            "deepseek",
            64_000,
            0.00014,
            0.00028,
            true,
            false,
            true,
            &["chat", "tools", "json"],
        ),
        model(
            "deepseek-reasoner",
            "deepseek",
            64_000,
            0.00055,
            0.00219,
            true,
            false,
            false,
            &["chat", "reasoning"],
        ),
        model(
            "grok-4",
            "xai",
            256_000,
            0.003,
            0.015,
            true,
            true,
            true,
            &["chat", "vision", "tools", "reasoning"],
        ),
        model(
            "grok-3",
            "xai",
            131_072,
            0.003,
            0.015,
            true,
            false,
            true,
            &["chat", "tools"],
        ),
        model(
            "glm-4.5",
            "zai",
            128_000,
            0.0006,
            0.0022,
            true,
            false,
            true,
            &["chat", "tools", "reasoning"],
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn model(
    model_id: &str,
    provider: &str,
    context_length: u32,
    pricing_input_per_1k: f64,
    pricing_output_per_1k: f64,
    supports_streaming: bool,
    supports_vision: bool,
    supports_tools: bool,
    capabilities: &[&str],
) -> ModelMetadata {
    ModelMetadata {
        model_id: model_id.to_string(),
        display_name: model_id.to_string(),
        provider: provider.to_string(),
        context_length,
        pricing_input_per_1k,
        pricing_output_per_1k,
        supports_streaming,
        supports_vision,
        supports_tools,
        capabilities: capabilities
            .iter()
            .map(|capability| capability.to_string())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_matching_returns_sorted_models() {
        let service = ModelInfoService::new();

        let matches = service.find_models(r"^gpt-4");
        let model_ids: Vec<String> = matches.into_iter().map(|model| model.model_id).collect();

        assert_eq!(
            model_ids,
            vec!["gpt-4", "gpt-4-turbo", "gpt-4o", "gpt-4o-mini"]
        );
    }

    #[test]
    fn provider_lookup_returns_only_requested_provider() {
        let service = ModelInfoService::new();

        let models = service.get_models_by_provider("anthropic");

        assert!(!models.is_empty());
        assert!(models.iter().all(|model| model.provider == "anthropic"));
    }

    #[test]
    fn default_service_looks_up_known_model_metadata() {
        let service = ModelInfoService::new();

        let model = service
            .get_model("gpt-4o")
            .expect("gpt-4o metadata should be registered");

        assert_eq!(model.provider, "openai");
        assert!(model.context_length >= 128_000);
        assert!(model.pricing_input_per_1k > 0.0);
        assert!(model.pricing_output_per_1k > 0.0);
        assert!(model.supports_streaming);
        assert!(model.supports_vision);
        assert!(model.supports_tools);
        assert!(model.capabilities.contains(&"chat".to_string()));
    }

    #[test]
    fn register_model_overrides_metadata_by_model_id() {
        let service = ModelInfoService::new();
        let metadata = ModelMetadata {
            model_id: "custom-model".to_string(),
            display_name: "custom-model".to_string(),
            provider: "custom".to_string(),
            context_length: 65_536,
            pricing_input_per_1k: 0.001,
            pricing_output_per_1k: 0.002,
            supports_streaming: true,
            supports_vision: false,
            supports_tools: true,
            capabilities: vec!["chat".to_string(), "tools".to_string()],
        };

        service.register_model(metadata.clone());

        assert_eq!(service.get_model("custom-model"), Some(metadata));
    }

    #[test]
    fn find_models_matches_regex_against_model_id() {
        let service = ModelInfoService::new();

        let matches = service.find_models(r"^claude-3.*sonnet");
        let model_ids: Vec<String> = matches.into_iter().map(|model| model.model_id).collect();

        assert!(model_ids.iter().any(|id| id.contains("claude-3-5-sonnet")));
        assert!(model_ids.iter().all(|id| id.contains("sonnet")));
    }

    #[test]
    fn find_models_reuses_compiled_pattern_cache() {
        let service = ModelInfoService::new();

        let first = service.find_models(r"^gemini-.*pro");
        let second = service.find_models(r"^gemini-.*pro");

        assert_eq!(first, second);
        assert!(first.iter().any(|model| model.model_id == "gemini-2.5-pro"));
    }

    #[test]
    fn invalid_regex_returns_no_matches() {
        let service = ModelInfoService::new();

        assert!(service.find_models("[").is_empty());
    }

    #[test]
    fn get_all_models_includes_common_provider_families() {
        let service = ModelInfoService::new();
        let all = service.get_all_models();

        assert!(all.len() >= 20);
        assert!(all.iter().any(|model| model.provider == "openai"));
        assert!(all.iter().any(|model| model.provider == "anthropic"));
        assert!(all.iter().any(|model| model.provider == "gemini"));
    }

    #[test]
    fn normalizes_openrouter_catalog_response() {
        let payload = serde_json::json!({
            "data": [{
                "id": "anthropic/claude-3.5-sonnet",
                "name": "Claude 3.5 Sonnet",
                "context_length": 200000,
                "pricing": {"prompt": 0.000003, "completion": 0.000015}
            }]
        });

        let models = normalize_openrouter_catalog(payload).expect("catalog should parse");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "anthropic/claude-3.5-sonnet");
        assert_eq!(models[0].display_name, "Claude 3.5 Sonnet");
        assert_eq!(models[0].provider, "anthropic");
        assert_eq!(models[0].context_length, 200_000);
        assert_eq!(models[0].pricing_input_per_1k, 0.003);
        assert_eq!(models[0].pricing_output_per_1k, 0.015);
    }

    #[test]
    fn resolves_common_model_aliases() {
        let service = ModelInfoService::new();

        assert_eq!(
            service.resolve_alias("claude-3.5-sonnet"),
            Some("claude-3-5-sonnet-20241022")
        );
        assert_eq!(service.resolve_alias("gpt-4"), Some("gpt-4-0613"));
        assert_eq!(service.resolve_alias("unknown"), None);
    }

    #[test]
    fn merge_external_models_keeps_static_model_on_conflict() {
        let service = ModelInfoService::new();
        let original = service
            .get_model("gpt-4o")
            .expect("static gpt-4o metadata should exist");
        let external = ModelMetadata {
            model_id: "gpt-4o".to_string(),
            display_name: "External GPT-4o".to_string(),
            provider: "openrouter".to_string(),
            context_length: 1,
            pricing_input_per_1k: 99.0,
            pricing_output_per_1k: 99.0,
            supports_streaming: false,
            supports_vision: false,
            supports_tools: false,
            capabilities: vec!["chat".to_string()],
        };

        service.merge_external_models(vec![external]);

        assert_eq!(service.get_model("gpt-4o"), Some(original));
    }
}
