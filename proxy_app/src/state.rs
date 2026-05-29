use proxy_config::ProxyConfig;
use rotator::{
    CircuitBreakerRegistry, CooldownManager, CredentialManager, EmbeddingBatcher, HttpClientPool,
    ModelInfoService, ProviderRegistry, RateLimiterRegistry, RotatorClient, UsageManager,
};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

type ModelCache = Arc<RwLock<HashMap<String, (Vec<String>, Instant)>>>;

#[derive(Debug, Clone)]
pub struct AppState {
    pub rotator: Arc<RotatorClient>,
    pub batcher: EmbeddingBatcher,
    pub registry: Arc<ProviderRegistry>,
    pub model_cache: ModelCache,
    pub model_info: Arc<RwLock<ModelInfoService>>,
    pub catalog_client: reqwest::Client,
    pub config: ProxyConfig,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self::from_config(ProxyConfig::default())
    }

    pub fn from_config(cfg: ProxyConfig) -> Self {
        let creds = CredentialManager::from_env();
        let pool = HttpClientPool::with_timeouts(
            cfg.timeout_read_non_streaming_secs,
            cfg.timeout_read_streaming_secs,
        );
        let mut registry = ProviderRegistry::new();
        registry.load_from_env();
        let registry = Arc::new(registry);
        let rate_limiter = Arc::new(RateLimiterRegistry::new());
        let cooldown = Arc::new(CooldownManager::new());
        let circuit_breakers = Arc::new(CircuitBreakerRegistry::new());
        let usage_manager = Arc::new(UsageManager::with_config(
            &cfg.usage_path,
            Duration::from_secs(cfg.usage_flush_interval_secs),
            cfg.usage_batch_size,
        ));
        let client = RotatorClient::new(
            creds,
            pool,
            registry.clone(),
            rate_limiter,
            cooldown,
            circuit_breakers,
            Some(usage_manager),
            cfg.max_retries,
        );
        let catalog_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.request_timeout_secs))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let rotator = Arc::new(client);
        let batcher = EmbeddingBatcher::new(rotator.clone(), registry.clone());
        Self {
            rotator,
            batcher,
            registry,
            model_cache: Arc::new(RwLock::new(HashMap::new())),
            model_info: Arc::new(RwLock::new(ModelInfoService::new())),
            catalog_client,
            config: cfg,
        }
    }

    pub fn with_parts(rotator: RotatorClient, registry: Arc<ProviderRegistry>) -> Self {
        let rotator = Arc::new(rotator);
        let batcher = EmbeddingBatcher::new(rotator.clone(), registry.clone());
        Self {
            rotator,
            batcher,
            registry,
            model_cache: Arc::new(RwLock::new(HashMap::new())),
            model_info: Arc::new(RwLock::new(ModelInfoService::new())),
            catalog_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            config: ProxyConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AppState;
    use proxy_config::ProxyConfig;

    #[tokio::test]
    async fn from_config_stores_proxy_config() {
        let config = ProxyConfig {
            host: "0.0.0.0".to_owned(),
            port: 9000,
            request_timeout_secs: 45,
            max_retries: 7,
            usage_path: "target/test-usage-a002.json".to_owned(),
            usage_flush_interval_secs: 5,
            usage_batch_size: 2,
            ..Default::default()
        };

        let state = AppState::from_config(config);

        assert_eq!(state.config.host, "0.0.0.0");
        assert_eq!(state.config.port, 9000);
        assert_eq!(state.config.request_timeout_secs, 45);
        assert_eq!(state.config.max_retries, 7);
        assert_eq!(state.config.usage_path, "target/test-usage-a002.json");
        assert_eq!(state.config.usage_flush_interval_secs, 5);
        assert_eq!(state.config.usage_batch_size, 2);
    }
}
