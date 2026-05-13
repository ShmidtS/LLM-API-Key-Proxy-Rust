use crate::errors::AppError;
use crate::state::AppState;
use axum::body::Body;
use axum::response::IntoResponse;
use axum::{Router, extract::State, response::Json, routing::post};
use axum::http::{StatusCode, header};
use models::chat::{ChatCompletionRequest, ChatCompletionResponse};

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/chat/completions", post(chat_completions))
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<axum::response::Response, AppError> {
    if req.stream == Some(true) {
        let sse_body = "data: {\"id\":\"chatcmpl-placeholder\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"placeholder\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n\
                        data: {\"id\":\"chatcmpl-placeholder\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"placeholder\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
                        data: [DONE]\n\n";
        return Ok(axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(sse_body))
            .unwrap());
    }

    let provider = "openai";
    let body = serde_json::to_value(&req)?;
    let resp = state
        .rotator
        .request(provider, "chat/completions", body)
        .await?;
    let data: ChatCompletionResponse = resp
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    Ok(Json(data).into_response())
}
