use crate::errors::AppError;
use crate::state::AppState;
use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Router, extract::State, response::Json, routing::post};
use futures::StreamExt;
use models::chat::{ChatCompletionRequest, ChatCompletionResponse};

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/chat/completions", post(chat_completions))
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Response, AppError> {
    let provider = state
        .registry
        .find_provider_for_model(&req.model)
        .unwrap_or_else(|| "openai".to_owned());
    let body = serde_json::to_value(&req)?;

    if req.stream == Some(true) {
        let upstream = state
            .rotator
            .request(&provider, "chat/completions", body)
            .await?;
        let status = upstream.status();
        let headers = upstream.headers().clone();
        let stream = upstream
            .bytes_stream()
            .map(|result| result.map_err(std::io::Error::other));
        let mut builder = Response::builder().status(status);
        if let Some(ct) = headers.get(header::CONTENT_TYPE) {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        return Ok(builder.body(Body::from_stream(stream)).unwrap());
    }

    let resp = state
        .rotator
        .request(&provider, "chat/completions", body)
        .await?;
    let data: ChatCompletionResponse = resp
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    Ok(Json(data).into_response())
}
