use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::time::Instant;

pub async fn require_admin_auth(request: Request, next: Next) -> Response {
    let token = std::env::var("ADMIN_TOKEN").unwrap_or_default();
    if token.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            "Unauthorized: ADMIN_TOKEN not configured",
        )
            .into_response();
    }

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") && header[7..] == token => {
            next.run(request).await
        }
        _ => (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    }
}

pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    headers.insert(
        "Referrer-Policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response
}

pub async fn log_requests(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let uri = request.uri().clone();
    let headers = redacted_headers(request.headers());
    let (parts, body) = request.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%method, %uri, error = %error, "failed to read request body");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    let preview = request_body_preview(&String::from_utf8_lossy(&body_bytes));
    let request = Request::from_parts(parts, Body::from(body_bytes));

    let response = next.run(request).await;
    let status = response.status();
    let latency_ms = started.elapsed().as_millis();

    if status.is_client_error() || status.is_server_error() {
        tracing::warn!(
            %method,
            %uri,
            %status,
            latency_ms,
            headers = %headers,
            request_body_preview = %preview,
            "request failed"
        );
    } else {
        tracing::info!(%method, %uri, %status, latency_ms, "request completed");
    }

    response
}

fn redacted_headers(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let value = value.to_str().unwrap_or("<non-utf8>");
            let value =
                if name == axum::http::header::AUTHORIZATION && value.starts_with("Bearer sk-") {
                    "Bearer <redacted>"
                } else {
                    value
                };
            format!("{}: {}", name.as_str(), value)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn request_body_preview(body: &str) -> String {
    body.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, header};

    #[test]
    fn redacted_headers_hide_bearer_api_keys() {
        let request = HttpRequest::builder()
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer sk-test-secret")
            .body(Body::empty())
            .unwrap();

        let headers = redacted_headers(request.headers());

        assert!(headers.contains("authorization: Bearer <redacted>"));
        assert!(!headers.contains("sk-test-secret"));
    }

    #[test]
    fn request_body_preview_limits_to_first_200_chars() {
        let body = "x".repeat(250);

        let preview = request_body_preview(&body);

        assert_eq!(preview.len(), 200);
    }
}
