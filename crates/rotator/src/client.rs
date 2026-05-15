use crate::circuit_breaker::{CircuitBreakerRegistry, CircuitState};
use crate::cooldown::CooldownManager;
use crate::credentials::{CredentialManager, CredentialPermit};
use crate::error::{Result, RotatorError};
use crate::http_pool::HttpClientPool;
use crate::provider_registry::{AuthType, ProviderRegistry};
use crate::provider_utils::extract_usage;
use crate::providers::oauth::{OAuthManager, OAuthToken, refresh_oauth_token};
use crate::rate_limiter::RateLimiterRegistry;
use crate::throttle::{ThrottleReason, classify_throttle_with_headers};
use crate::usage::UsageManager;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, warn};

fn oauth_cache_key(provider: &str, key: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{}:{}", provider, hasher.finish())
}

#[derive(Debug, Clone)]
pub struct RotatorClient {
    pub credentials: Arc<CredentialManager>,
    http_pool: Arc<HttpClientPool>,
    provider_registry: Arc<ProviderRegistry>,
    rate_limiter: Arc<RateLimiterRegistry>,
    cooldown: Arc<CooldownManager>,
    circuit_breakers: Arc<CircuitBreakerRegistry>,
    usage_manager: Option<Arc<UsageManager>>,
    last_latency_ms: Arc<DashMap<String, u64>>,
    max_retries: usize,
    oauth_manager: Arc<OAuthManager>,
    oauth_refresh_locks: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl RotatorClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        credentials: CredentialManager,
        http_pool: HttpClientPool,
        provider_registry: Arc<ProviderRegistry>,
        rate_limiter: Arc<RateLimiterRegistry>,
        cooldown: Arc<CooldownManager>,
        circuit_breakers: Arc<CircuitBreakerRegistry>,
        usage_manager: Option<Arc<UsageManager>>,
        max_retries: usize,
    ) -> Self {
        Self {
            credentials: Arc::new(credentials),
            http_pool: Arc::new(http_pool),
            provider_registry,
            rate_limiter,
            cooldown,
            circuit_breakers,
            usage_manager,
            last_latency_ms: Arc::new(DashMap::new()),
            max_retries,
            oauth_manager: Arc::new(OAuthManager::new()),
            oauth_refresh_locks: Arc::new(DashMap::new()),
        }
    }

    pub async fn request(
        &self,
        provider: &str,
        path: &str,
        mut body: serde_json::Value,
    ) -> Result<reqwest::Response> {
        Self::transform_request(provider, &mut body);

        for attempt in 0..=self.max_retries {
            if !self.circuit_breakers.is_allowed(provider) {
                return Err(RotatorError::CircuitOpen(provider.to_string()));
            }

            let cred = self
                .credentials
                .acquire_least_loaded_where(provider, |key| {
                    self.cooldown.is_available(provider, key)
                })
                .ok_or_else(|| RotatorError::NoCredentials(provider.to_string()))?;
            let permit =
                CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

            if !self.rate_limiter.acquire(provider, permit.key()) {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            let client = self.http_pool.get_or_create(provider);
            let url = format!(
                "{}/{}",
                self.resolve_base_url(provider),
                path.trim_start_matches('/')
            );
            let token = self.resolve_auth_token(provider, permit.key()).await?;
            let request = self.apply_auth_headers(provider, client.post(&url), &token);
            let started_at = Instant::now();
            let result = request.json(&body).send().await;
            self.last_latency_ms
                .insert(provider.to_owned(), started_at.elapsed().as_millis() as u64);

            match result {
                Ok(resp) if resp.status().is_success() => {
                    self.circuit_breakers.record_success(provider);
                    let stream = body
                        .get("stream")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    return self
                        .record_usage_from_response(provider, permit.key(), resp, stream)
                        .await;
                }
                Ok(resp) if resp.status().as_u16() == 429 => {
                    let headers = resp.headers().clone();
                    let body = resp
                        .json::<serde_json::Value>()
                        .await
                        .unwrap_or_else(|_| serde_json::json!({}));
                    let (reason, retry_after) =
                        classify_throttle_with_headers(429, Some(&headers), &body);
                    warn!(provider, attempt, ?reason, "throttled, retrying...");
                    if attempt < self.max_retries {
                        let delay = retry_after
                            .unwrap_or_else(|| Duration::from_millis(500 * (attempt as u64 + 1)));
                        self.cooldown.add_cooldown(provider, permit.key(), delay);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(RotatorError::RateLimited(
                        provider.to_string(),
                        retry_after.map(|duration| duration.as_secs()),
                    ));
                }
                Ok(resp) if resp.status().is_server_error() => {
                    let status = resp.status();
                    self.circuit_breakers.record_failure(provider);
                    error!(provider, attempt, status = %status, "server error, retrying...");
                    if attempt < self.max_retries {
                        let headers = resp.headers().clone();
                        let body = resp
                            .json::<serde_json::Value>()
                            .await
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let (reason, retry_after) =
                            classify_throttle_with_headers(status.as_u16(), Some(&headers), &body);
                        let delay = retry_after.unwrap_or_else(|| match reason {
                            ThrottleReason::ServerOverload => Duration::from_secs(5),
                            _ => Duration::from_millis(300 * (attempt as u64 + 1)),
                        });
                        self.cooldown.add_cooldown(provider, permit.key(), delay);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    self.cooldown
                        .add_cooldown(provider, permit.key(), Duration::from_secs(5));
                    return Ok(resp);
                }
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    if e.is_timeout() {
                        self.circuit_breakers.record_failure(provider);
                        self.cooldown
                            .add_cooldown(provider, permit.key(), Duration::from_secs(5));
                    }
                    error!(provider, attempt, error = %e, "request failed");
                    if attempt < self.max_retries {
                        tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                        continue;
                    }
                    return Err(RotatorError::Http(e.to_string()));
                }
            }
        }
        Err(RotatorError::Exhausted(self.max_retries))
    }

    pub fn circuit_state(&self, provider: &str) -> CircuitState {
        self.circuit_breakers.get_state(provider)
    }

    pub fn usage_entries(&self) -> Vec<crate::UsageEntry> {
        self.usage_manager
            .as_ref()
            .map(|usage_manager| usage_manager.get_all_usage())
            .unwrap_or_default()
    }

    pub fn last_latency_ms(&self, provider: &str) -> Option<u64> {
        self.last_latency_ms.get(provider).map(|entry| *entry)
    }

    pub async fn list_models(&self, provider: &str) -> Result<reqwest::Response> {
        self.get(provider, "models").await
    }

    pub async fn get(&self, provider: &str, path: &str) -> Result<reqwest::Response> {
        let cred = self
            .credentials
            .acquire_least_loaded(provider)
            .ok_or_else(|| RotatorError::NoCredentials(provider.to_string()))?;
        let permit = CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let token = self.resolve_auth_token(provider, permit.key()).await?;
        let request = self.apply_auth_headers(provider, client.get(&url), &token);
        let result = request.send().await;

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub async fn delete(&self, provider: &str, path: &str) -> Result<reqwest::Response> {
        let cred = self
            .credentials
            .acquire_least_loaded(provider)
            .ok_or_else(|| RotatorError::NoCredentials(provider.to_string()))?;
        let permit = CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let token = self.resolve_auth_token(provider, permit.key()).await?;
        let request = self.apply_auth_headers(provider, client.delete(&url), &token);
        let result = request.send().await;

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub async fn get_with_query(
        &self,
        provider: &str,
        path: &str,
        query: &[(String, String)],
    ) -> Result<reqwest::Response> {
        let cred = self
            .credentials
            .acquire_least_loaded(provider)
            .ok_or_else(|| RotatorError::NoCredentials(provider.to_string()))?;
        let permit = CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let token = self.resolve_auth_token(provider, permit.key()).await?;
        let request = self.apply_auth_headers(provider, client.get(&url), &token);
        let result = request.query(query).send().await;

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub async fn delete_with_query(
        &self,
        provider: &str,
        path: &str,
        query: &[(String, String)],
    ) -> Result<reqwest::Response> {
        let cred = self
            .credentials
            .acquire_least_loaded(provider)
            .ok_or_else(|| RotatorError::NoCredentials(provider.to_string()))?;
        let permit = CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let token = self.resolve_auth_token(provider, permit.key()).await?;
        let request = self.apply_auth_headers(provider, client.delete(&url), &token);
        let result = request.query(query).send().await;

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub fn transform_request(provider: &str, body: &mut serde_json::Value) {
        match provider {
            "anthropic" => {
                if let Some(object) = body.as_object_mut() {
                    object.remove("stream_options");
                }
            }
            "gemini" => {
                if let Some(model) = body.get("model").and_then(|value| value.as_str())
                    && !model.starts_with("models/")
                {
                    body["model"] = serde_json::Value::String(format!("models/{model}"));
                }
            }
            _ => {}
        }
    }

    async fn record_usage_from_response(
        &self,
        provider: &str,
        key: &str,
        resp: reqwest::Response,
        stream: bool,
    ) -> Result<reqwest::Response> {
        let Some(usage_manager) = &self.usage_manager else {
            return Ok(resp);
        };
        if stream {
            return Ok(resp);
        }

        let status = resp.status();
        let version = resp.version();
        let headers = resp.headers().clone();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| RotatorError::Http(e.to_string()))?;

        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let (prompt, completion) = extract_usage(&json);
            if let (Ok(prompt), Ok(completion)) = (u32::try_from(prompt), u32::try_from(completion))
                && (prompt > 0 || completion > 0)
            {
                usage_manager.record_usage(provider, key, prompt, completion);
            }
        }

        let mut builder = http::Response::builder().status(status).version(version);
        *builder.headers_mut().expect("response builder is valid") = headers;
        Ok(builder
            .body(bytes)
            .expect("response body rebuild should not fail")
            .into())
    }

    async fn resolve_auth_token(&self, provider: &str, key: &str) -> Result<String> {
        let definition = match self.provider_registry.get(provider) {
            Some(def) => def,
            None => return Ok(key.to_owned()),
        };

        if definition.auth_type != AuthType::OAuth {
            return Ok(key.to_owned());
        }

        let oauth_token: OAuthToken = match serde_json::from_str(key) {
            Ok(token) => token,
            Err(_) => {
                return Err(RotatorError::Other(format!(
                    "invalid OAuth credential JSON for provider {provider}"
                )));
            }
        };

        let cache_key = oauth_cache_key(provider, key);

        let is_expired_with_skew = |token: &OAuthToken| {
            token.expires_at.is_some_and(|expires_at| {
                let now = chrono::Utc::now().timestamp() as u64;
                let skew = 60; // refresh 60 s before hard expiry
                expires_at.saturating_sub(skew) <= now
            })
        };

        if let Some(cached) = self.oauth_manager.get_token(&cache_key)
            && !is_expired_with_skew(&cached)
        {
            return Ok(cached.access_token);
        }

        let raw_expired = is_expired_with_skew(&oauth_token);
        let cached = self.oauth_manager.get_token(&cache_key);

        if raw_expired || cached.is_some() {
            let token_endpoint = definition
                .token_endpoint
                .as_ref()
                .ok_or_else(|| {
                    RotatorError::Other(format!(
                        "Missing token_endpoint for provider {provider}"
                    ))
                })?;
            let client_id = definition
                .client_id
                .as_ref()
                .ok_or_else(|| {
                    RotatorError::Other(format!(
                        "Missing client_id for provider {provider}"
                    ))
                })?;
            let client_secret = definition.client_secret.as_deref();

            let lock = {
                let entry = self.oauth_refresh_locks.entry(cache_key.clone());
                let guard = entry
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())));
                guard.clone()
            };
            let _guard = lock.lock().await;

            if let Some(cached) = self.oauth_manager.get_token(&cache_key)
                && !is_expired_with_skew(&cached)
            {
                return Ok(cached.access_token);
            }

            let client = self.http_pool.get_or_create(provider);
            let refreshed = refresh_oauth_token(
                &client,
                token_endpoint,
                client_id,
                client_secret,
                &oauth_token,
            )
            .await?;
            let access_token = refreshed.access_token.clone();
            self.oauth_manager.set_token(&cache_key, refreshed);
            return Ok(access_token);
        }

        self.oauth_manager.set_token(&cache_key, oauth_token.clone());
        Ok(oauth_token.access_token)
    }

    fn apply_auth_headers(
        &self,
        provider: &str,
        mut request: reqwest::RequestBuilder,
        token: &str,
    ) -> reqwest::RequestBuilder {
        if provider == "gemini" {
            request = request.query(&[("key", token)]);
        }
        if let Some(definition) = self.provider_registry.get(provider) {
            for (header_key, value) in definition.default_headers {
                request = request.header(header_key, value);
            }
            match definition.auth_type {
                AuthType::ApiKey => {
                    if provider != "gemini" {
                        request = request.header("x-api-key", token);
                    }
                }
                AuthType::Bearer | AuthType::OAuth => {
                    request = request.header("Authorization", format!("Bearer {token}"));
                }
            }
        } else {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request
    }

    fn resolve_base_url(&self, provider: &str) -> String {
        self.provider_registry
            .resolve_base_url(provider)
            .unwrap_or_else(|| format!("https://api.{provider}.com/v1"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit_breaker::{CircuitBreakerRegistry, CircuitState};
    use crate::cooldown::CooldownManager;
    use crate::provider_registry::ProviderDefinition;
    use crate::rate_limiter::RateLimiterRegistry;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn test_provider(statuses: Vec<u16>) -> (Arc<ProviderRegistry>, Arc<AtomicUsize>) {
        test_provider_with_body(statuses, "{}".to_string()).await
    }

    async fn test_provider_with_body(
        statuses: Vec<u16>,
        body: String,
    ) -> (Arc<ProviderRegistry>, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = calls.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let request_number = server_calls.fetch_add(1, Ordering::SeqCst);
                let status = statuses
                    .get(request_number)
                    .copied()
                    .unwrap_or_else(|| *statuses.last().unwrap());
                let mut buffer = [0; 1024];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 {status} OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let registry = Arc::new(ProviderRegistry::default());
        registry.register(ProviderDefinition {
            id: "test".to_string(),
            display_name: "test".to_string(),
            base_url: format!("http://{addr}/v1"),
            auth_type: AuthType::ApiKey,
            model_patterns: Vec::new(),
            endpoints: vec!["/chat/completions".to_string()],
            features: vec!["chat".to_string()],
            model_count: 1,
            timeout_secs: 60,
            default_headers: HashMap::new(),
            token_endpoint: None,
            client_id: None,
            client_secret: None,
        });

        (registry, calls)
    }

    #[tokio::test]
    async fn request_records_server_failure_cooldown_and_opens_circuit() {
        let (registry, _) = test_provider(vec![500]).await;
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let rate_limiter = Arc::new(RateLimiterRegistry::new());
        let cooldown = Arc::new(CooldownManager::new());
        let circuit_breakers = Arc::new(CircuitBreakerRegistry::new());
        circuit_breakers.configure_provider("test", 1, 60, 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            rate_limiter,
            cooldown.clone(),
            circuit_breakers.clone(),
            None,
            0,
        );

        let first = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await;
        assert!(first.unwrap().status().is_server_error());
        assert_eq!(circuit_breakers.get_state("test"), CircuitState::Open);
        assert!(!cooldown.is_available("test", "key-1"));

        let second = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await;
        assert!(matches!(second, Err(RotatorError::CircuitOpen(provider)) if provider == "test"));
    }

    #[tokio::test]
    async fn request_records_openai_usage_after_success() {
        let usage_manager = Arc::new(crate::UsageManager::with_config(
            std::env::temp_dir().join("rotator-openai-usage-test.json"),
            Duration::from_secs(60),
            100,
        ));
        let (registry, _) = test_provider_with_body(
            vec![200],
            serde_json::json!({
                "id": "chatcmpl-test",
                "usage": {"prompt_tokens": 11, "completion_tokens": 7}
            })
            .to_string(),
        )
        .await;
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            Some(usage_manager.clone()),
            0,
        );

        let response = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .unwrap();
        let body: serde_json::Value = response.json().await.unwrap();

        assert_eq!(body["id"], "chatcmpl-test");
        assert_eq!(
            usage_manager
                .get_usage("test", "key-1")
                .unwrap()
                .prompt_tokens,
            11
        );
        assert_eq!(
            usage_manager
                .get_usage("test", "key-1")
                .unwrap()
                .completion_tokens,
            7
        );
        usage_manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn request_records_anthropic_usage_after_success() {
        let usage_manager = Arc::new(crate::UsageManager::with_config(
            std::env::temp_dir().join("rotator-anthropic-usage-test.json"),
            Duration::from_secs(60),
            100,
        ));
        let (registry, _) = test_provider_with_body(
            vec![200],
            serde_json::json!({
                "id": "msg_test",
                "usage": {"input_tokens": 13, "output_tokens": 5}
            })
            .to_string(),
        )
        .await;
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            Some(usage_manager.clone()),
            0,
        );

        let response = client
            .request("test", "messages", serde_json::json!({}))
            .await
            .unwrap();
        let body: serde_json::Value = response.json().await.unwrap();

        assert_eq!(body["id"], "msg_test");
        let usage = usage_manager.get_usage("test", "key-1").unwrap();
        assert_eq!(usage.prompt_tokens, 13);
        assert_eq!(usage.completion_tokens, 5);
        usage_manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn request_waits_and_retries_when_rate_limiter_denies_key() {
        let (registry, calls) = test_provider(vec![200]).await;
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let rate_limiter = Arc::new(RateLimiterRegistry::new());
        rate_limiter.configure("test", "key-1", 0, 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            rate_limiter,
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .unwrap();
        let second = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await;

        assert!(matches!(second, Err(RotatorError::Exhausted(0))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn anthropic_transform_removes_unsupported_stream_options() {
        let mut body = serde_json::json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [{"role": "user", "content": "hello"}],
            "stream_options": {"include_usage": true}
        });

        RotatorClient::transform_request("anthropic", &mut body);

        assert!(body.get("stream_options").is_none());
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[test]
    fn gemini_transform_prefixes_model_name_once() {
        let mut body = serde_json::json!({"model": "gemini-2.5-flash"});
        RotatorClient::transform_request("gemini", &mut body);
        assert_eq!(body["model"], "models/gemini-2.5-flash");

        RotatorClient::transform_request("gemini", &mut body);
        assert_eq!(body["model"], "models/gemini-2.5-flash");
    }

    #[tokio::test]
    async fn oauth_token_is_parsed_and_used_as_bearer() {
        let (registry, calls) = test_provider(vec![200]).await;
        let mut registry_def = registry.get("test").unwrap();
        registry_def.auth_type = AuthType::OAuth;
        registry.register(registry_def);

        let credentials = CredentialManager::new();
        let oauth_json = serde_json::to_string(&crate::providers::oauth::OAuthToken {
            access_token: "oauth-access-123".to_owned(),
            refresh_token: None,
            expires_at: Some(u64::MAX),
            token_type: "Bearer".to_owned(),
        })
        .unwrap();
        credentials.register_keys("test".to_string(), vec![oauth_json], 1);

        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let response = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oauth_refresh_triggered_when_token_expired() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let refresh_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refresh_addr = refresh_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut socket, _) = refresh_listener.accept().await.unwrap();
            let mut buffer = [0; 2048];
            let size = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..size]);
            assert!(request.starts_with("POST /token HTTP/1.1"));
            assert!(request.contains("grant_type=refresh_token"));
            let body = r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"token_type":"Bearer"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let (registry, calls) = test_provider(vec![200]).await;
        let mut registry_def = registry.get("test").unwrap();
        registry_def.auth_type = AuthType::OAuth;
        registry_def.token_endpoint = Some(format!("http://{refresh_addr}/token"));
        registry_def.client_id = Some("client-id".to_owned());
        registry_def.client_secret = Some("client-secret".to_owned());
        registry.register(registry_def);

        let credentials = CredentialManager::new();
        let oauth_json = serde_json::to_string(&crate::providers::oauth::OAuthToken {
            access_token: "old-access".to_owned(),
            refresh_token: Some("old-refresh".to_owned()),
            expires_at: Some(1),
            token_type: "Bearer".to_owned(),
        })
        .unwrap();
        credentials.register_keys("test".to_string(), vec![oauth_json], 1);

        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let response = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oauth_missing_refresh_token_returns_error() {
        let (registry, _) = test_provider(vec![200]).await;
        let mut registry_def = registry.get("test").unwrap();
        registry_def.auth_type = AuthType::OAuth;
        registry_def.token_endpoint = Some("http://localhost:1/token".to_owned());
        registry_def.client_id = Some("client-id".to_owned());
        registry.register(registry_def);

        let credentials = CredentialManager::new();
        let oauth_json = serde_json::to_string(&crate::providers::oauth::OAuthToken {
            access_token: "old-access".to_owned(),
            refresh_token: None,
            expires_at: Some(1),
            token_type: "Bearer".to_owned(),
        })
        .unwrap();
        credentials.register_keys("test".to_string(), vec![oauth_json], 1);

        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let result = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await;
        assert!(
            matches!(result, Err(RotatorError::Other(ref msg)) if msg.contains("missing OAuth refresh token")),
            "expected missing refresh token error, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn concurrent_refresh_is_single_flight() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let refresh_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refresh_addr = refresh_listener.local_addr().unwrap();
        let refresh_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&refresh_count);

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = refresh_listener.accept().await else {
                    break;
                };
                count_clone.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0; 2048];
                let size = socket.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..size]);
                assert!(request.starts_with("POST /token HTTP/1.1"));
                tokio::time::sleep(Duration::from_millis(100)).await;
                let body = r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"token_type":"Bearer"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let (registry, calls) = test_provider(vec![200, 200]).await;
        let mut registry_def = registry.get("test").unwrap();
        registry_def.auth_type = AuthType::OAuth;
        registry_def.token_endpoint = Some(format!("http://{refresh_addr}/token"));
        registry_def.client_id = Some("client-id".to_owned());
        registry_def.client_secret = Some("client-secret".to_owned());
        registry.register(registry_def);

        let credentials = CredentialManager::new();
        let oauth_json = serde_json::to_string(&crate::providers::oauth::OAuthToken {
            access_token: "old-access".to_owned(),
            refresh_token: Some("old-refresh".to_owned()),
            expires_at: Some(1),
            token_type: "Bearer".to_owned(),
        })
        .unwrap();
        credentials.register_keys("test".to_string(), vec![oauth_json], 2);

        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let (resp1, resp2) = tokio::join!(
            client.request("test", "chat/completions", serde_json::json!({})),
            client.request("test", "chat/completions", serde_json::json!({}))
        );
        assert!(resp1.is_ok());
        assert!(resp2.is_ok());
        assert_eq!(refresh_count.load(Ordering::SeqCst), 1, "refresh should happen exactly once");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
