use std::time::Duration;

use crate::error_journal::{ErrorClass, ErrorJournal};
use rand::Rng;
use reqwest::{
    StatusCode,
    header::{HeaderMap, RETRY_AFTER},
};

/// Область действия rate-limit, влияющая на стратегию восстановления.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleScope {
    /// Throttle на уровне IP: ротация ключа не поможет (все ключи с одного IP).
    /// Требуется provider-level cooldown + open circuit.
    Ip,
    /// Throttle на уровне отдельного credential: ротация на другой ключ помогает.
    Credential,
    /// Область неизвестна (нет явных индикаторов в body).
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FailureClass {
    RateLimit {
        retry_after: Option<Duration>,
        scope: ThrottleScope,
    },
    ProviderAbort,
    StreamError,
    QuotaExceeded,
    /// 401/403 — текущий ключ невалиден/запрещён. Другой ключ может работать,
    /// поэтому ротируем (паритет с Python authentication/forbidden rotation).
    AuthError,
    /// Ответ содержит мусорный/некачественный контент (гарbage detection).
    /// Ротируем на другой ключ, т.к. проблема может быть специфична
    /// для текущего бэкенда/квоты.
    GarbageResponse {
        reason: String,
        score: f32,
    },
    Transient,
    Fatal,
}

/// Провайдеры-прокси: агрегируют несколько бэкендов или общие квоты, поэтому
/// 429 на нескольких ключах НЕ означает IP-throttle. Для них IP-корреляция
/// пропускается, применяется credential-level cooldown (паритет с Python PROXY_PROVIDERS).
pub const PROXY_PROVIDERS: &[&str] = &[
    "kilocode",
    "openrouter",
    "requesty",
    "opencode",
    "inception",
    "nvidia",
    "zai",
    "friendli",
];

/// Явные индикаторы IP-throttle в теле ответа (паритет с Python IP_THROTTLE_INDICATORS).
const IP_THROTTLE_INDICATORS: &[&str] = &[
    "rate limit exceeded for your ip",
    "too many requests from your ip",
    "rate limit exceeded for ip",
    "too many requests from ip",
    "ip rate limit",
    "rate limit exceeded for your ip address",
    "per-ip rate limit",
];

pub fn is_proxy_provider(provider: &str) -> bool {
    PROXY_PROVIDERS.contains(&provider)
}

fn detect_ip_throttle(body_text: &str) -> bool {
    IP_THROTTLE_INDICATORS
        .iter()
        .any(|indicator| body_text.contains(indicator))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    RetrySameKey,
    RotateKey,
    CooldownKey { duration: Duration },
    CooldownProvider { duration: Duration },
    OpenCircuit,
    GiveUp,
    Abort,
}

pub fn decide_retry(failure: FailureClass, attempt: u32, max_retries: u32) -> RetryDecision {
    decide_retry_for_provider(failure, attempt, max_retries, None, None)
}

/// Решение о повторе с учётом провайдера и журнала ошибок. Для proxy-провайдеров (см. PROXY_PROVIDERS)
/// 429 без явного IP-индикатора трактуется как credential-throttle (ротация ключа),
/// а не provider-cooldown — паритет с Python handle_429_error.
/// Если error_journal.should_circuit_break(provider) -> возвращается GiveUp.
/// Если error_journal.should_escalate(provider) -> увеличивается duration cooldown.
pub fn decide_retry_for_provider(
    failure: FailureClass,
    attempt: u32,
    max_retries: u32,
    provider: Option<&str>,
    error_journal: Option<&ErrorJournal>,
) -> RetryDecision {
    let attempts_remain = attempt < max_retries;
    let is_proxy = provider.is_some_and(is_proxy_provider);
    let provider_id = provider.unwrap_or("unknown");

    // Circuit breaker escalation: если error_rate > 70%, сдаемся.
    if let Some(journal) = error_journal
        && journal.should_circuit_break(provider_id)
    {
        return RetryDecision::GiveUp;
    }

    // Базовый cooldown duration; увеличивается при escalation.
    let base_key_ms: u64 = 1_000;
    let base_provider_ms: u64 = 1_000;
    let max_backoff_ms: u64 = 60_000;
    let _key_backoff_ms: u64 = 500;

    let escalate_multiplier = if let Some(journal) = error_journal {
        if journal.should_escalate(provider_id) {
            3
        } else {
            1
        }
    } else {
        1
    };

    match failure {
        // Явный IP-throttle: ротация ключа бесполезна (все ключи с одного IP) →
        // provider-level cooldown.
        FailureClass::RateLimit {
            scope: ThrottleScope::Ip,
            retry_after,
        } if attempts_remain => RetryDecision::CooldownProvider {
            duration: retry_after.unwrap_or_else(|| {
                get_retry_backoff(
                    attempt,
                    base_provider_ms * escalate_multiplier,
                    max_backoff_ms,
                )
            }),
        },
        // Явный retry-after на уровне ключа → cooldown этого ключа.
        FailureClass::RateLimit {
            retry_after: Some(duration),
            scope: _,
        } if attempts_remain => RetryDecision::CooldownKey { duration },
        // 429 без retry-after: для proxy-провайдеров — credential cooldown (ротация),
        // иначе консервативно — provider cooldown.
        FailureClass::RateLimit {
            retry_after: None,
            scope: _,
        } if attempts_remain => {
            if is_proxy {
                RetryDecision::CooldownKey {
                    duration: get_retry_backoff(
                        attempt,
                        base_key_ms * escalate_multiplier,
                        max_backoff_ms,
                    ),
                }
            } else {
                RetryDecision::CooldownProvider {
                    duration: get_retry_backoff(
                        attempt,
                        base_provider_ms * escalate_multiplier,
                        max_backoff_ms,
                    ),
                }
            }
        }
        FailureClass::ProviderAbort
        | FailureClass::QuotaExceeded
        | FailureClass::GarbageResponse { .. }
            if attempts_remain =>
        {
            RetryDecision::RotateKey
        }
        FailureClass::AuthError if attempts_remain => {
            // Если высокий rate auth-ошибок — увеличиваем cooldown ключа.
            if let Some(journal) = error_journal {
                if journal.error_count_by_class(provider_id, ErrorClass::Auth) >= 3 {
                    RetryDecision::CooldownKey {
                        duration: Duration::from_secs(30),
                    }
                } else {
                    RetryDecision::RotateKey
                }
            } else {
                RetryDecision::RotateKey
            }
        }
        FailureClass::StreamError | FailureClass::Transient if attempts_remain => {
            // Если высокий rate 5xx — увеличиваем provider cooldown.
            if let Some(journal) = error_journal {
                if journal.error_count_by_class(provider_id, ErrorClass::ServerError) >= 3 {
                    RetryDecision::CooldownProvider {
                        duration: get_retry_backoff(attempt, 3_000, max_backoff_ms),
                    }
                } else {
                    RetryDecision::RetrySameKey
                }
            } else {
                RetryDecision::RetrySameKey
            }
        }
        FailureClass::Fatal
        | FailureClass::RateLimit { .. }
        | FailureClass::ProviderAbort
        | FailureClass::QuotaExceeded
        | FailureClass::AuthError
        | FailureClass::GarbageResponse { .. }
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
    let jitter_ms = random_jitter_ms(base_ms, max_ms);
    let backoff_ms = exponential_ms.saturating_add(jitter_ms).min(max_ms);

    Duration::from_millis(backoff_ms)
}

/// Недетерминированный аддитивный jitter.
///
/// Диапазон: `[base_ms / 2, base_ms]` (с потолком max_ms, чтобы не превышать cap).
///
/// Почему аддитивный в `[base/2, base]`, а не полный `decorrelated`/`equal` jitter:
/// - Соответствует OpenAI `wait_random_exponential`-стилю: экспонента + случайный
///   разброс в каждой волне, а не чистая экспонента.
/// - Каждый конкурентный запрос в одной attempt-волне теперь получает независимый
///   jitter из CSPRNG (thread_rng), поэтому синхронные 429 больше не попадают в
///   retry lockstep (thundering herd) — былая детерминированная функция зависела
///   только от (attempt, base, max) и выдавала идентичный jitter всей волне.
/// - Аддитивность в диапазоне длиной `base/2` сохраняет монотонность экспоненты и
///   оставляет backoff в пределах `[exp + base/2, exp + base]`, что совместимо с
///   существующими диапазонными тестами и удерживает cap max_ms.
fn random_jitter_ms(base_ms: u64, max_ms: u64) -> u64 {
    if max_ms == 0 {
        return 0;
    }
    let low = (base_ms / 2).min(max_ms);
    let high = base_ms.min(max_ms);
    if high <= low {
        return high;
    }
    rand::thread_rng().gen_range(low..=high)
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
        let scope = if detect_ip_throttle(&body_text) {
            ThrottleScope::Ip
        } else {
            ThrottleScope::Unknown
        };
        return FailureClass::RateLimit {
            retry_after: retry_after_from_headers(headers)
                .or_else(|| parsed_body.as_ref().and_then(retry_after_from_body)),
            scope,
        };
    }

    if status.is_server_error() || status.as_u16() == 529 {
        return FailureClass::Transient;
    }

    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return FailureClass::AuthError;
    }

    // 412 Precondition Failed, 422 Unprocessable Entity, 451 Unavailable For Legal
    // Reasons: provider rejects this credential/account (billing, quota, model access,
    // region). These are key-specific — rotating to another credential may succeed, so
    // treat them like an auth error rather than a fatal client error that aborts.
    if matches!(status.as_u16(), 412 | 422 | 451) {
        return FailureClass::AuthError;
    }

    if status.is_client_error() {
        return FailureClass::Fatal;
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

pub(crate) fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after)
}

/// Resolve a backoff hint from a throttling response: prefer the `Retry-After`
/// header, then fall back to `retry_after` / `retryAfter` /
/// `retry_after_seconds` in a JSON error body. `pub(crate)` so the rotator
/// client shares a single (panic-safe) parser instead of re-deriving it.
pub(crate) fn retry_after_from_headers_and_body(
    headers: &HeaderMap,
    body: Option<&str>,
) -> Option<Duration> {
    retry_after_from_headers(headers).or_else(|| {
        body.and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok())
            .and_then(|value| retry_after_from_body(&value))
    })
}

pub(crate) fn retry_after_from_body(body: &serde_json::Value) -> Option<Duration> {
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

/// Parse a `Retry-After` value (integer or fractional seconds) into a `Duration`.
///
/// Rejects values `Duration::from_secs_f64` would panic on (NaN, negative,
/// infinite, or overflowing), so a malformed upstream header (e.g.
/// `Retry-After: -1` or `nan`) can never crash the request task. `pub(crate)`
/// so the rotator client reuses one safe parser.
pub(crate) fn parse_retry_after(raw: &str) -> Option<Duration> {
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    duration_from_f64_secs(raw.parse::<f64>().ok()?)
}

/// Convert fractional seconds to a `Duration`, returning `None` for any value
/// `from_secs_f64` would reject (and panic on): NaN, negative, infinite, or not
/// representable as a `Duration`. The last case matters at the boundary —
/// `u64::MAX + 1` (1.8446744073709552e19) equals `Duration::MAX.as_secs_f64()`
/// but rounds past `Duration::MAX`, so a naive `secs <= MAX.as_secs_f64()` guard
/// still panics. `try_from_secs_f64` returns `Result` and rejects all of these
/// instead of panicking.
fn duration_from_f64_secs(secs: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(secs).ok()
}

fn parse_retry_after_value(value: &serde_json::Value) -> Option<Duration> {
    match value {
        serde_json::Value::Number(num) => {
            if let Some(secs) = num.as_u64() {
                Some(Duration::from_secs(secs))
            } else {
                num.as_f64().and_then(duration_from_f64_secs)
            }
        }
        serde_json::Value::String(s) => parse_retry_after(s),
        _ => None,
    }
}

pub fn get_cooldown_duration(
    status: StatusCode,
    default: Duration,
    fallback: Duration,
) -> Duration {
    if status == StatusCode::TOO_MANY_REQUESTS {
        default
    } else {
        fallback
    }
}

pub fn get_fallback_from_body(status: StatusCode, body: Option<&str>) -> Duration {
    if status != StatusCode::TOO_MANY_REQUESTS {
        return Duration::from_secs(1);
    }

    let parsed_body = body.and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok());
    let retry_after = parsed_body
        .as_ref()
        .and_then(|body| body.get("retry_after"))
        .and_then(|v| v.as_u64())
        .map(Duration::from_secs)
        .or_else(|| {
            parsed_body
                .as_ref()
                .and_then(|body| body.get("retry_after"))
                .and_then(|v| v.as_f64())
                .map(|f| Duration::from_millis((f * 1000.0) as u64))
        });

    retry_after.unwrap_or_else(|| Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_uses_exponential_growth() {
        let backoff = get_retry_backoff(0, 1_000, 60_000);
        assert!(backoff >= Duration::from_millis(1_000));
        assert!(backoff <= Duration::from_millis(2_000));

        let backoff = get_retry_backoff(1, 1_000, 60_000);
        assert!(backoff >= Duration::from_millis(2_000));
        assert!(backoff <= Duration::from_millis(3_000));

        let backoff = get_retry_backoff(2, 1_000, 60_000);
        assert!(backoff >= Duration::from_millis(4_000));
        assert!(backoff <= Duration::from_millis(5_000));

        let backoff = get_retry_backoff(10, 1_000, 60_000);
        assert!(backoff >= Duration::from_millis(10_000));
        assert!(backoff <= Duration::from_millis(60_000));

        let backoff = get_retry_backoff(100, 1_000, 60_000);
        assert!(backoff >= Duration::from_millis(0));
        assert!(backoff <= Duration::from_millis(60_000));
    }

    #[test]
    fn backoff_respects_max() {
        let backoff = get_retry_backoff(10, 1_000, 5_000);
        assert!(backoff >= Duration::from_millis(0));
        assert!(backoff <= Duration::from_millis(5_000));
    }

    #[test]
    fn backoff_zero_base() {
        let backoff = get_retry_backoff(0, 0, 60_000);
        assert_eq!(backoff, Duration::from_millis(0));
    }

    #[test]
    fn backoff_zero_max() {
        let backoff = get_retry_backoff(0, 1_000, 0);
        assert_eq!(backoff, Duration::from_millis(0));
    }

    #[test]
    fn detect_ip_throttle_patterns() {
        assert!(detect_ip_throttle("rate limit exceeded for your ip"));
        assert!(detect_ip_throttle("too many requests from your ip"));
        assert!(!detect_ip_throttle("some other error message"));
    }

    #[test]
    fn detect_ip_throttle_case_insensitive() {
        assert!(detect_ip_throttle("rate limit exceeded for your ip"));
    }

    #[test]
    fn proxy_provider_429_without_retry_after_cools_down_key() {
        // Proxy-провайдер: 429 без retry_after → credential cooldown (ротация ключа).
        let decision = decide_retry_for_provider(
            FailureClass::RateLimit {
                retry_after: None,
                scope: ThrottleScope::Unknown,
            },
            0,
            1,
            Some("openrouter"),
            None,
        );
        assert!(matches!(decision, RetryDecision::CooldownKey { .. }));
    }

    #[test]
    fn explicit_ip_throttle_cools_down_provider_even_for_proxy() {
        // Явный IP-throttle всегда provider-level, даже для proxy-провайдера.
        let decision = decide_retry_for_provider(
            FailureClass::RateLimit {
                retry_after: None,
                scope: ThrottleScope::Ip,
            },
            0,
            1,
            Some("openrouter"),
            None,
        );
        assert!(matches!(decision, RetryDecision::CooldownProvider { .. }));
    }

    #[test]
    fn classifies_429_ip_indicator_body_as_ip_scope() {
        let headers = HeaderMap::new();
        let failure = classify_upstream_failure(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            Some(r#"{"error":{"message":"Rate limit exceeded for your IP"}}"#),
        );
        assert!(matches!(
            failure,
            FailureClass::RateLimit {
                scope: ThrottleScope::Ip,
                ..
            }
        ));
    }

    #[test]
    fn non_proxy_429_without_retry_after_cools_down_provider() {
        let decision = decide_retry_for_provider(
            FailureClass::RateLimit {
                retry_after: None,
                scope: ThrottleScope::Unknown,
            },
            0,
            1,
            Some("openai"),
            None,
        );
        assert!(matches!(decision, RetryDecision::CooldownProvider { .. }));
    }

    #[test]
    fn classifies_412_422_451_as_key_specific_auth_error() {
        let headers = HeaderMap::new();
        for status in [412u16, 422, 451] {
            let failure = classify_upstream_failure(
                StatusCode::from_u16(status).unwrap(),
                &headers,
                Some("{}"),
            );
            assert!(
                matches!(failure, FailureClass::AuthError),
                "status {status} should classify as AuthError (key-specific, rotates)"
            );
        }
    }

    #[test]
    fn parse_retry_after_rejects_values_that_would_panic() {
        // Each of these reaches `f64::parse` (the u64 arm fails); previously the
        // result was handed straight to Duration::from_secs_f64, which panics on
        // NaN/negative/infinite/overflow. They must now yield None instead.
        for raw in [
            "nan",
            "NaN",
            "-1",
            "-0.5",
            "inf",
            "infinity",
            "1e400",
            "abc",
            "",
            // Boundary: u64::MAX + 1 and its scientific form both equal
            // Duration::MAX.as_secs_f64() yet round past Duration::MAX, so a
            // naive `<= MAX.as_secs_f64()` guard still panics in from_secs_f64.
            "18446744073709551616",
            "1.8446744073709552e19",
        ] {
            assert_eq!(
                parse_retry_after(raw),
                None,
                "{raw:?} must parse to None, not panic"
            );
        }
    }

    #[test]
    fn parse_retry_after_handles_integer_and_fractional_seconds() {
        assert_eq!(parse_retry_after("60"), Some(Duration::from_secs(60)));
        assert_eq!(parse_retry_after("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));
    }

    #[test]
    fn retry_after_prefers_header_then_body_without_panicking() {
        // Header wins over body.
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(
            retry_after_from_headers_and_body(&headers, Some(r#"{"retry_after":99}"#)),
            Some(Duration::from_secs(30))
        );

        // No header -> fall back to a JSON body field.
        assert_eq!(
            retry_after_from_headers_and_body(
                &HeaderMap::new(),
                Some(r#"{"error":{"retry_after_seconds":7}}"#)
            ),
            Some(Duration::from_secs(7))
        );

        // A malformed header must not panic; fall back to the body.
        let mut bad = HeaderMap::new();
        bad.insert(RETRY_AFTER, "nan".parse().unwrap());
        assert_eq!(
            retry_after_from_headers_and_body(&bad, Some(r#"{"retry_after":5}"#)),
            Some(Duration::from_secs(5))
        );

        // Nothing parseable -> None.
        assert_eq!(
            retry_after_from_headers_and_body(&HeaderMap::new(), None),
            None
        );
    }
}
