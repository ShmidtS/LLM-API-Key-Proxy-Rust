use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy)]
pub struct ProviderRateInfo {
    pub current_rps: f64,
    pub ceiling_rps: f64,
    pub floor_rps: f64,
}

#[derive(Debug)]
pub struct AdaptiveRateLimiter {
    current_rps: AtomicU64,
    ceiling_rps: AtomicU64,
    floor_rps: f64,
    additive_increase: f64,
    multiplicative_decrease: f64,
    last_429_time: AtomicU64,
    success_window: AtomicU64,
    success_window_threshold: u64,
}

impl AdaptiveRateLimiter {
    pub fn new(
        initial_rps: f64,
        ceiling_rps: f64,
        floor_rps: f64,
        additive_increase: f64,
        multiplicative_decrease: f64,
        success_window_threshold: u64,
    ) -> Self {
        Self {
            current_rps: AtomicU64::new(initial_rps.to_bits()),
            ceiling_rps: AtomicU64::new(ceiling_rps.to_bits()),
            floor_rps,
            additive_increase,
            multiplicative_decrease,
            last_429_time: AtomicU64::new(0),
            success_window: AtomicU64::new(0),
            success_window_threshold,
        }
    }

    pub fn record_success(&self) {
        let count = self.success_window.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= self.success_window_threshold {
            self.success_window.store(0, Ordering::Relaxed);
            let current = f64::from_bits(self.current_rps.load(Ordering::Relaxed));
            let ceiling = f64::from_bits(self.ceiling_rps.load(Ordering::Relaxed));
            let new = (current + self.additive_increase).min(ceiling);
            self.current_rps.store(new.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn record_429(&self) {
        let current = f64::from_bits(self.current_rps.load(Ordering::Relaxed));
        let new = (current * self.multiplicative_decrease).max(self.floor_rps);
        self.current_rps.store(new.to_bits(), Ordering::Relaxed);
        self.success_window.store(0, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0))
            .as_secs();
        self.last_429_time.store(now, Ordering::Relaxed);
    }

    pub fn get_provider_info(&self) -> ProviderRateInfo {
        ProviderRateInfo {
            current_rps: f64::from_bits(self.current_rps.load(Ordering::Relaxed)),
            ceiling_rps: f64::from_bits(self.ceiling_rps.load(Ordering::Relaxed)),
            floor_rps: self.floor_rps,
        }
    }

    pub fn last_429_time(&self) -> Option<u64> {
        let ts = self.last_429_time.load(Ordering::Relaxed);
        if ts == 0 { None } else { Some(ts) }
    }
}

#[derive(Debug, Default)]
pub struct AdaptiveRateLimiterRegistry {
    limiters: DashMap<(String, String), AdaptiveRateLimiter>,
}

impl AdaptiveRateLimiterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn configure(
        &self,
        provider: &str,
        key: &str,
        initial_rps: f64,
        ceiling_rps: f64,
        floor_rps: f64,
        additive_increase: f64,
        multiplicative_decrease: f64,
        success_window_threshold: u64,
    ) {
        self.limiters.insert(
            (provider.to_owned(), key.to_owned()),
            AdaptiveRateLimiter::new(
                initial_rps,
                ceiling_rps,
                floor_rps,
                additive_increase,
                multiplicative_decrease,
                success_window_threshold,
            ),
        );
    }

    pub fn record_success(&self, provider: &str, key: &str) {
        if let Some(entry) = self.limiters.get(&(provider.to_owned(), key.to_owned())) {
            entry.record_success();
        }
    }

    pub fn record_429(&self, provider: &str, key: &str) {
        if let Some(entry) = self.limiters.get(&(provider.to_owned(), key.to_owned())) {
            entry.record_429();
        }
    }

    pub fn get_provider_info(&self, provider: &str, key: &str) -> Option<ProviderRateInfo> {
        self.limiters
            .get(&(provider.to_owned(), key.to_owned()))
            .map(|entry| entry.get_provider_info())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

/// Token-bucket using lock-free atomics: 64-bit raw tokens for cheap fixed-point math.
/// One token is represented as `TOKEN_UNIT` (1 << 20). This provides enough fractional
/// precision while staying entirely inside `AtomicU64` CAS.
const TOKEN_UNIT: f64 = 1_048_576.0;

#[derive(Debug)]
pub struct TokenBucket {
    requests_per_minute: u32,
    burst_size: u32,
    raw_tokens: AtomicU64,
    last_refill_ms: AtomicU64,
}

impl TokenBucket {
    pub fn new(requests_per_minute: u32, burst_size: u32) -> Self {
        let burst = (f64::from(burst_size) * TOKEN_UNIT) as u64;
        Self {
            requests_per_minute,
            burst_size,
            raw_tokens: AtomicU64::new(burst),
            last_refill_ms: AtomicU64::new(now_ms()),
        }
    }

    pub fn acquire(&self) -> bool {
        let now = now_ms();
        let last_refill = self.last_refill_ms.load(Ordering::Acquire);
        let elapsed_ms = now.saturating_sub(last_refill);
        let refill_rate_per_second = f64::from(self.requests_per_minute) / 60.0;
        let refilled =
            (f64::from(elapsed_ms as u32) / 1000.0 * refill_rate_per_second * TOKEN_UNIT) as u64;
        let burst = (f64::from(self.burst_size) * TOKEN_UNIT) as u64;

        loop {
            let current = self.raw_tokens.load(Ordering::Acquire);
            let new_tokens = if refilled > 0 {
                (current + refilled).min(burst)
            } else {
                current
            };
            if new_tokens < TOKEN_UNIT as u64 {
                return false;
            }
            let next = new_tokens - TOKEN_UNIT as u64;
            match self.raw_tokens.compare_exchange(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if refilled > 0 {
                        self.last_refill_ms.store(now, Ordering::Release);
                    }
                    return true;
                }
                Err(_) => continue,
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct RateLimiterRegistry {
    buckets: DashMap<(String, String), TokenBucket>,
}

impl RateLimiterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn configure(&self, provider: &str, key: &str, rpm: u32, burst: u32) {
        self.buckets.insert(
            (provider.to_owned(), key.to_owned()),
            TokenBucket::new(rpm, burst),
        );
    }

    pub fn acquire(&self, provider: &str, key: &str) -> bool {
        self.buckets
            .entry((provider.to_owned(), key.to_owned()))
            .or_insert_with(|| TokenBucket::new(60, 60))
            .acquire()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn acquire_succeeds_when_tokens_available() {
        let registry = RateLimiterRegistry::new();
        registry.configure("openai", "key-1", 60, 1);

        assert!(registry.acquire("openai", "key-1"));
    }

    #[test]
    fn acquire_fails_when_rate_limited() {
        let registry = RateLimiterRegistry::new();
        registry.configure("openai", "key-1", 60, 1);

        assert!(registry.acquire("openai", "key-1"));
        assert!(!registry.acquire("openai", "key-1"));
    }

    #[tokio::test]
    async fn tokens_refill_over_time() {
        let registry = RateLimiterRegistry::new();
        registry.configure("openai", "key-1", 6_000, 1);

        assert!(registry.acquire("openai", "key-1"));
        assert!(!registry.acquire("openai", "key-1"));

        tokio::time::sleep(Duration::from_millis(20)).await;

        assert!(registry.acquire("openai", "key-1"));
    }

    #[test]
    fn rate_limits_are_isolated_per_key() {
        let registry = RateLimiterRegistry::new();
        registry.configure("openai", "key-1", 60, 1);
        registry.configure("openai", "key-2", 60, 1);

        assert!(registry.acquire("openai", "key-1"));
        assert!(!registry.acquire("openai", "key-1"));
        assert!(registry.acquire("openai", "key-2"));
    }

    #[test]
    fn burst_limit_is_enforced() {
        let registry = RateLimiterRegistry::new();
        registry.configure("openai", "key-1", 60, 2);

        assert!(registry.acquire("openai", "key-1"));
        assert!(registry.acquire("openai", "key-1"));
        assert!(!registry.acquire("openai", "key-1"));
    }

    #[test]
    fn adaptive_record_success_increases_rps_additively() {
        let limiter = AdaptiveRateLimiter::new(5.0, 10.0, 1.0, 0.5, 0.7, 3);
        limiter.record_success();
        limiter.record_success();
        limiter.record_success();
        assert_eq!(limiter.get_provider_info().current_rps, 5.5);
    }

    #[test]
    fn adaptive_record_success_caps_at_ceiling() {
        let limiter = AdaptiveRateLimiter::new(9.5, 10.0, 1.0, 0.5, 0.7, 3);
        limiter.record_success();
        limiter.record_success();
        limiter.record_success();
        assert_eq!(limiter.get_provider_info().current_rps, 10.0);
    }

    #[test]
    fn adaptive_record_429_decreases_rps_multiplicatively() {
        let limiter = AdaptiveRateLimiter::new(10.0, 10.0, 1.0, 0.5, 0.7, 10);
        limiter.record_429();
        assert_eq!(limiter.get_provider_info().current_rps, 7.0);
    }

    #[test]
    fn adaptive_record_429_floors_at_floor_rps() {
        let limiter = AdaptiveRateLimiter::new(1.2, 10.0, 1.0, 0.5, 0.7, 10);
        limiter.record_429();
        assert_eq!(limiter.get_provider_info().current_rps, 1.0);
    }

    #[test]
    fn adaptive_record_429_resets_success_window() {
        let limiter = AdaptiveRateLimiter::new(5.0, 10.0, 1.0, 0.5, 0.7, 3);
        limiter.record_success();
        limiter.record_success();
        limiter.record_429();
        limiter.record_success();
        assert_eq!(limiter.get_provider_info().current_rps, 3.5);
    }

    #[test]
    fn adaptive_registry_configure_and_get_info() {
        let registry = AdaptiveRateLimiterRegistry::new();
        registry.configure("openai", "key-1", 5.0, 10.0, 1.0, 0.5, 0.7, 3);
        let info = registry.get_provider_info("openai", "key-1").unwrap();
        assert_eq!(info.current_rps, 5.0);
        assert_eq!(info.ceiling_rps, 10.0);
        assert_eq!(info.floor_rps, 1.0);
    }

    #[test]
    fn adaptive_registry_record_success_and_429() {
        let registry = AdaptiveRateLimiterRegistry::new();
        registry.configure("openai", "key-1", 5.0, 10.0, 1.0, 0.5, 0.7, 3);
        registry.record_success("openai", "key-1");
        registry.record_success("openai", "key-1");
        registry.record_success("openai", "key-1");
        assert!(
            (registry
                .get_provider_info("openai", "key-1")
                .unwrap()
                .current_rps
                - 5.5)
                .abs()
                < 1e-9
        );
        registry.record_429("openai", "key-1");
        assert!(
            (registry
                .get_provider_info("openai", "key-1")
                .unwrap()
                .current_rps
                - 3.85)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn adaptive_registry_no_panic_on_missing_key() {
        let registry = AdaptiveRateLimiterRegistry::new();
        registry.record_success("openai", "key-1");
        registry.record_429("openai", "key-1");
        assert!(registry.get_provider_info("openai", "key-1").is_none());
    }

    #[test]
    fn adaptive_limiter_records_last_429_time() {
        let limiter = AdaptiveRateLimiter::new(5.0, 10.0, 1.0, 0.5, 0.7, 3);
        assert!(limiter.last_429_time().is_none());
        limiter.record_429();
        assert!(limiter.last_429_time().is_some());
    }
}
