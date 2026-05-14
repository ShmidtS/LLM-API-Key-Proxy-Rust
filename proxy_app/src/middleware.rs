use axum::{
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::time::Instant;
use uuid::Uuid;

use crate::{errors, state::AppState};

pub async fn require_admin_auth(
    State(app_state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !app_state.config.auth_enabled {
        return next.run(request).await;
    }

    let Some(token) = app_state.config.admin_token.as_deref() else {
        return errors::authentication_error().into_response();
    };

    if bearer_token(request.headers()).is_some_and(|header_token| header_token == token) {
        next.run(request).await
    } else {
        errors::authentication_error().into_response()
    }
}

pub async fn require_proxy_auth(
    State(app_state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !app_state.config.auth_enabled {
        return next.run(request).await;
    }

    let auth_token = bearer_token(request.headers());
    let api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|h| h.to_str().ok());

    if auth_token
        .into_iter()
        .chain(api_key)
        .any(|token| app_state.config.api_keys.iter().any(|key| key == token))
    {
        next.run(request).await
    } else {
        errors::authentication_error().into_response()
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
}

fn body_exceeds_limit(app_state: &AppState, headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > app_state.config.max_body_bytes)
}

pub async fn reject_oversized_body(
    State(app_state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if body_exceeds_limit(&app_state, request.headers()) {
        return errors::payload_too_large_error().into_response();
    }

    next.run(request).await
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

pub async fn add_request_id(request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

pub async fn log_requests(
    State(app_state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let uri = request.uri().clone();
    let headers = redacted_headers(request.headers());

    let (request, preview) = if app_state.config.log_request_body {
        let (parts, body) = request.into_parts();
        let body_bytes = match to_bytes(body, 2048).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%method, %uri, error = %error, "failed to read request body");
                return StatusCode::BAD_REQUEST.into_response();
            }
        };
        let preview = request_body_preview(&String::from_utf8_lossy(&body_bytes));
        (Request::from_parts(parts, Body::from(body_bytes)), preview)
    } else {
        (request, "<body redacted>".to_owned())
    };

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
            let value = if name == axum::http::header::AUTHORIZATION && value.starts_with("Bearer ")
            {
                "Bearer <redacted>"
            } else if name.as_str().eq_ignore_ascii_case("x-api-key") {
                "<redacted>"
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
            .header(header::AUTHORIZATION, "Bearer admin-token-123")
            .body(Body::empty())
            .unwrap();

        let headers = redacted_headers(request.headers());

        assert!(headers.contains("authorization: Bearer <redacted>"));
        assert!(!headers.contains("admin-token-123"));
    }

    #[test]
    fn redacted_headers_hide_x_api_key_values() {
        let request = HttpRequest::builder()
            .uri("/v1/chat/completions")
            .header("x-api-key", "proxy-secret")
            .body(Body::empty())
            .unwrap();

        let headers = redacted_headers(request.headers());

        assert!(headers.contains("x-api-key: <redacted>"));
        assert!(!headers.contains("proxy-secret"));
    }

    #[test]
    fn request_body_preview_limits_to_first_200_chars() {
        let body = "x".repeat(250);

        let preview = request_body_preview(&body);

        assert_eq!(preview.len(), 200);
    }
}
