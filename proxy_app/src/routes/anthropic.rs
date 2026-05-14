use crate::errors::AppError;
use crate::state::AppState;
use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Router, extract::State, response::Json, routing::post};
use futures::StreamExt;
use models::anthropic::{AnthropicCountTokensRequest, AnthropicMessagesRequest};
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/messages", post(create_message))
        .route("/v1/messages/count_tokens", post(count_tokens))
}

async fn create_message(
    State(state): State<AppState>,
    Json(req): Json<AnthropicMessagesRequest>,
) -> Result<Response, AppError> {
    let body = serde_json::to_value(&req)?;

    if req.stream == Some(true) {
        let upstream = state.rotator.request("anthropic", "messages", body).await?;
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

    let resp = state.rotator.request("anthropic", "messages", body).await?;
    let data: Value = resp
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    Ok(Json(data).into_response())
}

async fn count_tokens(
    State(state): State<AppState>,
    Json(req): Json<AnthropicCountTokensRequest>,
) -> Result<Json<Value>, AppError> {
    let body = serde_json::to_value(&req)?;

    let resp = state
        .rotator
        .request("anthropic", "messages/count_tokens", body)
        .await?;
    let data = resp
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    Ok(Json(data))
}
