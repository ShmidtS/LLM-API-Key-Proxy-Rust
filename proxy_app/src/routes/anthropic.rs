use crate::compat::anthropic::{
    anthropic_to_openai_chat_request, openai_chat_to_anthropic_response,
};
use crate::compat::anthropic_streaming::{ChunkBatcher, OpenAiToAnthropicStreamTranslator};
use crate::errors::AppError;
use crate::routes::utils::normalize_model_in_body;
use crate::state::AppState;
use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Router, extract::State, response::Json, routing::post};
use futures::StreamExt;
use models::anthropic::AnthropicCountTokensRequest;
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/messages", post(create_message))
        .route("/v1/messages/count_tokens", post(count_tokens))
}

async fn create_message(
    State(state): State<AppState>,
    Json(mut body): Json<Value>,
) -> Result<Response, AppError> {
    let stream = body.get("stream").and_then(Value::as_bool) == Some(true);
    let provider = resolve_anthropic_provider_for_body(&state, &body);
    normalize_model_in_body(&mut body, &provider);
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();

    let is_native_anthropic = provider == "anthropic";
    let upstream_path = if is_native_anthropic {
        "messages"
    } else {
        "chat/completions"
    };
    let upstream_body = if is_native_anthropic {
        body
    } else {
        anthropic_to_openai_chat_request(&body)
    };

    if stream {
        tracing::info!(
            method = "POST",
            provider = %provider,
            model = %model,
            upstream_path = upstream_path,
            "forwarding anthropic message request"
        );
        let upstream = state
            .rotator
            .request(&provider, upstream_path, upstream_body)
            .await?;
        tracing::info!(
            provider = %provider,
            status = %upstream.status(),
            "upstream anthropic message response"
        );
        let status = upstream.status();
        let headers = upstream.headers().clone();
        let mut batcher = ChunkBatcher::new();
        let mut translator = OpenAiToAnthropicStreamTranslator::new(model.clone());
        let stream = upstream.bytes_stream().map(move |result| {
            result
                .map(|bytes| {
                    if is_native_anthropic {
                        bytes
                    } else {
                        let output = batcher
                            .push(bytes)
                            .iter()
                            .flat_map(|record| translator.translate_sse_record_to_sse(record))
                            .collect::<String>();
                        axum::body::Bytes::from(output)
                    }
                })
                .map_err(std::io::Error::other)
        });
        let mut builder = Response::builder().status(status);
        if let Some(ct) = headers.get(header::CONTENT_TYPE) {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        return Ok(builder.body(Body::from_stream(stream)).unwrap());
    }

    tracing::info!(
        method = "POST",
        provider = %provider,
        model = %model,
        upstream_path = upstream_path,
        "forwarding anthropic message request"
    );
    let resp = state
        .rotator
        .request(&provider, upstream_path, upstream_body)
        .await?;
    tracing::info!(
        provider = %provider,
        status = %resp.status(),
        "upstream anthropic message response"
    );
    let data: Value = resp
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    let data = if is_native_anthropic {
        data
    } else {
        openai_chat_to_anthropic_response(&data, &model)
    };
    Ok(Json(data).into_response())
}

async fn count_tokens(
    State(state): State<AppState>,
    Json(req): Json<AnthropicCountTokensRequest>,
) -> Result<Json<Value>, AppError> {
    let mut body = serde_json::to_value(&req)?;
    let provider = resolve_anthropic_provider_for_body(&state, &body);
    normalize_model_in_body(&mut body, &provider);
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();

    tracing::info!(
        method = "POST",
        provider = %provider,
        model = %model,
        upstream_path = "messages/count_tokens",
        "forwarding anthropic count tokens request"
    );
    let resp = state
        .rotator
        .request(&provider, "messages/count_tokens", body)
        .await?;
    tracing::info!(
        provider = %provider,
        status = %resp.status(),
        "upstream anthropic count tokens response"
    );
    let data = resp
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    Ok(Json(data))
}

fn resolve_anthropic_provider_for_body(state: &AppState, body: &Value) -> String {
    body.get("model")
        .and_then(Value::as_str)
        .and_then(|model| {
            state
                .registry
                .resolve_provider_by_model(model)
                .map(ToOwned::to_owned)
                .or_else(|| state.registry.find_provider_for_model(model))
        })
        .unwrap_or_else(|| "anthropic".to_owned())
}
