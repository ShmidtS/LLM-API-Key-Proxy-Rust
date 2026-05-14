use crate::errors::AppError;
use crate::routes::utils::{not_implemented, upstream_response};
use crate::state::AppState;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/video/generations", post(video_post_passthrough))
        .route("/video/status", get(video_status))
        .route("/v1/video/generate", post(video_post_passthrough))
        .route("/v1/video/generations", post(video_post_passthrough))
        .route("/v1/video/status", get(video_status))
        .route("/v1/video/{video_id}/status", get(video_status))
}

async fn video_status() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn video_post_passthrough(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .request("openai", upstream_path(uri.path()), req)
        .await?;
    upstream_response(upstream).await
}

fn upstream_path(path: &str) -> &str {
    path.strip_prefix("/v1/")
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path)
}
