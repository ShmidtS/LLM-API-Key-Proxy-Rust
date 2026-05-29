use crate::errors::AppError;
use crate::routes::utils::{
    normalize_model_in_body, resolve_provider_for_model, upstream_response,
};
use crate::state::AppState;
use axum::response::Response;
use axum::{
    Json, Router,
    extract::{Path, State},
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

async fn async_generations(
    State(state): State<AppState>,
    Json(mut req): Json<Value>,
) -> Result<Response, AppError> {
    let model = req
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    normalize_model_in_body(&mut req, "zai");
    tracing::info!(
        method = "POST",
        provider = "zai",
        model = %model,
        upstream_path = "images/generations",
        "forwarding image generation request"
    );
    let upstream = state
        .rotator
        .request("zai", "images/generations", req)
        .await?;
    tracing::info!(
        provider = "zai",
        status = %upstream.status(),
        "upstream image generation response"
    );
    upstream_response(upstream).await
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

async fn get_image(
    State(state): State<AppState>,
    Path(image_id): Path<String>,
) -> Result<Response, AppError> {
    let path = format!("images/{image_id}");
    tracing::info!(
        method = "GET",
        provider = "zai",
        upstream_path = %path,
        "forwarding image retrieval request"
    );
    let upstream = state.rotator.get("zai", &path).await?;
    tracing::info!(
        provider = "zai",
        status = %upstream.status(),
        "upstream image retrieval response"
    );
    upstream_response(upstream).await
}

async fn proxy_image_request(
    state: AppState,
    path: &str,
    mut req: Value,
) -> Result<Response, AppError> {
    let model = req
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let provider = req
        .get("model")
        .and_then(Value::as_str)
        .map(|model| resolve_provider_for_model(&state, model))
        .unwrap_or_else(|| "openai".to_owned());
    normalize_model_in_body(&mut req, &provider);
    tracing::info!(
        method = "POST",
        provider = %provider,
        model = %model,
        upstream_path = %path,
        "forwarding image request"
    );
    let upstream = state.rotator.request(&provider, path, req).await?;
    tracing::info!(
        provider = %provider,
        status = %upstream.status(),
        "upstream image response"
    );
    upstream_response(upstream).await
}
