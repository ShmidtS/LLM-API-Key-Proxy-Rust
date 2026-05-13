use axum::{
    extract::State,
    response::Json,
    routing::post,
    Router,
};
use models::chat::{ChatCompletionRequest, ChatCompletionResponse};
use crate::errors::AppError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/chat/completions", post(chat_completions))
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Json<ChatCompletionResponse>, AppError> {
    let provider = "openai";
    let body = serde_json::to_value(&req)?;
    let resp = state.rotator.request(provider, "chat/completions", body).await?;
    let data: ChatCompletionResponse = resp.json().await.map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    Ok(Json(data))
}
