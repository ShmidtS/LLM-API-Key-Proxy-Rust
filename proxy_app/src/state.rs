use rotator::{CredentialManager, HttpClientPool, ProviderRegistry, RotatorClient};
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
        let registry = Arc::new(ProviderRegistry::new());
        registry.load_from_env();
        let client = RotatorClient::new(creds, pool, registry.clone(), 3);
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
