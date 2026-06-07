use dashmap::DashMap;
use reqwest::{Client, ClientBuilder};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tracing::{debug, error, warn};

pub const POOL_IDLE_TIMEOUT_SECS: u64 = 90;
pub const POOL_MAX_IDLE_PER_HOST: usize = 100;

/// Thin wrapper around a `reqwest` client that tracks active and idle
/// connection counts for metrics.
#[derive(Debug, Clone)]
pub struct PooledClient {
    pub client: Client,
    pub is_streaming: bool,
    active: Arc<AtomicUsize>,
    idle: Arc<AtomicUsize>,
}

impl PooledClient {
    pub fn new(client: Client, is_streaming: bool) -> Self {
        Self {
            client,
            is_streaming,
            active: Arc::new(AtomicUsize::new(0)),
            idle: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn inc_active(&self) {
        self.active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn set_idle(&self, value: usize) {
        self.idle.store(value, Ordering::Relaxed);
    }

    pub fn active_conns(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    pub fn idle_conns(&self) -> usize {
        self.idle.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct HttpClientPool {
    clients: Arc<DashMap<String, PooledClient>>,
    default_timeout: Duration,
    streaming_timeout: Duration,
    connect_timeout: Duration,
    pool_idle_timeout: Duration,
    pool_max_idle_per_host: usize,
    metrics: Option<Arc<crate::metrics::ProxyMetrics>>,
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
            pool_idle_timeout: Duration::from_secs(POOL_IDLE_TIMEOUT_SECS),
            pool_max_idle_per_host: POOL_MAX_IDLE_PER_HOST,
            metrics: None,
        }
    }

    pub fn with_metrics(mut self, metrics: Arc<crate::metrics::ProxyMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub fn get_or_create(&self, provider: &str) -> Client {
        self.get_or_create_with_timeout(provider, self.default_timeout, false)
    }

    pub fn get_or_create_streaming(&self, provider: &str) -> Client {
        self.get_or_create_with_timeout(&format!("{provider}:streaming"), self.streaming_timeout, true)
    }

    pub fn default_client(&self) -> Client {
        self.build_client(self.default_timeout, false)
    }

    fn build_client(&self, timeout: Duration, is_streaming: bool) -> Client {
        let mut builder = ClientBuilder::new()
            .timeout(timeout)
            .connect_timeout(self.connect_timeout)
            .pool_idle_timeout(self.pool_idle_timeout)
            .pool_max_idle_per_host(self.pool_max_idle_per_host)
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_nodelay(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .use_rustls_tls();

        if is_streaming {
            builder = builder.no_gzip().no_brotli();
        } else {
            builder = builder.gzip(true).brotli(true);
        }

        builder.build().unwrap_or_else(|e| {
            error!(error = %e, "failed to build reqwest client, falling back to default");
            Client::new()
        })
    }

    fn get_or_create_with_timeout(&self, key: &str, timeout: Duration, is_streaming: bool) -> Client {
        if let Some(client) = self.clients.get(key) {
            return client.client.clone();
        }
        let client = self.build_client(timeout, is_streaming);
        let pooled = PooledClient::new(client.clone(), is_streaming);
        self.clients.insert(key.to_string(), pooled);
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

    /// Synchronise connection counters with the metrics registry.
    pub fn sync_metrics(&self) {
        let Some(ref m) = self.metrics else { return };
        for entry in self.clients.iter() {
            let provider = entry.key();
            let client = entry.value();
            m.set_pool_active(provider, client.active_conns() as u64);
            m.set_pool_idle(provider, client.idle_conns() as u64);
        }
    }

    /// Set the pool-wide idle timeout used when building new clients.
    pub fn set_pool_idle_timeout(&mut self, secs: u64) {
        self.pool_idle_timeout = Duration::from_secs(secs);
    }

    /// Set the pool-wide max idle connections per host.
    pub fn set_pool_max_idle_per_host(&mut self, n: usize) {
        self.pool_max_idle_per_host = n;
    }
}

impl Default for HttpClientPool {
    fn default() -> Self {
        Self::new(30)
    }
}
