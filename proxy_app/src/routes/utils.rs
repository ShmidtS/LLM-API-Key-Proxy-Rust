use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;
use crate::errors::AppError;

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
