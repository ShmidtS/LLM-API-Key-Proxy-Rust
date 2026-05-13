use reqwest::{Client, ClientBuilder};
use std::time::Duration;
use std::sync::Arc;
use dashmap::DashMap;

#[derive(Debug, Clone)]
pub struct HttpClientPool {
    clients: Arc<DashMap<String, Client>>,
    default_timeout: Duration,
}

impl HttpClientPool {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self {
            clients: Arc::new(DashMap::new()),
            default_timeout: Duration::from_secs(default_timeout_secs),
        }
    }

    pub fn get_or_create(&self, _provider: &str) -> Client {
        if let Some(client) = self.clients.get(_provider) {
            return client.clone();
        }
        let client = ClientBuilder::new()
            .timeout(self.default_timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        self.clients.insert(_provider.to_string(), client.clone());
        client
    }

    pub fn default_client(&self) -> Client {
        ClientBuilder::new()
            .timeout(self.default_timeout)
            .build()
            .unwrap_or_else(|_| Client::new())
    }
}

impl Default for HttpClientPool {
    fn default() -> Self {
        Self::new(30)
    }
}
