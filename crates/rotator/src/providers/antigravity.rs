use super::oauth::{
    OAuthFlow, OAuthFlowConfig, OAuthToken, authenticate_with_config, refresh_oauth_token,
};
use super::{Provider, bearer_auth_headers, list_data_models, send_json_request};
use crate::error::Result;
use async_trait::async_trait;
use dashmap::DashMap;

#[derive(Debug, Default)]
pub struct AntigravityProvider {
    base_url: String,
    thinking_cache: DashMap<String, String>,
}

impl AntigravityProvider {
    pub fn new() -> Self {
        Self {
            base_url: "https://cloudcode-pa.googleapis.com/v1internal".to_owned(),
            thinking_cache: DashMap::new(),
        }
    }

    pub async fn recover_tool(&self, key: &str) -> Option<String> {
        self.thinking_cache
            .get(key)
            .map(|value| value.value().clone())
    }
}

#[async_trait]
impl Provider for AntigravityProvider {
    fn id(&self) -> &str {
        "antigravity"
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

#[derive(Debug, Default)]
pub struct AntigravityOAuthFlow;

impl AntigravityOAuthFlow {
    pub fn oauth_config() -> OAuthFlowConfig {
        OAuthFlowConfig {
            provider_id: "antigravity",
            client_id: "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com",
            client_secret: std::env::var("ANTIGRAVITY_CLIENT_SECRET")
                .ok()
                .map(|value| Box::leak(value.into_boxed_str()) as &'static str),
            auth_endpoint: "https://accounts.google.com/o/oauth2/v2/auth",
            token_endpoint: "https://oauth2.googleapis.com/token",
            scopes: &[
                "https://www.googleapis.com/auth/cloud-platform",
                "https://www.googleapis.com/auth/userinfo.email",
                "https://www.googleapis.com/auth/userinfo.profile",
                "https://www.googleapis.com/auth/cclog",
                "https://www.googleapis.com/auth/experimentsandconfigs",
            ],
            callback_path: "/oauthcallback",
            callback_port: 51121,
            credential_prefix: "antigravity",
        }
    }
}

#[async_trait]
impl OAuthFlow for AntigravityOAuthFlow {
    fn provider_id(&self) -> &str {
        "antigravity"
    }

    async fn authenticate(&self, client: &reqwest::Client) -> Result<OAuthToken> {
        authenticate_with_config(client, Self::oauth_config()).await
    }

    async fn refresh(&self, client: &reqwest::Client, token: &OAuthToken) -> Result<OAuthToken> {
        let config = Self::oauth_config();
        refresh_oauth_token(
            client,
            config.token_endpoint,
            config.client_id,
            config.client_secret,
            token,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_provider_exposes_expected_metadata_and_auth() {
        let provider = AntigravityProvider::new();

        assert_eq!(provider.id(), "antigravity");
        assert_eq!(
            provider.base_url(),
            "https://cloudcode-pa.googleapis.com/v1internal"
        );
        assert_eq!(
            provider.auth_headers("test-key"),
            vec![("authorization".to_owned(), "Bearer test-key".to_owned())]
        );
        assert!(provider.supports_streaming());
    }
}
