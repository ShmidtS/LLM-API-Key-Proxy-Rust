//! zai remaining-quota preflight check.
//!
//! Mirrors the standalone `check_zai_all.py` probe: GETs zai's monitor
//! endpoint with a bearer key, parses the `TIME_LIMIT` (unit 5) window, and
//! reports whether the key still has headroom. The rotator consults this
//! *before* dispatching a client request so an exhausted key is skipped (via
//! cooldown) instead of surfacing an upstream 429.
//!
//! Results are cached per key for `QUOTA_CACHE_TTL` to bound probe latency on
//! the hot path: the first use of a key pays one HTTP round-trip, subsequent
//! uses within the TTL reuse the cached verdict.
//!
//! zai's `TIME_LIMIT` (unit 5) window is **not** hourly — observed reset
//! horizons range from days to weeks depending on `level`. The API exposes
//! `nextResetTime` (epoch ms), so the cooldown for an exhausted key is set to
//! that exact horizon (plus a margin) rather than a fixed guess.

use dashmap::DashMap;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

const QUOTA_ENDPOINT: &str = "https://api.z.ai/api/monitor/usage/quota/limit";

/// How long a cached quota verdict is trusted. Keeps probe load off the
/// monitor endpoint; the exhausted-key cooldown is set from the API's own
/// `nextResetTime` so staleness only affects the *available* verdict.
pub const QUOTA_CACHE_TTL: Duration = Duration::from_secs(300);

/// Fallback cooldown when the API signals exhaustion but omits
/// `nextResetTime`. Long enough to avoid a tight re-probe loop yet short
/// enough to recover within the same session.
pub const EXHAUSTED_COOLDOWN_FALLBACK: Duration = Duration::from_secs(3600);

/// Cap on the exhausted-key cooldown. zai's reset horizon can be weeks for
/// some levels; capping prevents an effectively-permanent ban when the
/// monitor returns a stale/far-future timestamp.
pub const EXHAUSTED_COOLDOWN_CAP: Duration = Duration::from_secs(6 * 3600);

/// Margin added to `nextResetTime` so a key re-probed right at the boundary
/// does not race the upstream reset.
const RESET_MARGIN: Duration = Duration::from_secs(60);

/// `TIME_LIMIT` unit reported by zai's monitor API (the per-window request
/// budget, not the token limits on units 3/6).
const WINDOW_LIMIT_UNIT: i64 = 5;

/// `TOKENS_LIMIT` unit for the weekly/monthly token budget. At `percentage`
/// 100 the key is exhausted and zai answers with a `1310` 429.
const TOKEN_LIMIT_UNIT: i64 = 6;

/// Remaining-quota verdict for a single zai key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZaiQuotaStatus {
    /// Key has headroom. Carries the window remaining when the API exposed it,
    /// for observability.
    Available { remaining: Option<i64> },
    /// Key has no window headroom left, or the API rejected the key (auth
    /// failure / `code != 200`). Carries the cooldown duration to apply: the
    /// time until the API's `nextResetTime` (capped), or the fallback.
    Exhausted { cooldown: Duration },
}

impl ZaiQuotaStatus {
    pub fn is_available(self) -> bool {
        matches!(self, ZaiQuotaStatus::Available { .. })
    }
}

#[derive(Debug, Clone)]
struct CacheEntry {
    status: ZaiQuotaStatus,
    expires_at: Instant,
}

/// Per-key TTL cache of zai quota verdicts. Shared across all request loops
/// in `RotatorClient` so a key probed by one loop is not re-probed by another.
#[derive(Debug, Default)]
pub struct ZaiQuotaCache {
    entries: DashMap<String, CacheEntry>,
}

impl ZaiQuotaCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cached verdict if fresh, else `None` (caller must probe).
    fn get(&self, key: &str) -> Option<ZaiQuotaStatus> {
        if let Some(entry) = self.entries.get(key)
            && Instant::now() < entry.expires_at
        {
            return Some(entry.status);
        }
        None
    }

    fn store(&self, key: &str, status: ZaiQuotaStatus) {
        self.entries.insert(
            key.to_owned(),
            CacheEntry {
                status,
                expires_at: Instant::now() + QUOTA_CACHE_TTL,
            },
        );
    }

    /// Drop the cached verdict for `key`. Called when upstream signals the
    /// quota state changed (e.g. a 429 despite a cached "Available"), so the
    /// next preflight re-probes instead of trusting the stale entry.
    pub fn invalidate(&self, key: &str) {
        self.entries.remove(key);
    }
}

/// Wire shapes for `GET /api/monitor/usage/quota/limit`.
#[derive(Deserialize)]
struct QuotaResponse {
    code: i64,
    #[serde(default)]
    data: Option<QuotaData>,
}

#[derive(Deserialize)]
struct QuotaData {
    #[serde(default, rename = "limits")]
    limits: Vec<QuotaLimit>,
}

#[derive(Deserialize)]
struct QuotaLimit {
    #[serde(rename = "type")]
    kind: String,
    unit: i64,
    /// Window headroom reported directly by the API.
    #[serde(default)]
    remaining: Option<i64>,
    /// Window total, used to derive remaining when the API omits it.
    #[serde(default)]
    usage: Option<i64>,
    #[serde(default, rename = "currentValue")]
    current_value: Option<i64>,
    /// Percentage of the window budget consumed (0-100). A `TOKENS_LIMIT`
    /// at 100 means the token budget is exhausted — the real cause of zai's
    /// `1310` (Weekly/Monthly Limit Exhausted) 429s.
    #[serde(default)]
    percentage: Option<i64>,
    /// Epoch milliseconds at which the window resets.
    #[serde(default, rename = "nextResetTime")]
    next_reset_time: Option<i64>,
}

/// Cooldown to apply to an exhausted key, derived from the API's
/// `nextResetTime` (epoch ms). Capped and margined; falls back to a fixed
/// duration when the API omits the timestamp.
fn cooldown_until_reset(next_reset_ms: Option<i64>) -> Duration {
    let Some(reset_ms) = next_reset_ms else {
        return EXHAUSTED_COOLDOWN_FALLBACK;
    };
    let now_ms = epoch_ms_now();
    let delta_secs = reset_ms.saturating_sub(now_ms) / 1000;
    let delta = Duration::from_secs(delta_secs.max(0) as u64) + RESET_MARGIN;
    delta.min(EXHAUSTED_COOLDOWN_CAP)
}

/// Wall-clock epoch milliseconds. Used only to compute a cooldown horizon from
/// the API's `nextResetTime`; never feeds back into request routing.
fn epoch_ms_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Probe zai's quota endpoint for `key` and return a fresh verdict.
///
/// On any transport/parse failure the key is treated as **available** — a
/// flaky monitor endpoint must not take a working key out of rotation. Only
/// an explicit API signal (`code != 200` or window remaining <= 0) marks a
/// key exhausted, with the cooldown set to the API's `nextResetTime`.
///
/// zai reports two independent budgets in `limits`:
/// - `TIME_LIMIT unit=5` — the per-window request budget (the historical
///   signal this module watched).
/// - `TOKENS_LIMIT unit=6` — the weekly/monthly token budget. When its
///   `percentage` reaches 100 the key is exhausted and zai answers chat
///   requests with a `1310` (Weekly/Monthly Limit Exhausted) 429. This is
///   **not** visible in the `TIME_LIMIT` entry (which still reports
///   headroom), so a key that only tripped the token budget would otherwise
///   be treated as available and re-enter rotation to catch another 429.
///   We therefore treat a `TOKENS_LIMIT unit=6` at `percentage >= 100` as
///   exhausted, cooling down until its `nextResetTime`.
pub async fn check_quota(client: &reqwest::Client, key: &str) -> ZaiQuotaStatus {
    let response = match client
        .get(QUOTA_ENDPOINT)
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!(target: "zai_quota", error = %err, "quota probe failed, assuming available");
            return ZaiQuotaStatus::Available { remaining: None };
        }
    };

    let parsed: QuotaResponse = match response.json().await {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::debug!(target: "zai_quota", error = %err, "quota body unparseable, assuming available");
            return ZaiQuotaStatus::Available { remaining: None };
        }
    };

    if parsed.code != 200 {
        tracing::info!(target: "zai_quota", code = parsed.code, "zai key rejected by monitor, marking exhausted");
        return ZaiQuotaStatus::Exhausted {
            cooldown: EXHAUSTED_COOLDOWN_FALLBACK,
        };
    }

    let Some(data) = parsed.data else {
        return ZaiQuotaStatus::Available { remaining: None };
    };

    // The weekly/monthly token budget (`TOKENS_LIMIT unit=6`) is the real
    // cause of zai's `1310` (Weekly/Monthly Limit Exhausted) 429s. When its
    // `percentage` reaches 100 the key is exhausted even though the
    // `TIME_LIMIT unit=5` entry still reports headroom — so check it first
    // and cool down until its `nextResetTime`.
    for limit in &data.limits {
        if limit.kind == "TOKENS_LIMIT"
            && limit.unit == TOKEN_LIMIT_UNIT
            && limit.percentage.is_some_and(|p| p >= 100)
        {
            tracing::info!(target: "zai_quota", "zai token budget exhausted, cooling down until reset");
            return ZaiQuotaStatus::Exhausted {
                cooldown: cooldown_until_reset(limit.next_reset_time),
            };
        }
    }

    for limit in data.limits {
        if limit.kind == "TIME_LIMIT" && limit.unit == WINDOW_LIMIT_UNIT {
            // Prefer the API's own `remaining`; derive from usage/currentValue
            // only when absent (older responses).
            let remaining = limit
                .remaining
                .or_else(|| match (limit.usage, limit.current_value) {
                    (Some(total), Some(used)) => Some(total - used),
                    _ => None,
                });
            let cooldown = cooldown_until_reset(limit.next_reset_time);
            return match remaining {
                Some(r) if r <= 0 => ZaiQuotaStatus::Exhausted { cooldown },
                Some(r) => ZaiQuotaStatus::Available { remaining: Some(r) },
                None => ZaiQuotaStatus::Available { remaining: None },
            };
        }
    }

    // No window limit reported — treat as available.
    ZaiQuotaStatus::Available { remaining: None }
}

/// Cached probe: return a fresh verdict, probing the monitor endpoint only
/// when the cached entry is stale or missing.
pub async fn cached_quota_status(
    cache: &Arc<ZaiQuotaCache>,
    client: &reqwest::Client,
    key: &str,
) -> ZaiQuotaStatus {
    if let Some(status) = cache.get(key) {
        return status;
    }
    let status = check_quota(client, key).await;
    cache.store(key, status);
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(code: i64, limits: serde_json::Value) -> ZaiQuotaStatus {
        let body = json!({ "code": code, "data": { "limits": limits } });
        let parsed: QuotaResponse = serde_json::from_value(body).unwrap();
        if parsed.code != 200 {
            return ZaiQuotaStatus::Exhausted {
                cooldown: EXHAUSTED_COOLDOWN_FALLBACK,
            };
        }
        let Some(data) = parsed.data else {
            return ZaiQuotaStatus::Available { remaining: None };
        };
        for limit in &data.limits {
            if limit.kind == "TOKENS_LIMIT"
                && limit.unit == TOKEN_LIMIT_UNIT
                && limit.percentage.is_some_and(|p| p >= 100)
            {
                return ZaiQuotaStatus::Exhausted {
                    cooldown: cooldown_until_reset(limit.next_reset_time),
                };
            }
        }
        for limit in data.limits {
            if limit.kind == "TIME_LIMIT" && limit.unit == WINDOW_LIMIT_UNIT {
                let remaining =
                    limit
                        .remaining
                        .or_else(|| match (limit.usage, limit.current_value) {
                            (Some(total), Some(used)) => Some(total - used),
                            _ => None,
                        });
                let cooldown = cooldown_until_reset(limit.next_reset_time);
                return match remaining {
                    Some(r) if r <= 0 => ZaiQuotaStatus::Exhausted { cooldown },
                    Some(r) => ZaiQuotaStatus::Available { remaining: Some(r) },
                    None => ZaiQuotaStatus::Available { remaining: None },
                };
            }
        }
        ZaiQuotaStatus::Available { remaining: None }
    }

    #[test]
    fn available_key_reports_remaining() {
        // Real-shape payload: API exposes `remaining` and `nextResetTime`.
        let status = parse(
            200,
            json!([
                { "type": "TIME_LIMIT", "unit": 5, "usage": 4000, "currentValue": 7,
                  "remaining": 3993, "nextResetTime": epoch_ms_now() + 3_600_000 }
            ]),
        );
        assert!(status.is_available());
        assert_eq!(
            status,
            ZaiQuotaStatus::Available {
                remaining: Some(3993)
            }
        );
    }

    #[test]
    fn derives_remaining_when_api_omits_it() {
        // Older responses without `remaining` — derive from usage/currentValue.
        let status = parse(
            200,
            json!([
                { "type": "TIME_LIMIT", "unit": 5, "usage": 4000, "currentValue": 7 }
            ]),
        );
        assert_eq!(
            status,
            ZaiQuotaStatus::Available {
                remaining: Some(3993)
            }
        );
    }

    #[test]
    fn exhausted_key_uses_api_remaining() {
        let status = parse(
            200,
            json!([
                { "type": "TIME_LIMIT", "unit": 5, "usage": 100, "currentValue": 100,
                  "remaining": 0, "nextResetTime": epoch_ms_now() + 3_600_000 }
            ]),
        );
        assert!(!status.is_available());
        match status {
            ZaiQuotaStatus::Exhausted { cooldown } => {
                assert!(cooldown > RESET_MARGIN);
                assert!(cooldown <= EXHAUSTED_COOLDOWN_CAP);
            }
            _ => panic!("expected exhausted"),
        }
    }

    #[test]
    fn exhausted_cooldown_capped_for_far_reset() {
        // nextResetTime weeks out -> cooldown must hit the cap, not the horizon.
        let status = parse(
            200,
            json!([
                { "type": "TIME_LIMIT", "unit": 5, "remaining": 0,
                  "nextResetTime": epoch_ms_now() + 30 * 24 * 3_600_000 }
            ]),
        );
        match status {
            ZaiQuotaStatus::Exhausted { cooldown } => {
                assert_eq!(cooldown, EXHAUSTED_COOLDOWN_CAP);
            }
            _ => panic!("expected exhausted"),
        }
    }

    #[test]
    fn exhausted_cooldown_fallback_without_reset_time() {
        let status = parse(
            200,
            json!([
                { "type": "TIME_LIMIT", "unit": 5, "remaining": 0 }
            ]),
        );
        assert_eq!(
            status,
            ZaiQuotaStatus::Exhausted {
                cooldown: EXHAUSTED_COOLDOWN_FALLBACK
            }
        );
    }

    #[test]
    fn auth_failure_marks_exhausted() {
        let status = parse(401, json!([]));
        assert_eq!(
            status,
            ZaiQuotaStatus::Exhausted {
                cooldown: EXHAUSTED_COOLDOWN_FALLBACK
            }
        );
    }

    #[test]
    fn token_budget_exhausted_marks_exhausted() {
        // Real 1310 shape: TOKENS_LIMIT unit=6 at percentage 100 (exhausted),
        // while TIME_LIMIT unit=5 still reports headroom. The key must be
        // cooled down until the token budget's nextResetTime.
        let reset = epoch_ms_now() + 3_600_000;
        let status = parse(
            200,
            json!([
                { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 0 },
                { "type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 100,
                  "nextResetTime": reset },
                { "type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 1000,
                  "currentValue": 0, "remaining": 1000, "percentage": 0,
                  "nextResetTime": epoch_ms_now() + 7_200_000 }
            ]),
        );
        assert!(!status.is_available());
        match status {
            ZaiQuotaStatus::Exhausted { cooldown } => {
                // Cooldown tracks the token budget reset, not the TIME_LIMIT one.
                assert!(cooldown > Duration::from_secs(3500));
                assert!(cooldown <= EXHAUSTED_COOLDOWN_CAP);
            }
            _ => panic!("expected exhausted"),
        }
    }

    #[test]
    fn token_budget_below_100_stays_available() {
        // TOKENS_LIMIT unit=6 below 100 must not mark the key exhausted.
        let status = parse(
            200,
            json!([
                { "type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 68,
                  "nextResetTime": epoch_ms_now() + 3_600_000 },
                { "type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 4000,
                  "currentValue": 0, "remaining": 4000, "percentage": 0,
                  "nextResetTime": epoch_ms_now() + 7_200_000 }
            ]),
        );
        assert!(status.is_available());
    }

    #[test]
    fn ignores_non_window_limits() {
        let status = parse(
            200,
            json!([
                { "type": "TOKENS_LIMIT", "unit": 3, "percentage": 50 },
                { "type": "TIME_LIMIT", "unit": 6, "currentValue": 10, "usage": 20 }
            ]),
        );
        // No window (unit 5) limit -> available with unknown remaining.
        assert!(status.is_available());
        assert_eq!(status, ZaiQuotaStatus::Available { remaining: None });
    }

    #[tokio::test]
    async fn cache_returns_stored_verdict_without_probe() {
        let cache = Arc::new(ZaiQuotaCache::new());
        cache.store(
            "k",
            ZaiQuotaStatus::Exhausted {
                cooldown: EXHAUSTED_COOLDOWN_FALLBACK,
            },
        );

        // No server reachable; if cache misses this would return Available.
        let client = reqwest::Client::new();
        let status = cached_quota_status(&cache, &client, "k").await;
        assert!(!status.is_available());
    }
}
