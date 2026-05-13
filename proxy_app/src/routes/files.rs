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
        .route("/v1/files", post(upload_file).get(list_files))
        .route(
            "/v1/files/{file_id}",
            get(retrieve_file).delete(delete_file),
        )
}

async fn upload_file(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "files.upload"}))
}

async fn list_files(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "files.list", "data": []}))
}

async fn retrieve_file(State(_state): State<AppState>, Path(file_id): Path<String>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "files.retrieve", "id": file_id}))
}

async fn delete_file(State(_state): State<AppState>, Path(file_id): Path<String>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "files.delete", "id": file_id, "deleted": true}))
}
