use crate::dynamic_provider::{dynamic_provider_id, parse_models_csv};
use crate::model_filter::ModelFilterEngine;
use crate::provider_normalization::normalize_provider_id;
use crate::provider_runtime::{RuntimeProviderKind, RuntimeProviderRoute};
use dashmap::DashMap;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthType {
    ApiKey,
    OAuth,
    Bearer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAction {
    Chat,
    Embeddings,
}

#[derive(Debug, Clone)]
pub struct ProviderDefinition {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub auth_type: AuthType,
    pub model_patterns: Vec<String>,
    pub compiled_patterns: Vec<Regex>,
    pub endpoints: Vec<String>,
    pub features: Vec<String>,
    pub model_count: usize,
    pub timeout_secs: u64,
    pub default_headers: HashMap<String, String>,
    pub token_endpoint: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

impl PartialEq for ProviderDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.display_name == other.display_name
            && self.base_url == other.base_url
            && self.auth_type == other.auth_type
            && self.model_patterns == other.model_patterns
            && self.endpoints == other.endpoints
            && self.features == other.features
            && self.model_count == other.model_count
            && self.timeout_secs == other.timeout_secs
            && self.default_headers == other.default_headers
            && self.token_endpoint == other.token_endpoint
            && self.client_id == other.client_id
            && self.client_secret == other.client_secret
    }
}

impl Eq for ProviderDefinition {}

/// Registry of provider definitions.
///
/// **Note on `Default`:** `ProviderRegistry::default()` creates an *empty*
/// registry with no providers. Use `ProviderRegistry::new()` for a registry
/// pre-populated with the built-in default provider definitions.
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: DashMap<String, ProviderDefinition>,
    provider_models: HashMap<String, Vec<String>>,
    env_model_patterns: HashMap<String, Vec<String>>,
    model_filter: ModelFilterEngine,
    cached_providers: std::sync::RwLock<Vec<ProviderDefinition>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        let registry = Self {
            providers: DashMap::new(),
            provider_models: parse_provider_models_env(),
            env_model_patterns: HashMap::new(),
            model_filter: ModelFilterEngine::default(),
            cached_providers: std::sync::RwLock::new(Vec::new()),
        };
        for provider in default_provider_definitions() {
            registry.register(provider);
        }
        registry
    }

    pub fn register(&self, mut def: ProviderDefinition) {
        def.id = normalize_provider_id(&def.id);
        def.compiled_patterns = def
            .model_patterns
            .iter()
            .filter_map(|pattern| Regex::new(pattern).ok())
            .collect();
        let id = def.id.clone();
        self.providers.insert(id.clone(), def.clone());

        let mut cached = self.cached_providers.write().unwrap();
        if let Some(pos) = cached.iter().position(|p| p.id == id) {
            cached[pos] = def;
        } else {
            cached.push(def);
        }
        cached.sort_by(|left, right| left.id.cmp(&right.id));
    }

    pub fn get(&self, id: &str) -> Option<ProviderDefinition> {
        let id = normalize_provider_id(id);
        self.providers.get(&id).map(|entry| entry.value().clone())
    }

    pub fn resolve_base_url(&self, id: &str) -> Option<String> {
        self.get(id).map(|def| def.base_url)
    }

    pub fn resolve_runtime_route(
        &self,
        provider_id: &str,
        action: &str,
    ) -> Option<RuntimeProviderRoute> {
        let provider_id = normalize_provider_id(provider_id);
        let definition = self.get(&provider_id)?;
        let action = self.resolve_action(&provider_id, action);

        Some(RuntimeProviderRoute {
            provider_id,
            kind: RuntimeProviderKind::Registry,
            base_url: definition.base_url,
            action,
        })
    }

    /// Load provider definitions from environment variables.
    /// Variables are expected in the form:
    ///   PROXY_<PROVIDER>_URL=https://...
    ///   <PROVIDER>_API_BASE=https://...
    ///   PROXY_<PROVIDER>_AUTH=api_key|bearer|oauth
    ///   PROXY_<PROVIDER>_MODELS=pattern1,pattern2
    ///   PROXY_<PROVIDER>_TIMEOUT=60
    /// If a variable is set for a provider already in the default registry, it overrides the default.
    pub fn load_from_env(&mut self) {
        self.env_model_patterns.clear();
        let mut provider_keys: Vec<String> = self
            .cached_providers
            .read()
            .unwrap()
            .iter()
            .map(|p| provider_env_key(&p.id))
            .collect();
        for (key, _) in std::env::vars() {
            if let Some(provider_key) = key
                .strip_prefix("PROXY_")
                .and_then(|key| key.strip_suffix("_URL"))
            {
                provider_keys.push(provider_key.to_owned());
            } else if let Some(provider_key) = key.strip_suffix("_API_BASE") {
                provider_keys.push(provider_key.to_owned());
            }
        }
        provider_keys.sort();
        provider_keys.dedup();

        for provider_key in provider_keys {
            let id = dynamic_provider_id(&provider_key);
            let Some(base_url) = std::env::var(format!("PROXY_{provider_key}_URL"))
                .ok()
                .or_else(|| std::env::var(format!("{provider_key}_API_BASE")).ok())
                .or_else(|| self.get(&id).map(|def| def.base_url))
            else {
                continue;
            };
            let auth_type = std::env::var(format!("PROXY_{provider_key}_AUTH"))
                .ok()
                .and_then(|auth| parse_auth_type(&auth))
                .or_else(|| self.get(&id).map(|def| def.auth_type))
                .unwrap_or(AuthType::Bearer);
            let model_patterns = std::env::var(format!("PROXY_{provider_key}_MODELS"))
                .ok()
                .map(|models| parse_models_csv(&models))
                .or_else(|| self.get(&id).map(|def| def.model_patterns))
                .unwrap_or_default();
            let display_name = std::env::var(format!("PROXY_{provider_key}_DISPLAY_NAME"))
                .ok()
                .or_else(|| self.get(&id).map(|def| def.display_name))
                .unwrap_or_else(|| id.clone());
            let endpoints = std::env::var(format!("PROXY_{provider_key}_ENDPOINTS"))
                .ok()
                .map(|endpoints| parse_csv_list(&endpoints))
                .or_else(|| self.get(&id).map(|def| def.endpoints))
                .unwrap_or_else(|| provider_endpoints(&id));
            let features = std::env::var(format!("PROXY_{provider_key}_FEATURES"))
                .ok()
                .map(|features| parse_csv_list(&features))
                .or_else(|| self.get(&id).map(|def| def.features))
                .unwrap_or_else(|| provider_features(&id));
            let model_count = std::env::var(format!("PROXY_{provider_key}_MODEL_COUNT"))
                .ok()
                .and_then(|count| count.parse().ok())
                .or_else(|| self.get(&id).map(|def| def.model_count))
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
            let token_endpoint = std::env::var(format!("PROXY_{provider_key}_TOKEN_ENDPOINT"))
                .ok()
                .or_else(|| self.get(&id).and_then(|def| def.token_endpoint));
            let client_id = std::env::var(format!("PROXY_{provider_key}_CLIENT_ID"))
                .ok()
                .or_else(|| self.get(&id).and_then(|def| def.client_id));
            let client_secret = std::env::var(format!("PROXY_{provider_key}_CLIENT_SECRET"))
                .ok()
                .or_else(|| self.get(&id).and_then(|def| def.client_secret));

            self.register(ProviderDefinition {
                id: id.clone(),
                display_name,
                base_url,
                auth_type,
                model_patterns: model_patterns.clone(),
                compiled_patterns: Vec::new(),
                endpoints,
                features,
                model_count,
                timeout_secs,
                default_headers,
                token_endpoint,
                client_id,
                client_secret,
            });
            if std::env::var(format!("PROXY_{provider_key}_MODELS")).is_ok() {
                self.env_model_patterns.insert(id, model_patterns);
            }
        }

        let provider_ids: Vec<String> = self
            .cached_providers
            .read()
            .unwrap()
            .iter()
            .map(|p| p.id.clone())
            .collect();
        self.model_filter = ModelFilterEngine::from_env(provider_ids.iter().map(String::as_str));
    }

    pub fn all_providers(&self) -> Vec<ProviderDefinition> {
        self.cached_providers.read().unwrap().clone()
    }

    pub fn get_provider_endpoints(&self, id: &str) -> Option<Vec<String>> {
        self.get(id).map(|def| def.endpoints)
    }

    pub fn get_provider_features(&self, id: &str) -> Option<Vec<String>> {
        self.get(id).map(|def| def.features)
    }

    pub fn get_static_models(&self, id: &str) -> Vec<String> {
        static_provider_models(id)
            .iter()
            .map(|model| (*model).to_owned())
            .collect()
    }

    pub fn resolve_endpoint_path(&self, provider: &str, path: &str, body: &Value) -> String {
        let provider = normalize_provider_id(provider);
        if matches!(provider.as_str(), "elysiver" | "colin")
            && path.trim_start_matches('/') == "chat/completions"
        {
            return "responses".to_owned();
        }
        if provider.as_str() == "openai"
            && path.trim_start_matches('/') == "chat/completions"
            && crate::providers::is_openai_responses_model(body)
        {
            return "responses".to_owned();
        }

        match (provider.as_str(), provider_action(path)) {
            ("gemini", Some(ProviderAction::Chat)) => body
                .get("model")
                .and_then(Value::as_str)
                .map(|model| format!("{model}:generateContent"))
                .unwrap_or_else(|| path.trim_start_matches('/').to_owned()),
            ("gemini", Some(ProviderAction::Embeddings)) => body
                .get("model")
                .and_then(Value::as_str)
                .map(|model| format!("{model}:embedContent"))
                .unwrap_or_else(|| path.trim_start_matches('/').to_owned()),
            _ => self.resolve_action(&provider, path),
        }
    }

    fn resolve_action(&self, provider: &str, action: &str) -> String {
        let action = action.trim_start_matches('/');
        if matches!(provider, "elysiver" | "colin") && action == "chat/completions" {
            return "responses".to_owned();
        }
        action.to_owned()
    }

    pub fn get_provider_catalog(&self) -> Vec<ProviderDefinition> {
        self.all_providers()
    }

    pub fn find_provider_for_model(&self, model: &str) -> Option<String> {
        if let Some((provider, _)) = model.split_once('/') {
            let provider = normalize_provider_id(provider);
            if self.get(&provider).is_some() {
                return Some(provider);
            }
        }

        static_provider_for_model(model)
            .map(ToOwned::to_owned)
            .or_else(|| {
                self.providers.iter().find_map(|entry| {
                    let provider = entry.value();
                    provider
                        .compiled_patterns
                        .iter()
                        .any(|regex| regex.is_match(model))
                        .then_some(provider.id.clone())
                })
            })
    }

    pub fn resolve_provider_by_model(&self, model: &str) -> Option<String> {
        if let Some((provider, _)) = model.split_once('/') {
            let provider = normalize_provider_id(provider);
            if self.get(&provider).is_some() {
                return Some(provider);
            }
        }

        self.provider_models
            .iter()
            .find_map(|(provider, models)| {
                models
                    .iter()
                    .any(|name| name == model)
                    .then_some(normalize_provider_id(provider))
            })
            .or_else(|| {
                self.env_model_patterns
                    .iter()
                    .find_map(|(provider, models)| {
                        models
                            .iter()
                            .any(|name| name == model)
                            .then_some(normalize_provider_id(provider))
                    })
            })
            .or_else(|| static_provider_for_model(model).map(ToOwned::to_owned))
            .or_else(|| prefix_provider_for_model(model).map(ToOwned::to_owned))
    }

    pub fn is_model_allowed(&self, model: &str) -> bool {
        self.is_provider_model_allowed(None, model)
    }

    pub fn is_provider_model_allowed(&self, provider: Option<&str>, model: &str) -> bool {
        self.model_filter.is_allowed(provider, model)
    }
}

fn provider_env_key(id: &str) -> String {
    id.to_ascii_uppercase().replace('-', "_")
}

fn parse_csv_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn provider_action(path: &str) -> Option<ProviderAction> {
    match path.trim_start_matches('/') {
        "chat/completions" | "messages" => Some(ProviderAction::Chat),
        "embeddings" => Some(ProviderAction::Embeddings),
        _ => None,
    }
}

fn parse_provider_models_env() -> HashMap<String, Vec<String>> {
    std::env::var("PROVIDER_MODELS")
        .ok()
        .map(|value| {
            value
                .split(';')
                .filter_map(|entry| {
                    let (provider, models) = entry.split_once('=')?;
                    let models: Vec<_> = models
                        .split(',')
                        .map(str::trim)
                        .filter(|model| !model.is_empty())
                        .map(ToOwned::to_owned)
                        .collect();
                    (!provider.trim().is_empty() && !models.is_empty())
                        .then_some((provider.trim().to_owned(), models))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn static_provider_models(id: &str) -> &'static [&'static str] {
    match id {
        "openai" => &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-4.1-mini",
            "o3-mini",
            "text-embedding-3-small",
            "dall-e-3",
        ],
        "gemini" => &[
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.0-flash",
            "gemini-1.5-pro",
        ],
        "gemini_cli" => &["gemini_cli/gemini-2.5-pro", "gemini_cli/gemini-2.5-flash"],
        "anthropic" => &[
            "claude-opus-4-1-20250805",
            "claude-sonnet-4-20250514",
            "claude-3-7-sonnet-20250219",
            "claude-3-5-haiku-20241022",
        ],
        "fireworks" => &[
            "accounts/fireworks/models/llama-v3p1-405b-instruct",
            "accounts/fireworks/models/llama-v3p1-70b-instruct",
            "accounts/fireworks/models/deepseek-r1",
        ],
        "nvidia" => &[
            "nvidia/llama-3.1-nemotron-70b-instruct",
            "meta/llama-3.1-405b-instruct",
        ],
        "qwen" => &["qwen-plus", "qwen-max", "qwen-turbo", "qwq-plus"],
        "qwen_code" => &["qwen_code/qwen3-coder-plus", "qwen3-coder-plus"],
        "zai" => &[
            "zai/glm-5.1",
            "zai/glm-5",
            "zai/glm-5-turbo",
            "zai/glm-4.7",
            "zai/glm-4.6",
            "zai/glm-4.5",
            "zai/glm-4-32b-0414-128k",
            "zai/glm-5v-turbo",
            "zai/glm-4.6v",
            "zai/glm-ocr",
            "zai/autoglm-phone-multilingual",
            "zai/glm-4.5v",
            "zai/glm-image",
            "zai/cogView-4-250304",
            "zai/cogvideox-3",
            "zai/viduq1-text",
            "zai/viduq1-image",
            "zai/vidu2-image",
            "zai/glm-asr-2512",
            "zai/glm-4.5-air",
        ],
        "iflow" => &["iflow/Qwen3-Coder", "kimi-k2", "Qwen3-Coder"],
        "colin" => &[
            "colin/claude-sonnet-4",
            "colin/claude-3-7-sonnet",
            "gpt-5.3-codex",
            "gpt-5.4",
        ],
        "elysiver" => &["elysiver/claude-sonnet-4", "elysiver/gpt-4o", "gpt-5.5"],
        "chutes" => &["chutes/deepseek-ai/DeepSeek-V3", "deepseek-ai/DeepSeek-R1"],
        "nanogpt" => &[
            "nanogpt/gpt-4o",
            "nanogpt/gpt-4o-mini",
            "nanogpt/claude-3.5-sonnet",
            "nanogpt/claude-3.5-haiku",
            "nanogpt/gemini-2.5-flash",
            "nanogpt/gemini-2.5-pro",
            "nano-gpt/claude-sonnet-4",
        ],
        "opencode" => &["opencode/zen", "zen/gpt-4o"],
        "firmware" => &["firmware/gpt-4o", "fw/claude-sonnet-4"],
        "antigravity" => &["antigravity/gemini-2.5-pro", "ag/gemini-2.5-flash"],
        "openrouter" => &[
            "openrouter/openai/gpt-4o",
            "openrouter/anthropic/claude-sonnet-4",
        ],
        "xai" => &["grok-4", "grok-3", "xai/grok-3-mini"],
        "kilocode" => &["kilocode/claude-sonnet-4", "kilocode/gpt-4o"],
        _ => &[],
    }
}

fn static_provider_for_model(model: &str) -> Option<&'static str> {
    [
        "colin",
        "elysiver",
        "openai",
        "gemini",
        "gemini_cli",
        "anthropic",
        "fireworks",
        "nvidia",
        "qwen",
        "qwen_code",
        "zai",
        "iflow",
        "chutes",
        "nanogpt",
        "opencode",
        "firmware",
        "antigravity",
        "openrouter",
        "xai",
        "kilocode",
    ]
    .into_iter()
    .find(|provider| static_provider_models(provider).contains(&model))
}

fn prefix_provider_for_model(model: &str) -> Option<&'static str> {
    [
        ("gpt-", "openai"),
        ("o1-", "openai"),
        ("o3-", "openai"),
        ("o4-", "openai"),
        ("text-embedding-", "openai"),
        ("dall-e-", "openai"),
        ("claude-", "anthropic"),
        ("gemini_cli/", "gemini_cli"),
        ("gemini-", "gemini"),
        ("gemini", "gemini"),
        ("models/gemini-", "gemini"),
        ("grok-", "xai"),
        ("xai/", "xai"),
        ("qwen-", "qwen"),
        ("qwq-", "qwen"),
        ("qwen/", "qwen"),
        ("qwen_code/", "qwen_code"),
        ("qwen3-coder", "qwen_code"),
        ("zai/", "zai"),
        ("glm-", "zai"),
        ("GLM-", "zai"),
    ]
    .into_iter()
    .find_map(|(prefix, provider)| model.starts_with(prefix).then_some(provider))
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
            AuthType::Bearer,
            &[
                r"^(gpt|o1|o3|o4)([-/].*)?$",
                r"^openai/.*",
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
        {
            let mut def = provider(
                "gemini_cli",
                "https://cloudcode-pa.googleapis.com/v1internal",
                AuthType::OAuth,
                &[r"^gemini_cli/.*"],
                120,
                &[],
            );
            def.token_endpoint = Some("https://oauth2.googleapis.com/token".to_owned());
            def
        },
        provider(
            "anthropic",
            "https://api.anthropic.com/v1",
            AuthType::Bearer,
            &[r"^claude[-/].*", r"^anthropic/.*"],
            60,
            &[("anthropic-version", "2023-06-01")],
        ),
        provider(
            "fireworks",
            "https://api.fireworks.ai/inference/v1",
            AuthType::Bearer,
            &[r"^accounts/fireworks/.*", r"^fireworks/.*"],
            120,
            &[],
        ),
        provider(
            "nvidia",
            "https://integrate.api.nvidia.com/v1",
            AuthType::Bearer,
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
            AuthType::Bearer,
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
            AuthType::Bearer,
            &[r"^colin/.*", r"^colin[-/].*"],
            60,
            &[],
        ),
        provider(
            "elysiver",
            "https://elysiver.h-e.top/v1",
            AuthType::Bearer,
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
            AuthType::Bearer,
            &[r"^nanogpt/.*", r"^nano-gpt/.*"],
            60,
            &[],
        ),
        provider(
            "opencode",
            "https://opencode.ai/zen/v1",
            AuthType::Bearer,
            &[r"^opencode/.*", r"^zen/.*"],
            60,
            &[("HTTP-Referer", "https://opencode.ai")],
        ),
        provider(
            "firmware",
            "https://app.firmware.ai/api/v1",
            AuthType::Bearer,
            &[r"^firmware/.*", r"^fw/.*"],
            60,
            &[],
        ),
        {
            let mut def = provider(
                "antigravity",
                "https://cloudcode-pa.googleapis.com/v1internal",
                AuthType::OAuth,
                &[r"^antigravity/.*", r"^ag/.*"],
                120,
                &[],
            );
            def.token_endpoint = Some("https://oauth2.googleapis.com/token".to_owned());
            def
        },
        provider(
            "openrouter",
            "https://openrouter.ai/api/v1",
            AuthType::Bearer,
            &[r"^openrouter/.*"],
            60,
            &[],
        ),
        provider(
            "xai",
            "https://api.x.ai/v1",
            AuthType::Bearer,
            &[r"^xai/.*", r"^grok[-/].*"],
            60,
            &[],
        ),
        provider(
            "kilocode",
            "https://kilo.ai/api/openrouter",
            AuthType::Bearer,
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
    let model_patterns: Vec<String> = model_patterns
        .iter()
        .map(|pattern| (*pattern).to_owned())
        .collect();
    let model_count = model_patterns.len();
    let compiled_patterns = model_patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect();
    ProviderDefinition {
        id: id.to_owned(),
        display_name: provider_display_name(id),
        base_url: base_url.to_owned(),
        auth_type,
        model_patterns,
        compiled_patterns,
        endpoints: provider_endpoints(id),
        features: provider_features(id),
        model_count,
        timeout_secs,
        default_headers: default_headers
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        token_endpoint: None,
        client_id: None,
        client_secret: None,
    }
}

fn provider_display_name(id: &str) -> String {
    match id {
        "openai" => "OpenAI",
        "gemini" => "Gemini",
        "anthropic" => "Anthropic",
        other => other,
    }
    .to_owned()
}

fn provider_endpoints(id: &str) -> Vec<String> {
    let endpoints = match id {
        "openai" => [
            "/chat/completions",
            "/responses",
            "/embeddings",
            "/images/generations",
        ]
        .as_slice(),
        _ => ["/chat/completions"].as_slice(),
    };

    endpoints
        .iter()
        .map(|endpoint| (*endpoint).to_owned())
        .collect()
}

fn provider_features(id: &str) -> Vec<String> {
    let features = match id {
        "openai" => ["chat", "streaming", "embeddings", "vision", "images"].as_slice(),
        "anthropic" => ["chat", "streaming", "vision"].as_slice(),
        _ => ["chat", "streaming"].as_slice(),
    };

    features
        .iter()
        .map(|feature| (*feature).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_registry_is_empty() {
        let registry = ProviderRegistry::default();
        assert!(registry.all_providers().is_empty());
    }

    #[test]
    fn default_registry_resolves_provider_base_url() {
        let registry = ProviderRegistry::new();

        assert_eq!(
            registry.resolve_base_url("openai").as_deref(),
            Some("https://api.openai.com/v1")
        );
    }

    #[test]
    fn default_registry_uses_bearer_auth_for_anthropic() {
        let registry = ProviderRegistry::new();
        let provider = registry
            .get("anthropic")
            .expect("anthropic provider exists");

        assert_eq!(provider.auth_type, AuthType::Bearer);
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
        assert_eq!(
            registry.find_provider_for_model("elysiver/gpt-5.5"),
            Some("elysiver".to_owned())
        );
        assert_eq!(
            registry.find_provider_for_model("openai/gpt-5.5"),
            Some("openai".to_owned())
        );
    }

    #[test]
    fn gpt55_without_prefix_resolves_to_elysiver_static_catalog() {
        let registry = ProviderRegistry::new();

        assert_eq!(
            registry.find_provider_for_model("gpt-5.5"),
            Some("elysiver".to_owned())
        );
        assert_eq!(
            registry.resolve_provider_by_model("gpt-5.5"),
            Some("elysiver".to_owned())
        );
    }

    #[test]
    fn colin_static_catalog_contains_codex_and_gpt54() {
        let registry = ProviderRegistry::new();

        assert_eq!(
            registry.resolve_provider_by_model("gpt-5.3-codex"),
            Some("colin".to_owned())
        );
        assert_eq!(
            registry.resolve_provider_by_model("gpt-5.4"),
            Some("colin".to_owned())
        );
    }

    #[test]
    fn gemini_endpoint_paths_include_model_action_suffixes() {
        let registry = ProviderRegistry::new();

        assert_eq!(
            registry.resolve_endpoint_path(
                "gemini",
                "chat/completions",
                &serde_json::json!({"model": "models/gemini-2.5-flash"})
            ),
            "models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(
            registry.resolve_endpoint_path(
                "gemini",
                "embeddings",
                &serde_json::json!({"model": "models/gemini-embedding-001"})
            ),
            "models/gemini-embedding-001:embedContent"
        );
        assert_eq!(
            registry.resolve_endpoint_path(
                "openai",
                "chat/completions",
                &serde_json::json!({"model": "gpt-4o-mini"})
            ),
            "chat/completions"
        );
    }

    #[test]
    fn elysiver_and_colin_chat_requests_use_responses_endpoint() {
        let registry = ProviderRegistry::new();

        for provider in ["elysiver", "colin"] {
            assert_eq!(
                registry.resolve_endpoint_path(
                    provider,
                    "chat/completions",
                    &serde_json::json!({"model": "gpt-5.5"})
                ),
                "responses"
            );
        }
    }

    #[test]
    fn resolve_provider_by_model_uses_env_overrides_before_prefix_fallback() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(
                "PROVIDER_MODELS",
                "custom=gpt-4o-mini,exact-model;other=claude-custom",
            );
        }

        let mut registry = ProviderRegistry::new();
        registry.load_from_env();

        assert_eq!(
            registry.resolve_provider_by_model("gpt-4o-mini"),
            Some("custom".to_owned())
        );
        assert_eq!(
            registry.resolve_provider_by_model("claude-3-5-sonnet-20241022"),
            Some("anthropic".to_owned())
        );
        assert_eq!(
            registry.resolve_provider_by_model("text-embedding-3-small"),
            Some("openai".to_owned())
        );
        assert_eq!(registry.resolve_provider_by_model("unknown-model"), None);

        unsafe {
            std::env::remove_var("PROVIDER_MODELS");
        }
    }

    #[test]
    fn load_from_env_overrides_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("PROXY_OPENAI_URL", "https://override.example/v1");
            std::env::set_var("PROXY_OPENAI_AUTH", "bearer");
            std::env::set_var("PROXY_OPENAI_MODELS", "^override/.*,^custom-.*");
            std::env::set_var("PROXY_OPENAI_TIMEOUT", "45");
        }

        let mut registry = ProviderRegistry::new();
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

    #[test]
    fn load_from_env_uses_api_base_when_proxy_url_is_absent() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("PROXY_DEEPSEEK_URL");
            std::env::set_var("DEEPSEEK_API_BASE", "https://deepseek.example/v1");
        }

        let mut registry = ProviderRegistry::new();
        registry.load_from_env();

        let provider = registry.get("deepseek").expect("deepseek provider exists");
        assert_eq!(provider.base_url, "https://deepseek.example/v1");
        assert_eq!(provider.auth_type, AuthType::Bearer);

        unsafe {
            std::env::remove_var("DEEPSEEK_API_BASE");
        }
    }

    #[test]
    fn load_from_env_keeps_default_registry_auth_types() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("PROXY_GEMINI_URL", "https://gemini.override/v1beta");
            std::env::set_var("PROXY_ANTHROPIC_URL", "https://anthropic.override/v1");
            std::env::set_var("PROXY_OPENAI_URL", "https://openai.override/v1");
        }

        let mut registry = ProviderRegistry::new();
        registry.load_from_env();

        assert_eq!(
            registry
                .get("gemini")
                .expect("gemini provider exists")
                .auth_type,
            AuthType::ApiKey
        );
        assert_eq!(
            registry
                .get("anthropic")
                .expect("anthropic provider exists")
                .auth_type,
            AuthType::Bearer
        );
        assert_eq!(
            registry
                .get("openai")
                .expect("openai provider exists")
                .auth_type,
            AuthType::Bearer
        );

        unsafe {
            std::env::remove_var("PROXY_GEMINI_URL");
            std::env::remove_var("PROXY_ANTHROPIC_URL");
            std::env::remove_var("PROXY_OPENAI_URL");
        }
    }

    #[test]
    fn model_filters_require_allowlist_match_before_denylist() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("MODEL_ALLOWLIST", "^gpt-4.*,^claude-.*");
            std::env::set_var("MODEL_DENYLIST", "gpt-4-vision.*,claude-2.*");
        }

        let mut registry = ProviderRegistry::new();
        registry.load_from_env();

        assert!(registry.is_model_allowed("gpt-4o-mini"));
        assert!(registry.is_model_allowed("claude-3-5-sonnet-20241022"));
        assert!(!registry.is_model_allowed("gpt-4-vision-preview"));
        assert!(!registry.is_model_allowed("claude-2.1"));
        assert!(!registry.is_model_allowed("gemini-2.5-flash"));

        unsafe {
            std::env::remove_var("MODEL_ALLOWLIST");
            std::env::remove_var("MODEL_DENYLIST");
        }
    }

    #[test]
    fn nanogpt_static_catalog_contains_fallback_models() {
        let registry = ProviderRegistry::new();
        for model in [
            "nanogpt/gpt-4o",
            "nanogpt/gpt-4o-mini",
            "nanogpt/claude-3.5-sonnet",
            "nanogpt/claude-3.5-haiku",
            "nanogpt/gemini-2.5-flash",
            "nanogpt/gemini-2.5-pro",
            "nano-gpt/claude-sonnet-4",
        ] {
            assert!(
                registry.resolve_provider_by_model(model).as_deref() == Some("nanogpt"),
                "expected {model} to resolve to nanogpt"
            );
        }
    }

    #[test]
    fn zai_static_catalog_contains_documented_models() {
        let registry = ProviderRegistry::new();
        for model in [
            "zai/glm-5.1",
            "zai/glm-5",
            "zai/glm-5-turbo",
            "zai/glm-4.7",
            "zai/glm-4.6",
            "zai/glm-4.5",
            "zai/glm-5v-turbo",
            "zai/glm-ocr",
            "zai/cogvideox-3",
        ] {
            assert!(
                registry.resolve_provider_by_model(model).as_deref() == Some("zai"),
                "expected {model} to resolve to zai"
            );
        }
    }

    #[test]
    fn chutes_static_catalog_contains_fallback_models() {
        let registry = ProviderRegistry::new();
        for model in ["chutes/deepseek-ai/DeepSeek-V3", "deepseek-ai/DeepSeek-R1"] {
            assert!(
                registry.resolve_provider_by_model(model).as_deref() == Some("chutes"),
                "expected {model} to resolve to chutes"
            );
        }
    }

    #[test]
    fn firmware_static_catalog_contains_fallback_models() {
        let registry = ProviderRegistry::new();
        for model in ["firmware/gpt-4o", "fw/claude-sonnet-4"] {
            assert!(
                registry.resolve_provider_by_model(model).as_deref() == Some("firmware"),
                "expected {model} to resolve to firmware"
            );
        }
    }
}
