use crate::errors::AppError;
use crate::routes::utils::{resolve_provider_for_model, upstream_response};
use crate::state::AppState;
use axum::extract::Query;
use axum::response::Response;
use axum::{
    Router,
    extract::{Path, State},
    response::Json,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;

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
    let provider = resolve_provider_from_body(&state, &req);
    let upstream = state.rotator.request(&provider, "batches", req).await?;
    upstream_response(upstream).await
}

async fn list_batches(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let provider = resolve_provider_from_query(&state, &params);
    let query_vec = upstream_query(params);
    let upstream = state
        .rotator
        .get_with_query(&provider, "batches", &query_vec)
        .await?;
    upstream_response(upstream).await
}

async fn retrieve_batch(
    State(state): State<AppState>,
    Path(batch_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let provider = resolve_provider_from_query(&state, &params);
    let upstream = state
        .rotator
        .get(&provider, &format!("batches/{batch_id}"))
        .await?;
    upstream_response(upstream).await
}

async fn cancel_batch(
    State(state): State<AppState>,
    Path(batch_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let provider = resolve_provider_from_query(&state, &params);
    let upstream = state
        .rotator
        .request(&provider, &format!("batches/{batch_id}/cancel"), json!({}))
        .await?;
    upstream_response(upstream).await
}

fn resolve_provider_from_body(state: &AppState, body: &Value) -> String {
    explicit_provider(body)
        .map(ToOwned::to_owned)
        .or_else(|| {
            body.get("model")
                .and_then(Value::as_str)
                .map(|model| resolve_provider_for_model(state, model))
        })
        .unwrap_or_else(|| "openai".to_owned())
}

fn resolve_provider_from_query(state: &AppState, params: &HashMap<String, String>) -> String {
    params
        .get("provider")
        .or_else(|| params.get("custom_llm_provider"))
        .cloned()
        .or_else(|| {
            params
                .get("model")
                .map(|model| resolve_provider_for_model(state, model))
        })
        .unwrap_or_else(|| "openai".to_owned())
}

fn explicit_provider(body: &Value) -> Option<&str> {
    body.get("provider")
        .or_else(|| body.get("custom_llm_provider"))
        .and_then(Value::as_str)
}

fn upstream_query(params: HashMap<String, String>) -> Vec<(String, String)> {
    params
        .into_iter()
        .filter(|(key, _)| key != "provider" && key != "custom_llm_provider" && key != "model")
        .collect()
}
