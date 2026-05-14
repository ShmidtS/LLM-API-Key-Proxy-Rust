use crate::errors::AppError;
use crate::routes::utils::{not_implemented, upstream_response};
use crate::state::AppState;
use axum::http::StatusCode;
use axum::response::Response;
use axum::{
    Router,
    extract::State,
    response::Json,
    routing::{get, post},
};
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/images/generations", post(generations))
        .route("/images/{image_id}", get(get_image))
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

async fn async_generations() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
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

async fn get_image() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
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
