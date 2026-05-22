use crate::credentials::CredentialManager;
use crate::error::{Result, RotatorError};
use crate::provider_registry::ProviderRegistry;
use async_trait::async_trait;
use dashmap::DashMap;
use std::fmt::Debug;

pub mod anthropic;
pub mod antigravity;
pub mod chutes;
pub mod colin;
pub mod elysiver;
pub mod fireworks;
pub mod firmware;
pub mod gemini;
pub mod gemini_cli;
pub mod iflow;
pub mod kilocode;
pub mod nanogpt;
pub mod nvidia;
pub mod oauth;
pub mod openai;
pub mod opencode;
pub mod openrouter;
pub mod qwen;
pub mod qwen_code;
pub mod xai;
pub mod zai;

pub(crate) fn bearer_auth_headers(api_key: &str) -> Vec<(String, String)> {
    vec![("authorization".to_owned(), format!("Bearer {api_key}"))]
}

pub(crate) async fn send_json_request(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
) -> Result<reqwest::Response> {
    let url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let mut request = client.post(url).json(&body);

    for (name, value) in headers {
        request = request.header(name, value);
    }

    Ok(request.send().await?)
}

pub(crate) async fn list_data_models(
    client: &reqwest::Client,
    base_url: &str,
    headers: Vec<(String, String)>,
) -> Result<Vec<serde_json::Value>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = client.get(url);

    for (name, value) in headers {
        request = request.header(name, value);
    }

    let value: serde_json::Value = request.send().await?.json().await?;
    Ok(value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub fn transform_request_for_provider(provider: &str, body: &mut serde_json::Value) {
    match provider {
        "anthropic" => anthropic::AnthropicProvider::new().transform_request(body),
        "gemini" => gemini::GeminiProvider::new().transform_request(body),
        "nvidia" => nvidia::NvidiaProvider::new().transform_request(body),
        _ => {}
    }
}

#[async_trait]
pub trait Provider: Send + Sync + Debug {
    fn id(&self) -> &str;
    fn base_url(&self) -> &str;
    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)>;
    fn supports_streaming(&self) -> bool;

    fn transform_request(&self, _body: &mut serde_json::Value) {}

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

    /// Alias for list_models for backward compatibility
    async fn get_models(
        &self,
        client: &reqwest::Client,
        api_key: &str,
    ) -> Result<Vec<serde_json::Value>> {
        self.list_models(client, api_key).await
    }

    /// Stream a request. Default implementation delegates to request().
    async fn stream(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: serde_json::Value,
        api_key: &str,
    ) -> Result<reqwest::Response> {
        self.request(client, path, body, api_key).await
    }
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

    pub fn get(
        &self,
        id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, Box<dyn Provider>>> {
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
        let provider = self.get(&provider_id).ok_or_else(|| {
            RotatorError::Other(format!("provider not registered: {provider_id}"))
        })?;
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
            client: &reqwest::Client,
            path: &str,
            body: serde_json::Value,
            api_key: &str,
        ) -> Result<reqwest::Response> {
            send_json_request(
                client,
                self.base_url(),
                path,
                body,
                self.auth_headers(api_key),
            )
            .await
        }

        async fn list_models(
            &self,
            _client: &reqwest::Client,
            _api_key: &str,
        ) -> Result<Vec<serde_json::Value>> {
            Ok(vec![])
        }

        async fn get_models(
            &self,
            client: &reqwest::Client,
            api_key: &str,
        ) -> Result<Vec<serde_json::Value>> {
            self.list_models(client, api_key).await
        }

        async fn stream(
            &self,
            client: &reqwest::Client,
            path: &str,
            body: serde_json::Value,
            api_key: &str,
        ) -> Result<reqwest::Response> {
            self.request(client, path, body, api_key).await
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
