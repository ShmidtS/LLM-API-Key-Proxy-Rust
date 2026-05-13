use rotator::{CredentialManager, HttpClientPool, RotatorClient};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct AppState {
    pub rotator: Arc<RotatorClient>,
}

impl AppState {
    pub fn new() -> Self {
        let creds = CredentialManager::new();
        let pool = HttpClientPool::new(30);
        let client = RotatorClient::new(creds, pool, 3);
        Self {
            rotator: Arc::new(client),
        }
    }
}
