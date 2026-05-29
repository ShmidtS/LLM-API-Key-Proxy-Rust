use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use guardrails::{
    ContextCompactionConfig, GuardrailDecision, GuardrailError, GuardrailMode, GuardrailRequest,
    GuardrailsConfig, GuardrailsEngine, RecoveryConfig, RouteGuardrailConfig, RouteKind,
    TokenBudget,
};
use proxy_config::proxy::{GuardrailsProxyConfig, GuardrailsRouteConfig};
use rotator::RotatorClient;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug)]
pub struct BufferedUpstreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: axum::body::Bytes,
}

#[derive(Debug)]
pub struct GuardrailsAdapter {
    rotator: Arc<RotatorClient>,
    config: GuardrailsConfig,
    engine: GuardrailsEngine,
}

impl GuardrailsAdapter {
    pub fn new(rotator: Arc<RotatorClient>, config: GuardrailsConfig) -> Self {
        let engine = GuardrailsEngine::new(config.clone());
        Self {
            rotator,
            config,
            engine,
        }
    }

    pub fn from_proxy_config(rotator: Arc<RotatorClient>, config: &GuardrailsProxyConfig) -> Self {
        Self::new(rotator, guardrails_config_from_proxy(config))
    }

    pub fn config(&self) -> &GuardrailsConfig {
        &self.config
    }

    pub async fn invoke_json(
        &self,
        provider: &str,
        path: &str,
        body: Value,
    ) -> Result<BufferedUpstreamResponse, GuardrailError> {
        validate_adapter_target(provider, path)?;
        let response = self
            .rotator
            .request(provider, path, body)
            .await
            .map_err(|error| GuardrailError::Recovery(error.to_string()))?;
        let status = StatusCode::from_u16(response.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .map_err(|error| GuardrailError::Recovery(error.to_string()))?;

        Ok(BufferedUpstreamResponse {
            status,
            headers,
            body,
        })
    }

    pub async fn evaluate_non_streaming(
        &self,
        request: &GuardrailRequest,
        response_json: &Value,
    ) -> Result<GuardrailDecision, GuardrailError> {
        self.engine.evaluate(request, response_json)
    }

    pub fn preprocess_request(
        &self,
        request: &GuardrailRequest,
    ) -> Result<GuardrailRequest, GuardrailError> {
        self.engine.preprocess(request)
    }
}

pub fn build_guardrail_request(
    route: RouteKind,
    provider: String,
    upstream_path: String,
    model: String,
    body: Value,
    stream: bool,
) -> GuardrailRequest {
    GuardrailRequest {
        route,
        provider,
        upstream_path,
        model,
        body,
        stream,
        schema_hint: None,
        step_policy: None,
    }
}

pub fn should_enable_guardrails(route: RouteKind, config: &GuardrailsProxyConfig) -> bool {
    config.enabled
        || match route {
            RouteKind::ChatCompletions => config.chat.enabled || config.chat.compact_context,
            RouteKind::AnthropicMessages => {
                config.anthropic.enabled || config.anthropic.compact_context
            }
            RouteKind::Responses => config.responses.enabled || config.responses.compact_context,
        }
}

pub fn any_guardrails_enabled(config: &GuardrailsProxyConfig) -> bool {
    config.enabled
        || config.chat.enabled
        || config.anthropic.enabled
        || config.responses.enabled
        || config.chat.compact_context
        || config.anthropic.compact_context
        || config.responses.compact_context
}

pub fn append_nudge_message(body: &mut Value, nudge_message: Value) {
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        messages.push(nudge_message);
    } else {
        body["messages"] = Value::Array(vec![nudge_message]);
    }
}

fn validate_adapter_target(provider: &str, path: &str) -> Result<(), GuardrailError> {
    if !provider
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(GuardrailError::Recovery("invalid provider".into()));
    }
    if path.starts_with("http://") || path.starts_with("https://") || path.contains("..") {
        return Err(GuardrailError::Recovery("invalid upstream path".into()));
    }
    match path.trim_start_matches('/') {
        "chat/completions" | "messages" | "responses" => Ok(()),
        _ => Err(GuardrailError::Recovery("invalid upstream path".into())),
    }
}

pub fn decision_to_error_response(decision: &GuardrailDecision) -> Option<Response> {
    match decision {
        GuardrailDecision::Reject { client_error } => {
            tracing::warn!(reason = %client_error, "guardrail rejected response");
            Some(
                crate::errors::invalid_request_error("Response failed guardrail validation")
                    .into_response(),
            )
        }
        GuardrailDecision::Abort { internal_error } => {
            tracing::error!(reason = %internal_error, "guardrail aborted response processing");
            Some(crate::errors::api_error("Guardrail processing failed").into_response())
        }
        _ => None,
    }
}

pub fn buffered_json_response(status: StatusCode, headers: &HeaderMap, body: Value) -> Response {
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    } else {
        builder = builder.header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

pub fn buffered_bytes_response(response: BufferedUpstreamResponse) -> Response {
    let mut builder = Response::builder().status(response.status);
    if let Some(content_type) = response.headers.get(header::CONTENT_TYPE) {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder.body(Body::from(response.body)).unwrap()
}

pub fn guardrails_config_from_proxy(config: &GuardrailsProxyConfig) -> GuardrailsConfig {
    GuardrailsConfig {
        chat_completions: route_config_from_proxy(&config.chat, config),
        anthropic_messages: route_config_from_proxy(&config.anthropic, config),
        responses: route_config_from_proxy(&config.responses, config),
        max_rescue_attempts: config.max_rescue_attempts as u32,
        max_guardrail_retries: config.max_guardrail_retries.min(1) as u32,
        context_compaction: ContextCompactionConfig {
            enabled: config.chat.compact_context
                || config.anthropic.compact_context
                || config.responses.compact_context,
            token_budget: TokenBudget {
                max_context_tokens: config.context_compaction.max_context_messages,
                compact_above_ratio: config.context_compaction.compact_above_ratio as f32,
                ..TokenBudget::default()
            },
            ..ContextCompactionConfig::default()
        },
        recovery: RecoveryConfig {
            enabled: config.chat.recover_errors
                || config.anthropic.recover_errors
                || config.responses.recover_errors,
            ..RecoveryConfig::default()
        },
    }
}

fn route_config_from_proxy(
    route: &GuardrailsRouteConfig,
    proxy: &GuardrailsProxyConfig,
) -> RouteGuardrailConfig {
    RouteGuardrailConfig {
        mode: guardrail_mode(&proxy.mode, proxy.enabled || route.enabled),
        validate_tool_calls: route.validate_tools,
        validate_json_mode: route.validate_json,
        validate_schema: route.validate_json,
        validate_steps: route.enforce_steps,
        rescue_tool_calls: route.recover_errors,
        retry_with_nudge: route.recover_errors,
    }
}

fn guardrail_mode(mode: &str, enabled: bool) -> GuardrailMode {
    if !enabled {
        return GuardrailMode::Off;
    }

    match mode.to_ascii_lowercase().as_str() {
        "observe" => GuardrailMode::Observe,
        "enforce" | "rescue" => GuardrailMode::Enforce,
        _ => GuardrailMode::Off,
    }
}
