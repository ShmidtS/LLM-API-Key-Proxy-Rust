use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProxyConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_requests: usize,
    #[serde(default = "default_backlog")]
    pub backlog: u32,
    #[serde(default = "default_shutdown_timeout")]
    pub graceful_shutdown_timeout_secs: u64,
    #[serde(default = "default_global_timeout")]
    pub global_timeout_secs: u64,
    #[serde(default = "default_gzip_min_size")]
    pub gzip_min_size: usize,
    #[serde(default = "default_gzip_level")]
    pub gzip_compression_level: u32,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            max_concurrent_requests: default_max_concurrent(),
            backlog: default_backlog(),
            graceful_shutdown_timeout_secs: default_shutdown_timeout(),
            global_timeout_secs: default_global_timeout(),
            gzip_min_size: default_gzip_min_size(),
            gzip_compression_level: default_gzip_level(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    8000
}
fn default_max_concurrent() -> usize {
    1000
}
fn default_backlog() -> u32 {
    2048
}
fn default_shutdown_timeout() -> u64 {
    15
}
fn default_global_timeout() -> u64 {
    30
}
fn default_gzip_min_size() -> usize {
    2048
}
fn default_gzip_level() -> u32 {
    3
}
