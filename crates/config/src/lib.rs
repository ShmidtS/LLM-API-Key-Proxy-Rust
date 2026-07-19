pub mod proxy;

pub use proxy::ProxyConfig;

use std::{
    env,
    path::{Path, PathBuf},
    str::FromStr,
};
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
    if let Some(path) = find_env_file() {
        #[cfg(not(test))]
        let loaded = dotenvy::from_path_override(&path);
        #[cfg(test)]
        let loaded = dotenvy::from_path(&path);
        if loaded.is_ok() {
            tracing::info!(path = %path.display(), "loaded .env file");
        } else if let Err(err) = &loaded {
            tracing::warn!(path = %path.display(), error = %err, "dotenvy failed to parse .env; falling back to manual loader");
            load_env_file_lenient(&path);
        }
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
    apply_optional_string_env("PROXY_API_KEY", &mut config.proxy_api_key);
    apply_vec_env("API_KEYS", &mut config.api_keys);
    apply_env("AUTH_ENABLED", &mut config.auth_enabled)?;
    apply_vec_env("CORS_ALLOWED_ORIGINS", &mut config.cors_allowed_origins);
    apply_vec_env("CORS_ALLOWED_HEADERS", &mut config.cors_allowed_headers);
    apply_vec_env("CORS_ALLOWED_METHODS", &mut config.cors_allowed_methods);
    apply_env("REQUEST_TIMEOUT_SECS", &mut config.request_timeout_secs)?;
    apply_env(
        "TIMEOUT_READ_NON_STREAMING",
        &mut config.timeout_read_non_streaming_secs,
    )?;
    apply_env(
        "TIMEOUT_READ_STREAMING",
        &mut config.timeout_read_streaming_secs,
    )?;
    apply_env("MAX_BODY_BYTES", &mut config.max_body_bytes)?;
    apply_env("USAGE_PATH", &mut config.usage_path)?;
    apply_env(
        "USAGE_FLUSH_INTERVAL_SECS",
        &mut config.usage_flush_interval_secs,
    )?;
    apply_env("USAGE_BATCH_SIZE", &mut config.usage_batch_size)?;
    apply_env("MAX_RETRIES", &mut config.max_retries)?;
    apply_env("LOG_REQUEST_BODY", &mut config.log_request_body)?;
    apply_env("ENABLE_RAW_LOGGING", &mut config.enable_raw_logging)?;
    apply_optional_string_env(
        "OVERRIDE_TEMPERATURE_ZERO",
        &mut config.override_temperature_zero,
    );
    apply_env("HTTP_SSL_VERIFY", &mut config.http_ssl_verify)?;
    apply_vec_env("HTTP_SSL_VERIFY_HOSTS", &mut config.http_ssl_verify_hosts);
    apply_env("HTTP2_ENABLED", &mut config.http2_enabled)?;
    apply_optional_string_env("HTTP_DNS_RESOLVER", &mut config.http_dns_resolver);
    apply_optional_string_env("USER_AGENT", &mut config.user_agent);
    apply_env(
        "ADAPTIVE_RATE_LIMITER__ENABLED",
        &mut config.adaptive_rate_limiter.enabled,
    )?;
    apply_env(
        "ADAPTIVE_RATE_LIMITER__FLOOR_RPS",
        &mut config.adaptive_rate_limiter.floor_rps,
    )?;
    apply_env(
        "ADAPTIVE_RATE_LIMITER__ADDITIVE_INCREASE",
        &mut config.adaptive_rate_limiter.additive_increase,
    )?;
    apply_env(
        "ADAPTIVE_RATE_LIMITER__MULTIPLICATIVE_DECREASE",
        &mut config.adaptive_rate_limiter.multiplicative_decrease,
    )?;
    apply_env(
        "ADAPTIVE_RATE_LIMITER__SUCCESS_WINDOW_THRESHOLD",
        &mut config.adaptive_rate_limiter.success_window_threshold,
    )?;

    Ok(config)
}

/// Lenient `.env` loader used as a fallback when `dotenvy` rejects the file
/// (e.g. unquoted values containing spaces like `USER_AGENT=codex_cli_rs/0.1 (Win x86_64)`).
///
/// Mirrors `dotenvy` semantics for the subset we actually need:
/// - `KEY=VALUE` (VALUE may contain `=`, spaces, and any other char)
/// - `#` starts a comment only at line start (after optional whitespace)
/// - surrounding quotes (`"` or `'`) are stripped
/// - empty values are kept (override semantics same as `from_path_override`)
/// - lines that don't match `KEY=VALUE` are skipped silently
///
/// This is deliberately permissive — a single malformed line must not block
/// the whole registry from loading credentials and provider URLs.
fn load_env_file_lenient(path: &Path) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, mut value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        value = value.trim();
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            value = &value[1..value.len() - 1];
        }
        // Override semantics: always set, even if already present in std::env.
        // SAFETY: env::set_var is process-wide; called only during startup
        // before any concurrent access. Mirrors dotenvy::from_path_override.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
    }
}

pub fn find_env_file() -> Option<PathBuf> {
    env_file_candidates().into_iter().find(|path| path.exists())
}

fn env_file_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    candidates.push(PathBuf::from(".env"));

    if let Ok(exe_path) = env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
    {
        candidates.push(exe_dir.join(".env"));
    }

    if let Ok(current_dir) = env::current_dir()
        && let Some(workspace_root) = find_workspace_root(&current_dir)
    {
        candidates.push(workspace_root.join(".env"));
    }
    if let Ok(exe_path) = env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
        && let Some(workspace_root) = find_workspace_root(exe_dir)
    {
        candidates.push(workspace_root.join(".env"));
    }

    candidates
}

fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .filter(|dir| dir.join("Cargo.toml").exists())
        .last()
        .map(Path::to_path_buf)
}

fn trim_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value)
}

fn apply_env<T>(key: &str, target: &mut T) -> Result<(), ConfigError>
where
    T: FromStr,
{
    if let Ok(value) = env::var(key) {
        let trimmed = trim_quotes(&value);
        *target = trimmed.parse().map_err(|_| ConfigError::Parse {
            key: key.to_string(),
            value: trimmed.to_string(),
        })?;
    }
    Ok(())
}

fn apply_optional_string_env(key: &str, target: &mut Option<String>) {
    if let Ok(value) = env::var(key) {
        let trimmed = trim_quotes(&value);
        *target = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
}

fn apply_vec_env(key: &str, target: &mut Vec<String>) {
    if let Ok(value) = env::var(key) {
        *target = value
            .split(',')
            .map(str::trim)
            .map(trim_quotes)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::{load_env_file_lenient, load_from_env};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn load_from_env_reads_proxy_prefixed_admin_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("ADMIN_TOKEN");
            std::env::remove_var("PROXY__REQUEST_TIMEOUT_SECS");
            std::env::remove_var("TIMEOUT_READ_NON_STREAMING");
            std::env::remove_var("TIMEOUT_READ_STREAMING");
            std::env::set_var("PROXY__ADMIN_TOKEN", "test");
        }

        let config = load_from_env().unwrap();

        assert_eq!(config.admin_token.as_deref(), Some("test"));

        unsafe {
            std::env::remove_var("PROXY__ADMIN_TOKEN");
        }
    }

    #[test]
    fn load_from_env_reads_python_timeout_aliases() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("PROXY__REQUEST_TIMEOUT_SECS");
            std::env::set_var("TIMEOUT_READ_NON_STREAMING", "45");
            std::env::set_var("TIMEOUT_READ_STREAMING", "300");
        }

        let config = load_from_env().unwrap();

        assert_eq!(config.timeout_read_non_streaming_secs, 45);
        assert_eq!(config.timeout_read_streaming_secs, 300);

        unsafe {
            std::env::remove_var("TIMEOUT_READ_NON_STREAMING");
            std::env::remove_var("TIMEOUT_READ_STREAMING");
        }
    }

    #[test]
    fn load_from_env_reads_nested_guardrails_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("PROXY__GUARDRAILS__ENABLED", "true");
            std::env::set_var("PROXY__GUARDRAILS__MODE", "rescue");
            std::env::set_var("PROXY__GUARDRAILS__CHAT__VALIDATE_TOOLS", "true");
            std::env::set_var(
                "PROXY__GUARDRAILS__CONTEXT_COMPACTION__COMPACT_ABOVE_RATIO",
                "0.8",
            );
        }

        let config = load_from_env().unwrap();

        assert!(config.guardrails.enabled);
        assert_eq!(config.guardrails.mode, "rescue");
        assert!(config.guardrails.chat.validate_tools);
        assert_eq!(
            config.guardrails.context_compaction.compact_above_ratio,
            0.8
        );

        unsafe {
            std::env::remove_var("PROXY__GUARDRAILS__ENABLED");
            std::env::remove_var("PROXY__GUARDRAILS__MODE");
            std::env::remove_var("PROXY__GUARDRAILS__CHAT__VALIDATE_TOOLS");
            std::env::remove_var("PROXY__GUARDRAILS__CONTEXT_COMPACTION__COMPACT_ABOVE_RATIO");
        }
    }

    #[test]
    fn load_env_file_lenient_parses_unquoted_spaces_and_skips_bad_lines() {
        let dir = std::env::temp_dir().join("config_lenient_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(
            &path,
            // Mirrors the real-world .env that broke dotenvy:
            // - unquoted value with spaces and parens (USER_AGENT)
            // - a value containing `=`
            // - quoted value
            // - comment-only line
            // - malformed line (no `=`) that must be skipped
            "# comment line\n\
             USER_AGENT=codex_cli_rs/0.144.3 ai-sdk/4.0.23 (Windows 11; x86_64)\n\
             BAIDU_API_BASE=https://qianfan.baidubce.com/v2\n\
             FOO=bar=baz\n\
             QUOTED=\"hello world\"\n\
             malformed line without equals\n\
             EMPTY=\n",
        )
        .unwrap();

        load_env_file_lenient(&path);

        assert_eq!(
            std::env::var("USER_AGENT").unwrap(),
            "codex_cli_rs/0.144.3 ai-sdk/4.0.23 (Windows 11; x86_64)"
        );
        assert_eq!(
            std::env::var("BAIDU_API_BASE").unwrap(),
            "https://qianfan.baidubce.com/v2"
        );
        assert_eq!(std::env::var("FOO").unwrap(), "bar=baz");
        assert_eq!(std::env::var("QUOTED").unwrap(), "hello world");
        assert_eq!(std::env::var("EMPTY").unwrap(), "");

        unsafe {
            std::env::remove_var("USER_AGENT");
            std::env::remove_var("BAIDU_API_BASE");
            std::env::remove_var("FOO");
            std::env::remove_var("QUOTED");
            std::env::remove_var("EMPTY");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
