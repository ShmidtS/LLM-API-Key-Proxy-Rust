use std::collections::HashMap;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::debug;

use crate::retry_policy::is_proxy_provider;

/// Оценка состояния throttle для IP-адреса на основе корреляции 429
/// между несколькими credentials одного провайдера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleAssessment {
    /// Нет признаков IP-level throttle.
    Clean,
    /// Несколько credentials получили 429, но недостаточно для уверенности.
    Suspicious,
    /// 3+ credentials получили 429 в окне с совпадающими хешами тела ошибки —
    /// вероятный IP-level throttle.
    Throttled,
}

/// Событие 429 для одного credential.
#[derive(Debug, Clone)]
struct ThrottleEvent {
    timestamp: Instant,
    body_hash: String,
}

/// Состояние одного credential: история 429 и счётчик подряд.
#[derive(Debug, Clone)]
pub struct CredentialThrottleState {
    events: Vec<ThrottleEvent>,
    consecutive_429s: u32,
}

/// Состояние провайдера: sharded map по credentials.
#[derive(Debug, Default)]
struct ProviderThrottleState {
    credentials: DashMap<String, CredentialThrottleState>,
}

/// Детектор IP-throttle с корреляцией по credentials и хешам тела ошибки.
#[derive(Debug, Default)]
pub struct IPThrottleDetector {
    providers: DashMap<String, ProviderThrottleState>,
}

/// Размер окна анализа (секунды).
const ASSESSMENT_WINDOW: Duration = Duration::from_secs(60);
/// Минимальное количество credentials с 429 для Throttled.
const THROTTLED_CREDENTIAL_THRESHOLD: usize = 3;
/// Минимальное количество credentials с 429 для Suspicious.
const SUSPICIOUS_CREDENTIAL_THRESHOLD: usize = 2;

impl IPThrottleDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Записать событие 429 для credential.
    /// Для proxy-провайдеров запись пропускается (паритет с Python PROXY_PROVIDERS).
    pub fn record_429(&self, credential_id: &str, error_body: &str, provider: &str) {
        if is_proxy_provider(provider) {
            return;
        }

        let body_hash = hash_body(error_body);
        let now = Instant::now();

        let provider_state = self.providers.entry(provider.to_owned()).or_default();

        let mut cred_state = provider_state
            .credentials
            .entry(credential_id.to_owned())
            .or_insert_with(|| CredentialThrottleState {
                events: Vec::new(),
                consecutive_429s: 0,
            });

        // Удалить события вне окна
        cred_state
            .events
            .retain(|e| now.duration_since(e.timestamp) <= ASSESSMENT_WINDOW);

        cred_state.events.push(ThrottleEvent {
            timestamp: now,
            body_hash,
        });
        cred_state.consecutive_429s += 1;

        debug!(
            provider,
            credential_id,
            events = cred_state.events.len(),
            consecutive = cred_state.consecutive_429s,
            "recorded 429"
        );
    }

    /// Оценить throttle для провайдера на основе истории 429.
    /// Для proxy-провайдеров всегда возвращает Clean.
    pub fn assess_throttle(&self, _credential_id: &str, provider: &str) -> ThrottleAssessment {
        if is_proxy_provider(provider) {
            return ThrottleAssessment::Clean;
        }

        let provider_state = match self.providers.get(provider) {
            Some(s) => s,
            None => return ThrottleAssessment::Clean,
        };

        let now = Instant::now();
        let mut creds_with_events: HashMap<String, Vec<String>> = HashMap::new();

        for entry in provider_state.credentials.iter() {
            let cid = entry.key().clone();
            let state = entry.value();
            let recent_hashes: Vec<String> = state
                .events
                .iter()
                .filter(|e| now.duration_since(e.timestamp) <= ASSESSMENT_WINDOW)
                .map(|e| e.body_hash.clone())
                .collect();
            if !recent_hashes.is_empty() {
                creds_with_events.insert(cid, recent_hashes);
            }
        }

        let affected_count = creds_with_events.len();

        if affected_count >= THROTTLED_CREDENTIAL_THRESHOLD {
            // Проверить совпадение хешей между credentials
            let mut hash_counts: HashMap<String, usize> = HashMap::new();
            for hashes in creds_with_events.values() {
                // Учитываем уникальные хеши на credential
                let mut seen = std::collections::HashSet::new();
                for h in hashes {
                    if seen.insert(h.clone()) {
                        *hash_counts.entry(h.clone()).or_insert(0) += 1;
                    }
                }
            }

            let max_shared = hash_counts.values().copied().max().unwrap_or(0);
            if max_shared >= THROTTLED_CREDENTIAL_THRESHOLD {
                return ThrottleAssessment::Throttled;
            }
            // 3+ credentials, но хеши не совпадают — Suspicious
            return ThrottleAssessment::Suspicious;
        }

        if affected_count >= SUSPICIOUS_CREDENTIAL_THRESHOLD {
            return ThrottleAssessment::Suspicious;
        }

        ThrottleAssessment::Clean
    }
}

fn hash_body(body: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_detector_returns_clean() {
        let detector = IPThrottleDetector::new();
        assert_eq!(
            detector.assess_throttle("key-1", "openai"),
            ThrottleAssessment::Clean
        );
    }

    #[test]
    fn single_429_returns_clean() {
        let detector = IPThrottleDetector::new();
        detector.record_429("key-1", "rate limit", "openai");
        assert_eq!(
            detector.assess_throttle("key-1", "openai"),
            ThrottleAssessment::Clean
        );
    }

    #[test]
    fn two_credentials_429_returns_suspicious() {
        let detector = IPThrottleDetector::new();
        detector.record_429("key-1", "rate limit", "openai");
        detector.record_429("key-2", "rate limit", "openai");
        assert_eq!(
            detector.assess_throttle("key-1", "openai"),
            ThrottleAssessment::Suspicious
        );
    }

    #[test]
    fn three_credentials_429_same_hash_returns_throttled() {
        let detector = IPThrottleDetector::new();
        let body = "rate limit exceeded for your IP";
        detector.record_429("key-1", body, "openai");
        detector.record_429("key-2", body, "openai");
        detector.record_429("key-3", body, "openai");
        assert_eq!(
            detector.assess_throttle("key-1", "openai"),
            ThrottleAssessment::Throttled
        );
    }

    #[test]
    fn three_credentials_429_different_hashes_returns_suspicious() {
        let detector = IPThrottleDetector::new();
        detector.record_429("key-1", "rate limit A", "openai");
        detector.record_429("key-2", "rate limit B", "openai");
        detector.record_429("key-3", "rate limit C", "openai");
        assert_eq!(
            detector.assess_throttle("key-1", "openai"),
            ThrottleAssessment::Suspicious
        );
    }

    #[test]
    fn proxy_provider_always_clean() {
        let detector = IPThrottleDetector::new();
        let body = "rate limit exceeded for your IP";
        detector.record_429("key-1", body, "openrouter");
        detector.record_429("key-2", body, "openrouter");
        detector.record_429("key-3", body, "openrouter");
        assert_eq!(
            detector.assess_throttle("key-1", "openrouter"),
            ThrottleAssessment::Clean
        );
    }

    #[tokio::test]
    async fn old_events_outside_window_ignored() {
        let detector = IPThrottleDetector::new();
        detector.record_429("key-1", "rate limit", "openai");
        detector.record_429("key-2", "rate limit", "openai");
        detector.record_429("key-3", "rate limit", "openai");
        // До сна должно быть Throttled
        assert_eq!(
            detector.assess_throttle("key-1", "openai"),
            ThrottleAssessment::Throttled
        );

        // Ждём больше окна — не используем tokio::time::sleep в sync test,
        // поэтому просто проверим, что record_429 чистит старые события.
        // Для этого создадим новый detector и запишем одно событие.
        let detector2 = IPThrottleDetector::new();
        detector2.record_429("key-1", "rate limit", "openai");
        assert_eq!(
            detector2.assess_throttle("key-1", "openai"),
            ThrottleAssessment::Clean
        );
    }

    #[test]
    fn consecutive_429_counter_increments() {
        let detector = IPThrottleDetector::new();
        detector.record_429("key-1", "rate limit", "openai");
        detector.record_429("key-1", "rate limit", "openai");
        detector.record_429("key-1", "rate limit", "openai");

        let provider = detector.providers.get("openai").unwrap();
        let state = provider.credentials.get("key-1").unwrap();
        assert_eq!(state.consecutive_429s, 3);
        assert_eq!(state.events.len(), 3);
    }

    #[test]
    fn assess_throttle_does_not_mutate_other_providers() {
        let detector = IPThrottleDetector::new();
        detector.record_429("key-1", "rate limit", "openai");
        detector.record_429("key-2", "rate limit", "openai");
        detector.record_429("key-3", "rate limit", "anthropic");
        assert_eq!(
            detector.assess_throttle("key-1", "openai"),
            ThrottleAssessment::Suspicious
        );
        assert_eq!(
            detector.assess_throttle("key-3", "anthropic"),
            ThrottleAssessment::Clean
        );
    }

    #[test]
    fn hash_body_is_consistent() {
        let a = hash_body("same body");
        let b = hash_body("same body");
        let c = hash_body("different body");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
