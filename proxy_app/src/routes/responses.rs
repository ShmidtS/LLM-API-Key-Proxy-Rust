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
        .route("/responses", post(responses_post_passthrough))
        .route("/responses/{response_id}", get(get_response))
        .route("/v1/responses", post(responses_post_passthrough))
        .route("/v1/responses/{response_id}", get(get_response))
}

async fn get_response(
    State(state): State<AppState>,
    Path(response_id): Path<String>,
    OriginalUri(_uri): OriginalUri,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let query_vec = params.into_iter().collect::<Vec<_>>();
    let path = format!("responses/{response_id}");
    let upstream = state
        .rotator
        .get_with_query("openai", &path, &query_vec)
        .await?;
    upstream_response(upstream).await
}

async fn responses_post_passthrough(
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

fn upstream_path(path: &str) -> &str {
    path.strip_prefix("/v1/")
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path)
}
