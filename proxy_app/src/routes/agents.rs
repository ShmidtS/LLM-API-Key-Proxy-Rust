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
        .route("/v1/agents/chat", post(agent_chat))
        .route("/v1/agents/file-upload", post(agent_file_upload))
        .route("/v1/agents/async-result/{id}", get(async_result))
        .route("/v1/agents/conversation/{id}", get(conversation))
}

async fn agent_chat(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "agents.chat"}))
}

async fn agent_file_upload(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "agents.file_upload"}))
}

async fn async_result(State(_state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "agents.async_result", "id": id}))
}

async fn conversation(State(_state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    Json(json!({"object": "placeholder", "route": "agents.conversation", "id": id}))
}
