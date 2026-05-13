use dashmap::DashMap;
use regex::Regex;
use std::sync::OnceLock;

static REGEX_CACHE: OnceLock<DashMap<String, Regex>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMetadata {
    pub model_id: String,
    pub provider: String,
    pub context_length: u32,
    pub pricing_input_per_1k: f64,
    pub pricing_output_per_1k: f64,
    pub supports_streaming: bool,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ModelInfoService {
    models: DashMap<String, ModelMetadata>,
}

impl ModelInfoService {
    pub fn new() -> Self {
        let service = Self::default();
        for metadata in default_models() {
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
}
