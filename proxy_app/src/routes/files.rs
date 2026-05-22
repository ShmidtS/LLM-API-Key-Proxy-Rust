use crate::errors::AppError;
use crate::routes::utils::{content_type, is_json, is_multipart, json_body, upstream_response};
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::Query;
use axum::response::Response;
use axum::{
    Router,
    extract::{Path, State},
    routing::{get, post},
};
use std::collections::HashMap;

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
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let Some(content_type) = content_type(&headers) else {
        return Err(AppError::BadRequest("missing content-type".into()));
    };

    let upstream = if is_multipart(content_type) {
        state
            .rotator
            .request_raw("openai", "files", body, content_type)
            .await?
    } else if is_json(content_type) {
        state
            .rotator
            .request("openai", "files", json_body(body)?)
            .await?
    } else {
        return Err(AppError::BadRequest(format!(
            "unsupported content-type: {content_type}"
        )));
    };

    upstream_response(upstream).await
}

async fn list_files(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let query_vec = params.into_iter().collect::<Vec<_>>();
    let upstream = state
        .rotator
        .get_with_query("openai", "files", &query_vec)
        .await?;
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
