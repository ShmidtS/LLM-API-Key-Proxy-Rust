use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleReason {
    RateLimit,
    QuotaExceeded,
    ServerOverload,
    Other,
}

pub fn classify_throttle(
    status: u16,
    body: &serde_json::Value,
) -> (ThrottleReason, Option<Duration>) {
    classify_throttle_with_headers(status, None, body)
}

pub(crate) fn classify_throttle_with_headers(
    status: u16,
    headers: Option<&reqwest::header::HeaderMap>,
    body: &serde_json::Value,
) -> (ThrottleReason, Option<Duration>) {
    let reason = classify_reason(status, body);
    let retry_after = headers
        .and_then(retry_after_from_headers)
        .or_else(|| retry_after_from_body(body));

    (reason, retry_after)
}

fn classify_reason(status: u16, body: &serde_json::Value) -> ThrottleReason {
    if status == 429 {
        let text = body.to_string().to_ascii_lowercase();
        if text.contains("quota") || text.contains("insufficient_quota") {
            ThrottleReason::QuotaExceeded
        } else {
            ThrottleReason::RateLimit
        }
    } else if status == 503 || status == 529 || status == 502 || status == 504 {
        ThrottleReason::ServerOverload
    } else {
        ThrottleReason::Other
    }
}

fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
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
}

fn parse_retry_after_value(value: &serde_json::Value) -> Option<Duration> {
    value
        .as_u64()
        .map(Duration::from_secs)
        .or_else(|| value.as_f64().map(Duration::from_secs_f64))
        .or_else(|| value.as_str().and_then(parse_retry_after))
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .parse::<u64>()
        .map(Duration::from_secs)
        .or_else(|_| value.parse::<f64>().map(Duration::from_secs_f64))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    #[test]
    fn classifies_rate_limit_and_header_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("2"));

        let (reason, retry_after) = classify_throttle_with_headers(
            429,
            Some(&headers),
            &serde_json::json!({"error": {"message": "rate limit"}}),
        );

        assert_eq!(reason, ThrottleReason::RateLimit);
        assert_eq!(retry_after, Some(Duration::from_secs(2)));
    }

    #[test]
    fn classifies_quota_from_body() {
        let (reason, retry_after) = classify_throttle(
            429,
            &serde_json::json!({"error": {"code": "insufficient_quota", "retry_after": 3}}),
        );

        assert_eq!(reason, ThrottleReason::QuotaExceeded);
        assert_eq!(retry_after, Some(Duration::from_secs(3)));
    }

    #[test]
    fn classifies_server_overload() {
        let (reason, retry_after) = classify_throttle(503, &serde_json::json!({}));

        assert_eq!(reason, ThrottleReason::ServerOverload);
        assert_eq!(retry_after, None);
    }
}
