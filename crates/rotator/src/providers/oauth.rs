use super::{Provider, list_data_models, send_json_request};
use crate::error::{Result, RotatorError};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub token_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackResult {
    pub code: String,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentialFile {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub token_type: String,
    pub client_id: String,
    #[serde(alias = "token_uri")]
    pub token_endpoint: String,
}

impl OAuthCredentialFile {
    pub fn token(&self) -> OAuthToken {
        OAuthToken {
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at: self.expires_at,
            token_type: self.token_type.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OAuthFlowConfig {
    pub provider_id: &'static str,
    pub client_id: &'static str,
    pub client_secret: Option<&'static str>,
    pub auth_endpoint: &'static str,
    pub token_endpoint: &'static str,
    pub scopes: &'static [&'static str],
    pub callback_path: &'static str,
    pub callback_port: u16,
    pub credential_prefix: &'static str,
}

#[derive(Debug, Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    token_type: Option<String>,
}

#[async_trait]
pub trait OAuthFlow: Send + Sync + Debug {
    fn provider_id(&self) -> &str;
    async fn authenticate(&self, client: &reqwest::Client) -> Result<OAuthToken>;
    async fn refresh(&self, client: &reqwest::Client, token: &OAuthToken) -> Result<OAuthToken>;
}

fn env_static(name: &str) -> Option<&'static str> {
    std::env::var(name)
        .ok()
        .map(|value| Box::leak(value.into_boxed_str()) as &'static str)
}

pub fn generate_pkce() -> (String, String) {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut verifier = String::with_capacity(128);
    while verifier.len() < 128 {
        let id = Uuid::new_v4().simple().to_string();
        for byte in id.bytes() {
            if verifier.len() == 128 {
                break;
            }
            verifier.push(CHARS[byte as usize % CHARS.len()] as char);
        }
    }
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

pub async fn run_callback_server(port: u16) -> Result<CallbackResult> {
    let (_, handle) = start_callback_server(port).await?;
    handle
        .await
        .map_err(|error| RotatorError::Other(format!("OAuth callback task failed: {error}")))?
}

pub async fn start_callback_server(port: u16) -> Result<(u16, JoinHandle<Result<CallbackResult>>)> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| {
            RotatorError::Other(format!("failed to bind OAuth callback server: {error}"))
        })?;
    let actual_port = listener
        .local_addr()
        .map_err(|error| {
            RotatorError::Other(format!("failed to read OAuth callback address: {error}"))
        })?
        .port();
    let handle = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(300), handle_callback(listener))
            .await
            .map_err(|_| RotatorError::Timeout)?
    });
    Ok((actual_port, handle))
}

async fn handle_callback(listener: TcpListener) -> Result<CallbackResult> {
    let (mut socket, _) = listener
        .accept()
        .await
        .map_err(|error| RotatorError::Other(format!("OAuth callback accept failed: {error}")))?;
    let mut buffer = [0; 4096];
    let size = socket
        .read(&mut buffer)
        .await
        .map_err(|error| RotatorError::Other(format!("OAuth callback read failed: {error}")))?;
    let request = String::from_utf8_lossy(&buffer[..size]);
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| RotatorError::Other("OAuth callback request was empty".to_owned()))?;
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| RotatorError::Other("OAuth callback request path missing".to_owned()))?;
    let query = path.split_once('?').map(|(_, query)| query).unwrap_or("");
    let error = query_param(query, "error");
    let code = query_param(query, "code");
    let state = query_param(query, "state");

    let (status, body, result) = if let Some(error) = error {
        (
            "400 Bad Request",
            format!(
                "<html><body><h1>OAuth failed</h1><p>{}</p></body></html>",
                html_escape(&error)
            ),
            Err(RotatorError::Other(format!(
                "OAuth callback error: {error}"
            ))),
        )
    } else if let Some(code) = code {
        (
            "200 OK",
            "<html><body><h1>Authentication complete</h1><p>You can close this window.</p></body></html>".to_owned(),
            Ok(CallbackResult { code, state }),
        )
    } else {
        (
            "400 Bad Request",
            "<html><body><h1>OAuth failed</h1><p>Missing authorization code.</p></body></html>"
                .to_owned(),
            Err(RotatorError::Other(
                "OAuth callback missing code".to_owned(),
            )),
        )
    };

    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(response.as_bytes())
        .await
        .map_err(|error| RotatorError::Other(format!("OAuth callback response failed: {error}")))?;
    result
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3])
                    && let Ok(byte) = u8::from_str_radix(hex, 16)
                {
                    output.push(byte);
                    index += 3;
                    continue;
                }
                output.push(bytes[index]);
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn build_authorization_url(
    config: &OAuthFlowConfig,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    let scope = config.scopes.join(" ");
    format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        config.auth_endpoint,
        percent_encode(config.client_id),
        percent_encode(redirect_uri),
        percent_encode(&scope),
        percent_encode(state),
        percent_encode(code_challenge)
    )
}

pub async fn exchange_authorization_code(
    client: &reqwest::Client,
    config: &OAuthFlowConfig,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthToken> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", config.client_id),
        ("code_verifier", code_verifier),
    ];
    if let Some(client_secret) = config.client_secret {
        form.push(("client_secret", client_secret));
    }

    let response = client
        .post(config.token_endpoint)
        .form(&form)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(RotatorError::Other(format!(
            "OAuth token exchange failed ({status}): {body}"
        )));
    }
    let token_response: TokenExchangeResponse = response.json().await?;
    Ok(OAuthToken {
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        expires_at: token_response
            .expires_in
            .map(|expires_in| chrono::Utc::now().timestamp() as u64 + expires_in),
        token_type: token_response
            .token_type
            .unwrap_or_else(|| "Bearer".to_owned()),
    })
}

pub async fn authenticate_with_config(
    client: &reqwest::Client,
    config: OAuthFlowConfig,
) -> Result<OAuthToken> {
    let (code_verifier, code_challenge) = generate_pkce();
    let state = Uuid::new_v4().simple().to_string();
    let (port, callback) = start_callback_server(config.callback_port).await?;
    let redirect_uri = format!("http://127.0.0.1:{port}{}", config.callback_path);
    let auth_url = build_authorization_url(&config, &redirect_uri, &state, &code_challenge);

    println!(
        "Open this URL to authenticate {}:\n{}",
        config.provider_id, auth_url
    );

    let callback_result = callback
        .await
        .map_err(|error| RotatorError::Other(format!("OAuth callback task failed: {error}")))??;
    if callback_result.state.as_deref() != Some(state.as_str()) {
        return Err(RotatorError::Other("OAuth state mismatch".to_owned()));
    }
    let token = exchange_authorization_code(
        client,
        &config,
        &callback_result.code,
        &redirect_uri,
        &code_verifier,
    )
    .await?;
    save_oauth_credential(config.credential_prefix, &config, &token)?;
    Ok(token)
}

pub fn save_oauth_credential(
    provider: &str,
    config: &OAuthFlowConfig,
    token: &OAuthToken,
) -> Result<PathBuf> {
    let dir = Path::new("oauth_creds");
    std::fs::create_dir_all(dir)
        .map_err(|error| RotatorError::Other(format!("failed to create oauth_creds: {error}")))?;
    let mut index = 1;
    loop {
        let path = dir.join(format!("{provider}_oauth_{index}.json"));
        if !path.exists() {
            let credentials = OAuthCredentialFile {
                access_token: token.access_token.clone(),
                refresh_token: token.refresh_token.clone(),
                expires_at: token.expires_at,
                token_type: token.token_type.clone(),
                client_id: config.client_id.to_owned(),
                token_endpoint: config.token_endpoint.to_owned(),
            };
            let body = serde_json::to_string_pretty(&credentials)?;
            std::fs::write(&path, body).map_err(|error| {
                RotatorError::Other(format!("failed to write OAuth credentials: {error}"))
            })?;
            return Ok(path);
        }
        index += 1;
    }
}

#[derive(Debug, Default)]
pub struct GoogleOAuthFlow;

impl GoogleOAuthFlow {
    pub fn oauth_config() -> OAuthFlowConfig {
        OAuthFlowConfig {
            provider_id: "gemini",
            client_id: env_static("GOOGLE_OAUTH_CLIENT_ID").unwrap_or(
                "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com",
            ),
            client_secret: env_static("GOOGLE_OAUTH_CLIENT_SECRET")
                .or_else(|| env_static("GEMINI_CLIENT_SECRET")),
            auth_endpoint: "https://accounts.google.com/o/oauth2/v2/auth",
            token_endpoint: "https://oauth2.googleapis.com/token",
            scopes: &["https://www.googleapis.com/auth/generative-language"],
            callback_path: "/callback",
            callback_port: 0,
            credential_prefix: "google",
        }
    }
}

#[async_trait]
impl OAuthFlow for GoogleOAuthFlow {
    fn provider_id(&self) -> &str {
        "gemini"
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

#[derive(Debug, Default)]
pub struct QwenOAuthFlow;

impl QwenOAuthFlow {
    pub fn oauth_config() -> OAuthFlowConfig {
        OAuthFlowConfig {
            provider_id: "qwen",
            client_id: "f0304373b74a44d2b584a3fb70ca9e56",
            client_secret: None,
            auth_endpoint: "https://chat.qwen.ai/api/v1/oauth2/authorize",
            token_endpoint: "https://chat.qwen.ai/api/v1/oauth2/token",
            scopes: &["openid", "profile", "email", "model.completion"],
            callback_path: "/callback",
            callback_port: 0,
            credential_prefix: "qwen",
        }
    }
}

#[async_trait]
impl OAuthFlow for QwenOAuthFlow {
    fn provider_id(&self) -> &str {
        "qwen"
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

#[derive(Debug, Default)]
pub struct IflowOAuthFlow;

impl IflowOAuthFlow {
    pub fn oauth_config() -> OAuthFlowConfig {
        OAuthFlowConfig {
            provider_id: "iflow",
            client_id: "10009311001",
            client_secret: env_static("IFLOW_CLIENT_SECRET"),
            auth_endpoint: "https://iflow.cn/oauth",
            token_endpoint: "https://iflow.cn/oauth/token",
            scopes: &["read", "write"],
            callback_path: "/oauth2callback",
            callback_port: std::env::var("IFLOW_OAUTH_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(11451),
            credential_prefix: "iflow",
        }
    }
}

#[async_trait]
impl OAuthFlow for IflowOAuthFlow {
    fn provider_id(&self) -> &str {
        "iflow"
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
pub struct RefreshTokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub token_type: Option<String>,
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

        let response = client.post(&self.token_endpoint).form(&form).send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RotatorError::Other(format!(
                "OAuth refresh failed ({status}): {body}"
            )));
        }
        let refreshed: RefreshTokenResponse = response.json().await?;
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

    pub fn get_token(&self, cache_key: &str) -> Option<OAuthToken> {
        self.tokens.get(cache_key).map(|token| token.clone())
    }

    pub fn set_token(&self, cache_key: &str, token: OAuthToken) {
        self.tokens.insert(cache_key.to_owned(), token);
    }

    pub fn is_expired(&self, cache_key: &str) -> bool {
        self.tokens
            .get(cache_key)
            .and_then(|token| token.expires_at)
            .is_some_and(|expires_at| expires_at <= chrono::Utc::now().timestamp() as u64)
    }

    pub async fn refresh_if_needed<F, Fut>(
        &self,
        cache_key: &str,
        refresh_fn: F,
    ) -> Result<OAuthToken>
    where
        F: Fn(&str) -> Fut,
        Fut: Future<Output = Result<OAuthToken>>,
    {
        if !self.is_expired(cache_key)
            && let Some(token) = self.get_token(cache_key)
        {
            return Ok(token);
        }

        let token = refresh_fn(cache_key).await?;
        self.set_token(cache_key, token.clone());
        Ok(token)
    }
}

pub async fn refresh_oauth_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    token: &OAuthToken,
) -> Result<OAuthToken> {
    let refresh_token = token
        .refresh_token
        .as_ref()
        .ok_or_else(|| RotatorError::Other("missing OAuth refresh token".to_owned()))?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", client_id),
    ];
    if let Some(client_secret) = client_secret {
        form.push(("client_secret", client_secret));
    }

    let response = client.post(token_endpoint).form(&form).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(RotatorError::Other(format!(
            "OAuth refresh failed ({status}): {body}"
        )));
    }
    let refreshed: RefreshTokenResponse = response.json().await?;
    let expires_at = refreshed
        .expires_in
        .map(|expires_in| chrono::Utc::now().timestamp() as u64 + expires_in);
    Ok(OAuthToken {
        access_token: refreshed.access_token,
        refresh_token: refreshed
            .refresh_token
            .or_else(|| token.refresh_token.clone()),
        expires_at,
        token_type: refreshed.token_type.unwrap_or_else(|| "Bearer".to_owned()),
    })
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
                move |cache_key| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let access_token = format!("new-{cache_key}");
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

    #[test]
    fn generate_pkce_returns_128_char_verifier_and_base64url_challenge() {
        let (verifier, challenge) = generate_pkce();

        assert_eq!(verifier.len(), 128);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
        );
        assert!(!challenge.contains('='));
        assert!(
            challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        );
    }

    #[tokio::test]
    async fn callback_server_receives_code_and_state() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let (port, server) = start_callback_server(0).await.expect("server starts");
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(b"GET /callback?code=test-code&state=test-state HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut buffer = [0; 1024];
        let size = stream.read(&mut buffer).await.unwrap();
        let response = String::from_utf8_lossy(&buffer[..size]);
        assert!(response.starts_with("HTTP/1.1 200 OK"));

        let result = server.await.unwrap().expect("callback succeeds");
        assert_eq!(result.code, "test-code");
        assert_eq!(result.state.as_deref(), Some("test-state"));
    }

    #[test]
    fn builds_google_authorization_url_with_pkce() {
        let config = GoogleOAuthFlow::oauth_config();
        let url = build_authorization_url(
            &config,
            "http://127.0.0.1:1234/callback",
            "state",
            "challenge",
        );

        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id="));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1234%2Fcallback"));
        assert!(url.contains("response_type=code"));
        assert!(
            url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgenerative-language")
        );
        assert!(url.contains("state=state"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[tokio::test]
    async fn exchanges_authorization_code_for_token() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let size = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..size]);
            assert!(request.starts_with("POST /token HTTP/1.1"));
            assert!(request.contains("grant_type=authorization_code"));
            assert!(request.contains("code=auth-code"));
            assert!(request.contains("client_id=client-id"));
            assert!(request.contains("code_verifier=verifier"));
            let body = r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600,"token_type":"Bearer"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let config = OAuthFlowConfig {
            provider_id: "test",
            client_id: "client-id",
            client_secret: None,
            auth_endpoint: "http://127.0.0.1/auth",
            token_endpoint: Box::leak(format!("http://{addr}/token").into_boxed_str()),
            scopes: &["scope"],
            callback_path: "/callback",
            callback_port: 0,
            credential_prefix: "test",
        };
        let token = exchange_authorization_code(
            &reqwest::Client::new(),
            &config,
            "auth-code",
            "http://127.0.0.1/callback",
            "verifier",
        )
        .await
        .expect("exchange succeeds");

        assert_eq!(token.access_token, "access");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(token.token_type, "Bearer");
        assert!(token.expires_at.unwrap() > now());
    }

    #[tokio::test]
    async fn google_qwen_iflow_flows_report_provider_ids_and_refresh_missing_token_errors() {
        let client = reqwest::Client::new();
        let token = OAuthToken {
            access_token: "access".to_owned(),
            refresh_token: None,
            expires_at: None,
            token_type: "Bearer".to_owned(),
        };

        let google = GoogleOAuthFlow;
        assert_eq!(google.provider_id(), "gemini");
        assert!(matches!(
            google.refresh(&client, &token).await,
            Err(crate::error::RotatorError::Other(message)) if message == "missing OAuth refresh token"
        ));

        let qwen = QwenOAuthFlow;
        assert_eq!(qwen.provider_id(), "qwen");
        assert!(matches!(
            qwen.refresh(&client, &token).await,
            Err(crate::error::RotatorError::Other(message)) if message == "missing OAuth refresh token"
        ));

        let iflow = IflowOAuthFlow;
        assert_eq!(iflow.provider_id(), "iflow");
        assert!(matches!(
            iflow.refresh(&client, &token).await,
            Err(crate::error::RotatorError::Other(message)) if message == "missing OAuth refresh token"
        ));
    }
}
