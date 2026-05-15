use proxy_config::ProxyConfig;
use rotator::{
    CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool, ProviderRegistry,
    RateLimiterRegistry, RotatorClient, UsageManager,
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
    pub registry: Arc<ProviderRegistry>,
    pub model_cache: ModelCache,
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
        let pool = HttpClientPool::new(cfg.request_timeout_secs);
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
        Self {
            rotator: Arc::new(client),
            registry,
            model_cache: Arc::new(RwLock::new(HashMap::new())),
            config: cfg,
        }
    }

    pub fn with_parts(rotator: RotatorClient, registry: Arc<ProviderRegistry>) -> Self {
        Self {
            rotator: Arc::new(rotator),
            registry,
            model_cache: Arc::new(RwLock::new(HashMap::new())),
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
