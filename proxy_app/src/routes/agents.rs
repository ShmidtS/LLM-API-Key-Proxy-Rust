use crate::errors::AppError;
use crate::routes::utils::upstream_response;
use crate::state::AppState;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::response::Response;
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::Value;
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agents/chat", post(agents_post_passthrough))
        .route("/agents/file_upload", post(agents_post_passthrough))
        .route("/agents/async_result", get(agents_get_passthrough))
        .route("/agents/conversation", post(agents_post_passthrough))
        .route("/v1/agents", post(create_agent))
        .route("/v1/agents/{id}", get(get_agent).post(update_agent))
        .route("/v1/agents/chat", post(agents_post_passthrough))
        .route("/v1/agents/file-upload", post(agents_post_passthrough))
        .route("/v1/agents/async-result", get(agents_get_passthrough))
        .route("/v1/agents/conversation", post(agents_post_passthrough))
}

async fn agents_post_passthrough(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .request("openai", &agents_upstream_path(uri.path()), req)
        .await?;
    upstream_response(upstream).await
}

async fn agents_get_passthrough(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let query_vec = params.into_iter().collect::<Vec<_>>();
    let upstream = state
        .rotator
        .get_with_query("openai", &agents_upstream_path(uri.path()), &query_vec)
        .await?;
    upstream_response(upstream).await
}

fn agents_upstream_path(path: &str) -> String {
    let normalized = path
        .strip_prefix("/v1/")
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path);
    normalized.replace('-', "_")
}

async fn create_agent(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let upstream = state.rotator.request("openai", "agents", req).await?;
    upstream_response(upstream).await
}

async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let query_vec = params.into_iter().collect::<Vec<_>>();
    let upstream = state
        .rotator
        .get_with_query("openai", &format!("agents/{id}"), &query_vec)
        .await?;
    upstream_response(upstream).await
}

async fn update_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .request("openai", &format!("agents/{id}"), req)
        .await?;
    upstream_response(upstream).await
}
