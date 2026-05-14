use crate::circuit_breaker::{CircuitBreakerRegistry, CircuitState};
use crate::cooldown::CooldownManager;
use crate::credentials::{CredentialManager, CredentialPermit};
use crate::error::{Result, RotatorError};
use crate::http_pool::HttpClientPool;
use crate::provider_registry::{AuthType, ProviderRegistry};
use crate::provider_utils::extract_usage;
use crate::rate_limiter::RateLimiterRegistry;
use crate::throttle::{ThrottleReason, classify_throttle_with_headers};
use crate::usage::UsageManager;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, warn};

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
            let request = self.apply_auth_headers(provider, client.post(&url), permit.key());
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
        let request = self.apply_auth_headers(provider, client.get(&url), permit.key());
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
        let request = self.apply_auth_headers(provider, client.delete(&url), permit.key());
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
        let request = self.apply_auth_headers(provider, client.get(&url), permit.key());
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
        let request = self.apply_auth_headers(provider, client.delete(&url), permit.key());
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

    fn apply_auth_headers(
        &self,
        provider: &str,
        mut request: reqwest::RequestBuilder,
        key: &str,
    ) -> reqwest::RequestBuilder {
        if provider == "gemini" {
            request = request.query(&[("key", key)]);
        }
        if let Some(definition) = self.provider_registry.get(provider) {
            for (header_key, value) in definition.default_headers {
                request = request.header(header_key, value);
            }
            match definition.auth_type {
                AuthType::ApiKey => {
                    if provider != "gemini" {
                        request = request.header("x-api-key", key);
                    }
                }
                AuthType::Bearer | AuthType::OAuth => {
                    request = request.header("Authorization", format!("Bearer {key}"));
                }
            }
        } else {
            request = request.header("Authorization", format!("Bearer {key}"));
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
}
