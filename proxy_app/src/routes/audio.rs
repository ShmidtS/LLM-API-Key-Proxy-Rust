use crate::errors::AppError;
use crate::routes::utils::{content_type, is_json, is_multipart, json_body, upstream_response};
use crate::state::AppState;
use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::{Router, extract::State, routing::post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/audio/speech", post(speech))
        .route("/v1/audio/transcriptions", post(transcriptions))
}

async fn speech(State(state): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    let req = json_body(body)?;
    let upstream = state.rotator.request("openai", "audio/speech", req).await?;
    upstream_response(upstream).await
}

async fn transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let Some(content_type) = content_type(&headers) else {
        return Err(AppError::BadRequest("missing content-type".into()));
    };

    let upstream = if is_multipart(content_type) {
        state
            .rotator
            .request_raw("openai", "audio/transcriptions", body, content_type)
            .await?
    } else if is_json(content_type) {
        state
            .rotator
            .request("openai", "audio/transcriptions", json_body(body)?)
            .await?
    } else {
        return Err(AppError::BadRequest(format!(
            "unsupported content-type: {content_type}"
        )));
    };

    upstream_response(upstream).await
}
