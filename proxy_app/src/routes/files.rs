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
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/files", post(upload_file).get(list_files))
        .route(
            "/v1/files/{file_id}",
            get(retrieve_file).delete(delete_file),
        )
        .route("/v1/files/{file_id}/content", get(file_content))
}

async fn upload_file(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let upstream = state.rotator.request("openai", "files", req).await?;
    upstream_response(upstream).await
}

async fn list_files(State(state): State<AppState>) -> Result<Response, AppError> {
    let upstream = state.rotator.get("openai", "files").await?;
    upstream_response(upstream).await
}

async fn retrieve_file(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .get("openai", &format!("files/{file_id}"))
        .await?;
    upstream_response(upstream).await
}

async fn delete_file(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .delete("openai", &format!("files/{file_id}"))
        .await?;
    upstream_response(upstream).await
}

async fn file_content(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .get("openai", &format!("files/{file_id}/content"))
        .await?;
    upstream_response(upstream).await
}

