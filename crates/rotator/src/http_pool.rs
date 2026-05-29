use dashmap::DashMap;
use reqwest::{Client, ClientBuilder};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct HttpClientPool {
    clients: Arc<DashMap<String, Client>>,
    default_timeout: Duration,
    streaming_timeout: Duration,
}

impl HttpClientPool {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self::with_timeouts(default_timeout_secs, default_timeout_secs)
    }

    pub fn with_timeouts(default_timeout_secs: u64, streaming_timeout_secs: u64) -> Self {
        Self {
            clients: Arc::new(DashMap::new()),
            default_timeout: Duration::from_secs(default_timeout_secs),
            streaming_timeout: Duration::from_secs(streaming_timeout_secs),
        }
    }

    pub fn get_or_create(&self, provider: &str) -> Client {
        self.get_or_create_with_timeout(provider, self.default_timeout)
    }

    pub fn get_or_create_streaming(&self, provider: &str) -> Client {
        self.get_or_create_with_timeout(&format!("{provider}:streaming"), self.streaming_timeout)
    }

    pub fn default_client(&self) -> Client {
        ClientBuilder::new()
            .timeout(self.default_timeout)
            .build()
            .unwrap_or_else(|_| Client::new())
    }

    fn get_or_create_with_timeout(&self, key: &str, timeout: Duration) -> Client {
        if let Some(client) = self.clients.get(key) {
            return client.clone();
        }
        let client = ClientBuilder::new()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        self.clients.insert(key.to_string(), client.clone());
        client
    }
}

impl Default for HttpClientPool {
    fn default() -> Self {
        Self::new(30)
    }
}
