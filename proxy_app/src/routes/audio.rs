use crate::errors::AppError;
use crate::routes::utils::upstream_response;
use crate::state::AppState;
use axum::body::Bytes;
use axum::response::Response;
use axum::{Router, extract::State, routing::post};
use serde_json::{Value, json};

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

async fn transcriptions(State(state): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    let req = json_body(body)?;
    let upstream = state
        .rotator
        .request("openai", "audio/transcriptions", req)
        .await?;
    upstream_response(upstream).await
}

fn json_body(body: Bytes) -> Result<Value, AppError> {
    if body.is_empty() {
        return Ok(json!({}));
    }

    Ok(serde_json::from_slice(&body)?)
}
