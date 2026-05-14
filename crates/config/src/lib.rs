pub mod proxy;

pub use proxy::ProxyConfig;

use std::{env, path::Path, str::FromStr};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("failed to parse env var {key}: {value}")]
    Parse { key: String, value: String },
    #[error("env file not found: {0}")]
    EnvFile(String),
    #[error(transparent)]
    External(#[from] external_config::ConfigError),
}

pub fn load_from_env() -> Result<proxy::ProxyConfig, ConfigError> {
    if Path::new(".env").exists() {
        let _ = dotenvy::from_path(".env");
    }

    let cfg = external_config::Config::builder()
        .add_source(
            external_config::Environment::default()
                .separator("__")
                .prefix("PROXY"),
        )
        .build()?;
    let mut config: proxy::ProxyConfig = cfg.try_deserialize()?;

    apply_env("HOST", &mut config.host)?;
    apply_env("PORT", &mut config.port)?;
    apply_env(
        "MAX_CONCURRENT_REQUESTS",
        &mut config.max_concurrent_requests,
    )?;
    apply_env("BACKLOG", &mut config.backlog)?;
    apply_env(
        "GRACEFUL_SHUTDOWN_TIMEOUT_SECS",
        &mut config.graceful_shutdown_timeout_secs,
    )?;
    apply_env("GLOBAL_TIMEOUT_SECS", &mut config.global_timeout_secs)?;
    apply_env("GZIP_MIN_SIZE", &mut config.gzip_min_size)?;
    apply_env("GZIP_COMPRESSION_LEVEL", &mut config.gzip_compression_level)?;
    apply_optional_string_env("ADMIN_TOKEN", &mut config.admin_token);
    apply_vec_env("API_KEYS", &mut config.api_keys);
    apply_env("AUTH_ENABLED", &mut config.auth_enabled)?;
    apply_vec_env("CORS_ALLOWED_ORIGINS", &mut config.cors_allowed_origins);
    apply_vec_env("CORS_ALLOWED_HEADERS", &mut config.cors_allowed_headers);
    apply_vec_env("CORS_ALLOWED_METHODS", &mut config.cors_allowed_methods);
    apply_env("REQUEST_TIMEOUT_SECS", &mut config.request_timeout_secs)?;
    apply_env("MAX_BODY_BYTES", &mut config.max_body_bytes)?;
    apply_env("USAGE_PATH", &mut config.usage_path)?;
    apply_env(
        "USAGE_FLUSH_INTERVAL_SECS",
        &mut config.usage_flush_interval_secs,
    )?;
    apply_env("USAGE_BATCH_SIZE", &mut config.usage_batch_size)?;
    apply_env("MAX_RETRIES", &mut config.max_retries)?;
    apply_env("LOG_REQUEST_BODY", &mut config.log_request_body)?;

    Ok(config)
}

fn apply_env<T>(key: &str, target: &mut T) -> Result<(), ConfigError>
where
    T: FromStr,
{
    if let Ok(value) = env::var(key) {
        *target = value.parse().map_err(|_| ConfigError::Parse {
            key: key.to_string(),
            value,
        })?;
    }
    Ok(())
}

fn apply_optional_string_env(key: &str, target: &mut Option<String>) {
    if let Ok(value) = env::var(key) {
        *target = if value.is_empty() { None } else { Some(value) };
    }
}

fn apply_vec_env(key: &str, target: &mut Vec<String>) {
    if let Ok(value) = env::var(key) {
        *target = value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::load_from_env;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn load_from_env_reads_proxy_prefixed_admin_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("ADMIN_TOKEN");
            std::env::set_var("PROXY__ADMIN_TOKEN", "test");
        }

        let config = load_from_env().unwrap();

        assert_eq!(config.admin_token.as_deref(), Some("test"));

        unsafe {
            std::env::remove_var("PROXY__ADMIN_TOKEN");
        }
    }
}
