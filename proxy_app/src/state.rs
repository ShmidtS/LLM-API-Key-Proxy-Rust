use rotator::{
    CircuitBreakerRegistry, CooldownManager, CredentialManager, HttpClientPool, ProviderRegistry,
    RateLimiterRegistry, RotatorClient, UsageManager,
};
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::RwLock;

type ModelCache = Arc<RwLock<HashMap<String, (Vec<String>, Instant)>>>;

#[derive(Debug, Clone)]
pub struct AppState {
    pub rotator: Arc<RotatorClient>,
    pub registry: Arc<ProviderRegistry>,
    pub model_cache: ModelCache,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let creds = CredentialManager::from_env();
        let pool = HttpClientPool::new(30);
        let mut registry = ProviderRegistry::new();
        registry.load_from_env();
        let registry = Arc::new(registry);
        let rate_limiter = Arc::new(RateLimiterRegistry::new());
        let cooldown = Arc::new(CooldownManager::new());
        let circuit_breakers = Arc::new(CircuitBreakerRegistry::new());
        let usage_manager = Arc::new(UsageManager::new());
        let client = RotatorClient::new(
            creds,
            pool,
            registry.clone(),
            rate_limiter,
            cooldown,
            circuit_breakers,
            Some(usage_manager),
            3,
        );
        Self::with_parts(client, registry)
    }

    pub fn with_parts(rotator: RotatorClient, registry: Arc<ProviderRegistry>) -> Self {
        Self {
            rotator: Arc::new(rotator),
            registry,
            model_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}
