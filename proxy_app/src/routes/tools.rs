use crate::errors::AppError;
use crate::routes::utils::{not_implemented, upstream_response};
use crate::state::AppState;
use axum::extract::{OriginalUri, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{
    Json, Router,
    routing::{post},
};
use serde_json::Value;
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tools/web_search", post(legacy_tool))
        .route("/tools/tokenizer", post(legacy_tool))
        .route("/tools/layout", post(legacy_tool))
        .route("/tools/web_reader", post(legacy_tool))
        .route("/v1/tools/web-search", post(legacy_tool))
        .route("/v1/tools/tokenizer", post(legacy_tool))
        .route("/v1/tools/layout-parsing", post(legacy_tool))
        .route("/v1/tools/web-reader", post(legacy_tool))
        .route(
            "/v1/threads",
            post(tools_post_passthrough).get(tools_get_passthrough),
        )
        .route(
            "/v1/threads/{*path}",
            post(tools_post_passthrough).get(tools_get_passthrough),
        )
}

async fn legacy_tool() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn tools_post_passthrough(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let upstream = state
        .rotator
        .request("openai", upstream_path(uri.path()), req)
        .await?;
    upstream_response(upstream).await
}

async fn tools_get_passthrough(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let query_vec = params.into_iter().collect::<Vec<_>>();
    let upstream = state
        .rotator
        .get_with_query("openai", upstream_path(uri.path()), &query_vec)
        .await?;
    upstream_response(upstream).await
}

fn upstream_path(path: &str) -> &str {
    path.strip_prefix("/v1/")
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path)
}
