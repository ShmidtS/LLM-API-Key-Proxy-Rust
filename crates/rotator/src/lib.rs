pub mod batcher;
pub mod circuit_breaker;
pub mod client;
pub mod cooldown;
pub mod costs;
pub mod credential_store;
pub mod credentials;
pub mod dynamic_provider;
pub mod error;
pub mod error_journal;
pub mod garbage_detection;
pub mod http_pool;
pub mod metrics;
pub mod model_filter;
pub mod model_info;
pub mod model_parser;
pub mod openai_responses;
pub mod provider_normalization;
pub mod provider_registry;
pub mod provider_runtime;
pub mod provider_utils;
pub mod providers;
pub mod rate_limiter;
pub mod request_sanitizer;
pub mod retry_policy;
pub mod stale_retry;
pub mod throttle;
pub mod token_calculator;
pub mod tokenizer;
pub mod transaction_log;
pub mod usage;

pub use batcher::EmbeddingBatcher;
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerRegistry, CircuitState};
pub use client::RotatorClient;
pub use cooldown::{CooldownEntry, CooldownManager};
pub use costs::estimate_cost;
pub use credential_store::{export_credentials, import_credentials};
pub use credentials::{CredentialManager, CredentialPermit, SelectionStrategy};
pub use dynamic_provider::{DynamicProviderConfig, DynamicProviderEnvNames};
pub use error::{Result, RotatorError};
pub use error_journal::{
    ErrorClass, ErrorEntry, ErrorJournal, classify_reqwest_error, classify_status_code,
};
pub use http_pool::HttpClientPool;
pub use metrics::ProxyMetrics;
pub use model_filter::{ModelFilterEngine, ModelFilterRule, ModelFilterStatus};
pub use model_info::{ModelInfoService, ModelMetadata};
pub use model_parser::{
    is_image_only_model, parse_model_ids, parse_model_ids_body, parse_model_ids_response,
};
pub use openai_responses::{
    NativeResponsesRequest, ResponsesBridge, ResponsesBridgeError, ResponsesEndpoint,
    ResponsesRequestContext, TranslatedResponsesRequest, responses_request_to_native_request,
};
pub use provider_normalization::{
    NormalizedModelRef, ProviderAlias, normalize_model_ref, normalize_provider_id, public_model_id,
    strip_provider_prefix,
};
pub use provider_registry::{AuthType, ProviderDefinition, ProviderRegistry};
pub use provider_runtime::{RuntimeProviderKind, RuntimeProviderRoute, normalize_upstream_url};
pub use provider_utils::{extract_usage, transform_tool_schema};
pub use providers::antigravity::AntigravityOAuthFlow;
pub use providers::oauth::{
    GoogleOAuthFlow, IflowOAuthFlow, OAuthFlow, OAuthManager, OAuthProvider, OAuthToken,
    QwenOAuthFlow, RefreshTokenResponse, refresh_oauth_token,
};
pub use providers::{Provider, ProviderManager};
pub use rate_limiter::{
    AdaptiveRateLimiter, AdaptiveRateLimiterRegistry, ProviderRateInfo, RateLimiterRegistry,
    TokenBucket,
};
pub use request_sanitizer::{SanitizerAction, SanitizerContext, SanitizerRule, sanitize_request};
pub use stale_retry::is_stale_connection_error;
pub use throttle::{ThrottleReason, classify_throttle};
pub use token_calculator::{TokenCalculator, calculate_max_tokens};
pub use transaction_log::{TokenUsage, TransactionLog, TransactionLogger, credential_hash_prefix};
pub use usage::{UsageEntry, UsageManager};

tokio::task_local! {
    /// When set, this client User-Agent is forwarded to the upstream provider
    /// instead of the pool's default. Set by proxy middleware from the incoming
    /// request's `User-Agent` header.
    pub static FORWARDED_USER_AGENT: String;
}
