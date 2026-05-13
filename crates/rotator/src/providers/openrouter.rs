use super::Provider;
use crate::error::Result;
use crate::provider_registry::AuthType;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    base_url: String,
    auth_type: AuthType,
}

impl OpenRouterProvider {
    pub fn new() -> Self {
        Self {
            base_url: "https://openrouter.ai/api/v1".to_owned(),
            auth_type: AuthType::ApiKey,
        }
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn id(&self) -> &str {
        "openrouter"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        match self.auth_type {
            AuthType::ApiKey | AuthType::Bearer => {
                vec![("authorization".to_owned(), format!("Bearer {api_key}"))]
            }
            AuthType::OAuth => {
                vec![("authorization".to_owned(), format!("Bearer {api_key}"))]
            }
        }
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
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'));
        let mut request = client.post(url).json(&body);

        for (name, value) in self.auth_headers(api_key) {
            request = request.header(name, value);
        }

        Ok(request.send().await?)
    }

    async fn list_models(
        &self,
        client: &reqwest::Client,
        api_key: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let mut request = client.get(url);

        for (name, value) in self.auth_headers(api_key) {
            request = request.header(name, value);
        }

        let value: serde_json::Value = request.send().await?.json().await?;
        Ok(value
            .get("data")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_provider_exposes_expected_metadata_and_auth() {
        let provider = OpenRouterProvider::new();

        assert_eq!(provider.id(), "openrouter");
        assert_eq!(provider.base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(
            provider.auth_headers("test-key"),
            vec![("authorization".to_owned(), "Bearer test-key".to_owned())]
        );
        assert!(provider.supports_streaming());
    }
}
