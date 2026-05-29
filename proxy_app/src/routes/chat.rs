use crate::compat::anthropic::{anthropic_to_openai_response, openai_to_anthropic_messages};
use crate::compat::anthropic_streaming::{AnthropicStreamTranslator, ChunkBatcher};
use crate::errors::AppError;
use crate::routes::utils::{
    normalize_model_in_body, resolve_provider_for_model, upstream_response,
};
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, extract::State, response::Json, routing::post};
use futures::StreamExt;
use models::chat::ChatCompletionRequest;
use serde_json::Value;
use tokio::time::{Duration, timeout};

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/chat/completions", post(chat_completions))
}

const CHAT_COMPLETION_TIMEOUT: Duration = Duration::from_secs(15);
const CHAT_COMPLETION_STREAM_TIMEOUT: Duration = Duration::from_secs(25);

async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Response, AppError> {
    if !state.registry.is_model_allowed(&req.model) {
        return Ok(crate::errors::invalid_request_error(format!(
            "Model not allowed: {}",
            req.model
        ))
        .into_response());
    }

    let provider = resolve_provider_for_model(&state, &req.model);
    let is_anthropic = provider == "anthropic";
    let mut body = serde_json::to_value(&req)?;
    normalize_model_in_body(&mut body, &provider);
    let override_temperature_zero = state.config.override_temperature_zero.as_deref();
    apply_temperature_override(&mut body, override_temperature_zero);
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
        let upstream = request_chat_upstream(
            &state,
            &provider,
            upstream_path,
            upstream_body,
            &req.model,
            CHAT_COMPLETION_STREAM_TIMEOUT,
        )
        .await?;
        let status = upstream.status();
        let headers = upstream.headers().clone();
        let model = req.model.clone();
        let mut batcher = ChunkBatcher::new();
        let mut translator = AnthropicStreamTranslator::new(model);
        let stream = upstream.bytes_stream().map(move |result| {
            result
                .map(|bytes| {
                    if is_anthropic {
                        let output = batcher
                            .push(bytes)
                            .iter()
                            .flat_map(|record| translator.translate_sse_record_to_sse(record))
                            .collect::<String>();
                        Bytes::from(output)
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

    let resp = request_chat_upstream(
        &state,
        &provider,
        upstream_path,
        upstream_body,
        &req.model,
        CHAT_COMPLETION_TIMEOUT,
    )
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

async fn request_chat_upstream(
    state: &AppState,
    provider: &str,
    upstream_path: &str,
    upstream_body: Value,
    model: &str,
    timeout_duration: Duration,
) -> Result<reqwest::Response, AppError> {
    tracing::info!(
        method = "POST",
        provider = %provider,
        model = %model,
        upstream_path = %upstream_path,
        timeout_secs = timeout_duration.as_secs(),
        "forwarding chat completion request"
    );

    match timeout(
        timeout_duration,
        state
            .rotator
            .request(provider, upstream_path, upstream_body),
    )
    .await
    {
        Ok(Ok(response)) => {
            tracing::info!(
                provider = %provider,
                status = %response.status(),
                "upstream chat completion response"
            );
            Ok(response)
        }
        Ok(Err(error)) => Err(error.into()),
        Err(_) => Err(AppError::UpstreamTimeout(format!(
            "Upstream provider {provider} timed out after {} seconds",
            timeout_duration.as_secs()
        ))),
    }
}

fn apply_temperature_override(body: &mut Value, override_temperature_zero: Option<&str>) {
    let Some(temperature) = body.get("temperature") else {
        return;
    };
    if temperature.as_f64() != Some(0.0) {
        return;
    }

    match override_temperature_zero
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("remove") => {
            if let Some(object) = body.as_object_mut() {
                object.remove("temperature");
            }
        }
        Some("set") | Some("true") | Some("1") | Some("yes") => {
            body["temperature"] = serde_json::json!(1.0);
        }
        _ => {}
    }
}
