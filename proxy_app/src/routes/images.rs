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
        .route("/v1/images/generations", post(generations))
        .route("/v1/images/generations/async", post(async_generations))
        .route("/v1/images/edits", post(edits))
        .route("/v1/images/variations", post(variations))
        .route("/v1/images/{image_id}", get(get_image))
}

async fn generations(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    proxy_image_request(state, "images/generations", req).await
}

async fn async_generations() -> Json<Value> {
    Json(json!({"id": "img_placeholder", "object": "image.generation"}))
}

async fn edits(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    proxy_image_request(state, "images/edits", req).await
}

async fn variations(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    proxy_image_request(state, "images/variations", req).await
}

async fn get_image(Path(image_id): Path<String>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "images.get", "id": image_id}))
}

async fn proxy_image_request(
    state: AppState,
    path: &str,
    req: Value,
) -> Result<Response, AppError> {
    let provider = req
        .get("model")
        .and_then(Value::as_str)
        .and_then(|model| state.registry.find_provider_for_model(model))
        .unwrap_or_else(|| "openai".to_owned());
    let upstream = state.rotator.request(&provider, path, req).await?;
    upstream_response(upstream).await
}
