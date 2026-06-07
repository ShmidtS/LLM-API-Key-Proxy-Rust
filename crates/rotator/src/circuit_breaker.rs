use dashmap::DashMap;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug)]
struct CircuitBreakerInner {
    state: CircuitState,
    failure_count: usize,
    opened_at: Option<Instant>,
    half_open_calls: usize,
}

#[derive(Debug)]
pub struct CircuitBreaker {
    failure_threshold: usize,
    recovery_timeout: Duration,
    half_open_max_calls: usize,
    inner: Mutex<CircuitBreakerInner>,
}

impl CircuitBreaker {
    pub fn new(
        failure_threshold: usize,
        recovery_timeout_secs: u64,
        half_open_max_calls: usize,
    ) -> Self {
        Self {
            failure_threshold,
            recovery_timeout: Duration::from_secs(recovery_timeout_secs),
            half_open_max_calls,
            inner: Mutex::new(CircuitBreakerInner {
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at: None,
                half_open_calls: 0,
            }),
        }
    }

    pub fn record_success(&self) {
        let mut inner = self.inner.lock();
        match inner.state {
            CircuitState::Closed => {
                inner.failure_count = 0;
            }
            CircuitState::Open => {
                inner.state = CircuitState::HalfOpen;
                inner.failure_count = 0;
                inner.opened_at = None;
                inner.half_open_calls = 0;
            }
            CircuitState::HalfOpen => {
                inner.state = CircuitState::Closed;
                inner.failure_count = 0;
                inner.opened_at = None;
                inner.half_open_calls = 0;
            }
        }
    }

    pub fn record_failure(&self) {
        let mut inner = self.inner.lock();
        match inner.state {
            CircuitState::Closed => {
                inner.failure_count += 1;
                if inner.failure_count >= self.failure_threshold.max(1) {
                    inner.state = CircuitState::Open;
                    inner.opened_at = Some(Instant::now());
                    inner.half_open_calls = 0;
                }
            }
            CircuitState::Open | CircuitState::HalfOpen => {
                inner.state = CircuitState::Open;
                inner.failure_count = self.failure_threshold.max(1);
                inner.opened_at = Some(Instant::now());
                inner.half_open_calls = 0;
            }
        }
    }

    pub fn is_allowed(&self) -> bool {
        let mut inner = self.inner.lock();
        match inner.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let recovered = inner
                    .opened_at
                    .is_some_and(|opened_at| opened_at.elapsed() >= self.recovery_timeout);
                if recovered {
                    inner.state = CircuitState::HalfOpen;
                    inner.opened_at = None;
                    inner.half_open_calls = 0;
                    Self::allow_half_open_call(&mut inner, self.half_open_max_calls)
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                Self::allow_half_open_call(&mut inner, self.half_open_max_calls)
            }
        }
    }

    pub fn get_state(&self) -> CircuitState {
        self.inner.lock().state
    }

    fn allow_half_open_call(inner: &mut CircuitBreakerInner, half_open_max_calls: usize) -> bool {
        if inner.half_open_calls < half_open_max_calls.max(1) {
            inner.half_open_calls += 1;
            true
        } else {
            false
        }
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(5, 60, 1)
    }
}

#[derive(Debug, Clone, Default)]
pub struct CircuitBreakerRegistry {
    breakers: Arc<DashMap<String, Arc<CircuitBreaker>>>,
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn configure_provider(
        &self,
        provider: &str,
        failure_threshold: usize,
        recovery_timeout_secs: u64,
        half_open_max_calls: usize,
    ) {
        self.breakers.insert(
            provider.to_string(),
            Arc::new(CircuitBreaker::new(
                failure_threshold,
                recovery_timeout_secs,
                half_open_max_calls,
            )),
        );
    }

    pub fn record_success(&self, provider: &str) {
        self.get_or_create(provider).record_success();
    }

    pub fn record_failure(&self, provider: &str) {
        self.get_or_create(provider).record_failure();
    }

    pub fn is_allowed(&self, provider: &str) -> bool {
        self.get_or_create(provider).is_allowed()
    }

    pub fn get_state(&self, provider: &str) -> CircuitState {
        self.get_or_create(provider).get_state()
    }

    fn get_or_create(&self, provider: &str) -> Arc<CircuitBreaker> {
        self.breakers
            .entry(provider.to_string())
            .or_insert_with(|| Arc::new(CircuitBreaker::default()))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_closed_and_allows_calls() {
        let breaker = CircuitBreaker::new(2, 30, 1);

        assert_eq!(breaker.get_state(), CircuitState::Closed);
        assert!(breaker.is_allowed());
    }

    #[test]
    fn closed_opens_after_failure_threshold() {
        let breaker = CircuitBreaker::new(2, 30, 1);

        breaker.record_failure();
        assert_eq!(breaker.get_state(), CircuitState::Closed);
        assert!(breaker.is_allowed());

        breaker.record_failure();
        assert_eq!(breaker.get_state(), CircuitState::Open);
        assert!(!breaker.is_allowed());
    }

    #[test]
    fn open_allows_half_open_after_recovery_timeout() {
        let breaker = CircuitBreaker::new(1, 0, 2);

        breaker.record_failure();
        assert_eq!(breaker.get_state(), CircuitState::Open);

        assert!(breaker.is_allowed());
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);
    }

    #[test]
    fn half_open_respects_max_calls_limit() {
        let breaker = CircuitBreaker::new(1, 0, 2);

        breaker.record_failure();

        assert!(breaker.is_allowed());
        assert!(breaker.is_allowed());
        assert!(!breaker.is_allowed());
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);
    }

    #[test]
    fn success_moves_open_to_half_open_then_closed() {
        let breaker = CircuitBreaker::new(1, 30, 2);

        breaker.record_failure();
        assert_eq!(breaker.get_state(), CircuitState::Open);

        breaker.record_success();
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);

        breaker.record_success();
        assert_eq!(breaker.get_state(), CircuitState::Closed);
        assert!(breaker.is_allowed());
    }

    #[test]
    fn failure_in_half_open_reopens() {
        let breaker = CircuitBreaker::new(1, 30, 1);

        breaker.record_failure();
        breaker.record_success();
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);

        breaker.record_failure();
        assert_eq!(breaker.get_state(), CircuitState::Open);
        assert!(!breaker.is_allowed());
    }

    #[test]
    fn registry_stores_breakers_per_provider() {
        let registry = CircuitBreakerRegistry::new();
        registry.configure_provider("openai", 2, 30, 1);
        registry.configure_provider("anthropic", 1, 30, 1);

        registry.record_failure("openai");
        registry.record_failure("anthropic");

        assert_eq!(registry.get_state("openai"), CircuitState::Closed);
        assert_eq!(registry.get_state("anthropic"), CircuitState::Open);
        assert!(registry.is_allowed("openai"));
        assert!(!registry.is_allowed("anthropic"));
    }

    #[test]
    fn registry_uses_default_config_for_unconfigured_provider() {
        let registry = CircuitBreakerRegistry::new();

        assert_eq!(registry.get_state("unknown"), CircuitState::Closed);
        assert!(registry.is_allowed("unknown"));
    }
}
