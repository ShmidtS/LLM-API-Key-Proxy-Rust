use crate::compat::anthropic::{anthropic_to_openai_response, openai_to_anthropic_messages};
use crate::compat::anthropic_streaming::{AnthropicStreamTranslator, ChunkBatcher};
use crate::errors::AppError;
use crate::guardrails_adapter::{
    append_nudge_message, buffered_json_response, build_guardrail_request,
    decision_to_error_response, should_enable_guardrails,
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
use serde_json::Value;

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
        let upstream = request_chat_upstream(
            &state,
            &provider,
            upstream_path,
            upstream_body,
            &req.model,
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

    if state.guardrails.is_none()
        || !should_enable_guardrails(RouteKind::ChatCompletions, &state.config.guardrails)
    {
        let resp = request_chat_upstream(
            &state,
            &provider,
            upstream_path,
            upstream_body,
            &req.model,
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
        return Ok(response);
    }

    let Some(adapter) = state.guardrails.as_ref() else {
        let resp = request_chat_upstream(
            &state,
            &provider,
            upstream_path,
            upstream_body,
            &req.model,
        )
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
            GuardrailDecision::Accept => {
                let mut response = buffered_json_response(status, &headers, response_json);
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
            GuardrailDecision::CompactAndRetry { compacted_body, .. } => {
                let mut response = buffered_json_response(status, &headers, compacted_body);
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
            GuardrailDecision::RetryWithNudge { nudge_message, .. } => {
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
                guardrail_body = preprocessed_body;
                append_nudge_message(&mut guardrail_body, nudge_message);
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
