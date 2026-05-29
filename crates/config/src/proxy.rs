use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_comma_separated<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(ToString::to_string)
        .collect())
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = Option::<String>::deserialize(deserializer)?;
    Ok(s.filter(|x| !x.is_empty()))
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GuardrailsRouteConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub validate_tools: bool,
    #[serde(default)]
    pub validate_json: bool,
    #[serde(default)]
    pub enforce_steps: bool,
    #[serde(default)]
    pub compact_context: bool,
    #[serde(default)]
    pub recover_errors: bool,
    #[serde(default)]
    pub validate_streaming: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextCompactionConfig {
    #[serde(default)]
    pub max_context_messages: usize,
    #[serde(default)]
    pub compact_above_ratio: f64,
}

impl Default for ContextCompactionConfig {
    fn default() -> Self {
        Self {
            max_context_messages: 0,
            compact_above_ratio: 0.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GuardrailsProxyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_guardrails_mode")]
    pub mode: String,
    #[serde(default)]
    pub chat: GuardrailsRouteConfig,
    #[serde(default)]
    pub anthropic: GuardrailsRouteConfig,
    #[serde(default)]
    pub responses: GuardrailsRouteConfig,
    #[serde(default)]
    pub max_rescue_attempts: usize,
    #[serde(default)]
    pub max_guardrail_retries: usize,
    #[serde(default)]
    pub context_compaction: ContextCompactionConfig,
}

impl Default for GuardrailsProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_guardrails_mode(),
            chat: GuardrailsRouteConfig::default(),
            anthropic: GuardrailsRouteConfig::default(),
            responses: GuardrailsRouteConfig::default(),
            max_rescue_attempts: 0,
            max_guardrail_retries: 0,
            context_compaction: ContextCompactionConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GuardrailsConfig {
    #[serde(default)]
    pub proxy: GuardrailsProxyConfig,
}

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
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub admin_token: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub proxy_api_key: Option<String>,
    #[serde(default, deserialize_with = "deserialize_comma_separated")]
    pub api_keys: Vec<String>,
    #[serde(default = "default_auth_enabled")]
    pub auth_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_comma_separated")]
    pub cors_allowed_origins: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_comma_separated")]
    pub cors_allowed_headers: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_comma_separated")]
    pub cors_allowed_methods: Vec<String>,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_timeout_read_non_streaming_secs")]
    pub timeout_read_non_streaming_secs: u64,
    #[serde(default = "default_timeout_read_streaming_secs")]
    pub timeout_read_streaming_secs: u64,
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(default = "default_usage_path")]
    pub usage_path: String,
    #[serde(default = "default_usage_flush_interval_secs")]
    pub usage_flush_interval_secs: u64,
    #[serde(default = "default_usage_batch_size")]
    pub usage_batch_size: usize,
    #[serde(default = "default_max_retries")]
    pub max_retries: usize,
    #[serde(default = "default_log_request_body")]
    pub log_request_body: bool,
    #[serde(default)]
    pub enable_raw_logging: bool,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub override_temperature_zero: Option<String>,
    #[serde(default = "default_http_ssl_verify")]
    pub http_ssl_verify: bool,
    #[serde(default, deserialize_with = "deserialize_comma_separated")]
    pub http_ssl_verify_hosts: Vec<String>,
    #[serde(default = "default_http2_enabled")]
    pub http2_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub http_dns_resolver: Option<String>,
    #[serde(default)]
    pub guardrails: GuardrailsProxyConfig,
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
            admin_token: default_admin_token(),
            proxy_api_key: default_proxy_api_key(),
            api_keys: default_api_keys(),
            auth_enabled: default_auth_enabled(),
            cors_allowed_origins: default_cors_allowed_origins(),
            cors_allowed_headers: default_cors_allowed_headers(),
            cors_allowed_methods: default_cors_allowed_methods(),
            request_timeout_secs: default_request_timeout_secs(),
            timeout_read_non_streaming_secs: default_timeout_read_non_streaming_secs(),
            timeout_read_streaming_secs: default_timeout_read_streaming_secs(),
            max_body_bytes: default_max_body_bytes(),
            usage_path: default_usage_path(),
            usage_flush_interval_secs: default_usage_flush_interval_secs(),
            usage_batch_size: default_usage_batch_size(),
            max_retries: default_max_retries(),
            log_request_body: default_log_request_body(),
            enable_raw_logging: default_enable_raw_logging(),
            override_temperature_zero: default_override_temperature_zero(),
            http_ssl_verify: default_http_ssl_verify(),
            http_ssl_verify_hosts: default_http_ssl_verify_hosts(),
            http2_enabled: default_http2_enabled(),
            http_dns_resolver: default_http_dns_resolver(),
            guardrails: GuardrailsProxyConfig::default(),
        }
    }
}

fn default_guardrails_mode() -> String {
    "off".into()
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
fn default_admin_token() -> Option<String> {
    None
}
fn default_proxy_api_key() -> Option<String> {
    None
}
fn default_api_keys() -> Vec<String> {
    Vec::new()
}
fn default_auth_enabled() -> bool {
    true
}
fn default_cors_allowed_origins() -> Vec<String> {
    Vec::new()
}
fn default_cors_allowed_headers() -> Vec<String> {
    Vec::new()
}
fn default_cors_allowed_methods() -> Vec<String> {
    Vec::new()
}
fn default_request_timeout_secs() -> u64 {
    600
}
fn default_timeout_read_non_streaming_secs() -> u64 {
    120
}
fn default_timeout_read_streaming_secs() -> u64 {
    300
}
fn default_max_body_bytes() -> usize {
    10 * 1024 * 1024
}
fn default_usage_path() -> String {
    "usage.json".into()
}
fn default_usage_flush_interval_secs() -> u64 {
    60
}
fn default_usage_batch_size() -> usize {
    100
}
fn default_max_retries() -> usize {
    3
}
fn default_log_request_body() -> bool {
    false
}
fn default_enable_raw_logging() -> bool {
    false
}
fn default_override_temperature_zero() -> Option<String> {
    None
}
fn default_http_ssl_verify() -> bool {
    true
}
fn default_http_ssl_verify_hosts() -> Vec<String> {
    Vec::new()
}
fn default_http2_enabled() -> bool {
    false
}
fn default_http_dns_resolver() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::ProxyConfig;

    #[test]
    fn default_includes_auth_cors_usage_retry_and_logging_settings() {
        let config = ProxyConfig::default();

        assert_eq!(config.admin_token, None);
        assert!(config.api_keys.is_empty());
        assert!(config.auth_enabled);
        assert!(config.cors_allowed_origins.is_empty());
        assert!(config.cors_allowed_headers.is_empty());
        assert!(config.cors_allowed_methods.is_empty());
        assert_eq!(config.request_timeout_secs, 600);
        assert_eq!(config.timeout_read_non_streaming_secs, 120);
        assert_eq!(config.timeout_read_streaming_secs, 300);
        assert_eq!(config.max_body_bytes, 10 * 1024 * 1024);
        assert_eq!(config.usage_path, "usage.json");
        assert_eq!(config.usage_flush_interval_secs, 60);
        assert_eq!(config.usage_batch_size, 100);
        assert_eq!(config.max_retries, 3);
        assert!(!config.log_request_body);
        assert!(!config.enable_raw_logging);
        assert!(!config.guardrails.enabled);
        assert_eq!(config.guardrails.mode, "off");
        assert!(!config.guardrails.chat.enabled);
        assert!(!config.guardrails.chat.validate_tools);
        assert!(!config.guardrails.chat.validate_json);
        assert!(!config.guardrails.chat.enforce_steps);
        assert!(!config.guardrails.chat.compact_context);
        assert!(!config.guardrails.chat.recover_errors);
        assert!(!config.guardrails.chat.validate_streaming);
        assert_eq!(config.guardrails.max_rescue_attempts, 0);
        assert_eq!(config.guardrails.max_guardrail_retries, 0);
        assert_eq!(config.guardrails.context_compaction.max_context_messages, 0);
        assert_eq!(
            config.guardrails.context_compaction.compact_above_ratio,
            0.0
        );
    }

    #[test]
    fn parses_guardrails_config() {
        let config: ProxyConfig = external_config::Config::builder()
            .add_source(external_config::File::from_str(
                r#"
                    [guardrails]
                    enabled = true
                    mode = "rescue"
                    max_rescue_attempts = 2
                    max_guardrail_retries = 3

                    [guardrails.chat]
                    enabled = true
                    validate_tools = true
                    validate_json = true
                    enforce_steps = true
                    compact_context = true
                    recover_errors = true
                    validate_streaming = true

                    [guardrails.anthropic]
                    enabled = true
                    validate_json = true

                    [guardrails.responses]
                    validate_tools = true

                    [guardrails.context_compaction]
                    max_context_messages = 50
                    compact_above_ratio = 0.75
                "#,
                external_config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();

        assert!(config.guardrails.enabled);
        assert_eq!(config.guardrails.mode, "rescue");
        assert_eq!(config.guardrails.max_rescue_attempts, 2);
        assert_eq!(config.guardrails.max_guardrail_retries, 3);
        assert!(config.guardrails.chat.enabled);
        assert!(config.guardrails.chat.validate_tools);
        assert!(config.guardrails.chat.validate_json);
        assert!(config.guardrails.chat.enforce_steps);
        assert!(config.guardrails.chat.compact_context);
        assert!(config.guardrails.chat.recover_errors);
        assert!(config.guardrails.chat.validate_streaming);
        assert!(config.guardrails.anthropic.enabled);
        assert!(config.guardrails.anthropic.validate_json);
        assert!(!config.guardrails.anthropic.validate_tools);
        assert!(!config.guardrails.responses.enabled);
        assert!(config.guardrails.responses.validate_tools);
        assert_eq!(
            config.guardrails.context_compaction.max_context_messages,
            50
        );
        assert_eq!(
            config.guardrails.context_compaction.compact_above_ratio,
            0.75
        );
    }
}
