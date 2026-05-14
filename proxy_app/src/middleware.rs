use axum::{
    extract::Request,
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

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
