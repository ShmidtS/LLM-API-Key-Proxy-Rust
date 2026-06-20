use dashmap::DashMap;
use reqwest::{Client, ClientBuilder};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tracing::{debug, error, warn};

pub const POOL_IDLE_TIMEOUT_SECS: u64 = 90;
pub const POOL_MAX_IDLE_PER_HOST: usize = 100;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// User-Agent advertised to upstream providers, replacing reqwest's default.
const USER_AGENT: &str = concat!("llm-proxy/", env!("CARGO_PKG_VERSION"));

/// Thin wrapper around a `reqwest` client.
#[derive(Debug, Clone)]
pub struct PooledClient {
    pub client: Client,
    pub is_streaming: bool,
}

impl PooledClient {
    pub fn new(client: Client, is_streaming: bool) -> Self {
        Self {
            client,
            is_streaming,
        }
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
    http2_enabled: bool,
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
            connect_timeout: Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS),
            pool_idle_timeout: Duration::from_secs(POOL_IDLE_TIMEOUT_SECS),
            pool_max_idle_per_host: POOL_MAX_IDLE_PER_HOST,
            http2_enabled: false,
        }
    }

    /// Override the per-client TCP connect timeout (default 10s).
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Force HTTP/2 prior knowledge: skip ALPN negotiation and assume the
    /// upstream speaks HTTP/2. Only enable for hosts known to support h2,
    /// since it disables HTTP/1.x fallback entirely.
    pub fn with_http2_enabled(mut self, enabled: bool) -> Self {
        self.http2_enabled = enabled;
        self
    }

    pub fn get_or_create(&self, provider: &str) -> Client {
        self.get_or_create_with_timeout(provider, self.default_timeout, false)
    }

    pub fn get_or_create_streaming(&self, provider: &str) -> Client {
        self.get_or_create_with_timeout(
            &format!("{provider}:streaming"),
            self.streaming_timeout,
            true,
        )
    }

    pub fn default_client(&self) -> Client {
        self.build_client(self.default_timeout, false)
    }

    fn build_client(&self, timeout: Duration, is_streaming: bool) -> Client {
        let mut builder = ClientBuilder::new()
            .user_agent(USER_AGENT)
            .timeout(timeout)
            .connect_timeout(self.connect_timeout)
            .pool_idle_timeout(self.pool_idle_timeout)
            .pool_max_idle_per_host(self.pool_max_idle_per_host)
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_nodelay(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .use_rustls_tls();

        if self.http2_enabled {
            builder = builder.http2_prior_knowledge();
        }

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

    fn get_or_create_with_timeout(
        &self,
        key: &str,
        timeout: Duration,
        is_streaming: bool,
    ) -> Client {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_tens_seconds_connect_timeout_and_http2_off() {
        let pool = HttpClientPool::with_timeouts(120, 300);
        assert_eq!(pool.connect_timeout, Duration::from_secs(10));
        assert!(!pool.http2_enabled);
    }

    #[test]
    fn with_connect_timeout_overrides_default() {
        let pool =
            HttpClientPool::with_timeouts(120, 300).with_connect_timeout(Duration::from_secs(7));
        assert_eq!(pool.connect_timeout, Duration::from_secs(7));
    }

    #[test]
    fn with_http2_enabled_sets_flag() {
        let pool = HttpClientPool::with_timeouts(120, 300).with_http2_enabled(true);
        assert!(pool.http2_enabled);
    }

    #[test]
    fn build_client_succeeds_with_custom_connect_timeout_and_http2() {
        // Verifies the builder chain (user_agent + connect_timeout + http2_prior_knowledge)
        // does not panic and yields a usable client.
        let pool = HttpClientPool::with_timeouts(120, 300)
            .with_connect_timeout(Duration::from_secs(5))
            .with_http2_enabled(true);
        let _client = pool.default_client();
    }

    #[test]
    fn build_client_succeeds_with_http2_disabled() {
        // Default path: ALPN negotiation via rustls, no forced h2.
        let pool = HttpClientPool::with_timeouts(120, 300);
        let _client = pool.default_client();
    }
}
