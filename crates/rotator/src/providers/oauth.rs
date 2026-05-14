use super::{Provider, list_data_models, send_json_request};
use crate::error::{Result, RotatorError};
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

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

#[derive(Debug)]
struct OAuthTokenState {
    token: OAuthToken,
    token_expiry: Option<Instant>,
}

#[derive(Debug)]
pub struct OAuthProvider {
    id: String,
    base_url: String,
    token_endpoint: String,
    client_id: String,
    client_secret: Option<String>,
    pub token_expiry: Option<Instant>,
    token_state: Mutex<OAuthTokenState>,
}

#[derive(Debug, Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    token_type: Option<String>,
}

impl OAuthProvider {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        token_endpoint: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: Option<String>,
        token: OAuthToken,
        token_expiry: Option<Instant>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            token_endpoint: token_endpoint.into(),
            client_id: client_id.into(),
            client_secret,
            token_expiry,
            token_state: Mutex::new(OAuthTokenState {
                token,
                token_expiry,
            }),
        }
    }

    pub async fn refresh_token_if_needed(&self, client: &reqwest::Client) -> Result<OAuthToken> {
        let mut state = self.token_state.lock().await;
        if state
            .token_expiry
            .is_none_or(|expiry| expiry > Instant::now())
        {
            return Ok(state.token.clone());
        }

        let refresh_token = state
            .token
            .refresh_token
            .clone()
            .ok_or_else(|| RotatorError::Other("missing OAuth refresh token".to_owned()))?;
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", self.client_id.as_str()),
        ];
        if let Some(client_secret) = self.client_secret.as_deref() {
            form.push(("client_secret", client_secret));
        }

        let refreshed: RefreshTokenResponse = client
            .post(&self.token_endpoint)
            .form(&form)
            .send()
            .await?
            .error_for_status()
            .map_err(|error| RotatorError::Http(error.to_string()))?
            .json()
            .await?;
        let token_expiry = refreshed
            .expires_in
            .map(|expires_in| Instant::now() + Duration::from_secs(expires_in));
        let token = OAuthToken {
            access_token: refreshed.access_token,
            refresh_token: refreshed
                .refresh_token
                .or_else(|| state.token.refresh_token.clone()),
            expires_at: None,
            token_type: refreshed.token_type.unwrap_or_else(|| "Bearer".to_owned()),
        };

        state.token = token.clone();
        state.token_expiry = token_expiry;
        Ok(token)
    }

    pub async fn auth_headers(&self, client: &reqwest::Client) -> Result<Vec<(String, String)>> {
        let token = self.refresh_token_if_needed(client).await?;
        Ok(auth_headers_oauth(&token))
    }
}

#[async_trait]
impl Provider for OAuthProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![("authorization".to_owned(), format!("Bearer {api_key}"))]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn request(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: serde_json::Value,
        _api_key: &str,
    ) -> Result<reqwest::Response> {
        send_json_request(
            client,
            &self.base_url,
            path,
            body,
            self.auth_headers(client).await?,
        )
        .await
    }

    async fn list_models(
        &self,
        client: &reqwest::Client,
        _api_key: &str,
    ) -> Result<Vec<serde_json::Value>> {
        list_data_models(client, &self.base_url, self.auth_headers(client).await?).await
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
        if !self.is_expired(provider_id)
            && let Some(token) = self.get_token(provider_id)
        {
            return Ok(token);
        }

        let token = refresh_fn(provider_id).await?;
        self.set_token(provider_id, token.clone());
        Ok(token)
    }
}

pub fn auth_headers_oauth(token: &OAuthToken) -> Vec<(String, String)> {
    vec![(
        "authorization".to_owned(),
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

    #[tokio::test]
    async fn oauth_provider_posts_refresh_request_when_token_expired() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            server_calls.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0; 2048];
            let size = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..size]);
            assert!(request.starts_with("POST /token HTTP/1.1"));
            assert!(request.contains("grant_type=refresh_token"));
            assert!(request.contains("refresh_token=old-refresh"));
            let body = r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"token_type":"Bearer"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let provider = OAuthProvider::new(
            "oauth-test",
            format!("http://{addr}/v1"),
            format!("http://{addr}/token"),
            "client-id",
            Some("client-secret".to_owned()),
            OAuthToken {
                access_token: "old-access".to_owned(),
                refresh_token: Some("old-refresh".to_owned()),
                expires_at: None,
                token_type: "Bearer".to_owned(),
            },
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        );
        let client = reqwest::Client::new();

        let headers = provider
            .auth_headers(&client)
            .await
            .expect("refresh succeeds");

        assert_eq!(
            headers,
            vec![("authorization".to_owned(), "Bearer new-access".to_owned())]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn builds_oauth_auth_header() {
        let token = token("access", None);

        assert_eq!(
            auth_headers_oauth(&token),
            vec![("authorization".to_owned(), "Bearer access".to_owned())]
        );
    }

    #[tokio::test]
    async fn google_oauth_flow_reports_provider_and_stub_errors() {
        let flow = GoogleOAuthFlow;
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
        let flow = QwenOAuthFlow;
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
        let flow = IflowOAuthFlow;
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
