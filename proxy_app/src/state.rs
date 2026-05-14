use rotator::{CredentialManager, HttpClientPool, ProviderRegistry, RotatorClient};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct AppState {
    pub rotator: Arc<RotatorClient>,
    pub registry: Arc<ProviderRegistry>,
}

impl AppState {
    pub fn new() -> Self {
        let creds = CredentialManager::from_env();
        let pool = HttpClientPool::new(30);
        let registry = Arc::new(ProviderRegistry::new());
        registry.load_from_env();
        let client = RotatorClient::new(creds, pool, registry.clone(), 3);
        Self {
            rotator: Arc::new(client),
            registry,
        }
    }
}
