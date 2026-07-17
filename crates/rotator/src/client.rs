use crate::circuit_breaker::{CircuitBreakerRegistry, CircuitState};
use crate::cooldown::CooldownManager;
use crate::credentials::{CredentialManager, CredentialPermit};
use crate::error::{Result, RotatorError};
use crate::error_journal::{ErrorClass, ErrorJournal};
use crate::http_pool::HttpClientPool;
use crate::provider_registry::{AuthType, ProviderDefinition, ProviderRegistry};
use crate::provider_runtime::normalize_upstream_url;
use crate::provider_utils::extract_usage;
use crate::providers::oauth::{OAuthManager, OAuthToken, refresh_oauth_token};
use crate::providers::transform_request_for_provider;
use crate::rate_limiter::{AdaptiveRateLimiterRegistry, RateLimiterRegistry};
use crate::request_sanitizer::{SanitizerContext, sanitize_request};
use crate::retry_policy::{
    FailureClass, RetryDecision, classify_upstream_failure, decide_retry_for_provider,
    get_retry_backoff, retry_after_from_headers_and_body,
};
use crate::usage::UsageManager;
use crate::zai_quota::{ZaiQuotaCache, ZaiQuotaStatus};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, warn};

fn oauth_cache_key(provider: &str, key: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{}:{}", provider, hasher.finish())
}

/// Default same-key retries on stale/dead pooled connections. Matches bifrost's
/// `maxStaleConnRetries` (core/network/http.go): lets a request walk past a few
/// dead keep-alive connections to a live one while staying well under the pool's
/// internal attempt cap.
const DEFAULT_MAX_STALE_RETRIES: u32 = 3;

/// Upper bound on how many times a single request may sleep waiting for a
/// full-cooldown window to lift. Each wait is additionally capped at 120s, so
/// even a permanently throttled provider surfaces an error within a few
/// minutes instead of parking the task until the client gives up.
const MAX_COOLDOWN_WAITS: u32 = 5;

/// zai rate-limits by request frequency (per-minute), not just window quota.
/// A 429 with no Retry-After on a key the quota probe still calls "Available"
/// is a rate-limit hit: cool the key for one full rate window so it does not
/// re-enter rotation too early and catch another 429.
const ZAI_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct RotatorClient {
    pub credentials: Arc<CredentialManager>,
    http_pool: Arc<HttpClientPool>,
    provider_registry: Arc<ProviderRegistry>,
    rate_limiter: Arc<RateLimiterRegistry>,
    cooldown: Arc<CooldownManager>,
    circuit_breakers: Arc<CircuitBreakerRegistry>,
    usage_manager: Option<Arc<UsageManager>>,
    last_latency_ms: Arc<DashMap<String, u64>>,
    max_retries: usize,
    /// Extra same-key attempts on stale/dead pooled connections before falling
    /// back to credential rotation. Mirrors bifrost's `maxStaleConnRetries`
    /// (default 3). See `stale_retry::is_stale_connection_error`.
    max_stale_retries: u32,
    oauth_manager: Arc<OAuthManager>,
    oauth_refresh_locks: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    adaptive_rate_limiter: Option<Arc<AdaptiveRateLimiterRegistry>>,
    error_journal: Option<Arc<ErrorJournal>>,
    metrics: Arc<crate::metrics::ProxyMetrics>,
    provider_cache: Arc<DashMap<String, Arc<ProviderDefinition>>>,
    /// Per-key TTL cache of zai quota verdicts. Probed before dispatch so an
    /// exhausted key is rotated past instead of surfacing an upstream 429.
    zai_quota_cache: Arc<ZaiQuotaCache>,
    /// Dedicated short-timeout client for zai quota probes. Kept separate from
    /// the upstream pool so a slow monitor endpoint cannot starve inference
    /// keep-alive slots.
    quota_probe_client: reqwest::Client,
}

impl RotatorClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        credentials: CredentialManager,
        http_pool: HttpClientPool,
        provider_registry: Arc<ProviderRegistry>,
        rate_limiter: Arc<RateLimiterRegistry>,
        cooldown: Arc<CooldownManager>,
        circuit_breakers: Arc<CircuitBreakerRegistry>,
        usage_manager: Option<Arc<UsageManager>>,
        max_retries: usize,
    ) -> Self {
        Self {
            credentials: Arc::new(credentials),
            http_pool: Arc::new(http_pool),
            provider_registry,
            rate_limiter,
            cooldown,
            circuit_breakers,
            usage_manager,
            last_latency_ms: Arc::new(DashMap::new()),
            max_retries,
            max_stale_retries: DEFAULT_MAX_STALE_RETRIES,
            oauth_manager: Arc::new(OAuthManager::new()),
            oauth_refresh_locks: Arc::new(DashMap::new()),
            adaptive_rate_limiter: None,
            error_journal: None,
            metrics: Arc::new(crate::metrics::ProxyMetrics::new()),
            provider_cache: Arc::new(DashMap::new()),
            zai_quota_cache: Arc::new(ZaiQuotaCache::new()),
            quota_probe_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    pub fn with_adaptive_rate_limiter(
        mut self,
        adaptive_rate_limiter: Arc<AdaptiveRateLimiterRegistry>,
    ) -> Self {
        self.adaptive_rate_limiter = Some(adaptive_rate_limiter);
        self
    }

    pub fn with_error_journal(mut self, error_journal: Arc<ErrorJournal>) -> Self {
        self.error_journal = Some(error_journal);
        self
    }

    /// Override the number of same-key retries performed on stale/dead pooled
    /// connections before falling back to credential rotation.
    pub fn with_max_stale_retries(mut self, max_stale_retries: u32) -> Self {
        self.max_stale_retries = max_stale_retries;
        self
    }

    /// Blocking zai quota preflight for `key`. Probes the monitor endpoint
    /// (cached per key for `QUOTA_CACHE_TTL`); if the key is exhausted it is
    /// placed on cooldown for `EXHAUSTED_COOLDOWN` and `false` is returned so
    /// the caller rotates to the next key. Non-zai providers short-circuit to
    /// `true`. Must be awaited *before* dispatching the client request.
    async fn ensure_zai_key_available(&self, provider: &str, key: &str) -> bool {
        if provider != "zai" {
            return true;
        }
        let status = crate::zai_quota::cached_quota_status(
            &self.zai_quota_cache,
            &self.quota_probe_client,
            key,
        )
        .await;
        match status {
            ZaiQuotaStatus::Available { remaining } => {
                if let Some(remaining) = remaining {
                    tracing::debug!(target: "zai_quota", key = %crate::transaction_log::credential_hash_prefix(key), remaining, "zai key available");
                }
                true
            }
            ZaiQuotaStatus::Exhausted { cooldown } => {
                tracing::info!(target: "zai_quota", key = %crate::transaction_log::credential_hash_prefix(key), cooldown_secs = cooldown.as_secs(), "zai key quota exhausted, cooling down");
                self.cooldown.add_cooldown(provider, key, cooldown);
                false
            }
        }
    }

    /// On a 429 from zai, probe the monitor endpoint for the *exact* reset
    /// horizon (`nextResetTime`) instead of guessing a clamp window. zai's
    /// quota window can be days/weeks; the precise cooldown keeps the key out
    /// of rotation until it actually recovers. Returns `None` for non-zai
    /// providers or when the probe cannot determine a horizon (caller falls
    /// back to the clamped Retry-After window).
    async fn zai_exact_cooldown_on_429(&self, provider: &str, key: &str) -> Option<Duration> {
        if provider != "zai" {
            return None;
        }
        // Fresh probe, bypassing the cache: the 429 already proved the cached
        // verdict stale, and we need the API's current nextResetTime.
        match crate::zai_quota::check_quota(&self.quota_probe_client, key).await {
            ZaiQuotaStatus::Exhausted { cooldown } => Some(cooldown),
            ZaiQuotaStatus::Available { .. } => None,
        }
    }

    pub fn error_journal(&self) -> Option<Arc<ErrorJournal>> {
        self.error_journal.clone()
    }

    pub fn max_retries(&self) -> usize {
        self.max_retries
    }

    pub fn metrics(&self) -> Arc<crate::metrics::ProxyMetrics> {
        self.metrics.clone()
    }

    /// Dispatch a POST, transparently retrying on stale/dead pooled-connection
    /// errors using the SAME credential — no rotation, no cooldown, no circuit
    /// update. Mirrors bifrost's `StaleConnectionRetryIfErr`: a dead pooled
    /// connection is not credential-specific, and these errors occur before the
    /// upstream commits the request, so re-issuing the POST is safe (no
    /// duplicate inference). Only persistent transport errors propagate to the
    /// caller, which then rotates keys exactly as before.
    async fn dispatch_with_stale_retry(
        &self,
        provider: &str,
        client: &reqwest::Client,
        url: &str,
        token: &str,
        body: bytes::Bytes,
        content_type: &str,
    ) -> std::result::Result<reqwest::Response, reqwest::Error> {
        let mut stale_attempt: u32 = 0;
        loop {
            let mut request = self
                .apply_auth_headers(provider, client.post(url), token)
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(reqwest::Body::from(body.clone()));
            if !self.http_pool.has_custom_user_agent()
                && let Ok(ua) = crate::FORWARDED_USER_AGENT.try_with(|ua| ua.clone())
            {
                request = request.header(reqwest::header::USER_AGENT, ua.as_str());
            }
            match request.send().await {
                Ok(resp) => return Ok(resp),
                Err(err) => {
                    // Retry only while budget remains AND the failure is a
                    // pre-commit stale/dead connection (never a read timeout).
                    if stale_attempt < self.max_stale_retries
                        && crate::stale_retry::is_stale_connection_error(&err)
                    {
                        // Log only safe kind flags — reqwest's Display can embed
                        // the request URL, so we avoid `%err` here. The final
                        // persistent error is logged (URL-stripped) by the caller.
                        tracing::debug!(
                            provider,
                            stale_attempt,
                            connect = err.is_connect(),
                            timeout = err.is_timeout(),
                            "stale/dead pooled connection, retrying same key"
                        );
                        stale_attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    pub async fn request(
        &self,
        provider: &str,
        path: &str,
        mut body: serde_json::Value,
    ) -> Result<reqwest::Response> {
        Self::transform_request(provider, path, &mut body);

        // Pre-serialize body once to avoid expensive re-serialization on every retry.
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(bytes) => bytes::Bytes::from(bytes),
            Err(e) => return Err(RotatorError::Serialization(e.to_string())),
        };

        let stream = body
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let route = self
            .provider_registry
            .resolve_runtime_route(provider, path)
            .unwrap_or_else(|| crate::provider_runtime::RuntimeProviderRoute {
                provider_id: provider.to_owned(),
                kind: crate::provider_runtime::RuntimeProviderKind::LegacyModule,
                base_url: self.resolve_base_url(provider),
                action: path.trim_start_matches('/').to_owned(),
            });
        let upstream_path =
            self.provider_registry
                .resolve_endpoint_path(&route.provider_id, &route.action, &body);
        let url = normalize_upstream_url(&route.base_url, &upstream_path);

        let key_count = self.credentials.key_count(provider);
        let mut last_key: Option<String> = None;
        let mut tried_412_keys: Vec<String> = Vec::new();
        // Retained 412/422/451 response so we can surface it to the client even if a
        // later acquire finds no fresh key (e.g. some keys on cooldown) — otherwise a
        // request-level rejection degrades to an opaque AllKeysOnCooldown error.
        let mut pending_412 = None;
        // Number of times this request waited out a full-cooldown window. Bounded so a
        // permanently throttled provider cannot park the task forever.
        let mut cooldown_waits = 0u32;
        // Set by the terminal-429 branch to restart the whole rotation round after
        // waiting out the cooldown window — a transient throttle must not exhaust
        // the retry budget into a 502.
        let mut restart_rotation = false;

        // Outer loop: normally runs once. A terminal 429 (retry budget exhausted on
        // throttled keys) waits out the cooldown window and restarts the rotation.
        loop {
            for attempt in 0..=self.max_retries {
                if !self.circuit_breakers.is_allowed(provider) {
                    // Surface a retained request-level rejection (412/422/451) even if a
                    // concurrent failure tripped the provider circuit mid-rotation, instead
                    // of hiding it behind an opaque CircuitOpen.
                    if let Some((status, version, headers, body_text)) = pending_412.take() {
                        let mut builder = http::Response::builder().status(status).version(version);
                        *builder.headers_mut().expect("response builder is valid") = headers;
                        return Ok(builder
                            .body(bytes::Bytes::from(body_text))
                            .expect("response body rebuild should not fail")
                            .into());
                    }
                    return Err(RotatorError::CircuitOpen(provider.to_string()));
                }

                // Acquire a fresh key for this attempt. When every key is on a short
                // per-key cooldown (e.g. all zai keys throttled with 429), wait for the
                // nearest expiry and re-acquire *within the same attempt* — waiting out a
                // cooldown window is not a real retry and must not consume `attempt`.
                let cred = loop {
                    let cred = self.credentials.acquire_where(provider, |key| {
                        self.cooldown.is_available(provider, key)
                            && (key_count <= 1 || Some(key) != last_key.as_deref())
                            && !tried_412_keys.iter().any(|k| k.as_str() == key)
                    });
                    let none_reason = match cred {
                        Some(cred) => break cred,
                        None => {
                            // Mid-rotation through a request-level 412/422/451 with no fresh key
                            // available (e.g. some keys on cooldown): surface the retained upstream
                            // response instead of an opaque AllKeysOnCooldown error.
                            if let Some((status, version, headers, body_text)) = pending_412.take()
                            {
                                let mut builder =
                                    http::Response::builder().status(status).version(version);
                                *builder.headers_mut().expect("response builder is valid") =
                                    headers;
                                return Ok(builder
                                    .body(bytes::Bytes::from(body_text))
                                    .expect("response body rebuild should not fail")
                                    .into());
                            }
                            let has_any_keys = self.credentials.has_any_keys(provider);
                            if !has_any_keys {
                                return Err(RotatorError::NoCredentials(provider.to_string()));
                            }
                            let key_status = self.credentials.get_key_status(provider);
                            let status_str = key_status
                                .iter()
                                .map(|(key, current, limit)| {
                                    format!("{}: {}/{}", key, current, limit)
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            // acquire_least_loaded_where filters by cooldown.is_available;
                            // if keys exist but none was acquired, distinguish cooldown
                            // (keys idle 0/N) from a genuine concurrent-limit exhaustion.
                            let any_busy = key_status
                                .iter()
                                .any(|(_, current, limit)| *current >= *limit);
                            if any_busy {
                                return Err(RotatorError::AllKeysBusy(
                                    provider.to_string(),
                                    status_str,
                                ));
                            }
                            status_str
                        }
                    };
                    // Every key is on a short per-key cooldown. The client timeout
                    // (300-600s) is far longer than the cooldown window, so wait for the
                    // nearest expiry and retry instead of failing fast with a 502.
                    // Bounded: at most a few waits per request, each capped.
                    let waited = if cooldown_waits < MAX_COOLDOWN_WAITS {
                        self.cooldown.next_expiry(provider).map(|wait| {
                            let wait = wait.min(Duration::from_secs(120));
                            cooldown_waits += 1;
                            warn!(
                                provider,
                                attempt,
                                wait_ms = wait.as_millis() as u64,
                                cooldown_waits,
                                "all keys on cooldown, waiting for nearest expiry"
                            );
                            wait
                        })
                    } else {
                        None
                    };
                    match waited {
                        Some(wait) => {
                            tokio::time::sleep(wait + Duration::from_millis(50)).await;
                            continue;
                        }
                        None => {
                            return Err(RotatorError::AllKeysOnCooldown(
                                provider.to_string(),
                                none_reason,
                            ));
                        }
                    }
                };
                // We're about to try a fresh key, so a 412/422/451 retained from an earlier
                // attempt is no longer the current signal — drop it. A subsequent 429/5xx/
                // connection error must not be masked by a stale request-level rejection.
                pending_412 = None;
                // zai quota preflight (blocking): probe the monitor endpoint before
                // dispatching the client request. An exhausted key is cooled down and
                // skipped so rotation advances to the next available key rather than
                // surfacing an upstream 429. Cached per key to bound probe latency.
                if !self.ensure_zai_key_available(provider, &cred.key).await {
                    drop(cred);
                    continue;
                }
                let permit =
                    CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

                if !self.rate_limiter.acquire(provider, permit.key()) {
                    drop(permit);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }

                let client = if stream {
                    self.http_pool.get_or_create_streaming(provider)
                } else {
                    self.http_pool.get_or_create(provider)
                };
                let token = self.resolve_auth_token(provider, permit.key()).await?;
                let dispatch_started = Instant::now();
                let started_at = dispatch_started;
                let result = self
                    .dispatch_with_stale_retry(
                        provider,
                        &client,
                        &url,
                        &token,
                        body_bytes.clone(),
                        "application/json",
                    )
                    .await;
                let dispatch_latency_ms = dispatch_started.elapsed().as_millis() as u64;
                self.metrics
                    .record_request_dispatch_latency(provider, dispatch_latency_ms);
                self.last_latency_ms
                    .insert(provider.to_owned(), started_at.elapsed().as_millis() as u64);

                match result {
                    Ok(resp) if resp.status().is_success() => {
                        self.circuit_breakers.record_success(provider);
                        if let Some(ref arl) = self.adaptive_rate_limiter {
                            arl.record_success(provider, permit.key());
                        }
                        if stream {
                            return Ok(resp);
                        }

                        let status = resp.status();
                        let version = resp.version();
                        let headers = resp.headers().clone();
                        let bytes = resp
                            .bytes()
                            .await
                            .map_err(|e| RotatorError::Http(e.to_string()))?;
                        let body_text = String::from_utf8_lossy(&bytes);

                        if let Err(garbage_err) =
                            crate::garbage_detection::validate_response(&body_text)
                        {
                            if let Some(ref ej) = self.error_journal {
                                ej.record_error(
                                    provider,
                                    ErrorClass::Garbage,
                                    None,
                                    &garbage_err.reason,
                                );
                            }
                            let failure = FailureClass::GarbageResponse {
                                reason: garbage_err.reason,
                                score: garbage_err.score,
                            };
                            let decision = decide_retry_for_provider(
                                failure.clone(),
                                attempt as u32,
                                self.max_retries as u32,
                                Some(provider),
                                self.error_journal.as_deref(),
                            );
                            warn!(
                                provider,
                                attempt,
                                ?failure,
                                ?decision,
                                "garbage response detected, retrying..."
                            );
                            match decision {
                                RetryDecision::RotateKey => {
                                    self.cooldown.add_cooldown(
                                        provider,
                                        permit.key(),
                                        Duration::from_secs(5),
                                    );
                                    drop(permit);
                                    tokio::time::sleep(get_retry_backoff(
                                        attempt as u32,
                                        500,
                                        60_000,
                                    ))
                                    .await;
                                    continue;
                                }
                                _ => {
                                    let mut builder =
                                        http::Response::builder().status(status).version(version);
                                    *builder.headers_mut().expect("response builder is valid") =
                                        headers;
                                    return Ok(builder
                                        .body(bytes)
                                        .expect("response body rebuild should not fail")
                                        .into());
                                }
                            }
                        }

                        let mut builder = http::Response::builder().status(status).version(version);
                        *builder.headers_mut().expect("response builder is valid") = headers;
                        let resp = builder
                            .body(bytes)
                            .expect("response body rebuild should not fail")
                            .into();
                        return self
                            .record_usage_from_response(provider, permit.key(), resp)
                            .await;
                    }
                    Ok(resp) if resp.status().as_u16() == 429 => {
                        // 429 follows the credential's quota, not the request: rotate to
                        // another key. The throttled key gets a short per-key cooldown so the
                        // next acquire skips it instead of ping-ponging back onto the same
                        // exhausted credential. This is deliberately per-key only — a
                        // provider-wide circuit turned a transient throttle into a 502 storm.
                        let headers = resp.headers().clone();
                        let body_text = resp.text().await.unwrap_or_default();
                        if let Some(ref arl) = self.adaptive_rate_limiter {
                            arl.record_429(provider, permit.key());
                        }
                        // Single panic-safe Retry-After parse (header, then JSON body) shared
                        // with the rest of the crate — see retry_policy::parse_retry_after.
                        let retry_after =
                            retry_after_from_headers_and_body(&headers, Some(&body_text));
                        // A 429 contradicts a cached "Available" quota verdict — drop it so
                        // the next zai preflight re-probes instead of trusting stale state.
                        self.zai_quota_cache.invalidate(permit.key());
                        // Determine the per-key cooldown:
                        // - zai + no Retry-After: a rate-limit hit (the quota probe would
                        //   still say "Available" — probing only wastes an HTTP round-trip).
                        //   Cool for one full rate window so the key does not re-enter too
                        //   early and catch another 429.
                        // - zai + Retry-After present: probe for the exact nextResetTime
                        //   (zai's quota window can be days; only the API knows the horizon).
                        // - non-zai: clamped Retry-After guess as before.
                        let key_cooldown = if provider == "zai" && retry_after.is_none() {
                            ZAI_RATE_LIMIT_COOLDOWN
                        } else {
                            match self.zai_exact_cooldown_on_429(provider, permit.key()).await {
                                Some(exact) => exact,
                                None => retry_after
                                    .unwrap_or(Duration::from_secs(30))
                                    .clamp(Duration::from_secs(5), Duration::from_secs(300)),
                            }
                        };
                        self.cooldown
                            .add_cooldown(provider, permit.key(), key_cooldown);
                        warn!(
                            provider,
                            attempt,
                            key = %crate::transaction_log::credential_hash_prefix(permit.key()),
                            ?retry_after,
                            ?key_cooldown,
                            "429 throttled, key cooled down, rotating"
                        );
                        if attempt < self.max_retries {
                            last_key = Some(permit.key().to_string());
                            drop(permit);
                            // Rotate quickly: only the throttled key's quota is exhausted, so
                            // another key may serve immediately. Use a short backoff — NOT the
                            // full (possibly huge) Retry-After — so a bad header can't stall the
                            // task. The hint is surfaced in the terminal error below.
                            tokio::time::sleep(get_retry_backoff(attempt as u32, 500, 60_000))
                                .await;
                            continue;
                        }
                        // Retry budget exhausted on 429s. Every key we tried is throttled —
                        // but a 429 is transient (the quota window resets), so instead of a
                        // terminal 502 wait out the nearest cooldown window and restart the
                        // rotation round. Bounded by MAX_COOLDOWN_WAITS so a permanently
                        // throttled provider still surfaces an error eventually.
                        if cooldown_waits < MAX_COOLDOWN_WAITS
                            && let Some(wait) = self.cooldown.next_expiry(provider)
                        {
                            let wait = wait.min(Duration::from_secs(120));
                            cooldown_waits += 1;
                            warn!(
                                provider,
                                attempt,
                                wait_ms = wait.as_millis() as u64,
                                cooldown_waits,
                                "429 budget exhausted, waiting out cooldown window and retrying"
                            );
                            drop(permit);
                            tokio::time::sleep(wait + Duration::from_millis(50)).await;
                            restart_rotation = true;
                            break;
                        }
                        return Err(RotatorError::RateLimited(
                            provider.to_string(),
                            retry_after.map(|duration| duration.as_secs()),
                        ));
                    }
                    Ok(resp) if resp.status().is_server_error() => {
                        let status = resp.status();
                        let version = resp.version();
                        let headers = resp.headers().clone();
                        let body_text = resp.text().await.unwrap_or_default();
                        let failure = classify_upstream_failure(status, &headers, Some(&body_text));
                        if let Some(ref ej) = self.error_journal {
                            ej.record_error(
                                provider,
                                ErrorClass::ServerError,
                                Some(status.as_u16()),
                                &body_text,
                            );
                        }
                        let decision = decide_retry_for_provider(
                            failure.clone(),
                            attempt as u32,
                            self.max_retries as u32,
                            Some(provider),
                            self.error_journal.as_deref(),
                        );
                        error!(provider, attempt, status = %status, ?failure, ?decision, "server error, retrying...");
                        match decision {
                            RetryDecision::RetrySameKey => {
                                drop(permit);
                                tokio::time::sleep(get_retry_backoff(attempt as u32, 300, 60_000))
                                    .await;
                                continue;
                            }
                            RetryDecision::RotateKey => {
                                self.cooldown.add_cooldown(
                                    provider,
                                    permit.key(),
                                    Duration::from_secs(5),
                                );
                                drop(permit);
                                tokio::time::sleep(get_retry_backoff(attempt as u32, 300, 60_000))
                                    .await;
                                continue;
                            }
                            RetryDecision::CooldownKey { duration } => {
                                self.circuit_breakers.record_failure(provider);
                                self.cooldown.add_cooldown(provider, permit.key(), duration);
                                drop(permit);
                                tokio::time::sleep(duration).await;
                                continue;
                            }
                            RetryDecision::CooldownProvider { duration } => {
                                self.circuit_breakers.record_failure(provider);
                                self.cooldown.add_provider_cooldown(provider, duration);
                                drop(permit);
                                tokio::time::sleep(duration).await;
                                continue;
                            }
                            RetryDecision::OpenCircuit => {
                                self.circuit_breakers.record_failure(provider);
                                return Err(RotatorError::CircuitOpen(provider.to_string()));
                            }
                            RetryDecision::GiveUp => {
                                self.circuit_breakers.record_failure(provider);
                                return Err(RotatorError::CircuitOpen(provider.to_string()));
                            }
                            RetryDecision::Abort => {
                                self.circuit_breakers.record_failure(provider);
                                self.cooldown.add_cooldown(
                                    provider,
                                    permit.key(),
                                    Duration::from_secs(5),
                                );
                                let mut builder =
                                    http::Response::builder().status(status).version(version);
                                *builder.headers_mut().expect("response builder is valid") =
                                    headers;
                                return Ok(builder
                                    .body(bytes::Bytes::from(body_text))
                                    .expect("response body rebuild should not fail")
                                    .into());
                            }
                        }
                    }
                    Ok(resp)
                        if matches!(resp.status().as_u16(), 401 | 403) && self.max_retries > 0 =>
                    {
                        // 401/403: current credential is invalid/forbidden. Rotate to
                        // another key (parity with Python authentication/forbidden rotation)
                        // and cool down the offending key. On exhaustion, return the original
                        // upstream response to the client.
                        let status = resp.status();
                        let version = resp.version();
                        let headers = resp.headers().clone();
                        let body_text = resp.text().await.unwrap_or_default();
                        let failure = classify_upstream_failure(status, &headers, Some(&body_text));
                        let key_prefix =
                            crate::transaction_log::credential_hash_prefix(permit.key());
                        if let Some(ref ej) = self.error_journal {
                            ej.record_error(
                                provider,
                                ErrorClass::Auth,
                                Some(status.as_u16()),
                                &body_text,
                            );
                        }
                        let decision = decide_retry_for_provider(
                            failure.clone(),
                            attempt as u32,
                            self.max_retries as u32,
                            Some(provider),
                            self.error_journal.as_deref(),
                        );
                        warn!(
                            provider,
                            attempt,
                            status = %status,
                            key = %key_prefix,
                            ?failure,
                            ?decision,
                            "key-specific upstream error, rotating key..."
                        );
                        match decision {
                            RetryDecision::RotateKey => {
                                self.cooldown.add_cooldown(
                                    provider,
                                    permit.key(),
                                    Duration::from_secs(5),
                                );
                                drop(permit);
                                tokio::time::sleep(get_retry_backoff(attempt as u32, 300, 60_000))
                                    .await;
                                continue;
                            }
                            RetryDecision::GiveUp => {
                                self.circuit_breakers.record_failure(provider);
                                return Err(RotatorError::CircuitOpen(provider.to_string()));
                            }
                            _ => {
                                let mut builder =
                                    http::Response::builder().status(status).version(version);
                                *builder.headers_mut().expect("response builder is valid") =
                                    headers;
                                return Ok(builder
                                    .body(bytes::Bytes::from(body_text))
                                    .expect("response body rebuild should not fail")
                                    .into());
                            }
                        }
                    }
                    Ok(resp) if matches!(resp.status().as_u16(), 412 | 422 | 451) => {
                        // 412/422/451: provider rejects the *request* (model access, billing
                        // state, region). These are NOT credential-specific in practice — every
                        // key gets the same 412 for the same request. Rotating through the whole
                        // pool and cooling every key down (then tripping the provider circuit)
                        // turned a single bad model into a 502 storm. Instead: try at most 3
                        // distinct keys without cooldown/journal/breaker, then surface the
                        // upstream response to the client as-is so the caller sees the real error.
                        let status = resp.status();
                        let version = resp.version();
                        let headers = resp.headers().clone();
                        let body_text = resp.text().await.unwrap_or_default();
                        let key_prefix =
                            crate::transaction_log::credential_hash_prefix(permit.key());
                        warn!(
                            provider,
                            attempt,
                            status = %status,
                            key = %key_prefix,
                            tried = tried_412_keys.len(),
                            "request-level upstream error (412/422/451)"
                        );
                        tried_412_keys.push(permit.key().to_string());
                        if attempt < self.max_retries
                            && tried_412_keys.len() < 3
                            && tried_412_keys.len() < key_count
                        {
                            // Retain this response so a later acquire that finds no fresh key
                            // (e.g. some keys on cooldown) surfaces the real upstream error via
                            // pending_412 instead of degrading to an opaque AllKeysOnCooldown.
                            pending_412 = Some((status, version, headers, body_text));
                            drop(permit);
                            tokio::time::sleep(get_retry_backoff(attempt as u32, 200, 60_000))
                                .await;
                            continue;
                        }
                        let mut builder = http::Response::builder().status(status).version(version);
                        *builder.headers_mut().expect("response builder is valid") = headers;
                        return Ok(builder
                            .body(bytes::Bytes::from(body_text))
                            .expect("response body rebuild should not fail")
                            .into());
                    }
                    Ok(resp) => return Ok(resp),
                    Err(e) => {
                        // Connection/timeout error: rotate to another key without
                        // cooldown/journal/breaker — a network blip is not credential-
                        // specific and must not burn the pool or open the circuit.
                        let sanitized = e.without_url();
                        warn!(
                            provider,
                            attempt,
                            error = %sanitized,
                            key = %crate::transaction_log::credential_hash_prefix(permit.key()),
                            "connection error, rotating key"
                        );
                        if attempt < self.max_retries {
                            last_key = Some(permit.key().to_string());
                            drop(permit);
                            tokio::time::sleep(get_retry_backoff(attempt as u32, 300, 60_000))
                                .await;
                            continue;
                        }
                        // Retry budget exhausted on connection errors. A network blip
                        // is transient — wait out the nearest cooldown window (keys may
                        // be on per-key cooldown from earlier 429s) and restart the
                        // rotation round instead of failing fast into a 502.
                        if cooldown_waits < MAX_COOLDOWN_WAITS
                            && let Some(wait) = self.cooldown.next_expiry(provider)
                        {
                            let wait = wait.min(Duration::from_secs(120));
                            cooldown_waits += 1;
                            warn!(
                                provider,
                                attempt,
                                wait_ms = wait.as_millis() as u64,
                                cooldown_waits,
                                "connection-error budget exhausted, waiting out cooldown window and retrying"
                            );
                            drop(permit);
                            tokio::time::sleep(wait + Duration::from_millis(50)).await;
                            restart_rotation = true;
                            break;
                        }
                        return Err(RotatorError::Http(sanitized.to_string()));
                    }
                }
            }
            // The `for` rotation round ended. A terminal 429 asked to restart the whole
            // round after waiting out the cooldown window — honor that before giving up.
            if restart_rotation {
                restart_rotation = false;
                last_key = None;
                continue;
            }
            // Loop exhausted without a definitive return. If a request-level rejection
            // (412/422/451) was retained earlier, surface it instead of a generic Exhausted.
            if let Some((status, version, headers, body_text)) = pending_412.take() {
                let mut builder = http::Response::builder().status(status).version(version);
                *builder.headers_mut().expect("response builder is valid") = headers;
                return Ok(builder
                    .body(bytes::Bytes::from(body_text))
                    .expect("response body rebuild should not fail")
                    .into());
            }
            if let Some(ref ej) = self.error_journal {
                ej.record_error(
                    provider,
                    ErrorClass::Unknown,
                    None,
                    format!("exhausted after {} retries", self.max_retries),
                );
            }
            return Err(RotatorError::Exhausted(self.max_retries));
        }
    }

    pub async fn request_raw(
        &self,
        provider: &str,
        path: &str,
        body: bytes::Bytes,
        content_type: &str,
    ) -> Result<reqwest::Response> {
        let key_count = self.credentials.key_count(provider);
        let mut last_key: Option<String> = None;
        let mut tried_412_keys: Vec<String> = Vec::new();
        // Retained 412/422/451 response so we can surface it to the client even if a
        // later acquire finds no fresh key (e.g. some keys on cooldown) — otherwise a
        // request-level rejection degrades to an opaque AllKeysOnCooldown error.
        let mut pending_412 = None;
        // Number of times this request waited out a full-cooldown window. Bounded so a
        // permanently throttled provider cannot park the task forever.
        let mut cooldown_waits = 0u32;
        // Set by the terminal-429 branch to restart the whole rotation round after
        // waiting out the cooldown window (see request()).
        let mut restart_rotation = false;

        // Outer loop: normally runs once; restarted by a terminal 429 (see request()).
        loop {
            for attempt in 0..=self.max_retries {
                if !self.circuit_breakers.is_allowed(provider) {
                    // Surface a retained request-level rejection (412/422/451) even if a
                    // concurrent failure tripped the provider circuit mid-rotation, instead
                    // of hiding it behind an opaque CircuitOpen.
                    if let Some((status, version, headers, body_text)) = pending_412.take() {
                        let mut builder = http::Response::builder().status(status).version(version);
                        *builder.headers_mut().expect("response builder is valid") = headers;
                        return Ok(builder
                            .body(bytes::Bytes::from(body_text))
                            .expect("response body rebuild should not fail")
                            .into());
                    }
                    return Err(RotatorError::CircuitOpen(provider.to_string()));
                }

                // Acquire a fresh key for this attempt. When every key is on a short
                // per-key cooldown (e.g. all zai keys throttled with 429), wait for the
                // nearest expiry and re-acquire *within the same attempt* — waiting out a
                // cooldown window is not a real retry and must not consume `attempt`.
                let cred = loop {
                    let cred = self.credentials.acquire_where(provider, |key| {
                        self.cooldown.is_available(provider, key)
                            && (key_count <= 1 || Some(key) != last_key.as_deref())
                            && !tried_412_keys.iter().any(|k| k.as_str() == key)
                    });
                    let none_reason = match cred {
                        Some(cred) => break cred,
                        None => {
                            // Mid-rotation through a request-level 412/422/451 with no fresh key
                            // available (e.g. some keys on cooldown): surface the retained upstream
                            // response instead of an opaque AllKeysOnCooldown error.
                            if let Some((status, version, headers, body_text)) = pending_412.take()
                            {
                                let mut builder =
                                    http::Response::builder().status(status).version(version);
                                *builder.headers_mut().expect("response builder is valid") =
                                    headers;
                                return Ok(builder
                                    .body(bytes::Bytes::from(body_text))
                                    .expect("response body rebuild should not fail")
                                    .into());
                            }
                            let has_any_keys = self.credentials.has_any_keys(provider);
                            if !has_any_keys {
                                return Err(RotatorError::NoCredentials(provider.to_string()));
                            }
                            let key_status = self.credentials.get_key_status(provider);
                            let status_str = key_status
                                .iter()
                                .map(|(key, current, limit)| {
                                    format!("{}: {}/{}", key, current, limit)
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            let any_busy = key_status
                                .iter()
                                .any(|(_, current, limit)| *current >= *limit);
                            if any_busy {
                                return Err(RotatorError::AllKeysBusy(
                                    provider.to_string(),
                                    status_str,
                                ));
                            }
                            status_str
                        }
                    };
                    // Every key is on a short per-key cooldown. Wait for the nearest expiry
                    // and retry instead of failing fast with a 502 — the client timeout is
                    // far longer than the cooldown window. Bounded per request.
                    let waited = if cooldown_waits < MAX_COOLDOWN_WAITS {
                        self.cooldown.next_expiry(provider).map(|wait| {
                            let wait = wait.min(Duration::from_secs(120));
                            cooldown_waits += 1;
                            warn!(
                                provider,
                                attempt,
                                wait_ms = wait.as_millis() as u64,
                                cooldown_waits,
                                "all keys on cooldown, waiting for nearest expiry"
                            );
                            wait
                        })
                    } else {
                        None
                    };
                    match waited {
                        Some(wait) => {
                            tokio::time::sleep(wait + Duration::from_millis(50)).await;
                            continue;
                        }
                        None => {
                            return Err(RotatorError::AllKeysOnCooldown(
                                provider.to_string(),
                                none_reason,
                            ));
                        }
                    }
                };
                // We're about to try a fresh key, so a 412/422/451 retained from an earlier
                // attempt is no longer the current signal — drop it. A subsequent 429/5xx/
                // connection error must not be masked by a stale request-level rejection.
                pending_412 = None;
                // zai quota preflight (blocking): probe the monitor endpoint before
                // dispatching the client request. An exhausted key is cooled down and
                // skipped so rotation advances to the next available key rather than
                // surfacing an upstream 429. Cached per key to bound probe latency.
                if !self.ensure_zai_key_available(provider, &cred.key).await {
                    drop(cred);
                    continue;
                }
                let permit =
                    CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

                if !self.rate_limiter.acquire(provider, permit.key()) {
                    drop(permit);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }

                let client = self.http_pool.get_or_create(provider);
                let url = format!(
                    "{}/{}",
                    self.resolve_base_url(provider),
                    path.trim_start_matches('/')
                );
                let token = self.resolve_auth_token(provider, permit.key()).await?;
                let started_at = Instant::now();
                let result = self
                    .dispatch_with_stale_retry(
                        provider,
                        &client,
                        &url,
                        &token,
                        body.clone(),
                        content_type,
                    )
                    .await;
                self.last_latency_ms
                    .insert(provider.to_owned(), started_at.elapsed().as_millis() as u64);

                match result {
                    Ok(resp) if resp.status().is_success() => {
                        self.circuit_breakers.record_success(provider);
                        if let Some(ref arl) = self.adaptive_rate_limiter {
                            arl.record_success(provider, permit.key());
                        }
                        return self
                            .record_usage_from_response(provider, permit.key(), resp)
                            .await;
                    }
                    Ok(resp) if resp.status().as_u16() == 429 => {
                        // 429: rotate to another key. The throttled key gets a short per-key
                        // cooldown (see the request() branch for the rationale) so rotation
                        // cannot ping-pong back onto the same exhausted credential.
                        let headers = resp.headers().clone();
                        let body_text = resp.text().await.unwrap_or_default();
                        if let Some(ref arl) = self.adaptive_rate_limiter {
                            arl.record_429(provider, permit.key());
                        }
                        let retry_after =
                            retry_after_from_headers_and_body(&headers, Some(&body_text));
                        self.zai_quota_cache.invalidate(permit.key());
                        // zai rate-limit vs quota cooldown (see request() branch).
                        let key_cooldown = if provider == "zai" && retry_after.is_none() {
                            ZAI_RATE_LIMIT_COOLDOWN
                        } else {
                            match self.zai_exact_cooldown_on_429(provider, permit.key()).await {
                                Some(exact) => exact,
                                None => retry_after
                                    .unwrap_or(Duration::from_secs(30))
                                    .clamp(Duration::from_secs(5), Duration::from_secs(300)),
                            }
                        };
                        self.cooldown
                            .add_cooldown(provider, permit.key(), key_cooldown);
                        warn!(
                            provider,
                            attempt,
                            key = %crate::transaction_log::credential_hash_prefix(permit.key()),
                            ?retry_after,
                            ?key_cooldown,
                            "429 throttled, key cooled down, rotating"
                        );
                        if attempt < self.max_retries {
                            last_key = Some(permit.key().to_string());
                            drop(permit);
                            // Rotate on a short backoff, not the full Retry-After.
                            tokio::time::sleep(get_retry_backoff(attempt as u32, 500, 60_000))
                                .await;
                            continue;
                        }
                        // Retry budget exhausted on 429s: wait out the nearest cooldown
                        // window and restart the rotation round (see request()).
                        if cooldown_waits < MAX_COOLDOWN_WAITS
                            && let Some(wait) = self.cooldown.next_expiry(provider)
                        {
                            let wait = wait.min(Duration::from_secs(120));
                            cooldown_waits += 1;
                            warn!(
                                provider,
                                attempt,
                                wait_ms = wait.as_millis() as u64,
                                cooldown_waits,
                                "429 budget exhausted, waiting out cooldown window and retrying"
                            );
                            drop(permit);
                            tokio::time::sleep(wait + Duration::from_millis(50)).await;
                            restart_rotation = true;
                            break;
                        }
                        return Err(RotatorError::RateLimited(
                            provider.to_string(),
                            retry_after.map(|duration| duration.as_secs()),
                        ));
                    }
                    Ok(resp) if resp.status().is_server_error() => {
                        let status = resp.status();
                        let version = resp.version();
                        let headers = resp.headers().clone();
                        let body_text = resp.text().await.unwrap_or_default();
                        let failure = classify_upstream_failure(status, &headers, Some(&body_text));
                        if let Some(ref ej) = self.error_journal {
                            ej.record_error(
                                provider,
                                ErrorClass::ServerError,
                                Some(status.as_u16()),
                                &body_text,
                            );
                        }
                        let decision = decide_retry_for_provider(
                            failure.clone(),
                            attempt as u32,
                            self.max_retries as u32,
                            Some(provider),
                            self.error_journal.as_deref(),
                        );
                        error!(provider, attempt, status = %status, ?failure, ?decision, "server error, retrying...");
                        match decision {
                            RetryDecision::RetrySameKey => {
                                drop(permit);
                                tokio::time::sleep(get_retry_backoff(attempt as u32, 300, 60_000))
                                    .await;
                                continue;
                            }
                            RetryDecision::RotateKey => {
                                self.cooldown.add_cooldown(
                                    provider,
                                    permit.key(),
                                    Duration::from_secs(5),
                                );
                                drop(permit);
                                tokio::time::sleep(get_retry_backoff(attempt as u32, 300, 60_000))
                                    .await;
                                continue;
                            }
                            RetryDecision::CooldownKey { duration } => {
                                self.circuit_breakers.record_failure(provider);
                                self.cooldown.add_cooldown(provider, permit.key(), duration);
                                drop(permit);
                                tokio::time::sleep(duration).await;
                                continue;
                            }
                            RetryDecision::CooldownProvider { duration } => {
                                self.circuit_breakers.record_failure(provider);
                                self.cooldown.add_provider_cooldown(provider, duration);
                                drop(permit);
                                tokio::time::sleep(duration).await;
                                continue;
                            }
                            RetryDecision::OpenCircuit => {
                                self.circuit_breakers.record_failure(provider);
                                return Err(RotatorError::CircuitOpen(provider.to_string()));
                            }
                            RetryDecision::GiveUp => {
                                self.circuit_breakers.record_failure(provider);
                                return Err(RotatorError::CircuitOpen(provider.to_string()));
                            }
                            RetryDecision::Abort => {
                                self.circuit_breakers.record_failure(provider);
                                self.cooldown.add_cooldown(
                                    provider,
                                    permit.key(),
                                    Duration::from_secs(5),
                                );
                                let mut builder =
                                    http::Response::builder().status(status).version(version);
                                *builder.headers_mut().expect("response builder is valid") =
                                    headers;
                                return Ok(builder
                                    .body(bytes::Bytes::from(body_text))
                                    .expect("response body rebuild should not fail")
                                    .into());
                            }
                        }
                    }
                    Ok(resp)
                        if matches!(resp.status().as_u16(), 401 | 403) && self.max_retries > 0 =>
                    {
                        // 401/403: current credential is invalid/forbidden. Rotate to another
                        // key and cool down the offending key; on exhaustion return upstream resp.
                        let status = resp.status();
                        let version = resp.version();
                        let headers = resp.headers().clone();
                        let body_text = resp.text().await.unwrap_or_default();
                        let failure = classify_upstream_failure(status, &headers, Some(&body_text));
                        let key_prefix =
                            crate::transaction_log::credential_hash_prefix(permit.key());
                        if let Some(ref ej) = self.error_journal {
                            ej.record_error(
                                provider,
                                ErrorClass::Auth,
                                Some(status.as_u16()),
                                &body_text,
                            );
                        }
                        let decision = decide_retry_for_provider(
                            failure.clone(),
                            attempt as u32,
                            self.max_retries as u32,
                            Some(provider),
                            self.error_journal.as_deref(),
                        );
                        warn!(
                            provider,
                            attempt,
                            status = %status,
                            key = %key_prefix,
                            ?failure,
                            ?decision,
                            "key-specific upstream error, rotating key..."
                        );
                        match decision {
                            RetryDecision::RotateKey => {
                                self.cooldown.add_cooldown(
                                    provider,
                                    permit.key(),
                                    Duration::from_secs(5),
                                );
                                drop(permit);
                                tokio::time::sleep(get_retry_backoff(attempt as u32, 300, 60_000))
                                    .await;
                                continue;
                            }
                            RetryDecision::GiveUp => {
                                self.circuit_breakers.record_failure(provider);
                                return Err(RotatorError::CircuitOpen(provider.to_string()));
                            }
                            _ => {
                                let mut builder =
                                    http::Response::builder().status(status).version(version);
                                *builder.headers_mut().expect("response builder is valid") =
                                    headers;
                                return Ok(builder
                                    .body(bytes::Bytes::from(body_text))
                                    .expect("response body rebuild should not fail")
                                    .into());
                            }
                        }
                    }
                    Ok(resp) if matches!(resp.status().as_u16(), 412 | 422 | 451) => {
                        // 412/422/451: request-level rejection, not credential-specific.
                        // Try at most 3 distinct keys without cooldown/journal/breaker,
                        // then surface the upstream response to the client.
                        let status = resp.status();
                        let version = resp.version();
                        let headers = resp.headers().clone();
                        let body_text = resp.text().await.unwrap_or_default();
                        let key_prefix =
                            crate::transaction_log::credential_hash_prefix(permit.key());
                        warn!(
                            provider,
                            attempt,
                            status = %status,
                            key = %key_prefix,
                            tried = tried_412_keys.len(),
                            "request-level upstream error (412/422/451)"
                        );
                        tried_412_keys.push(permit.key().to_string());
                        if attempt < self.max_retries
                            && tried_412_keys.len() < 3
                            && tried_412_keys.len() < key_count
                        {
                            // Retain this response so a later acquire that finds no fresh key
                            // (e.g. some keys on cooldown) surfaces the real upstream error via
                            // pending_412 instead of degrading to an opaque AllKeysOnCooldown.
                            pending_412 = Some((status, version, headers, body_text));
                            drop(permit);
                            tokio::time::sleep(get_retry_backoff(attempt as u32, 200, 60_000))
                                .await;
                            continue;
                        }
                        let mut builder = http::Response::builder().status(status).version(version);
                        *builder.headers_mut().expect("response builder is valid") = headers;
                        return Ok(builder
                            .body(bytes::Bytes::from(body_text))
                            .expect("response body rebuild should not fail")
                            .into());
                    }
                    Ok(resp) => return Ok(resp),
                    Err(e) => {
                        // Connection/timeout error: rotate to another key without
                        // cooldown/journal/breaker — a network blip is not credential-
                        // specific and must not burn the pool or open the circuit.
                        let sanitized = e.without_url();
                        warn!(
                            provider,
                            attempt,
                            error = %sanitized,
                            key = %crate::transaction_log::credential_hash_prefix(permit.key()),
                            "connection error, rotating key"
                        );
                        if attempt < self.max_retries {
                            last_key = Some(permit.key().to_string());
                            drop(permit);
                            tokio::time::sleep(get_retry_backoff(attempt as u32, 300, 60_000))
                                .await;
                            continue;
                        }
                        // Retry budget exhausted on connection errors (see request()).
                        if cooldown_waits < MAX_COOLDOWN_WAITS
                            && let Some(wait) = self.cooldown.next_expiry(provider)
                        {
                            let wait = wait.min(Duration::from_secs(120));
                            cooldown_waits += 1;
                            warn!(
                                provider,
                                attempt,
                                wait_ms = wait.as_millis() as u64,
                                cooldown_waits,
                                "connection-error budget exhausted, waiting out cooldown window and retrying"
                            );
                            drop(permit);
                            tokio::time::sleep(wait + Duration::from_millis(50)).await;
                            restart_rotation = true;
                            break;
                        }
                        return Err(RotatorError::Http(sanitized.to_string()));
                    }
                }
            }
            // The `for` rotation round ended. A terminal 429 asked to restart the whole
            // round after waiting out the cooldown window — honor that before giving up.
            if restart_rotation {
                restart_rotation = false;
                last_key = None;
                continue;
            }
            // Loop exhausted without a definitive return. If a request-level rejection
            // (412/422/451) was retained earlier, surface it instead of a generic Exhausted.
            if let Some((status, version, headers, body_text)) = pending_412.take() {
                let mut builder = http::Response::builder().status(status).version(version);
                *builder.headers_mut().expect("response builder is valid") = headers;
                return Ok(builder
                    .body(bytes::Bytes::from(body_text))
                    .expect("response body rebuild should not fail")
                    .into());
            }
            if let Some(ref ej) = self.error_journal {
                ej.record_error(
                    provider,
                    ErrorClass::Unknown,
                    None,
                    format!("exhausted after {} retries", self.max_retries),
                );
            }
            return Err(RotatorError::Exhausted(self.max_retries));
        }
    }

    pub async fn provider_method_call(
        &self,
        provider: &str,
        method: &str,
        body: serde_json::Value,
    ) -> Result<reqwest::Response> {
        self.request(provider, method, body).await
    }

    pub fn circuit_state(&self, provider: &str) -> CircuitState {
        self.circuit_breakers.get_state(provider)
    }

    pub fn usage_entries(&self) -> Vec<crate::UsageEntry> {
        self.usage_manager
            .as_ref()
            .map(|usage_manager| usage_manager.get_all_usage())
            .unwrap_or_default()
    }

    pub fn active_cooldowns(&self) -> Vec<(String, String, Duration)> {
        self.cooldown.get_active_cooldowns()
    }

    pub fn last_latency_ms(&self, provider: &str) -> Option<u64> {
        self.last_latency_ms.get(provider).map(|entry| *entry)
    }

    pub async fn list_models(&self, provider: &str) -> Result<reqwest::Response> {
        self.get(provider, "models").await
    }

    pub async fn get(&self, provider: &str, path: &str) -> Result<reqwest::Response> {
        let cred = self.credentials.acquire_least_loaded(provider);
        let cred = match cred {
            Some(cred) => cred,
            None => {
                let has_any_keys = self.credentials.has_any_keys(provider);
                if !has_any_keys {
                    return Err(RotatorError::NoCredentials(provider.to_string()));
                }
                let key_status = self.credentials.get_key_status(provider);
                let status_str = key_status
                    .iter()
                    .map(|(key, current, limit)| format!("{}: {}/{}", key, current, limit))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(RotatorError::AllKeysBusy(provider.to_string(), status_str));
            }
        };
        let permit = CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let token = self.resolve_auth_token(provider, permit.key()).await?;
        let request = self.apply_auth_headers(provider, client.get(&url), &token);
        let result = request.send().await;

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub async fn delete(&self, provider: &str, path: &str) -> Result<reqwest::Response> {
        let cred = self.credentials.acquire_least_loaded(provider);
        let cred = match cred {
            Some(cred) => cred,
            None => {
                let has_any_keys = self.credentials.has_any_keys(provider);
                if !has_any_keys {
                    return Err(RotatorError::NoCredentials(provider.to_string()));
                }
                let key_status = self.credentials.get_key_status(provider);
                let status_str = key_status
                    .iter()
                    .map(|(key, current, limit)| format!("{}: {}/{}", key, current, limit))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(RotatorError::AllKeysBusy(provider.to_string(), status_str));
            }
        };
        let permit = CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let token = self.resolve_auth_token(provider, permit.key()).await?;
        let request = self.apply_auth_headers(provider, client.delete(&url), &token);
        let result = request.send().await;

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub async fn get_with_query(
        &self,
        provider: &str,
        path: &str,
        query: &[(String, String)],
    ) -> Result<reqwest::Response> {
        let cred = self.credentials.acquire_least_loaded(provider);
        let cred = match cred {
            Some(cred) => cred,
            None => {
                let has_any_keys = self.credentials.has_any_keys(provider);
                if !has_any_keys {
                    return Err(RotatorError::NoCredentials(provider.to_string()));
                }
                let key_status = self.credentials.get_key_status(provider);
                let status_str = key_status
                    .iter()
                    .map(|(key, current, limit)| format!("{}: {}/{}", key, current, limit))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(RotatorError::AllKeysBusy(provider.to_string(), status_str));
            }
        };
        let permit = CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let token = self.resolve_auth_token(provider, permit.key()).await?;
        let request = self.apply_auth_headers(provider, client.get(&url), &token);
        let result = request.query(query).send().await;

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub async fn delete_with_query(
        &self,
        provider: &str,
        path: &str,
        query: &[(String, String)],
    ) -> Result<reqwest::Response> {
        let cred = self.credentials.acquire_least_loaded(provider);
        let cred = match cred {
            Some(cred) => cred,
            None => {
                let has_any_keys = self.credentials.has_any_keys(provider);
                if !has_any_keys {
                    return Err(RotatorError::NoCredentials(provider.to_string()));
                }
                let key_status = self.credentials.get_key_status(provider);
                let status_str = key_status
                    .iter()
                    .map(|(key, current, limit)| format!("{}: {}/{}", key, current, limit))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(RotatorError::AllKeysBusy(provider.to_string(), status_str));
            }
        };
        let permit = CredentialPermit::new(self.credentials.clone(), provider, cred.key.clone());

        let client = self.http_pool.get_or_create(provider);
        let url = format!(
            "{}/{}",
            self.resolve_base_url(provider),
            path.trim_start_matches('/')
        );
        let token = self.resolve_auth_token(provider, permit.key()).await?;
        let request = self.apply_auth_headers(provider, client.delete(&url), &token);
        let result = request.query(query).send().await;

        result.map_err(|e| RotatorError::Http(e.to_string()))
    }

    pub fn transform_request(provider: &str, path: &str, body: &mut serde_json::Value) {
        let model = body
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let endpoint = path.trim_start_matches('/');
        if !(provider == "openai" && endpoint == "responses") {
            sanitize_request(
                &SanitizerContext {
                    provider_id: provider.to_owned(),
                    model,
                    endpoint: endpoint.to_owned(),
                },
                body,
            );
        }
        if provider == "openai" && endpoint == "responses" && body.get("messages").is_none() {
            return;
        }
        transform_request_for_provider(provider, body);
    }

    async fn record_usage_from_response(
        &self,
        provider: &str,
        key: &str,
        resp: reqwest::Response,
    ) -> Result<reqwest::Response> {
        let Some(usage_manager) = &self.usage_manager else {
            return Ok(resp);
        };

        let status = resp.status();
        let version = resp.version();
        let headers = resp.headers().clone();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| RotatorError::Http(e.to_string()))?;

        let usage_json = serde_json::from_slice::<serde_json::Value>(&bytes).ok();

        if let Some(json) = usage_json {
            let (prompt, completion) = extract_usage(&json);
            if let (Ok(prompt), Ok(completion)) = (u32::try_from(prompt), u32::try_from(completion))
                && (prompt > 0 || completion > 0)
            {
                usage_manager.record_usage(provider, key, prompt, completion);
            }
        }

        let mut builder = http::Response::builder().status(status).version(version);
        *builder.headers_mut().expect("response builder is valid") = headers;
        Ok(builder
            .body(bytes)
            .expect("response body rebuild should not fail")
            .into())
    }

    async fn resolve_auth_token(&self, provider: &str, key: &str) -> Result<String> {
        let definition = self.get_cached_provider(provider);
        if definition.auth_type != AuthType::OAuth {
            return Ok(key.to_owned());
        }

        let oauth_token: OAuthToken = match serde_json::from_str(key) {
            Ok(token) => token,
            Err(_) => {
                return Err(RotatorError::Other(format!(
                    "invalid OAuth credential JSON for provider {provider}"
                )));
            }
        };

        let cache_key = oauth_cache_key(provider, key);

        let is_expired_with_skew = |token: &OAuthToken| {
            token.expires_at.is_some_and(|expires_at| {
                let now = chrono::Utc::now().timestamp() as u64;
                let skew = 60; // refresh 60 s before hard expiry
                expires_at.saturating_sub(skew) <= now
            })
        };

        if let Some(cached) = self.oauth_manager.get_token(&cache_key)
            && !is_expired_with_skew(&cached)
        {
            return Ok(cached.access_token);
        }

        let raw_expired = is_expired_with_skew(&oauth_token);
        let cached = self.oauth_manager.get_token(&cache_key);

        if raw_expired || cached.is_some() {
            let token_endpoint = definition.token_endpoint.as_ref().ok_or_else(|| {
                RotatorError::Other(format!("Missing token_endpoint for provider {provider}"))
            })?;
            let client_id = definition.client_id.as_ref().ok_or_else(|| {
                RotatorError::Other(format!("Missing client_id for provider {provider}"))
            })?;
            let client_secret = definition.client_secret.as_deref();

            let lock = {
                let entry = self.oauth_refresh_locks.entry(cache_key.clone());
                let guard = entry.or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())));
                guard.clone()
            };
            let _guard = lock.lock().await;

            if let Some(cached) = self.oauth_manager.get_token(&cache_key)
                && !is_expired_with_skew(&cached)
            {
                return Ok(cached.access_token);
            }

            let client = self.http_pool.get_or_create(provider);
            let refreshed = refresh_oauth_token(
                &client,
                token_endpoint,
                client_id,
                client_secret,
                &oauth_token,
            )
            .await?;
            let access_token = refreshed.access_token.clone();
            self.oauth_manager.set_token(&cache_key, refreshed);
            return Ok(access_token);
        }

        self.oauth_manager
            .set_token(&cache_key, oauth_token.clone());
        Ok(oauth_token.access_token)
    }

    fn apply_auth_headers(
        &self,
        provider: &str,
        mut request: reqwest::RequestBuilder,
        token: &str,
    ) -> reqwest::RequestBuilder {
        let definition = self.get_cached_provider(provider);
        if provider == "gemini" {
            request = request.header("x-goog-api-key", token);
        }
        for (header_key, value) in &definition.default_headers {
            request = request.header(header_key, value);
        }
        match definition.auth_type {
            AuthType::ApiKey => {
                if provider != "gemini" {
                    request = request.header("x-api-key", token);
                }
            }
            AuthType::Bearer | AuthType::OAuth => {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
        }
        request
    }

    fn resolve_base_url(&self, provider: &str) -> String {
        let definition = self.get_cached_provider(provider);
        let base_url = definition.base_url.as_str();
        if base_url.is_empty() {
            format!("https://api.{provider}.com/v1")
        } else {
            base_url.to_owned()
        }
    }

    fn get_cached_provider(&self, provider: &str) -> Arc<ProviderDefinition> {
        if let Some(cached) = self.provider_cache.get(provider) {
            return Arc::clone(cached.value());
        }
        let def = self
            .provider_registry
            .get(provider)
            .unwrap_or_else(|| ProviderDefinition {
                id: provider.to_owned(),
                display_name: provider.to_owned(),
                base_url: format!("https://api.{provider}.com/v1"),
                auth_type: AuthType::Bearer,
                model_patterns: Vec::new(),
                compiled_patterns: Vec::new(),
                endpoints: Vec::new(),
                features: Vec::new(),
                model_count: 0,
                timeout_secs: 60,
                default_headers: std::collections::HashMap::new(),
                token_endpoint: None,
                client_id: None,
                client_secret: None,
            });
        let def = Arc::new(def);
        self.provider_cache
            .insert(provider.to_owned(), Arc::clone(&def));
        def
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit_breaker::{CircuitBreakerRegistry, CircuitState};
    use crate::cooldown::CooldownManager;
    use crate::provider_registry::ProviderDefinition;
    use crate::rate_limiter::RateLimiterRegistry;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn normalize_upstream_url_keeps_versioned_bases() {
        assert_eq!(
            normalize_upstream_url("https://api.openai.com/v1", "chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_upstream_url("https://api.fireworks.ai/inference/v1", "chat/completions"),
            "https://api.fireworks.ai/inference/v1/chat/completions"
        );
    }

    #[test]
    fn normalize_upstream_url_inserts_v1_for_unversioned_bases() {
        assert_eq!(
            normalize_upstream_url("https://api.deepseek.com", "chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_upstream_url("https://api.z.ai/api/coding/paas/v4", "chat/completions"),
            "https://api.z.ai/api/coding/paas/v4/chat/completions"
        );
    }

    #[test]
    fn normalize_upstream_url_preserves_provider_specific_actions() {
        assert_eq!(
            normalize_upstream_url(
                "https://generativelanguage.googleapis.com/v1beta/models",
                "gemini-2.5-flash:generateContent"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
    }

    async fn test_provider(statuses: Vec<u16>) -> (Arc<ProviderRegistry>, Arc<AtomicUsize>) {
        test_provider_with_body(statuses, "{}".to_string()).await
    }

    async fn test_provider_with_body(
        statuses: Vec<u16>,
        body: String,
    ) -> (Arc<ProviderRegistry>, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = calls.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let request_number = server_calls.fetch_add(1, Ordering::SeqCst);
                let status = statuses
                    .get(request_number)
                    .copied()
                    .unwrap_or_else(|| *statuses.last().unwrap());
                let mut buffer = [0; 1024];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 {status} OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let registry = Arc::new(ProviderRegistry::default());
        registry.register(ProviderDefinition {
            id: "test".to_string(),
            display_name: "test".to_string(),
            base_url: format!("http://{addr}/v1"),
            auth_type: AuthType::ApiKey,
            model_patterns: Vec::new(),
            compiled_patterns: Vec::new(),
            endpoints: vec!["/chat/completions".to_string()],
            features: vec!["chat".to_string()],
            model_count: 1,
            timeout_secs: 60,
            default_headers: HashMap::new(),
            token_endpoint: None,
            client_id: None,
            client_secret: None,
        });

        (registry, calls)
    }

    async fn captured_path_provider(
        provider: &str,
    ) -> (
        Arc<ProviderRegistry>,
        Arc<Mutex<String>>,
        Arc<Mutex<Option<String>>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured_path = Arc::new(Mutex::new(String::new()));
        let captured_header = Arc::new(Mutex::new(None));
        let server_path = captured_path.clone();
        let server_header = captured_header.clone();

        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = [0; 2048];
            let Ok(size) = socket.read(&mut buffer).await else {
                return;
            };
            let request = String::from_utf8_lossy(&buffer[..size]);
            if let Some(path) = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
            {
                *server_path.lock().unwrap() = path.to_owned();
            }
            for line in request.lines() {
                if let Some(value) = line.strip_prefix("x-goog-api-key:") {
                    *server_header.lock().unwrap() = Some(value.trim().to_owned());
                    break;
                }
            }
            let body = "{}";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let registry = Arc::new(ProviderRegistry::default());
        registry.register(ProviderDefinition {
            id: provider.to_owned(),
            display_name: provider.to_owned(),
            base_url: format!("http://{addr}/v1beta"),
            auth_type: AuthType::ApiKey,
            model_patterns: Vec::new(),
            compiled_patterns: Vec::new(),
            endpoints: vec!["/chat/completions".to_string()],
            features: vec!["chat".to_string()],
            model_count: 1,
            timeout_secs: 60,
            default_headers: HashMap::new(),
            token_endpoint: None,
            client_id: None,
            client_secret: None,
        });

        (registry, captured_path, captured_header)
    }

    #[tokio::test]
    async fn request_uses_gemini_native_chat_endpoint() {
        let (registry, captured_path, captured_header) = captured_path_provider("gemini").await;
        let credentials = CredentialManager::new();
        credentials.register_keys("gemini".to_string(), vec!["key-1".to_string()], 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let response = client
            .request(
                "gemini",
                "chat/completions",
                serde_json::json!({"model": "gemini-2.5-flash"}),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        // Gemini auth moved from `?key=` query param to `x-goog-api-key` header
        // (parity with apply_auth_headers); the URL path no longer carries the key.
        assert_eq!(
            captured_path.lock().unwrap().as_str(),
            "/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(
            captured_header.lock().unwrap().as_deref(),
            Some("key-1"),
            "gemini key must be sent via x-goog-api-key header"
        );
    }

    #[tokio::test]
    async fn request_recovers_from_dead_pooled_connection_via_stale_retry() {
        // Outer retries are disabled (max_retries = 0). The upstream drops the
        // first two connections without sending an HTTP response — the same
        // signature as a dead keep-alive connection pulled from the pool. Only
        // the inner same-key stale-connection retry can recover this; without
        // it the request would fail immediately (transport error, no outer
        // retry budget).
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepts = Arc::new(AtomicUsize::new(0));
        let server_accepts = accepts.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let n = server_accepts.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0; 4096];
                let _ = socket.read(&mut buffer).await;
                if n < 2 {
                    // Dead connection: close before returning any HTTP response.
                    continue;
                }
                let body = r#"{"choices":[]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let registry = Arc::new(ProviderRegistry::default());
        registry.register(ProviderDefinition {
            id: "test".to_string(),
            display_name: "test".to_string(),
            base_url: format!("http://{addr}/v1"),
            auth_type: AuthType::ApiKey,
            model_patterns: Vec::new(),
            compiled_patterns: Vec::new(),
            endpoints: vec!["/chat/completions".to_string()],
            features: vec!["chat".to_string()],
            model_count: 1,
            timeout_secs: 60,
            default_headers: HashMap::new(),
            token_endpoint: None,
            client_id: None,
            client_secret: None,
        });
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0, // max_retries = 0: only the inner stale retry can recover
        )
        .with_max_stale_retries(3);

        let response = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .expect("stale-connection retry should recover the request");
        assert_eq!(response.status(), 200);
        // Two dropped connections were transparently retried before success.
        assert!(
            accepts.load(Ordering::SeqCst) >= 3,
            "expected at least 3 upstream connections (2 stale + 1 success)"
        );
    }

    #[tokio::test]
    async fn request_records_server_failure_cooldown_and_opens_circuit() {
        let (registry, _) = test_provider(vec![500]).await;
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let rate_limiter = Arc::new(RateLimiterRegistry::new());
        let cooldown = Arc::new(CooldownManager::new());
        let circuit_breakers = Arc::new(CircuitBreakerRegistry::new());
        circuit_breakers.configure_provider("test", 1, 60, 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            rate_limiter,
            cooldown.clone(),
            circuit_breakers.clone(),
            None,
            0,
        );

        let first = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await;
        assert!(first.unwrap().status().is_server_error());
        assert_eq!(circuit_breakers.get_state("test"), CircuitState::Open);
        assert!(!cooldown.is_available("test", "key-1"));

        let second = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await;
        assert!(matches!(second, Err(RotatorError::CircuitOpen(provider)) if provider == "test"));
    }

    #[tokio::test]
    async fn request_records_openai_usage_after_success() {
        let usage_manager = Arc::new(crate::UsageManager::with_config(
            std::env::temp_dir().join("rotator-openai-usage-test.json"),
            Duration::from_secs(60),
            100,
        ));
        let (registry, _) = test_provider_with_body(
            vec![200],
            serde_json::json!({
                "id": "chatcmpl-test",
                "usage": {"prompt_tokens": 11, "completion_tokens": 7}
            })
            .to_string(),
        )
        .await;
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            Some(usage_manager.clone()),
            0,
        );

        let response = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .unwrap();
        let body: serde_json::Value = response.json().await.unwrap();

        assert_eq!(body["id"], "chatcmpl-test");
        assert_eq!(
            usage_manager
                .get_usage("test", "key-1")
                .unwrap()
                .prompt_tokens,
            11
        );
        assert_eq!(
            usage_manager
                .get_usage("test", "key-1")
                .unwrap()
                .completion_tokens,
            7
        );
        usage_manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn request_returns_streaming_response_without_buffering_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            let _ = socket.read(&mut buffer).await;
            let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket
                .write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
            socket
                .write_all(b"data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\ndata: [DONE]\n")
                .await
                .unwrap();
        });

        let registry = Arc::new(ProviderRegistry::default());
        registry.register(ProviderDefinition {
            id: "test".to_string(),
            display_name: "test".to_string(),
            base_url: format!("http://{addr}/v1"),
            auth_type: AuthType::ApiKey,
            model_patterns: Vec::new(),
            compiled_patterns: Vec::new(),
            endpoints: vec!["/chat/completions".to_string()],
            features: vec!["chat".to_string()],
            model_count: 1,
            timeout_secs: 60,
            default_headers: HashMap::new(),
            token_endpoint: None,
            client_id: None,
            client_secret: None,
        });
        let usage_manager = Arc::new(crate::UsageManager::with_config(
            std::env::temp_dir().join("rotator-openai-streaming-usage-test.json"),
            Duration::from_secs(60),
            100,
        ));
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            Some(usage_manager.clone()),
            0,
        );

        let response = tokio::time::timeout(
            Duration::from_millis(100),
            client.request(
                "test",
                "chat/completions",
                serde_json::json!({"stream": true}),
            ),
        )
        .await
        .expect("streaming response should return before upstream body completes")
        .unwrap();

        assert_eq!(response.status(), 200);
        assert!(usage_manager.get_usage("test", "key-1").is_none());
        usage_manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn request_records_anthropic_usage_after_success() {
        let usage_manager = Arc::new(crate::UsageManager::with_config(
            std::env::temp_dir().join("rotator-anthropic-usage-test.json"),
            Duration::from_secs(60),
            100,
        ));
        let (registry, _) = test_provider_with_body(
            vec![200],
            serde_json::json!({
                "id": "msg_test",
                "usage": {"input_tokens": 13, "output_tokens": 5}
            })
            .to_string(),
        )
        .await;
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            Some(usage_manager.clone()),
            0,
        );

        let response = client
            .request("test", "messages", serde_json::json!({}))
            .await
            .unwrap();
        let body: serde_json::Value = response.json().await.unwrap();

        assert_eq!(body["id"], "msg_test");
        let usage = usage_manager.get_usage("test", "key-1").unwrap();
        assert_eq!(usage.prompt_tokens, 13);
        assert_eq!(usage.completion_tokens, 5);
        usage_manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn request_waits_and_retries_when_rate_limiter_denies_key() {
        let (registry, calls) = test_provider(vec![200]).await;
        let credentials = CredentialManager::new();
        credentials.register_keys("test".to_string(), vec!["key-1".to_string()], 1);
        let rate_limiter = Arc::new(RateLimiterRegistry::new());
        rate_limiter.configure("test", "key-1", 0, 1);
        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            rate_limiter,
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .unwrap();
        let second = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await;

        assert!(matches!(second, Err(RotatorError::Exhausted(0))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn anthropic_transform_removes_unsupported_stream_options() {
        let mut body = serde_json::json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [{"role": "user", "content": "hello"}],
            "stream_options": {"include_usage": true}
        });

        RotatorClient::transform_request("anthropic", "messages", &mut body);

        assert!(body.get("stream_options").is_none());
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[test]
    fn gemini_transform_prefixes_model_name_once() {
        let mut body = serde_json::json!({"model": "gemini-2.5-flash"});
        RotatorClient::transform_request("gemini", "chat/completions", &mut body);
        assert_eq!(body["model"], "models/gemini-2.5-flash");

        RotatorClient::transform_request("gemini", "chat/completions", &mut body);
        assert_eq!(body["model"], "models/gemini-2.5-flash");
    }

    #[test]
    fn nvidia_transform_removes_unsupported_anthropic_fields() {
        let mut body = serde_json::json!({
            "model": "nvidia/llama-3.1",
            "thinking": {"type": "enabled"},
            "cache_control": {"type": "ephemeral"},
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "thinking",
                    "thinking": "reasoning",
                    "thinking_signature": "sig",
                    "cache_control": {"type": "ephemeral"}
                }]
            }]
        });

        RotatorClient::transform_request("nvidia", "chat/completions", &mut body);

        assert!(body.get("thinking").is_none());
        assert!(body.get("cache_control").is_none());
        assert!(body["messages"][0]["content"][0].get("thinking").is_none());
        assert!(
            body["messages"][0]["content"][0]
                .get("thinking_signature")
                .is_none()
        );
        assert!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
    }

    #[test]
    fn unknown_transform_leaves_body_unchanged() {
        let mut body = serde_json::json!({
            "model": "custom-model",
            "stream_options": {"include_usage": true},
            "thinking": {"type": "enabled"}
        });
        let original = body.clone();

        RotatorClient::transform_request("unknown", "chat/completions", &mut body);

        assert_eq!(body, original);
    }

    #[tokio::test]
    async fn oauth_token_is_parsed_and_used_as_bearer() {
        let (registry, calls) = test_provider(vec![200]).await;
        let mut registry_def = registry.get("test").unwrap();
        registry_def.auth_type = AuthType::OAuth;
        registry.register(registry_def);

        let credentials = CredentialManager::new();
        let oauth_json = serde_json::to_string(&crate::providers::oauth::OAuthToken {
            access_token: "oauth-access-123".to_owned(),
            refresh_token: None,
            expires_at: Some(u64::MAX),
            token_type: "Bearer".to_owned(),
        })
        .unwrap();
        credentials.register_keys("test".to_string(), vec![oauth_json], 1);

        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let response = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oauth_refresh_triggered_when_token_expired() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let refresh_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refresh_addr = refresh_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut socket, _) = refresh_listener.accept().await.unwrap();
            let mut buffer = [0; 2048];
            let size = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..size]);
            assert!(request.starts_with("POST /token HTTP/1.1"));
            assert!(request.contains("grant_type=refresh_token"));
            let body = r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"token_type":"Bearer"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let (registry, calls) = test_provider(vec![200]).await;
        let mut registry_def = registry.get("test").unwrap();
        registry_def.auth_type = AuthType::OAuth;
        registry_def.token_endpoint = Some(format!("http://{refresh_addr}/token"));
        registry_def.client_id = Some("client-id".to_owned());
        registry_def.client_secret = Some("client-secret".to_owned());
        registry.register(registry_def);

        let credentials = CredentialManager::new();
        let oauth_json = serde_json::to_string(&crate::providers::oauth::OAuthToken {
            access_token: "old-access".to_owned(),
            refresh_token: Some("old-refresh".to_owned()),
            expires_at: Some(1),
            token_type: "Bearer".to_owned(),
        })
        .unwrap();
        credentials.register_keys("test".to_string(), vec![oauth_json], 1);

        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let response = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oauth_missing_refresh_token_returns_error() {
        let (registry, _) = test_provider(vec![200]).await;
        let mut registry_def = registry.get("test").unwrap();
        registry_def.auth_type = AuthType::OAuth;
        registry_def.token_endpoint = Some("http://localhost:1/token".to_owned());
        registry_def.client_id = Some("client-id".to_owned());
        registry.register(registry_def);

        let credentials = CredentialManager::new();
        let oauth_json = serde_json::to_string(&crate::providers::oauth::OAuthToken {
            access_token: "old-access".to_owned(),
            refresh_token: None,
            expires_at: Some(1),
            token_type: "Bearer".to_owned(),
        })
        .unwrap();
        credentials.register_keys("test".to_string(), vec![oauth_json], 1);

        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let result = client
            .request("test", "chat/completions", serde_json::json!({}))
            .await;
        assert!(
            matches!(result, Err(RotatorError::Other(ref msg)) if msg.contains("missing OAuth refresh token")),
            "expected missing refresh token error, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn concurrent_refresh_is_single_flight() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let refresh_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refresh_addr = refresh_listener.local_addr().unwrap();
        let refresh_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&refresh_count);

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = refresh_listener.accept().await else {
                    break;
                };
                count_clone.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0; 2048];
                let size = socket.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..size]);
                assert!(request.starts_with("POST /token HTTP/1.1"));
                tokio::time::sleep(Duration::from_millis(100)).await;
                let body = r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"token_type":"Bearer"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let (registry, calls) = test_provider(vec![200, 200]).await;
        let mut registry_def = registry.get("test").unwrap();
        registry_def.auth_type = AuthType::OAuth;
        registry_def.token_endpoint = Some(format!("http://{refresh_addr}/token"));
        registry_def.client_id = Some("client-id".to_owned());
        registry_def.client_secret = Some("client-secret".to_owned());
        registry.register(registry_def);

        let credentials = CredentialManager::new();
        let oauth_json = serde_json::to_string(&crate::providers::oauth::OAuthToken {
            access_token: "old-access".to_owned(),
            refresh_token: Some("old-refresh".to_owned()),
            expires_at: Some(1),
            token_type: "Bearer".to_owned(),
        })
        .unwrap();
        credentials.register_keys("test".to_string(), vec![oauth_json], 2);

        let client = RotatorClient::new(
            credentials,
            HttpClientPool::new(30),
            registry,
            Arc::new(RateLimiterRegistry::new()),
            Arc::new(CooldownManager::new()),
            Arc::new(CircuitBreakerRegistry::new()),
            None,
            0,
        );

        let (resp1, resp2) = tokio::join!(
            client.request("test", "chat/completions", serde_json::json!({})),
            client.request("test", "chat/completions", serde_json::json!({}))
        );
        assert!(resp1.is_ok());
        assert!(resp2.is_ok());
        assert_eq!(
            refresh_count.load(Ordering::SeqCst),
            1,
            "refresh should happen exactly once"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
