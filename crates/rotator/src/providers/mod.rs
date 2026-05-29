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

// Legacy provider modules are helper implementations only. Runtime parity must be wired through
// ProviderRegistry/provider_runtime before changing dispatch behavior here.
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
        "elysiver" | "colin" => {
            transform_responses_compat_request(provider, body);
        }
        _ => {
            sanitize_gpt5_request(body);
            match provider {
                "anthropic" => anthropic::AnthropicProvider::new().transform_request(body),
                "gemini" => gemini::GeminiProvider::new().transform_request(body),
                "nvidia" => nvidia::NvidiaProvider::new().transform_request(body),
                _ => {}
            }
        }
    }
}

fn transform_responses_compat_request(provider: &str, body: &mut serde_json::Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    if let Some(model) = object.get("model").and_then(serde_json::Value::as_str) {
        let prefix = format!("{provider}/");
        if let Some(stripped) = model.strip_prefix(&prefix) {
            object.insert(
                "model".to_owned(),
                serde_json::Value::String(stripped.to_owned()),
            );
        }
    }

    if let Some(messages) = object.remove("messages") {
        let mut input = Vec::new();
        let mut instructions = Vec::new();
        if let Some(messages) = messages.as_array() {
            for message in messages {
                if message.get("role").and_then(serde_json::Value::as_str) == Some("system") {
                    if let Some(content) = message.get("content") {
                        instructions.push(message_content_to_text(content));
                    }
                    continue;
                }
                input.push(message.clone());
            }
        }
        object.insert("input".to_owned(), serde_json::Value::Array(input));
        if !instructions.is_empty() {
            object.insert(
                "instructions".to_owned(),
                serde_json::Value::String(instructions.join("\n")),
            );
        }
    }

    if let Some(max_tokens) = object.remove("max_tokens") {
        object.insert("max_output_tokens".to_owned(), max_tokens);
    }
    if let Some(max_completion_tokens) = object.remove("max_completion_tokens") {
        object.insert("max_output_tokens".to_owned(), max_completion_tokens);
    }
    object.insert("stream".to_owned(), serde_json::Value::Bool(true));
}

fn message_content_to_text(content: &serde_json::Value) -> String {
    content
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| content.to_string())
}

fn sanitize_gpt5_request(body: &mut serde_json::Value) {
    let is_gpt5 = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|model| model.starts_with("gpt-5"));
    if !is_gpt5 {
        return;
    }

    let Some(object) = body.as_object_mut() else {
        return;
    };

    object.remove("temperature");
    if let Some(max_tokens) = object.remove("max_tokens") {
        object.insert("max_completion_tokens".to_owned(), max_tokens);
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

    #[test]
    fn gpt5_transform_removes_temperature_and_renames_max_tokens() {
        let mut body = serde_json::json!({
            "model": "gpt-5.5",
            "temperature": 0.2,
            "max_tokens": 128
        });

        transform_request_for_provider("openai", &mut body);

        assert!(body.get("temperature").is_none());
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 128);
    }

    #[test]
    fn elysiver_transform_uses_responses_api_body_and_forces_streaming() {
        let mut body = serde_json::json!({
            "model": "elysiver/gpt-5.5",
            "messages": [
                {"role": "system", "content": "You are concise."},
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"}
            ],
            "max_tokens": 128,
            "stream": false
        });

        transform_request_for_provider("elysiver", &mut body);

        assert_eq!(body["model"], "gpt-5.5");
        assert_eq!(body["instructions"], "You are concise.");
        assert!(body.get("messages").is_none());
        assert_eq!(
            body["input"],
            serde_json::json!([
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"}
            ])
        );
        assert_eq!(body["max_output_tokens"], 128);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["stream"], true);
    }
}
