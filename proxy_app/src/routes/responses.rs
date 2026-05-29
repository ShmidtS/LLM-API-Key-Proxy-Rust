use crate::errors::AppError;
use crate::routes::utils::{
    normalize_model_in_body, resolve_provider_for_model, upstream_response,
};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get, routing::post};
use models::chat::ChatCompletionResponse;
use models::responses::CreateResponseRequest;
use rotator::{ResponsesBridge, ResponsesBridgeError};
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/responses", post(create_response))
        .route("/responses/{response_id}", get(get_response))
        .route("/v1/responses", post(create_response))
        .route("/v1/responses/{response_id}", get(get_response))
}

async fn get_response(
    State(state): State<AppState>,
    Path(response_id): Path<String>,
    OriginalUri(_uri): OriginalUri,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let query_vec = params.into_iter().collect::<Vec<_>>();
    let path = format!("responses/{response_id}");
    let upstream = state
        .rotator
        .get_with_query("openai", &path, &query_vec)
        .await?;
    upstream_response(upstream).await
}

async fn create_response(
    State(state): State<AppState>,
    Json(original_request): Json<CreateResponseRequest>,
) -> Result<Response, AppError> {
    if !state.registry.is_model_allowed(&original_request.model) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Model not allowed"})),
        )
            .into_response());
    }

    let bridge = ResponsesBridge::default();
    let translated = bridge
        .translate_request(original_request)
        .map_err(responses_bridge_error_to_app_error)?;
    let provider = resolve_provider_for_model(&state, &translated.chat_request.model);
    let mut upstream_body = serde_json::to_value(&translated.chat_request)?;
    normalize_model_in_body(&mut upstream_body, &provider);
    tracing::info!(
        method = "POST",
        provider = %provider,
        model = %translated.chat_request.model,
        upstream_path = %translated.upstream_path,
        "forwarding responses request"
    );
    let upstream = state
        .rotator
        .request(&provider, &translated.upstream_path, upstream_body)
        .await?;
    tracing::info!(
        provider = %provider,
        status = %upstream.status(),
        "upstream responses response"
    );

    if translated.context.stream {
        let status = upstream.status();
        let stream = bridge.translate_stream(upstream.bytes_stream(), translated.context);
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from_stream(stream))
            .map_err(|e| AppError::Internal(e.to_string()));
    }

    let status = upstream.status();
    if !status.is_success() {
        return upstream_response(upstream).await;
    }
    let chat: ChatCompletionResponse = upstream
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    let response = bridge
        .translate_response(chat, &translated.context)
        .map_err(responses_bridge_error_to_app_error)?;
    Ok((
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK),
        Json(response),
    )
        .into_response())
}

fn responses_bridge_error_to_app_error(error: ResponsesBridgeError) -> AppError {
    match error {
        ResponsesBridgeError::UnsupportedToolType { tool_type } => {
            AppError::BadRequest(format!("Unsupported tool type: {tool_type}"))
        }
        ResponsesBridgeError::MissingFunctionDefinition => {
            AppError::BadRequest("Function tool missing function definition".to_owned())
        }
        ResponsesBridgeError::UnsupportedInputPart { part_type } => {
            AppError::BadRequest(format!("Unsupported input part type: {part_type}"))
        }
        ResponsesBridgeError::InvalidToolChoice { reason } => AppError::BadRequest(reason),
        ResponsesBridgeError::Serialization(message) => AppError::BadRequest(message),
    }
}
