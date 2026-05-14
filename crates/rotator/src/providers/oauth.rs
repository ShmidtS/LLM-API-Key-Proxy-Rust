use crate::error::{Result, RotatorError};
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::future::Future;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub token_type: String,
}

#[async_trait]
pub trait OAuthFlow: Send + Sync + Debug {
    fn provider_id(&self) -> &str;
    async fn authenticate(&self, client: &reqwest::Client) -> Result<OAuthToken>;
    async fn refresh(&self, client: &reqwest::Client, token: &OAuthToken) -> Result<OAuthToken>;
}

#[derive(Debug, Default)]
pub struct GoogleOAuthFlow;

#[async_trait]
impl OAuthFlow for GoogleOAuthFlow {
    fn provider_id(&self) -> &str {
        "gemini"
    }

    async fn authenticate(&self, _client: &reqwest::Client) -> Result<OAuthToken> {
        Err(RotatorError::Other("OAuth flow not implemented".into()))
    }

    async fn refresh(&self, _client: &reqwest::Client, _token: &OAuthToken) -> Result<OAuthToken> {
        Err(RotatorError::Other("OAuth refresh not implemented".into()))
    }
}

#[derive(Debug, Default)]
pub struct QwenOAuthFlow;

#[async_trait]
impl OAuthFlow for QwenOAuthFlow {
    fn provider_id(&self) -> &str {
        "qwen"
    }

    async fn authenticate(&self, _client: &reqwest::Client) -> Result<OAuthToken> {
        Err(RotatorError::Other("OAuth flow not implemented".into()))
    }

    async fn refresh(&self, _client: &reqwest::Client, _token: &OAuthToken) -> Result<OAuthToken> {
        Err(RotatorError::Other("OAuth refresh not implemented".into()))
    }
}

#[derive(Debug, Default)]
pub struct IflowOAuthFlow;

#[async_trait]
impl OAuthFlow for IflowOAuthFlow {
    fn provider_id(&self) -> &str {
        "iflow"
    }

    async fn authenticate(&self, _client: &reqwest::Client) -> Result<OAuthToken> {
        Err(RotatorError::Other("OAuth flow not implemented".into()))
    }

    async fn refresh(&self, _client: &reqwest::Client, _token: &OAuthToken) -> Result<OAuthToken> {
        Err(RotatorError::Other("OAuth refresh not implemented".into()))
    }
}

#[derive(Debug, Default)]
pub struct OAuthManager {
    tokens: DashMap<String, OAuthToken>,
}

impl OAuthManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_token(&self, provider_id: &str) -> Option<OAuthToken> {
        self.tokens.get(provider_id).map(|token| token.clone())
    }

    pub fn set_token(&self, provider_id: &str, token: OAuthToken) {
        self.tokens.insert(provider_id.to_owned(), token);
    }

    pub fn is_expired(&self, provider_id: &str) -> bool {
        self.tokens
            .get(provider_id)
            .and_then(|token| token.expires_at)
            .is_some_and(|expires_at| expires_at <= chrono::Utc::now().timestamp() as u64)
    }

    pub async fn refresh_if_needed<F, Fut>(
        &self,
        provider_id: &str,
        refresh_fn: F,
    ) -> Result<OAuthToken>
    where
        F: Fn(&str) -> Fut,
        Fut: Future<Output = Result<OAuthToken>>,
    {
        if !self.is_expired(provider_id) {
            if let Some(token) = self.get_token(provider_id) {
                return Ok(token);
            }
        }

        let token = refresh_fn(provider_id).await?;
        self.set_token(provider_id, token.clone());
        Ok(token)
    }
}

pub fn auth_headers_oauth(token: &OAuthToken) -> Vec<(String, String)> {
    vec![(
        "Authorization".to_owned(),
        format!("Bearer {}", token.access_token),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after unix epoch")
            .as_secs()
    }

    fn token(access_token: &str, expires_at: Option<u64>) -> OAuthToken {
        OAuthToken {
            access_token: access_token.to_owned(),
            refresh_token: Some("refresh".to_owned()),
            expires_at,
            token_type: "Bearer".to_owned(),
        }
    }

    #[test]
    fn stores_and_retrieves_token() {
        let manager = OAuthManager::new();
        let token = token("access", Some(now() + 60));

        manager.set_token("provider", token.clone());

        let stored = manager.get_token("provider").expect("token stored");
        assert_eq!(stored.access_token, token.access_token);
        assert_eq!(stored.refresh_token, token.refresh_token);
        assert_eq!(stored.expires_at, token.expires_at);
        assert_eq!(stored.token_type, token.token_type);
    }

    #[test]
    fn detects_expired_tokens() {
        let manager = OAuthManager::new();

        manager.set_token("expired", token("expired", Some(now() - 1)));
        manager.set_token("valid", token("valid", Some(now() + 60)));
        manager.set_token("unknown", token("unknown", None));

        assert!(manager.is_expired("expired"));
        assert!(!manager.is_expired("valid"));
        assert!(!manager.is_expired("unknown"));
        assert!(!manager.is_expired("missing"));
    }

    #[tokio::test]
    async fn refreshes_token_when_expired() {
        let manager = OAuthManager::new();
        let calls = Arc::new(AtomicUsize::new(0));
        manager.set_token("provider", token("old", Some(now() - 1)));

        let refreshed = manager
            .refresh_if_needed("provider", {
                let calls = Arc::clone(&calls);
                move |provider_id| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let access_token = format!("new-{provider_id}");
                    async move { Ok(token(&access_token, Some(now() + 60))) }
                }
            })
            .await
            .expect("refresh succeeds");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(refreshed.access_token, "new-provider");
        assert_eq!(
            manager
                .get_token("provider")
                .expect("refreshed token stored")
                .access_token,
            "new-provider"
        );
    }

    #[test]
    fn builds_oauth_auth_header() {
        let token = token("access", None);

        assert_eq!(
            auth_headers_oauth(&token),
            vec![("Authorization".to_owned(), "Bearer access".to_owned())]
        );
    }

    #[tokio::test]
    async fn google_oauth_flow_reports_provider_and_stub_errors() {
        let flow = GoogleOAuthFlow::default();
        let client = reqwest::Client::new();
        let token = token("access", None);

        assert_eq!(flow.provider_id(), "gemini");
        assert!(matches!(
            flow.authenticate(&client).await,
            Err(crate::error::RotatorError::Other(message)) if message == "OAuth flow not implemented"
        ));
        assert!(matches!(
            flow.refresh(&client, &token).await,
            Err(crate::error::RotatorError::Other(message)) if message == "OAuth refresh not implemented"
        ));
    }

    #[tokio::test]
    async fn qwen_oauth_flow_reports_provider_and_stub_errors() {
        let flow = QwenOAuthFlow::default();
        let client = reqwest::Client::new();
        let token = token("access", None);

        assert_eq!(flow.provider_id(), "qwen");
        assert!(matches!(
            flow.authenticate(&client).await,
            Err(crate::error::RotatorError::Other(message)) if message == "OAuth flow not implemented"
        ));
        assert!(matches!(
            flow.refresh(&client, &token).await,
            Err(crate::error::RotatorError::Other(message)) if message == "OAuth refresh not implemented"
        ));
    }

    #[tokio::test]
    async fn iflow_oauth_flow_reports_provider_and_stub_errors() {
        let flow = IflowOAuthFlow::default();
        let client = reqwest::Client::new();
        let token = token("access", None);

        assert_eq!(flow.provider_id(), "iflow");
        assert!(matches!(
            flow.authenticate(&client).await,
            Err(crate::error::RotatorError::Other(message)) if message == "OAuth flow not implemented"
        ));
        assert!(matches!(
            flow.refresh(&client, &token).await,
            Err(crate::error::RotatorError::Other(message)) if message == "OAuth refresh not implemented"
        ));
    }
}
