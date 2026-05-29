use crate::errors::{AppError, invalid_request_error};
use crate::routes::utils::{resolve_provider_for_model, upstream_response};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{OriginalUri, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tools/web_search", post(tool_web_search))
        .route("/tools/tokenizer", post(tools_post_passthrough))
        .route("/tools/layout", post(tools_post_passthrough))
        .route("/tools/web_reader", post(tools_post_passthrough))
        .route("/v1/tools/web-search", post(tool_web_search))
        .route("/v1/tools/tokenizer", post(tools_post_passthrough))
        .route("/v1/tools/layout-parsing", post(tools_post_passthrough))
        .route("/v1/tools/web-reader", post(tools_post_passthrough))
        .route(
            "/v1/threads",
            post(tools_post_passthrough).get(tools_get_passthrough),
        )
        .route(
            "/v1/threads/{*path}",
            post(tools_post_passthrough).get(tools_get_passthrough),
        )
}

async fn tools_post_passthrough(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let Some(method) = tool_method(uri.path()) else {
        let upstream = state
            .rotator
            .request("openai", upstream_path(uri.path()), req)
            .await?;
        return upstream_response(upstream).await;
    };
    let upstream = state
        .rotator
        .provider_method_call("zai", method, req)
        .await?;
    upstream_response(upstream).await
}

async fn tool_web_search(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Response, AppError> {
    let Some(query) = req
        .get("query")
        .or_else(|| req.get("input"))
        .and_then(Value::as_str)
        .filter(|query| !query.trim().is_empty())
    else {
        return Ok(invalid_request_error("Missing required field: query or input").into_response());
    };

    let model = req
        .get("model")
        .cloned()
        .unwrap_or_else(|| Value::String("glm-4.5".to_owned()));
    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": query}],
        "tools": [{"type": "function", "function": {"name": "web_search", "parameters": {}}}]
    });
    for field in ["stream", "temperature", "max_tokens", "top_p"] {
        if let Some(value) = req.get(field) {
            body[field] = value.clone();
        }
    }

    let provider = body
        .get("model")
        .and_then(Value::as_str)
        .map(|model| resolve_provider_for_model(&state, model))
        .unwrap_or_else(|| "openai".to_owned());
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let upstream = state
        .rotator
        .request(&provider, "chat/completions", body)
        .await?;
    if stream {
        let status = upstream.status();
        let headers = upstream.headers().clone();
        let stream = upstream
            .bytes_stream()
            .map(|result| result.map_err(std::io::Error::other));
        let mut builder = Response::builder().status(status);
        if let Some(ct) = headers.get(header::CONTENT_TYPE) {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        return builder
            .body(Body::from_stream(stream))
            .map_err(|e| AppError::Internal(e.to_string()));
    }
    upstream_response(upstream).await
}

async fn tools_get_passthrough(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let query_vec = params.into_iter().collect::<Vec<_>>();
    let upstream = state
        .rotator
        .get_with_query("openai", upstream_path(uri.path()), &query_vec)
        .await?;
    upstream_response(upstream).await
}

fn tool_method(path: &str) -> Option<&'static str> {
    match path {
        "/tools/tokenizer" | "/v1/tools/tokenizer" => Some("tool_tokenizer"),
        "/tools/layout" | "/v1/tools/layout-parsing" => Some("tool_layout_parsing"),
        "/tools/web_reader" | "/v1/tools/web-reader" => Some("tool_web_reader"),
        _ => None,
    }
}

fn upstream_path(path: &str) -> &str {
    path.strip_prefix("/v1/")
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path)
}
