use super::Provider;
use crate::error::Result;
use crate::providers::openai::OpenAiProvider;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct GeminiProvider {
    inner: OpenAiProvider,
}

impl GeminiProvider {
    pub fn new() -> Self {
        Self::new_with_base_url("https://generativelanguage.googleapis.com/v1beta")
    }

    pub fn new_with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            inner: OpenAiProvider::new(base_url),
        }
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn id(&self) -> &str {
        "gemini"
    }

    fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        self.inner.auth_headers(api_key)
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    async fn request(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: serde_json::Value,
        api_key: &str,
    ) -> Result<reqwest::Response> {
        self.inner.request(client, path, body, api_key).await
    }

    async fn list_models(
        &self,
        client: &reqwest::Client,
        api_key: &str,
    ) -> Result<Vec<serde_json::Value>> {
        self.inner.list_models(client, api_key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_provider_exposes_expected_metadata_and_auth() {
        let provider = GeminiProvider::new();

        assert_eq!(provider.id(), "gemini");
        assert_eq!(provider.base_url(), "https://generativelanguage.googleapis.com/v1beta");
        assert_eq!(
            provider.auth_headers("test-key"),
            vec![("authorization".to_owned(), "Bearer test-key".to_owned())]
        );
        assert!(provider.supports_streaming());
    }
}
