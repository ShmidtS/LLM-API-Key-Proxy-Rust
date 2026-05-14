use dashmap::DashMap;
use regex::Regex;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthType {
    ApiKey,
    OAuth,
    Bearer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDefinition {
    pub id: String,
    pub base_url: String,
    pub auth_type: AuthType,
    pub model_patterns: Vec<String>,
    pub timeout_secs: u64,
    pub default_headers: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: DashMap<String, ProviderDefinition>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        let registry = Self::default();
        for provider in default_provider_definitions() {
            registry.register(provider);
        }
        registry
    }

    pub fn register(&self, def: ProviderDefinition) {
        self.providers.insert(def.id.clone(), def);
    }

    pub fn get(&self, id: &str) -> Option<ProviderDefinition> {
        self.providers.get(id).map(|entry| entry.value().clone())
    }

    pub fn resolve_base_url(&self, id: &str) -> Option<String> {
        self.get(id).map(|def| def.base_url)
    }

    /// Load provider definitions from environment variables.
    /// Variables are expected in the form:
    ///   PROXY_<PROVIDER>_URL=https://...
    ///   PROXY_<PROVIDER>_AUTH=api_key|bearer|oauth
    ///   PROXY_<PROVIDER>_MODELS=pattern1,pattern2
    ///   PROXY_<PROVIDER>_TIMEOUT=60
    /// If a variable is set for a provider already in the default registry, it overrides the default.
    pub fn load_from_env(&self) {
        for (key, base_url) in std::env::vars() {
            let Some(provider_key) = key
                .strip_prefix("PROXY_")
                .and_then(|key| key.strip_suffix("_URL"))
            else {
                continue;
            };

            let id = provider_key.to_ascii_lowercase();
            let auth_type = std::env::var(format!("PROXY_{provider_key}_AUTH"))
                .ok()
                .and_then(|auth| parse_auth_type(&auth))
                .or_else(|| self.get(&id).map(|def| def.auth_type))
                .unwrap_or(AuthType::ApiKey);
            let model_patterns = std::env::var(format!("PROXY_{provider_key}_MODELS"))
                .ok()
                .map(|models| {
                    models
                        .split(',')
                        .map(str::trim)
                        .filter(|model| !model.is_empty())
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .or_else(|| self.get(&id).map(|def| def.model_patterns))
                .unwrap_or_default();
            let timeout_secs = std::env::var(format!("PROXY_{provider_key}_TIMEOUT"))
                .ok()
                .and_then(|timeout| timeout.parse().ok())
                .or_else(|| self.get(&id).map(|def| def.timeout_secs))
                .unwrap_or(60);
            let default_headers = self
                .get(&id)
                .map(|def| def.default_headers)
                .unwrap_or_default();

            self.register(ProviderDefinition {
                id,
                base_url,
                auth_type,
                model_patterns,
                timeout_secs,
                default_headers,
            });
        }
    }

    pub fn all_providers(&self) -> Vec<ProviderDefinition> {
        let mut providers: Vec<_> = self
            .providers
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        providers.sort_by(|left, right| left.id.cmp(&right.id));
        providers
    }

    pub fn find_provider_for_model(&self, model: &str) -> Option<String> {
        self.all_providers().into_iter().find_map(|provider| {
            provider
                .model_patterns
                .iter()
                .filter_map(|pattern| Regex::new(pattern).ok())
                .any(|regex| regex.is_match(model))
                .then_some(provider.id)
        })
    }
}

fn parse_auth_type(value: &str) -> Option<AuthType> {
    match value.to_ascii_lowercase().as_str() {
        "api_key" => Some(AuthType::ApiKey),
        "oauth" => Some(AuthType::OAuth),
        "bearer" => Some(AuthType::Bearer),
        _ => None,
    }
}

fn default_provider_definitions() -> Vec<ProviderDefinition> {
    vec![
        provider(
            "openai",
            "https://api.openai.com/v1",
            AuthType::ApiKey,
            &[
                r"^(gpt|o1|o3|o4)([-/].*)?$",
                r"^text-embedding-.*",
                r"^dall-e-.*",
            ],
            60,
            &[],
        ),
        provider(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta",
            AuthType::ApiKey,
            &[r"^(models/)?gemini[-/].*"],
            60,
            &[],
        ),
        provider(
            "gemini_cli",
            "https://cloudcode-pa.googleapis.com/v1internal",
            AuthType::OAuth,
            &[r"^gemini_cli/.*"],
            120,
            &[],
        ),
        provider(
            "anthropic",
            "https://api.anthropic.com/v1",
            AuthType::ApiKey,
            &[r"^claude[-/].*"],
            60,
            &[("anthropic-version", "2023-06-01")],
        ),
        provider(
            "fireworks",
            "https://api.fireworks.ai/inference/v1",
            AuthType::ApiKey,
            &[r"^accounts/fireworks/models/.*", r"^fireworks/.*"],
            120,
            &[],
        ),
        provider(
            "nvidia",
            "https://integrate.api.nvidia.com/v1",
            AuthType::ApiKey,
            &[
                r"^nvidia/.*",
                r"^meta/llama.*",
                r"^mistralai/.*",
                r"^nv-.*",
                r".*nemotron.*",
            ],
            60,
            &[],
        ),
        provider(
            "qwen",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            AuthType::ApiKey,
            &[r"^qwen[-/].*", r"^qwq[-/].*", r"^deepseek[-/].*"],
            60,
            &[],
        ),
        provider(
            "qwen_code",
            "https://portal.qwen.ai/v1",
            AuthType::OAuth,
            &[r"^qwen_code/.*", r"^qwen3-coder.*"],
            60,
            &[],
        ),
        provider(
            "zai",
            "https://api.z.ai/api/coding/paas/v4",
            AuthType::Bearer,
            &[r"^zai/.*", r"^glm[-/].*", r"^GLM[-/].*"],
            60,
            &[],
        ),
        provider(
            "iflow",
            "https://apis.iflow.cn/v1",
            AuthType::Bearer,
            &[r"^iflow/.*", r"^kimi[-/].*", r"^Qwen.*"],
            60,
            &[],
        ),
        provider(
            "colin",
            "https://claude.colin1112.tech/v1",
            AuthType::ApiKey,
            &[r"^colin/.*", r"^colin[-/].*"],
            60,
            &[],
        ),
        provider(
            "elysiver",
            "https://elysiver.h-e.top/v1",
            AuthType::ApiKey,
            &[r"^elysiver/.*", r"^elysiver[-/].*"],
            60,
            &[],
        ),
        provider(
            "chutes",
            "https://llm.chutes.ai/v1",
            AuthType::Bearer,
            &[r"^chutes/.*", r"^unsloth/.*", r"^deepseek-ai/.*"],
            120,
            &[],
        ),
        provider(
            "nanogpt",
            "https://nano-gpt.com/api/v1",
            AuthType::ApiKey,
            &[r"^nanogpt/.*", r"^nano-gpt/.*"],
            60,
            &[],
        ),
        provider(
            "opencode",
            "https://opencode.ai/zen/v1",
            AuthType::ApiKey,
            &[r"^opencode/.*", r"^zen/.*"],
            60,
            &[("HTTP-Referer", "https://opencode.ai")],
        ),
        provider(
            "firmware",
            "https://app.firmware.ai/api/v1",
            AuthType::ApiKey,
            &[r"^firmware/.*", r"^fw/.*"],
            60,
            &[],
        ),
        provider(
            "antigravity",
            "https://cloudcode-pa.googleapis.com/v1internal",
            AuthType::OAuth,
            &[r"^antigravity/.*", r"^ag/.*"],
            120,
            &[],
        ),
        provider(
            "openrouter",
            "https://openrouter.ai/api/v1",
            AuthType::ApiKey,
            &[r"^openrouter/.*"],
            60,
            &[],
        ),
        provider(
            "xai",
            "https://api.x.ai/v1",
            AuthType::ApiKey,
            &[r"^xai/.*", r"^grok[-/].*"],
            60,
            &[],
        ),
        provider(
            "kilocode",
            "https://kilo.ai/api/openrouter",
            AuthType::ApiKey,
            &[r"^kilocode/.*"],
            60,
            &[],
        ),
    ]
}

fn provider(
    id: &str,
    base_url: &str,
    auth_type: AuthType,
    model_patterns: &[&str],
    timeout_secs: u64,
    default_headers: &[(&str, &str)],
) -> ProviderDefinition {
    ProviderDefinition {
        id: id.to_owned(),
        base_url: base_url.to_owned(),
        auth_type,
        model_patterns: model_patterns
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .collect(),
        timeout_secs,
        default_headers: default_headers
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_resolves_provider_base_url() {
        let registry = ProviderRegistry::new();

        assert_eq!(
            registry.resolve_base_url("openai").as_deref(),
            Some("https://api.openai.com/v1")
        );
    }

    #[test]
    fn default_registry_contains_all_required_provider_ids() {
        let registry = ProviderRegistry::new();
        let providers = registry.all_providers();

        for id in [
            "openai",
            "gemini",
            "anthropic",
            "fireworks",
            "nvidia",
            "qwen",
            "qwen_code",
            "zai",
            "iflow",
            "colin",
            "elysiver",
            "chutes",
            "nanogpt",
            "opencode",
            "firmware",
            "antigravity",
        ] {
            assert!(providers.iter().any(|provider| provider.id == id));
        }
    }

    #[test]
    fn registered_provider_can_be_looked_up() {
        let registry = ProviderRegistry::default();
        let def = provider(
            "custom",
            "https://custom.example/v1",
            AuthType::Bearer,
            &[r"^custom-.*"],
            30,
            &[("X-Custom", "yes")],
        );

        registry.register(def.clone());

        assert_eq!(registry.get("custom"), Some(def));
    }

    #[test]
    fn model_matching_uses_regex_patterns() {
        let registry = ProviderRegistry::new();

        assert_eq!(
            registry.find_provider_for_model("gpt-4o-mini").as_deref(),
            Some("openai")
        );
        assert_eq!(
            registry
                .find_provider_for_model("gemini-2.5-flash")
                .as_deref(),
            Some("gemini")
        );
        assert_eq!(
            registry
                .find_provider_for_model("claude-3-5-sonnet-20241022")
                .as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn load_from_env_overrides_defaults() {
        unsafe {
            std::env::set_var("PROXY_OPENAI_URL", "https://override.example/v1");
            std::env::set_var("PROXY_OPENAI_AUTH", "bearer");
            std::env::set_var("PROXY_OPENAI_MODELS", "^override/.*,^custom-.*");
            std::env::set_var("PROXY_OPENAI_TIMEOUT", "45");
        }

        let registry = ProviderRegistry::new();
        registry.load_from_env();

        let provider = registry.get("openai").expect("openai provider exists");
        assert_eq!(provider.base_url, "https://override.example/v1");
        assert_eq!(provider.auth_type, AuthType::Bearer);
        assert_eq!(
            provider.model_patterns,
            vec!["^override/.*".to_owned(), "^custom-.*".to_owned()]
        );
        assert_eq!(provider.timeout_secs, 45);

        unsafe {
            std::env::remove_var("PROXY_OPENAI_URL");
            std::env::remove_var("PROXY_OPENAI_AUTH");
            std::env::remove_var("PROXY_OPENAI_MODELS");
            std::env::remove_var("PROXY_OPENAI_TIMEOUT");
        }
    }
}
