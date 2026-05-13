use dashmap::DashMap;
use tokio::time::Instant;

#[derive(Debug)]
pub struct TokenBucket {
    requests_per_minute: u32,
    burst_size: u32,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(requests_per_minute: u32, burst_size: u32) -> Self {
        Self {
            requests_per_minute,
            burst_size,
            tokens: f64::from(burst_size),
            last_refill: Instant::now(),
        }
    }

    pub fn acquire(&mut self) -> bool {
        self.refill();

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill);
        self.last_refill = now;

        if self.requests_per_minute == 0 || self.burst_size == 0 {
            self.tokens = self.tokens.min(f64::from(self.burst_size));
            return;
        }

        let refill_rate_per_second = f64::from(self.requests_per_minute) / 60.0;
        let refilled_tokens = elapsed.as_secs_f64() * refill_rate_per_second;
        self.tokens = (self.tokens + refilled_tokens).min(f64::from(self.burst_size));
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
}
