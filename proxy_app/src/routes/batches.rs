use axum::{
    Router,
    extract::{Path, State},
    response::Json,
    routing::{get, post},
};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/batches", post(create_batch).get(list_batches))
        .route("/v1/batches/{batch_id}", get(retrieve_batch))
        .route("/v1/batches/{batch_id}/cancel", post(cancel_batch))
}

async fn create_batch(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "batches.create"}))
}

async fn list_batches(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "batches.list", "data": []}))
}

async fn retrieve_batch(
    State(_state): State<AppState>,
    Path(batch_id): Path<String>,
) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "batches.retrieve", "id": batch_id}))
}

async fn cancel_batch(State(_state): State<AppState>, Path(batch_id): Path<String>) -> Json<Value> {
    Json(
        json!({"object": "placeholder", "route": "batches.cancel", "id": batch_id, "cancelled": true}),
    )
}
