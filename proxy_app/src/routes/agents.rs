use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::Value;

use crate::{routes::utils::not_implemented, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agents/chat", post(agent_chat))
        .route("/agents/file_upload", post(agent_file_upload))
        .route("/agents/async_result", post(async_result))
        .route("/agents/conversation", post(conversation))
        .route("/v1/agents/chat", post(agent_chat))
        .route("/v1/agents/file-upload", post(agent_file_upload))
        .route("/v1/agents/async-result", get(async_result))
        .route("/v1/agents/conversation", post(conversation))
}

async fn agent_chat() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn agent_file_upload() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn async_result() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}

async fn conversation() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, not_implemented())
}
