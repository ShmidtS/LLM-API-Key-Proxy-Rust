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
        .route("/v1/batches", post(create_batch).get(list_batches))
        .route("/v1/batches/{batch_id}", get(retrieve_batch))
        .route("/v1/batches/{batch_id}/cancel", post(cancel_batch))
}

async fn create_batch(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let upstream = state.rotator.request("openai", "batches", req).await?;
    upstream_response(upstream).await
}

async fn list_batches(State(state): State<AppState>) -> Result<Response, AppError> {
    let upstream = state.rotator.get("openai", "batches").await?;
    upstream_response(upstream).await
}

async fn retrieve_batch(
    State(state): State<AppState>,
    Path(batch_id): Path<String>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .get("openai", &format!("batches/{batch_id}"))
        .await?;
    upstream_response(upstream).await
}

async fn cancel_batch(
    State(state): State<AppState>,
    Path(batch_id): Path<String>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .request("openai", &format!("batches/{batch_id}/cancel"), json!({}))
        .await?;
    upstream_response(upstream).await
}
