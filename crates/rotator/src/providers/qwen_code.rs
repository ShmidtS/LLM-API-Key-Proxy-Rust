use super::{Provider, bearer_auth_headers, list_data_models, send_json_request};
use crate::error::Result;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct QwenCodeProvider {
    base_url: String,
}

impl QwenCodeProvider {
    pub fn new() -> Self {
        Self {
            base_url: "https://portal.qwen.ai/v1".to_owned(),
        }
    }
}

#[async_trait]
impl Provider for QwenCodeProvider {
    fn id(&self) -> &str {
        "qwen_code"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        bearer_auth_headers(api_key)
    }

    fn supports_streaming(&self) -> bool {
        true
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
            &self.base_url,
            path,
            body,
            self.auth_headers(api_key),
        )
        .await
    }

    async fn list_models(
        &self,
        client: &reqwest::Client,
        api_key: &str,
    ) -> Result<Vec<serde_json::Value>> {
        list_data_models(client, &self.base_url, self.auth_headers(api_key)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwen_code_provider_exposes_expected_metadata_and_auth() {
        let provider = QwenCodeProvider::new();

        assert_eq!(provider.id(), "qwen_code");
        assert_eq!(provider.base_url(), "https://portal.qwen.ai/v1");
        assert_eq!(
            provider.auth_headers("test-key"),
            vec![("authorization".to_owned(), "Bearer test-key".to_owned())]
        );
        assert!(provider.supports_streaming());
    }
}
