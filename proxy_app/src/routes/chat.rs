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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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

    if rotator::is_image_only_model(&req.model) {
        return handle_image_only_request(state, req).await;
    }

    let provider = if is_openai_responses_model(&req.model) {
        "openai".to_owned()
    } else {
        resolve_provider_for_model(&state, &req.model)
    };
    let is_anthropic = provider == "anthropic";
    let is_responses_compat = matches!(provider.as_str(), "elysiver" | "colin")
        || (provider == "openai" && is_openai_responses_model(&req.model));
    let mut body = serde_json::to_value(&req)?;
    normalize_model_in_body(&mut body, &provider);
    let override_temperature_zero = state.config.override_temperature_zero.as_deref();
    apply_temperature_override(&mut body, override_temperature_zero);
    let input_tokens = body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .map(|messages| rotator::tokenizer::count_chat_tokens(messages, &req.model))
        .unwrap_or(0);
    let auto_max_tokens =
        rotator::calculate_max_tokens(&req.model, input_tokens as u32, req.max_tokens, 1000);
    if req.max_tokens.is_none() {
        body["max_tokens"] = serde_json::json!(auto_max_tokens);
    } else if let Some(existing) = req.max_tokens
        && existing > auto_max_tokens
    {
        body["max_tokens"] = serde_json::json!(auto_max_tokens);
    }
    let body = Arc::new(body);
    let upstream_body = if is_anthropic {
        openai_to_anthropic_messages(&body)
    } else {
        (*body).clone()
    };
    let upstream_path = if is_anthropic {
        "messages"
    } else if is_responses_compat {
        "responses"
    } else {
        "chat/completions"
    };
    let input_tokens_header = HeaderValue::from_str(&input_tokens.to_string())
        .unwrap_or_else(|_| HeaderValue::from_static("0"));

    if req.stream == Some(true) {
        let guardrails_enabled = state.guardrails.is_some()
            && should_enable_guardrails(RouteKind::ChatCompletions, &state.config.guardrails);
        if !guardrails_enabled {
            let upstream =
                request_chat_upstream(&state, &provider, upstream_path, upstream_body, &req.model)
                    .await?;
            // Forward non-success upstream responses as-is instead of piping an error
            // body through the SSE translator (which would mangle/empty it).
            if !upstream.status().is_success() {
                let mut response = upstream_response(upstream).await?;
                response
                    .headers_mut()
                    .insert("x-input-tokens", input_tokens_header);
                return Ok(response);
            }
            let status = upstream.status();
            let headers = upstream.headers().clone();
            let model = req.model.clone();
            let mut batcher = ChunkBatcher::new();
            let mut translator = AnthropicStreamTranslator::new(model.clone());
            let metrics = state.rotator.metrics();
            let provider_for_metrics = provider.clone();
            let first_chunk_recorded = Arc::new(AtomicBool::new(false));
            let stream_start = Instant::now();
            let stream = upstream.bytes_stream().map(move |result| {
                if !first_chunk_recorded.swap(true, Ordering::Relaxed) {
                    let latency_ms = stream_start.elapsed().as_millis() as u64;
                    metrics.record_first_chunk_latency(&provider_for_metrics, latency_ms);
                }
                metrics.record_stream_chunk(&provider_for_metrics);
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

        let adapter = state.guardrails.as_ref().unwrap();
        let mut guardrail_body = Arc::clone(&body);
        let mut guardrail_attempts = 0;
        let mut upstream_attempts = 0;
        loop {
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

            let guardrail_request = build_guardrail_request(
                RouteKind::ChatCompletions,
                provider.clone(),
                upstream_path.to_owned(),
                req.model.clone(),
                Arc::clone(&guardrail_body),
                true,
            );
            let guardrail_request = adapter.preprocess_request(&guardrail_request)?;
            let preprocessed_body = Arc::clone(&guardrail_request.body);
            let attempt_upstream_body = if is_anthropic {
                openai_to_anthropic_messages(&preprocessed_body)
            } else {
                (*preprocessed_body).clone()
            };

            let resp = request_chat_upstream(
                &state,
                &provider,
                upstream_path,
                attempt_upstream_body,
                &req.model,
            )
            .await?;
            if !resp.status().is_success() {
                let mut response = upstream_response(resp).await?;
                response
                    .headers_mut()
                    .insert("x-input-tokens", input_tokens_header);
                return Ok(response);
            }
            let status = resp.status();
            let _headers = resp.headers().clone();
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;

            let openai_sse_bytes = if is_anthropic {
                let mut batcher = ChunkBatcher::new();
                let mut translator = AnthropicStreamTranslator::new(req.model.clone());
                let records = batcher.push(bytes);
                let mut output = String::new();
                for record in records {
                    for part in translator.translate_sse_record_to_sse(&record) {
                        output.push_str(&part);
                    }
                }
                output.into_bytes()
            } else if is_responses_compat {
                translate_responses_sse_chunk(&bytes, &req.model).into_bytes()
            } else {
                bytes.to_vec()
            };

            let records = sse_data_records(&openai_sse_bytes);
            let mut frames = Vec::with_capacity(records.len());
            for record in records {
                if record == "[DONE]" {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(&record) {
                    frames.push(value);
                }
            }

            let decision = adapter
                .evaluate_streaming(&guardrail_request, frames)
                .await?;

            match decision {
                GuardrailDecision::Accept { ref response, .. }
                | GuardrailDecision::Repair { ref response, .. } => {
                    let sse_body = guardrails::chat_completion_to_sse_bytes(&response.body);
                    let mut builder = Response::builder().status(status);
                    builder = builder.header(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    builder = builder.header("x-input-tokens", input_tokens_header);
                    if cfg!(debug_assertions) {
                        let trace = match decision {
                            GuardrailDecision::Accept { .. } => "accept",
                            GuardrailDecision::Repair { .. } => "repair",
                            _ => unreachable!(),
                        };
                        builder =
                            builder.header("x-guardrail-trace", HeaderValue::from_static(trace));
                    }
                    return Ok(builder.body(Body::from(sse_body)).unwrap());
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

    if state.guardrails.is_none()
        || !should_enable_guardrails(RouteKind::ChatCompletions, &state.config.guardrails)
    {
        let resp =
            request_chat_upstream(&state, &provider, upstream_path, upstream_body, &req.model)
                .await?;
        let mut response = if provider == "anthropic" {
            if !resp.status().is_success() {
                // Forward upstream errors (412/422/451, etc.) as-is: translating an
                // error body or forcing .json() would corrupt the status/body.
                upstream_response(resp).await?
            } else {
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
            }
        } else if is_responses_compat {
            if !resp.status().is_success() {
                upstream_response(resp).await?
            } else {
                responses_sse_to_json_response(resp, &req.model).await?
            }
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
            if !resp.status().is_success() {
                upstream_response(resp).await?
            } else {
                let (status, headers, response_json) =
                    buffer_chat_response(resp, is_anthropic, &req.model).await?;
                buffered_json_response(status, &headers, response_json)
            }
        } else {
            upstream_response(resp).await?
        };
        response
            .headers_mut()
            .insert("x-input-tokens", input_tokens_header);
        return Ok(response);
    };

    let mut guardrail_body = Arc::clone(&body);
    let mut guardrail_attempts = 0;
    let mut upstream_attempts = 0;
    loop {
        let guardrail_request = build_guardrail_request(
            RouteKind::ChatCompletions,
            provider.clone(),
            upstream_path.to_owned(),
            req.model.clone(),
            Arc::clone(&guardrail_body),
            false,
        );
        let guardrail_request = adapter.preprocess_request(&guardrail_request)?;
        let preprocessed_body = Arc::clone(&guardrail_request.body);
        let attempt_upstream_body = if is_anthropic {
            openai_to_anthropic_messages(&preprocessed_body)
        } else {
            (*preprocessed_body).clone()
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
        if !resp.status().is_success() {
            // Forward upstream errors as-is; buffer_chat_response would force .json()
            // on a possibly non-JSON error body and drop the real status.
            let mut response = upstream_response(resp).await?;
            response
                .headers_mut()
                .insert("x-input-tokens", input_tokens_header);
            return Ok(response);
        }
        let (status, headers, response_json) =
            buffer_chat_response(resp, is_anthropic, &req.model).await?;

        let decision = adapter
            .evaluate_non_streaming(&guardrail_request, &response_json)
            .await?;

        match decision {
            GuardrailDecision::Accept {
                response: accepted, ..
            } => {
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
            GuardrailDecision::Repair {
                response: repaired, ..
            } => {
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
    let mut reasoning_content = String::new();
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
            Some("response.thinking.delta") => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    reasoning_content.push_str(delta);
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
                    if reasoning_content.is_empty() {
                        reasoning_content.push_str(&response_reasoning_text(response));
                    }
                }
                if let Some(value) = event.get("usage") {
                    usage = response_usage_to_chat_usage(value);
                }
            }
            _ => {}
        }
    }

    let mut message = json!({
        "role": "assistant",
        "content": content,
    });
    if !reasoning_content.is_empty() {
        message["reasoning_content"] = Value::String(reasoning_content);
    }

    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
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
            Some("response.thinking.delta") => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                output.push_str(&chat_sse_payload(
                    &event,
                    fallback_model,
                    json!({"reasoning_content": delta}),
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
    let text = String::from_utf8_lossy(bytes);
    let mut records = Vec::new();
    let mut current_data = String::new();

    for line in text.lines() {
        if line.is_empty() {
            if !current_data.is_empty() {
                records.push(std::mem::take(&mut current_data));
            }
        } else if let Some(data) = line.strip_prefix("data: ") {
            if !current_data.is_empty() {
                current_data.push('\n');
            }
            current_data.push_str(data);
        } else if line.starts_with(':') {
            // SSE comment line (e.g. DeepSeek `: keep-alive`): ignore per spec.
            continue;
        }
    }

    if !current_data.is_empty() {
        records.push(current_data);
    }

    records
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

fn response_reasoning_text(response: &Value) -> String {
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
        .filter_map(|content| {
            if content.get("type").and_then(Value::as_str) == Some("thinking") {
                content.get("thinking").and_then(Value::as_str)
            } else {
                None
            }
        })
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

async fn handle_image_only_request(
    state: AppState,
    req: ChatCompletionRequest,
) -> Result<Response, AppError> {
    let prompt = req
        .messages
        .iter()
        .filter_map(|msg| msg.content.as_ref())
        .map(|content| match content {
            models::chat::ChatMessageContent::Text(text) => text.clone(),
            models::chat::ChatMessageContent::Blocks(_) => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut image_body = json!({
        "model": req.model,
        "prompt": prompt,
    });
    if let Some(n) = req.extra.get("n").and_then(Value::as_u64) {
        image_body["n"] = json!(n);
    } else {
        image_body["n"] = json!(1);
    }
    if let Some(size) = req.extra.get("size").and_then(Value::as_str) {
        image_body["size"] = json!(size);
    }
    if let Some(quality) = req.extra.get("quality").and_then(Value::as_str) {
        image_body["quality"] = json!(quality);
    }
    if let Some(style) = req.extra.get("style").and_then(Value::as_str) {
        image_body["style"] = json!(style);
    }
    image_body["stream"] = json!(false);
    crate::routes::images::proxy_image_request(state, "images/generations", image_body).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_data_records_skips_keep_alive_comments() {
        let bytes = b": keep-alive\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n\
            : keep-alive\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"!\"}}]}\n\n\
            data: [DONE]\n\n";

        let records = sse_data_records(bytes);

        assert_eq!(records.len(), 3);
        assert!(records[0].contains("\"Hi\""));
        assert!(records[1].contains("\"!\""));
        assert_eq!(records[2], "[DONE]");
    }

    #[test]
    fn sse_data_records_skips_keep_alive_comments_crlf() {
        let bytes = b": keep-alive\r\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\r\n\r\n\
            : keep-alive\r\n\
            data: [DONE]\r\n\r\n";

        let records = sse_data_records(bytes);

        assert_eq!(records.len(), 2);
        assert!(records[0].contains("\"Hi\""));
        assert_eq!(records[1], "[DONE]");
    }

    #[test]
    fn sse_data_records_ignores_lone_comment_without_data() {
        let bytes = b": keep-alive\n\n: another comment\n\n";

        let records = sse_data_records(bytes);

        assert!(records.is_empty());
    }
}
