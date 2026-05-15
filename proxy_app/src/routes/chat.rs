use crate::compat::anthropic::{
    anthropic_stream_to_openai_sse, anthropic_to_openai_response, openai_to_anthropic_messages,
};
use crate::errors::AppError;
use crate::routes::utils::upstream_response;
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, extract::State, response::Json, routing::post};
use futures::StreamExt;
use models::chat::ChatCompletionRequest;
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/chat/completions", post(chat_completions))
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Response, AppError> {
    if !state.registry.is_model_allowed(&req.model) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Model not allowed"})),
        )
            .into_response());
    }

    let provider = state
        .registry
        .resolve_provider_by_model(&req.model)
        .unwrap_or("openai")
        .to_owned();
    let is_anthropic = provider == "anthropic";
    let body = serde_json::to_value(&req)?;
    let upstream_body = if is_anthropic {
        openai_to_anthropic_messages(&body)
    } else {
        body.clone()
    };
    let upstream_path = if is_anthropic {
        "messages"
    } else {
        "chat/completions"
    };
    let input_tokens = body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .map(|messages| rotator::tokenizer::count_chat_tokens(messages, &req.model))
        .unwrap_or(0);
    let input_tokens_header = HeaderValue::from_str(&input_tokens.to_string())
        .unwrap_or_else(|_| HeaderValue::from_static("0"));

    if req.stream == Some(true) {
        let upstream = state
            .rotator
            .request(&provider, upstream_path, upstream_body)
            .await?;
        let status = upstream.status();
        let headers = upstream.headers().clone();
        let model = req.model.clone();
        let stream = upstream.bytes_stream().map(move |result| {
            result
                .map(|bytes| {
                    if is_anthropic {
                        anthropic_stream_to_openai_sse(
                            std::str::from_utf8(&bytes).unwrap_or_default(),
                            &model,
                        )
                        .map(Bytes::from)
                        .unwrap_or_default()
                    } else {
                        bytes
                    }
                })
                .map_err(std::io::Error::other)
        });
        let mut builder = Response::builder().status(status);
        if let Some(ct) = headers.get(header::CONTENT_TYPE) {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        builder = builder.header("x-input-tokens", input_tokens_header);
        return Ok(builder.body(Body::from_stream(stream)).unwrap());
    }

    let resp = state
        .rotator
        .request(&provider, upstream_path, upstream_body)
        .await?;
    let mut response = if provider == "anthropic" {
        let status = resp.status();
        let headers = resp.headers().clone();
        let data: Value = resp
            .json()
            .await
            .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
        let data = anthropic_to_openai_response(&data, &req.model);
        let mut builder = Response::builder().status(status);
        if let Some(ct) = headers.get(header::CONTENT_TYPE) {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        builder.body(Body::from(data.to_string())).unwrap()
    } else {
        upstream_response(resp).await?
    };
    response
        .headers_mut()
        .insert("x-input-tokens", input_tokens_header);
    Ok(response)
}
