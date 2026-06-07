use dashmap::DashMap;
use reqwest::{Client, ClientBuilder};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tracing::{debug, error, warn};

#[derive(Debug, Clone)]
pub struct HttpClientPool {
    clients: Arc<DashMap<String, Client>>,
    default_timeout: Duration,
    streaming_timeout: Duration,
    connect_timeout: Duration,
    pool_idle_timeout: Duration,
    pool_max_idle_per_host: usize,
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
            connect_timeout: Duration::from_secs(10),
            pool_idle_timeout: Duration::from_secs(30),
            pool_max_idle_per_host: 50,
        }
    }

    pub fn get_or_create(&self, provider: &str) -> Client {
        self.get_or_create_with_timeout(provider, self.default_timeout)
    }

    pub fn get_or_create_streaming(&self, provider: &str) -> Client {
        self.get_or_create_with_timeout(&format!("{provider}:streaming"), self.streaming_timeout)
    }

    pub fn default_client(&self) -> Client {
        self.build_client(self.default_timeout)
    }

    fn build_client(&self, timeout: Duration) -> Client {
        ClientBuilder::new()
            .timeout(timeout)
            .connect_timeout(self.connect_timeout)
            .pool_idle_timeout(self.pool_idle_timeout)
            .pool_max_idle_per_host(self.pool_max_idle_per_host)
            .tcp_keepalive(Duration::from_secs(30))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .gzip(true)
            .brotli(true)
            .build()
            .unwrap_or_else(|e| {
                error!(error = %e, "failed to build reqwest client, falling back to default");
                Client::new()
            })
    }

    fn get_or_create_with_timeout(&self, key: &str, timeout: Duration) -> Client {
        if let Some(client) = self.clients.get(key) {
            return client.clone();
        }
        let client = self.build_client(timeout);
        self.clients.insert(key.to_string(), client.clone());
        client
    }

    /// Warm up connections by sending parallel HEAD requests to known hosts.
    /// Call this after all providers are registered.
    pub async fn warmup(&self, hosts: Vec<String>) {
        let mut set = JoinSet::new();
        for host in hosts {
            let client = self.default_client();
            set.spawn(async move {
                for attempt in 0..3 {
                    match client.head(&host).send().await {
                        Ok(_) => {
                            debug!(host = %host, "connection warmed up");
                            return;
                        }
                        Err(e) if attempt < 2 => {
                            debug!(host = %host, attempt = attempt, error = %e, "warmup attempt failed, retrying");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Err(e) => {
                            warn!(host = %host, error = %e, "connection warmup failed after 3 attempts");
                        }
                    }
                }
            });
        }
        while set.join_next().await.is_some() {}
    }
}

impl Default for HttpClientPool {
    fn default() -> Self {
        Self::new(30)
    }
}
