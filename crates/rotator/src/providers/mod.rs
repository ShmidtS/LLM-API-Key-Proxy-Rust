use crate::credentials::CredentialManager;
use crate::error::{Result, RotatorError};
use crate::provider_registry::ProviderRegistry;
use async_trait::async_trait;
use dashmap::DashMap;
use std::fmt::Debug;

pub mod anthropic;
pub mod chutes;
pub mod colin;
pub mod elysiver;
pub mod firmware;
pub mod fireworks;
pub mod gemini;
pub mod iflow;
pub mod kilocode;
pub mod nanogpt;
pub mod nvidia;
pub mod oauth;
pub mod opencode;
pub mod openai;
pub mod openrouter;
pub mod qwen;
pub mod xai;
pub mod zai;

#[async_trait]
pub trait Provider: Send + Sync + Debug {
    fn id(&self) -> &str;
    fn base_url(&self) -> &str;
    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)>;
    fn supports_streaming(&self) -> bool;

    /// Forward a request to the provider. Path is relative (e.g., "chat/completions").
    async fn request(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: serde_json::Value,
        api_key: &str,
    ) -> Result<reqwest::Response>;

    /// List models from the provider
    async fn list_models(
        &self,
        client: &reqwest::Client,
        api_key: &str,
    ) -> Result<Vec<serde_json::Value>>;
}

#[derive(Debug, Default)]
pub struct ProviderManager {
    providers: DashMap<String, Box<dyn Provider>>,
}

impl ProviderManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, id: String, provider: Box<dyn Provider>) {
        self.providers.insert(id, provider);
    }

    pub fn get(&self, id: &str) -> Option<dashmap::mapref::one::Ref<'_, String, Box<dyn Provider>>> {
        self.providers.get(id)
    }

    pub fn resolve_provider_for_model(
        &self,
        model: &str,
        registry: &ProviderRegistry,
    ) -> Option<String> {
        registry.find_provider_for_model(model)
    }

    pub async fn request_by_model(
        &self,
        client: &reqwest::Client,
        model: &str,
        path: &str,
        body: serde_json::Value,
        registry: &ProviderRegistry,
        credentials: &CredentialManager,
    ) -> Result<reqwest::Response> {
        let provider_id = self
            .resolve_provider_for_model(model, registry)
            .ok_or_else(|| RotatorError::Other(format!("no provider found for model: {model}")))?;
        let provider = self
            .get(&provider_id)
            .ok_or_else(|| RotatorError::Other(format!("provider not registered: {provider_id}")))?;
        let credential = credentials
            .get_least_loaded(&provider_id)
            .ok_or_else(|| RotatorError::NoCredentials(provider_id.clone()))?;

        credentials.increment(&provider_id, &credential.key);
        let result = provider.request(client, path, body, &credential.key).await;
        credentials.decrement(&provider_id, &credential.key);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestProvider;

    #[async_trait]
    impl Provider for TestProvider {
        fn id(&self) -> &str {
            "test"
        }

        fn base_url(&self) -> &str {
            "https://test.example/v1"
        }

        fn auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
            vec![("authorization".to_owned(), format!("Bearer {api_key}"))]
        }

        fn supports_streaming(&self) -> bool {
            false
        }

        async fn request(
            &self,
            _client: &reqwest::Client,
            _path: &str,
            _body: serde_json::Value,
            _api_key: &str,
        ) -> Result<reqwest::Response> {
            unimplemented!()
        }

        async fn list_models(
            &self,
            _client: &reqwest::Client,
            _api_key: &str,
        ) -> Result<Vec<serde_json::Value>> {
            unimplemented!()
        }
    }

    #[test]
    fn provider_manager_registers_and_looks_up_provider() {
        let manager = ProviderManager::new();

        manager.register("test".to_owned(), Box::new(TestProvider));

        let provider = manager.get("test").expect("provider registered");
        assert_eq!(provider.id(), "test");
        assert_eq!(provider.base_url(), "https://test.example/v1");
    }
}
