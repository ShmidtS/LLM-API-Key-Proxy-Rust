use crate::errors::AppError;
use crate::routes::utils::upstream_response;
use crate::state::AppState;
use axum::response::Response;
use axum::{
    Router,
    extract::{Path, State},
    response::Json,
    routing::{get, post},
};
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/video/generate", post(generations))
        .route("/v1/video/generations", post(generations))
        .route("/v1/video/{video_id}/status", get(video_status))
}

async fn generations(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let provider = req
        .get("model")
        .and_then(Value::as_str)
        .and_then(|model| state.registry.find_provider_for_model(model))
        .unwrap_or_else(|| "openai".to_owned());
    let upstream = state
        .rotator
        .request(&provider, "videos/generations", req)
        .await?;
    upstream_response(upstream).await
}

async fn video_status(Path(video_id): Path<String>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "video.status", "id": video_id}))
}
