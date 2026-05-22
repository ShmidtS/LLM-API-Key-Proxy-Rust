use crate::errors::AppError;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use serde_json::Value;

pub async fn upstream_response(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = upstream.headers().clone();
    let bytes = upstream
        .bytes()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    Ok(builder.body(Body::from(bytes)).unwrap())
}

pub fn content_type(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
}

pub fn is_multipart(content_type: &str) -> bool {
    content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
}

pub fn is_json(content_type: &str) -> bool {
    content_type
        .to_ascii_lowercase()
        .starts_with("application/json")
}

pub fn json_body(body: axum::body::Bytes) -> Result<Value, AppError> {
    if body.is_empty() {
        return Ok(serde_json::json!({}));
    }
    Ok(serde_json::from_slice(&body)?)
}
