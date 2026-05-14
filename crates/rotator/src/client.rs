use crate::credentials::CredentialManager;
use crate::error::{Result, RotatorError};
use crate::http_pool::HttpClientPool;
use crate::provider_registry::{AuthType, ProviderRegistry};
use std::sync::Arc;
use tracing::{error, warn};

#[derive(Debug, Clone)]
pub struct RotatorClient {
    pub credentials: Arc<CredentialManager>,
    http_pool: Arc<HttpClientPool>,
    provider_registry: Arc<ProviderRegistry>,
    max_retries: usize,
}

impl RotatorClient {
    pub fn new(
        credentials: CredentialManager,
        http_pool: HttpClientPool,
        provider_registry: Arc<ProviderRegistry>,
        max_retries: usize,
    ) -> Self {
        Self {
            credentials: Arc::new(credentials),
            http_pool: Arc::new(http_pool),
            provider_registry,
            max_retries,
        }
    }

    pub async fn request(
        &self,
        provider: &str,
        path: &str,
        body: serde_json::Value,
    ) -> Result<reqwest::Response> {
        for attempt in 0..=self.max_retries {
            let cred = self
                .credentials
                .get_least_loaded(provider)
                .ok_or_else(|| RotatorError::NoCredentials(provider.to_string()))?;

            self.credentials.increment(provider, &cred.key);

            let client = self.http_pool.get_or_create(provider);
            let url = format!(
                "{}/{}",
                self.resolve_base_url(provider),
                path.trim_start_matches('/')
            );
            let request = self.apply_auth_headers(provider, client.post(&url), &cred.key);
            let result = request.json(&body).send().await;

            self.credentials.decrement(provider, &cred.key);

            match result {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) if resp.status().as_u16() == 429 => {
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse().ok());
                    warn!(provider, attempt, "rate limited, retrying...");
                    if attempt < self.max_retries {
                        if let Some(secs) = retry_after {
                            tokio::time::sleep(tokio::time::Duration::from_secs(secs)).await;
                        } else {
                            tokio::time::sleep(tokio::time::Duration::from_millis(
                                500 * (attempt as u64 + 1),
                            ))
                            .await;
                        }
                        continue;
                    }
                    return Err(RotatorError::RateLimited(provider.to_string(), retry_after));
                }
                Ok(resp) if resp.status().is_server_error() => {
                    let status = resp.status();
                    error!(provider, attempt, status = %status, "server error, retrying...");
                    if attempt < self.max_retries {
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            300 * (attempt as u64 + 1),
                        ))
                        .await;
                        continue;
                    }
                    return Ok(resp);
                }
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    error!(provider, attempt, error = %e, "request failed");
                    if attempt < self.max_retries {
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            200 * (attempt as u64 + 1),
                        ))
                        .await;
                        continue;
                    }
                    return Err(RotatorError::Http(e.to_string()));
                }
            }
        }
        Err(RotatorError::Exhausted(self.max_retries))
    }

    pub async fn get(&self, provider: &str, path: &str) -> Result<reqwest::Response> {
        let cred = self
            .credentials
            .get_least_loaded(provider)
            .ok_or_else(|| RotatorError::NoCredentials(provider.to_string()))?;

        self.credentials.increment(provider, &cred.key);

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let request = self.apply_auth_headers(provider, client.get(&url), &cred.key);
        let result = request.send().await;

        self.credentials.decrement(provider, &cred.key);

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub async fn delete(&self, provider: &str, path: &str) -> Result<reqwest::Response> {
        let cred = self
            .credentials
            .get_least_loaded(provider)
            .ok_or_else(|| RotatorError::NoCredentials(provider.to_string()))?;

        self.credentials.increment(provider, &cred.key);

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let request = self.apply_auth_headers(provider, client.delete(&url), &cred.key);
        let result = request.send().await;

        self.credentials.decrement(provider, &cred.key);

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    fn apply_auth_headers(
        &self,
        provider: &str,
        mut request: reqwest::RequestBuilder,
        key: &str,
    ) -> reqwest::RequestBuilder {
        if let Some(definition) = self.provider_registry.get(provider) {
            for (header_key, value) in definition.default_headers {
                request = request.header(header_key, value);
            }
            match definition.auth_type {
                AuthType::ApiKey | AuthType::Bearer | AuthType::OAuth => {
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
