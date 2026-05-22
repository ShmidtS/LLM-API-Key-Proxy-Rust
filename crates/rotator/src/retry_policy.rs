use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{
    StatusCode,
    header::{HeaderMap, RETRY_AFTER},
};

#[derive(Debug, Clone, PartialEq)]
pub enum FailureClass {
    RateLimit { retry_after: Option<Duration> },
    ProviderAbort,
    StreamError,
    QuotaExceeded,
    Transient,
    Fatal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    RetrySameKey,
    RotateKey,
    CooldownKey { duration: Duration },
    CooldownProvider { duration: Duration },
    OpenCircuit,
    Abort,
}

pub fn decide_retry(failure: FailureClass, attempt: u32, max_retries: u32) -> RetryDecision {
    let attempts_remain = attempt < max_retries;

    match failure {
        FailureClass::RateLimit {
            retry_after: Some(duration),
        } if attempts_remain => RetryDecision::CooldownKey { duration },
        FailureClass::RateLimit { retry_after: None } if attempts_remain => {
            RetryDecision::CooldownProvider {
                duration: get_retry_backoff(attempt, 1_000, 60_000),
            }
        }
        FailureClass::ProviderAbort | FailureClass::QuotaExceeded if attempts_remain => {
            RetryDecision::RotateKey
        }
        FailureClass::StreamError | FailureClass::Transient if attempts_remain => {
            RetryDecision::RetrySameKey
        }
        FailureClass::Fatal
        | FailureClass::RateLimit { .. }
        | FailureClass::ProviderAbort
        | FailureClass::QuotaExceeded
        | FailureClass::StreamError
        | FailureClass::Transient => RetryDecision::Abort,
    }
}

pub fn get_retry_backoff(attempt: u32, base_ms: u64, max_ms: u64) -> Duration {
    if base_ms == 0 || max_ms == 0 {
        return Duration::from_millis(0);
    }

    let multiplier = 1_u64.checked_shl(attempt.min(63)).unwrap_or(u64::MAX);
    let exponential_ms = base_ms.saturating_mul(multiplier);
    let jitter_ms = deterministic_jitter_ms(attempt, base_ms, max_ms);
    let backoff_ms = exponential_ms.saturating_add(jitter_ms).min(max_ms);

    Duration::from_millis(backoff_ms)
}

fn deterministic_jitter_ms(attempt: u32, base_ms: u64, max_ms: u64) -> u64 {
    let jitter_bound = (base_ms / 2).max(1).min(max_ms);
    let mut value = u64::from(attempt)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(base_ms.rotate_left(13))
        .wrapping_add(max_ms.rotate_right(7));

    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;

    value % jitter_bound
}

pub fn classify_upstream_failure(
    status: StatusCode,
    headers: &HeaderMap,
    body: Option<&str>,
) -> FailureClass {
    let parsed_body = body.and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok());
    let body_text = body.unwrap_or_default().to_ascii_lowercase();

    if has_quota_pattern(parsed_body.as_ref(), &body_text) {
        return FailureClass::QuotaExceeded;
    }

    if has_provider_abort_pattern(parsed_body.as_ref(), &body_text) {
        return FailureClass::ProviderAbort;
    }

    if has_stream_error_pattern(parsed_body.as_ref(), &body_text) {
        return FailureClass::StreamError;
    }

    if status == StatusCode::TOO_MANY_REQUESTS {
        return FailureClass::RateLimit {
            retry_after: retry_after_from_headers(headers)
                .or_else(|| parsed_body.as_ref().and_then(retry_after_from_body)),
        };
    }

    if status.is_server_error() || status.as_u16() == 529 {
        return FailureClass::Transient;
    }

    FailureClass::Fatal
}

fn has_quota_pattern(body: Option<&serde_json::Value>, body_text: &str) -> bool {
    body_text.contains("insufficient_quota")
        || body_text.contains("quota_exceeded")
        || body_text.contains("quota exceeded")
        || body_text.contains("resource_exhausted")
        || body_text.contains("billing_hard_limit_reached")
        || field_equals(body, &["error", "code"], "insufficient_quota")
        || field_equals(body, &["error", "type"], "insufficient_quota")
        || field_equals(body, &["error", "status"], "resource_exhausted")
}

fn has_provider_abort_pattern(body: Option<&serde_json::Value>, body_text: &str) -> bool {
    body_text.contains("provider_abort")
        || body_text.contains("provider aborted")
        || body_text.contains("upstream aborted")
        || body_text.contains("request aborted by provider")
        || field_equals(body, &["error", "type"], "provider_abort")
        || field_equals(body, &["error", "code"], "provider_abort")
        || field_equals(body, &["error", "type"], "aborted")
        || field_equals(body, &["error", "status"], "aborted")
}

fn has_stream_error_pattern(body: Option<&serde_json::Value>, body_text: &str) -> bool {
    body_text.contains("stream_error")
        || body_text.contains("stream error")
        || body_text.contains("error reading stream")
        || body_text.contains("stream disconnected")
        || field_equals(body, &["error", "type"], "stream_error")
        || field_equals(body, &["error", "code"], "stream_error")
}

fn field_equals(body: Option<&serde_json::Value>, path: &[&str], expected: &str) -> bool {
    let Some(value) = body else {
        return false;
    };

    let Some(field) = path.iter().try_fold(value, |current, key| current.get(key)) else {
        return false;
    };

    field
        .as_str()
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after)
}

fn retry_after_from_body(body: &serde_json::Value) -> Option<Duration> {
    ["retry_after", "retryAfter", "retry_after_seconds"]
        .iter()
        .find_map(|field| body.get(field).and_then(parse_retry_after_value))
        .or_else(|| {
            body.get("error").and_then(|error| {
                ["retry_after", "retryAfter", "retry_after_seconds"]
                    .iter()
                    .find_map(|field| error.get(field).and_then(parse_retry_after_value))
            })
        })
        .or_else(|| {
            body.get("error")
                .and_then(|error| error.get("details"))
                .and_then(|details| details.as_array())
                .and_then(|details| {
                    details.iter().find_map(|detail| {
                        ["retryDelay", "retry_delay", "retry_after"]
                            .iter()
                            .find_map(|field| detail.get(field).and_then(parse_retry_after_value))
                    })
                })
        })
}

fn parse_retry_after_value(value: &serde_json::Value) -> Option<Duration> {
    value
        .as_u64()
        .map(Duration::from_secs)
        .or_else(|| {
            value.as_f64().and_then(|secs| {
                if secs.is_finite() && secs >= 0.0 {
                    Some(Duration::from_secs_f64(secs))
                } else {
                    None
                }
            })
        })
        .or_else(|| value.as_str().and_then(parse_retry_after))
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();

    value
        .parse::<u64>()
        .map(Duration::from_secs)
        .ok()
        .or_else(|| {
            value.parse::<f64>().ok().and_then(|secs| {
                if secs.is_finite() && secs >= 0.0 {
                    Some(Duration::from_secs_f64(secs))
                } else {
                    None
                }
            })
        })
        .or_else(|| parse_retry_after_http_date(value))
}

fn parse_retry_after_http_date(value: &str) -> Option<Duration> {
    let date = DateTime::parse_from_rfc2822(value).ok()?;
    let duration = date.with_timezone(&Utc).signed_duration_since(Utc::now());

    duration.to_std().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use reqwest::header::HeaderValue;

    #[test]
    fn rate_limit_with_retry_after_cools_down_key() {
        assert_eq!(
            decide_retry(
                FailureClass::RateLimit {
                    retry_after: Some(Duration::from_secs(3))
                },
                0,
                1,
            ),
            RetryDecision::CooldownKey {
                duration: Duration::from_secs(3)
            }
        );
    }

    #[test]
    fn rate_limit_without_retry_after_cools_down_provider() {
        let decision = decide_retry(FailureClass::RateLimit { retry_after: None }, 0, 1);

        let RetryDecision::CooldownProvider { duration } = decision else {
            panic!("expected provider cooldown");
        };
        assert!(duration <= Duration::from_millis(60_000));
        assert!(duration >= Duration::from_millis(1_000));
    }

    #[test]
    fn provider_abort_rotates_key() {
        assert_eq!(
            decide_retry(FailureClass::ProviderAbort, 0, 1),
            RetryDecision::RotateKey
        );
    }

    #[test]
    fn transient_retries_same_key() {
        assert_eq!(
            decide_retry(FailureClass::Transient, 0, 1),
            RetryDecision::RetrySameKey
        );
    }

    #[test]
    fn fatal_aborts_immediately() {
        assert_eq!(
            decide_retry(FailureClass::Fatal, 0, 10),
            RetryDecision::Abort
        );
    }

    #[test]
    fn max_retries_respected() {
        assert_eq!(
            decide_retry(FailureClass::Transient, 1, 1),
            RetryDecision::Abort
        );
        assert_eq!(
            decide_retry(FailureClass::ProviderAbort, 1, 1),
            RetryDecision::Abort
        );
        assert_eq!(
            decide_retry(
                FailureClass::RateLimit {
                    retry_after: Some(Duration::from_secs(3))
                },
                1,
                1,
            ),
            RetryDecision::Abort
        );
    }

    #[test]
    fn backoff_calculation_caps_at_max() {
        assert_eq!(
            get_retry_backoff(10, 1_000, 2_000),
            Duration::from_millis(2_000)
        );
    }

    #[test]
    fn backoff_calculation_grows_with_attempt() {
        let first = get_retry_backoff(0, 1_000, 60_000);
        let second = get_retry_backoff(1, 1_000, 60_000);

        assert!(first >= Duration::from_millis(1_000));
        assert!(first < Duration::from_millis(1_500));
        assert!(second >= Duration::from_millis(2_000));
        assert!(second < Duration::from_millis(2_500));
    }

    #[test]
    fn edge_cases_attempt_zero_and_zero_max_retries() {
        assert_eq!(
            decide_retry(FailureClass::StreamError, 0, 1),
            RetryDecision::RetrySameKey
        );
        assert_eq!(
            decide_retry(FailureClass::StreamError, 0, 0),
            RetryDecision::Abort
        );
        assert_eq!(get_retry_backoff(0, 0, 1_000), Duration::from_millis(0));
        assert_eq!(get_retry_backoff(0, 1_000, 0), Duration::from_millis(0));
    }

    #[test]
    fn quota_exceeded_rotates_key() {
        assert_eq!(
            decide_retry(FailureClass::QuotaExceeded, 0, 1),
            RetryDecision::RotateKey
        );
    }

    #[test]
    fn classifies_429_with_retry_after_header() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("7"));

        let failure = classify_upstream_failure(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            Some(r#"{"error":{"message":"rate limit"}}"#),
        );

        assert_eq!(
            failure,
            FailureClass::RateLimit {
                retry_after: Some(Duration::from_secs(7))
            }
        );
    }

    #[test]
    fn classifies_429_with_retry_after_body() {
        let headers = HeaderMap::new();

        let failure = classify_upstream_failure(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            Some(r#"{"error":{"message":"rate limit","retry_after":2.5}}"#),
        );

        assert_eq!(
            failure,
            FailureClass::RateLimit {
                retry_after: Some(Duration::from_secs_f64(2.5))
            }
        );
    }

    #[test]
    fn classifies_429_with_http_date_retry_after_header() {
        let retry_at = Utc::now() + ChronoDuration::seconds(60);
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_str(&retry_at.to_rfc2822()).unwrap(),
        );

        let failure = classify_upstream_failure(StatusCode::TOO_MANY_REQUESTS, &headers, None);

        let FailureClass::RateLimit {
            retry_after: Some(retry_after),
        } = failure
        else {
            panic!("expected rate limit with retry_after");
        };
        assert!(retry_after <= Duration::from_secs(60));
        assert!(retry_after > Duration::from_secs(0));
    }

    #[test]
    fn classifies_500_502_503_as_transient() {
        let headers = HeaderMap::new();

        for status in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert_eq!(
                classify_upstream_failure(status, &headers, None),
                FailureClass::Transient
            );
        }
    }

    #[test]
    fn classifies_400_as_fatal() {
        let headers = HeaderMap::new();

        assert_eq!(
            classify_upstream_failure(StatusCode::BAD_REQUEST, &headers, None),
            FailureClass::Fatal
        );
    }

    #[test]
    fn classifies_provider_specific_abort_patterns() {
        let headers = HeaderMap::new();

        for body in [
            r#"{"error":{"type":"provider_abort","message":"provider aborted"}}"#,
            r#"{"error":{"status":"ABORTED","message":"Gemini request aborted by provider"}}"#,
            r#"{"type":"error","error":{"code":"provider_abort"}}"#,
        ] {
            assert_eq!(
                classify_upstream_failure(StatusCode::BAD_GATEWAY, &headers, Some(body)),
                FailureClass::ProviderAbort
            );
        }
    }

    #[test]
    fn classifies_provider_specific_stream_error_patterns() {
        let headers = HeaderMap::new();

        for body in [
            r#"{"error":{"type":"stream_error","message":"OpenAI stream error"}}"#,
            r#"{"error":{"code":"stream_error","message":"Anthropic stream disconnected"}}"#,
        ] {
            assert_eq!(
                classify_upstream_failure(StatusCode::BAD_GATEWAY, &headers, Some(body)),
                FailureClass::StreamError
            );
        }
    }

    #[test]
    fn classifies_provider_specific_quota_patterns() {
        let headers = HeaderMap::new();

        for body in [
            r#"{"error":{"code":"insufficient_quota","message":"OpenAI quota exceeded"}}"#,
            r#"{"error":{"type":"quota_exceeded","message":"Anthropic quota exceeded"}}"#,
            r#"{"error":{"status":"RESOURCE_EXHAUSTED","message":"Gemini quota exceeded"}}"#,
        ] {
            assert_eq!(
                classify_upstream_failure(StatusCode::TOO_MANY_REQUESTS, &headers, Some(body)),
                FailureClass::QuotaExceeded
            );
        }
    }

    #[test]
    fn missing_or_bad_retry_after_defaults_to_none() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("not-a-duration"));

        assert_eq!(
            classify_upstream_failure(StatusCode::TOO_MANY_REQUESTS, &headers, None),
            FailureClass::RateLimit { retry_after: None }
        );

        assert_eq!(
            classify_upstream_failure(
                StatusCode::TOO_MANY_REQUESTS,
                &HeaderMap::new(),
                Some(r#"{"error":{"retry_after":"bad"}}"#),
            ),
            FailureClass::RateLimit { retry_after: None }
        );
    }
}
