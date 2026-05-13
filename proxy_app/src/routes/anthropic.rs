use crate::errors::AppError;
use crate::state::AppState;
use axum::{Router, extract::State, response::Json, routing::post};
use models::anthropic::{AnthropicCountTokensRequest, AnthropicMessagesRequest};
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/messages", post(create_message))
        .route("/v1/messages/count_tokens", post(count_tokens))
}

async fn create_message(
    State(_state): State<AppState>,
    Json(req): Json<AnthropicMessagesRequest>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({
        "id": "msg_placeholder",
        "type": "message",
        "role": "assistant",
        "content": [],
        "model": req.model,
        "stop_reason": null,
        "stop_sequence": null,
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0
        }
    })))
}

async fn count_tokens(
    State(_state): State<AppState>,
    Json(_req): Json<AnthropicCountTokensRequest>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({ "input_tokens": 0 })))
}
