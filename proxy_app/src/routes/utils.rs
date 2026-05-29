use crate::errors::AppError;
use crate::state::AppState;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use serde_json::Value;

pub async fn upstream_response(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = upstream.headers().clone();
    let bytes = upstream
        .bytes()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    Ok(builder.body(Body::from(bytes)).unwrap())
}

pub fn content_type(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
}

pub fn is_multipart(content_type: &str) -> bool {
    content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
}

pub fn is_json(content_type: &str) -> bool {
    content_type
        .to_ascii_lowercase()
        .starts_with("application/json")
}

pub fn json_body(body: axum::body::Bytes) -> Result<Value, AppError> {
    if body.is_empty() {
        return Ok(serde_json::json!({}));
    }
    Ok(serde_json::from_slice(&body)?)
}

pub fn resolve_provider_for_model(state: &AppState, model: &str) -> String {
    state
        .registry
        .resolve_provider_by_model(model)
        .map(ToOwned::to_owned)
        .or_else(|| state.registry.find_provider_for_model(model))
        .unwrap_or_else(|| "openai".to_owned())
}

pub fn strip_provider_prefix(model: &str, provider: &str) -> String {
    let prefix = format!("{provider}/");
    model.strip_prefix(&prefix).unwrap_or(model).to_owned()
}

pub fn normalize_model_in_body(body: &mut Value, provider: &str) {
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return;
    };
    let normalized = strip_provider_prefix(model, provider);
    if provider != "openai" && normalized != model {
        body["model"] = Value::String(normalized);
    }
}

#[cfg(test)]
mod tests {
    use super::strip_provider_prefix;

    #[test]
    fn strip_provider_prefix_removes_matching_provider_prefix() {
        assert_eq!(strip_provider_prefix("openai/gpt-5.5", "openai"), "gpt-5.5");
        assert_eq!(
            strip_provider_prefix("anthropic/claude-3-5-sonnet", "anthropic"),
            "claude-3-5-sonnet"
        );
        assert_eq!(strip_provider_prefix("zai/glm-5.1", "zai"), "glm-5.1");
    }

    #[test]
    fn strip_provider_prefix_keeps_non_matching_model() {
        assert_eq!(strip_provider_prefix("gpt-5.5", "openai"), "gpt-5.5");
        assert_eq!(
            strip_provider_prefix("anthropic/claude-3-5-sonnet", "openai"),
            "anthropic/claude-3-5-sonnet"
        );
    }
}
