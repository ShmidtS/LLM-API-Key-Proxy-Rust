use crate::compat::anthropic::{anthropic_to_openai_response, openai_to_anthropic_messages};
use crate::compat::anthropic_streaming::{AnthropicStreamTranslator, ChunkBatcher};
use crate::errors::AppError;
use crate::guardrails_adapter::{
    buffered_json_response, build_guardrail_request, decision_to_error_response,
    should_enable_guardrails,
};
use crate::routes::utils::{
    normalize_model_in_body, resolve_provider_for_model, upstream_response,
};
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, extract::State, response::Json, routing::post};
use futures::StreamExt;
use guardrails::{GuardrailDecision, RouteKind};
use models::chat::ChatCompletionRequest;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/chat/completions", post(chat_completions))
}

const MAX_GUARDRAIL_RETRY_ATTEMPTS: u32 = 1;
const MAX_GUARDRAIL_UPSTREAM_ATTEMPTS: usize = 4;

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
    let is_responses_compat = matches!(provider.as_str(), "elysiver" | "colin")
        || (provider == "openai" && is_openai_responses_model(&req.model));
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
    } else if is_responses_compat {
        "responses"
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
        let guardrails_enabled = state.guardrails.is_some()
            && should_enable_guardrails(RouteKind::ChatCompletions, &state.config.guardrails);
        if guardrails_enabled && state.config.guardrails.chat.validate_streaming {
            return Ok(crate::errors::invalid_request_error(
                "Streaming is not supported while chat guardrails.validate_streaming is enabled.",
            )
            .into_response());
        }
        if guardrails_enabled {
            tracing::trace!(
                "streaming chat request bypasses guardrails because validate_streaming is disabled"
            );
        }
        let upstream =
            request_chat_upstream(&state, &provider, upstream_path, upstream_body, &req.model)
                .await?;
        let status = upstream.status();
        let headers = upstream.headers().clone();
        let model = req.model.clone();
        let mut batcher = ChunkBatcher::new();
        let mut translator = AnthropicStreamTranslator::new(model.clone());
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
                    } else if is_responses_compat {
                        Bytes::from(translate_responses_sse_chunk(&bytes, &model))
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

    if state.guardrails.is_none()
        || !should_enable_guardrails(RouteKind::ChatCompletions, &state.config.guardrails)
    {
        let resp =
            request_chat_upstream(&state, &provider, upstream_path, upstream_body, &req.model)
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
        } else if is_responses_compat {
            responses_sse_to_json_response(resp, &req.model).await?
        } else {
            upstream_response(resp).await?
        };
        response
            .headers_mut()
            .insert("x-input-tokens", input_tokens_header);
        return Ok(response);
    }

    let Some(adapter) = state.guardrails.as_ref() else {
        let resp =
            request_chat_upstream(&state, &provider, upstream_path, upstream_body, &req.model)
                .await?;
        let mut response = if provider == "anthropic" {
            let (status, headers, response_json) =
                buffer_chat_response(resp, is_anthropic, &req.model).await?;
            buffered_json_response(status, &headers, response_json)
        } else {
            upstream_response(resp).await?
        };
        response
            .headers_mut()
            .insert("x-input-tokens", input_tokens_header);
        return Ok(response);
    };

    let mut guardrail_body = body.clone();
    let mut guardrail_attempts = 0;
    let mut upstream_attempts = 0;
    loop {
        let guardrail_request = build_guardrail_request(
            RouteKind::ChatCompletions,
            provider.clone(),
            upstream_path.to_owned(),
            req.model.clone(),
            guardrail_body.clone(),
            false,
        );
        let guardrail_request = adapter.preprocess_request(&guardrail_request)?;
        let preprocessed_body = guardrail_request.body.clone();
        let attempt_upstream_body = if is_anthropic {
            openai_to_anthropic_messages(&preprocessed_body)
        } else {
            preprocessed_body.clone()
        };
        if upstream_attempts >= MAX_GUARDRAIL_UPSTREAM_ATTEMPTS {
            return Err(guardrails::GuardrailError::MaxRetriesExceeded {
                attempts: guardrail_attempts,
            }
            .into());
        }
        upstream_attempts += state
            .rotator
            .max_retries()
            .saturating_add(1)
            .min(MAX_GUARDRAIL_UPSTREAM_ATTEMPTS.saturating_sub(upstream_attempts));
        let resp = request_chat_upstream(
            &state,
            &provider,
            upstream_path,
            attempt_upstream_body,
            &req.model,
        )
        .await?;
        let (status, headers, response_json) =
            buffer_chat_response(resp, is_anthropic, &req.model).await?;

        let decision = adapter
            .evaluate_non_streaming(&guardrail_request, &response_json)
            .await?;

        match decision {
            GuardrailDecision::Accept { response: accepted, .. } => {
                let mut response = buffered_json_response(status, &headers, accepted.body);
                response
                    .headers_mut()
                    .insert("x-input-tokens", input_tokens_header);
                if cfg!(debug_assertions) {
                    response
                        .headers_mut()
                        .insert("x-guardrail-trace", HeaderValue::from_static("accept"));
                }
                return Ok(response);
            }
            GuardrailDecision::Repair { response: repaired, .. } => {
                let mut response = buffered_json_response(status, &headers, repaired.body);
                response
                    .headers_mut()
                    .insert("x-input-tokens", input_tokens_header);
                if cfg!(debug_assertions) {
                    response
                        .headers_mut()
                        .insert("x-guardrail-trace", HeaderValue::from_static("repaired"));
                }
                return Ok(response);
            }
            GuardrailDecision::Retry { request, .. } => {
                let max_guardrail_retries = adapter
                    .config()
                    .max_guardrail_retries
                    .min(MAX_GUARDRAIL_RETRY_ATTEMPTS);
                if guardrail_attempts >= max_guardrail_retries {
                    return Err(guardrails::GuardrailError::MaxRetriesExceeded {
                        attempts: guardrail_attempts,
                    }
                    .into());
                }
                guardrail_body = request.body;
                guardrail_attempts += 1;
            }
            decision @ (GuardrailDecision::Reject { .. } | GuardrailDecision::Abort { .. }) => {
                if let Some(response) = decision_to_error_response(&decision) {
                    return Ok(response);
                }
            }
        }
    }
}

async fn buffer_chat_response(
    resp: reqwest::Response,
    is_anthropic: bool,
    model: &str,
) -> Result<(StatusCode, HeaderMap, Value), AppError> {
    let status =
        StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = resp.headers().clone();
    let data: Value = resp
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    let data = if is_anthropic {
        anthropic_to_openai_response(&data, model)
    } else {
        data
    };

    Ok((status, headers, data))
}

async fn responses_sse_to_json_response(
    resp: reqwest::Response,
    fallback_model: &str,
) -> Result<Response, AppError> {
    let status =
        StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    let body = aggregate_responses_sse(&bytes, fallback_model);

    Ok(Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap())
}

fn aggregate_responses_sse(bytes: &[u8], fallback_model: &str) -> Value {
    let mut id = "chatcmpl-responses-compat".to_owned();
    let mut created = now_unix_seconds();
    let mut model = fallback_model.to_owned();
    let mut content = String::new();
    let mut usage = json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0});

    for data in sse_data_records(bytes) {
        if data == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    content.push_str(delta);
                }
            }
            Some("response.created") | Some("response.completed") => {
                if let Some(response) = event.get("response") {
                    if let Some(value) = response.get("id").and_then(Value::as_str) {
                        id = value.to_owned();
                    }
                    if let Some(value) = response.get("created_at").and_then(Value::as_i64) {
                        created = value;
                    }
                    if let Some(value) = response.get("model").and_then(Value::as_str) {
                        model = value.to_owned();
                    }
                    if let Some(value) = response.get("usage") {
                        usage = response_usage_to_chat_usage(value);
                    }
                    if content.is_empty() {
                        content.push_str(&response_output_text(response));
                    }
                }
                if let Some(value) = event.get("usage") {
                    usage = response_usage_to_chat_usage(value);
                }
            }
            _ => {}
        }
    }

    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": usage
    })
}

fn translate_responses_sse_chunk(bytes: &[u8], fallback_model: &str) -> String {
    let mut output = String::new();
    for data in sse_data_records(bytes) {
        if data == "[DONE]" {
            output.push_str("data: [DONE]\n\n");
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("response.created") => {
                output.push_str(&chat_sse_payload(
                    &event,
                    fallback_model,
                    json!({"role": "assistant"}),
                    None,
                ));
            }
            Some("response.output_text.delta") => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                output.push_str(&chat_sse_payload(
                    &event,
                    fallback_model,
                    json!({"content": delta}),
                    None,
                ));
            }
            Some("response.completed") => {
                output.push_str(&chat_sse_payload(
                    &event,
                    fallback_model,
                    json!({}),
                    Some("stop"),
                ));
            }
            _ => {}
        }
    }
    output
}

fn chat_sse_payload(
    event: &Value,
    fallback_model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> String {
    let response = event.get("response");
    let id = response
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-responses-compat");
    let created = response
        .and_then(|value| value.get("created_at"))
        .and_then(Value::as_i64)
        .unwrap_or_else(now_unix_seconds);
    let model = response
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .unwrap_or(fallback_model);
    let payload = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]
    });
    format!("data: {payload}\n\n")
}

fn sse_data_records(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split("\n\n")
        .filter_map(|record| {
            let lines: Vec<_> = record
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect();
            (!lines.is_empty()).then(|| lines.join("\n"))
        })
        .collect()
}

fn response_output_text(response: &Value) -> String {
    response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect()
}

fn response_usage_to_chat_usage(usage: &Value) -> Value {
    json!({
        "prompt_tokens": usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        "completion_tokens": usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        "total_tokens": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0)
    })
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

async fn request_chat_upstream(
    state: &AppState,
    provider: &str,
    upstream_path: &str,
    upstream_body: Value,
    model: &str,
) -> Result<reqwest::Response, AppError> {
    tracing::info!(
        method = "POST",
        provider = %provider,
        model = %model,
        upstream_path = %upstream_path,
        "forwarding chat completion request"
    );

    let response = state
        .rotator
        .request(provider, upstream_path, upstream_body)
        .await?;
    tracing::info!(
        provider = %provider,
        status = %response.status(),
        "upstream chat completion response"
    );
    Ok(response)
}

fn is_openai_responses_model(model: &str) -> bool {
    let bare = model.strip_prefix("openai/").unwrap_or(model);
    bare.starts_with("gpt-5") || bare.starts_with("o4")
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
