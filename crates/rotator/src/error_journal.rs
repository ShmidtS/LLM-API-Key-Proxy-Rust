use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{Duration, interval};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorClass {
    RateLimit,
    Auth,
    Timeout,
    ServerError,
    StreamError,
    Network,
    Garbage,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEntry {
    pub provider: String,
    pub error_class: ErrorClass,
    pub timestamp: chrono::DateTime<Utc>,
    pub status_code: Option<u16>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ErrorJournal {
    inner: Arc<ErrorJournalInner>,
}

#[derive(Debug)]
struct ErrorJournalInner {
    map: DashMap<String, Vec<ErrorEntry>>,
}

impl ErrorJournal {
    pub fn new() -> Self {
        let inner = Arc::new(ErrorJournalInner {
            map: DashMap::new(),
        });
        Self { inner }
    }

    pub fn record_error(
        &self,
        provider: impl Into<String>,
        error_class: ErrorClass,
        status_code: Option<u16>,
        message: impl Into<String>,
    ) {
        let provider = provider.into();
        let entry = ErrorEntry {
            provider: provider.clone(),
            error_class,
            timestamp: Utc::now(),
            status_code,
            message: message.into(),
        };
        self.inner
            .map
            .entry(provider)
            .or_default()
            .value_mut()
            .push(entry);
    }

    fn entries_in_last_5min(&self, provider: &str) -> Vec<ErrorEntry> {
        let cutoff = Utc::now() - chrono::Duration::minutes(5);
        self.inner
            .map
            .get(provider)
            .map(|v| {
                v.iter()
                    .filter(|e| e.timestamp >= cutoff)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn error_rate_5min(&self, provider: &str) -> f64 {
        let entries = self.entries_in_last_5min(provider);
        if entries.is_empty() {
            return 0.0;
        }
        // Rate is estimated as fraction of errors relative to total attempts.
        // We approximate by counting entries in the last 5 minutes and treating
        // each as one failed request; since we don't have total request count,
        // we report the error count as a percentage scaled against an arbitrary
        // window of 10 attempts. Callers should interpret this as a relative
        // severity metric rather than a strict statistical rate.
        let count = entries.len().min(100) as f64;
        (count / 10.0) * 100.0
    }

    pub fn error_count_by_class(&self, provider: &str, error_class: ErrorClass) -> usize {
        let cutoff = Utc::now() - chrono::Duration::minutes(5);
        self.inner
            .map
            .get(provider)
            .map(|v| {
                v.iter()
                    .filter(|e| e.timestamp >= cutoff && e.error_class == error_class)
                    .count()
            })
            .unwrap_or(0)
    }

    pub fn should_escalate(&self, provider: &str) -> bool {
        self.error_rate_5min(provider) > 50.0
    }

    pub fn should_circuit_break(&self, provider: &str) -> bool {
        self.error_rate_5min(provider) > 90.0
    }

    pub fn cleanup_task(&self) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                let cutoff = Utc::now() - chrono::Duration::minutes(5);
                for mut entry in inner.map.iter_mut() {
                    entry.retain(|e| e.timestamp >= cutoff);
                }
                inner.map.retain(|_, v| !v.is_empty());
            }
        });
    }

    pub fn export_json(&self) -> String {
        let mut summary = serde_json::Map::new();
        for entry in self.inner.map.iter() {
            let provider = entry.key();
            let entries = entry.value();
            let total_5min = entries.len();
            let by_class: serde_json::Map<String, serde_json::Value> = {
                let mut m = serde_json::Map::new();
                for e in entries.iter() {
                    let key = format!("{:?}", e.error_class);
                    let count = m.get(&key).and_then(|v| v.as_u64()).unwrap_or(0) + 1;
                    m.insert(key, json!(count));
                }
                m
            };
            let provider_summary = json!({
                "total_5min": total_5min,
                "by_class": by_class,
                "error_rate": self.error_rate_5min(provider),
                "should_escalate": self.should_escalate(provider),
                "should_circuit_break": self.should_circuit_break(provider),
                "latest_entries": entries.iter().rev().take(10).collect::<Vec<_>>(),
            });
            summary.insert(provider.clone(), provider_summary);
        }
        json!(summary).to_string()
    }
}

impl Default for ErrorJournal {
    fn default() -> Self {
        Self::new()
    }
}

/// Classify an HTTP status code into an error class.
pub fn classify_status_code(status: u16) -> ErrorClass {
    match status {
        429 => ErrorClass::RateLimit,
        401 | 403 => ErrorClass::Auth,
        408 | 504 => ErrorClass::Timeout,
        500 | 502 | 503 => ErrorClass::ServerError,
        _ => ErrorClass::Unknown,
    }
}

/// Classify a `reqwest::Error` into an error class.
pub fn classify_reqwest_error(err: &reqwest::Error) -> ErrorClass {
    if err.is_timeout() {
        ErrorClass::Network
    } else {
        ErrorClass::Unknown
    }
}
