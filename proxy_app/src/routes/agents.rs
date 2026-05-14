use axum::{
    Router,
    extract::{Query, State},
    response::Json,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/agents/chat", post(agent_chat))
        .route("/v1/agents/file-upload", post(agent_file_upload))
        .route("/v1/agents/async-result", get(async_result))
        .route("/v1/agents/conversation", post(conversation))
}

async fn agent_chat(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "agents.chat"}))
}

async fn agent_file_upload(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "agents.file_upload"}))
}

#[derive(Deserialize)]
struct AsyncResultParams {
    id: String,
}

async fn async_result(
    State(_state): State<AppState>,
    Query(params): Query<AsyncResultParams>,
) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "agents.async_result", "id": params.id}))
}

async fn conversation(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "agents.conversation"}))
}
