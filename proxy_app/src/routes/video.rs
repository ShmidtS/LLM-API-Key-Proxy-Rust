use crate::{routes::utils::not_implemented, state::AppState};
use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/video/generations", post(generations))
        .route("/video/status", get(video_status))
        .route("/v1/video/generate", post(generations))
        .route("/v1/video/generations", post(generations))
        .route("/v1/video/status", get(video_status))
        .route("/v1/video/{video_id}/status", get(video_status))
}

async fn generations() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn video_status() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}
