//! Transparent retry for stale / dead pooled connections.
//!
//! Mirrors bifrost's `StaleConnectionRetryIfErr`
//! (`core/network/http.go`): LLM inference requests are POSTs, which HTTP
//! clients will not transparently retry even when the failure is caused by a
//! dead connection pulled from the keep-alive pool. With short upstream
//! keep-alives (e.g. vLLM's default 5s) several pooled connections can be dead
//! at once, so a single request can hit a stale connection and still fail even
//! after one reconnect.
//!
//! Retrying is safe here because these failures occur *before* the upstream
//! processes the request — during TCP connect, TLS handshake, request-body
//! write, or response-header parse — so re-issuing the POST cannot create a
//! duplicate inference. The retry uses the *same* credential: a network blip is
//! not credential-specific, and rotating on every dead connection would burn
//! keys (and, on a single-key pool, fail the client outright).
//!
//! Crucially this excludes generic *read* timeouts, which can fire while the
//! upstream is mid-processing and would risk duplicate POSTs.

use std::error::Error as StdError;

/// Lower-cased error fragments that indicate a dead/reused connection or a
/// pre-commit transport failure. Kept aligned with bifrost's matcher so we
/// recover from the same classes of transient connection failure.
///
/// Deliberately excludes bare timeout wording: reqwest/hyper surface a read
/// (or processing) timeout with the same OS string ("operation timed out")
/// as a connect timeout, and the two are indistinguishable by text. Retrying a
/// read timeout would risk a duplicate inference, so timeouts are handled
/// structurally via `is_connect()` / `is_timeout()` instead of pattern matching.
const STALE_CONNECTION_PATTERNS: &[&str] = &[
    "connection reset by peer",
    "connection reset",
    "broken pipe",
    "incomplete message",
    "connection closed before message completed",
    "end of file before message length reached",
    "cannot find whitespace",
    "unexpected eof",
    "unexpected end of file",
    "server closed connection",
    "use of closed network connection",
];

fn matches_stale_pattern(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    STALE_CONNECTION_PATTERNS.iter().any(|p| lower.contains(p))
}

/// Returns true when `err` represents a stale/dead pooled connection or any
/// transport failure that happens before the upstream commits the request.
///
/// Decision order:
/// 1. `is_connect()` — TCP/TLS dial failures, dead pooled connections, and
///    connect-stage timeouts. The server never saw the request, so retrying is
///    always safe.
/// 2. `is_timeout()` (and not connect) — a read/processing timeout. The
///    upstream may already be mid-processing the POST, so retrying would risk a
///    duplicate inference. Never retry these.
/// 3. Otherwise walk the cause chain for hyper/low-level write & early-read
///    messages that occur before the upstream commits (reset, broken pipe,
///    incomplete message, EOF before headers, stale chunked framing).
pub fn is_stale_connection_error(err: &reqwest::Error) -> bool {
    // Connect-stage failures: safe to retry unconditionally.
    if err.is_connect() {
        return true;
    }

    // A pure (non-connect) timeout is a read/processing timeout: retrying risks
    // a duplicate inference. This guard is what upholds the module's no-duplicate
    // invariant; do not weaken it.
    if err.is_timeout() {
        return false;
    }

    // Walk the whole cause chain: reqwest wraps hyper, which wraps io errors.
    // The dead-pooled-connection messages surface on inner layers, often
    // classified by reqwest as request/body errors rather than connect ones.
    let mut current: Option<&dyn StdError> = Some(err);
    while let Some(e) = current {
        if matches_stale_pattern(&e.to_string()) {
            return true;
        }
        current = e.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_matcher_covers_bifrost_failure_classes() {
        for msg in [
            "connection reset by peer",
            "Broken pipe (os error 32)",
            "incomplete message",
            "connection closed before message completed",
            "cannot find whitespace in the first line of response",
            "unexpected EOF while parsing",
            "end of file before message length reached",
            "server closed connection without response",
        ] {
            assert!(matches_stale_pattern(msg), "should match: {msg}");
        }
        assert!(!matches_stale_pattern("invalid API key"));
        assert!(!matches_stale_pattern("404 not found"));
        // Bare timeout wording must NOT match: read and connect timeouts share
        // this OS string and are indistinguishable by text, so timeouts are
        // handled structurally via is_connect()/is_timeout() instead. Matching
        // it here would let a read-timeout slip into the retry path.
        assert!(!matches_stale_pattern("operation timed out (os error 110)"));
    }

    #[test]
    fn chain_walk_detects_inner_io_error_message() {
        // Simulate reqwest wrapping a low-level io::Error whose Display carries
        // a stale-connection message. reqwest::Error cannot be constructed
        // directly in unit tests, so we exercise the same source-chain walker
        // on a stand-in io::Error and confirm it surfaces the inner message.
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "connection reset");
        let mut current: Option<&dyn StdError> = Some(&inner);
        let mut found = false;
        while let Some(e) = current {
            if matches_stale_pattern(&e.to_string()) {
                found = true;
                break;
            }
            current = e.source();
        }
        assert!(found, "chain walker must surface inner stale-connection text");
    }
}
