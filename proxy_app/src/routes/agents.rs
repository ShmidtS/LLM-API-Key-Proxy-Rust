use crate::errors::AppError;
use crate::routes::utils::{not_implemented, upstream_response};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::Value;
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agents/chat", post(legacy_agent))
        .route("/agents/file_upload", post(legacy_agent))
        .route("/agents/async_result", post(legacy_agent))
        .route("/agents/conversation", post(legacy_agent))
        .route("/v1/agents", post(create_agent))
        .route("/v1/agents/{id}", get(get_agent).post(update_agent))
        .route("/v1/agents/chat", post(legacy_agent))
        .route("/v1/agents/file-upload", post(legacy_agent))
        .route("/v1/agents/async-result", get(legacy_agent))
        .route("/v1/agents/conversation", post(legacy_agent))
}

async fn legacy_agent() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
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
