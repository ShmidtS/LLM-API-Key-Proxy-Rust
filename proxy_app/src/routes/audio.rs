use crate::errors::AppError;
use crate::routes::utils::{
    content_type, is_json, is_multipart, json_body, normalize_model_in_body,
    resolve_provider_for_model, upstream_response,
};
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use axum::{Router, extract::State, routing::post};
use futures::StreamExt;
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/audio/speech", post(speech))
        .route("/v1/audio/transcriptions", post(transcriptions))
}

async fn speech(State(state): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    let mut req = json_body(body)?;
    let response_format = req
        .as_object_mut()
        .and_then(|object| object.remove("response_format"))
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "mp3".to_owned());
    let model = req
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let provider = req
        .get("model")
        .and_then(Value::as_str)
        .map(|model| resolve_provider_for_model(&state, model))
        .unwrap_or_else(|| "openai".to_owned());
    normalize_model_in_body(&mut req, &provider);
    tracing::info!(
        method = "POST",
        provider = %provider,
        model = %model,
        upstream_path = "audio/speech",
        "forwarding audio speech request"
    );
    let upstream = state
        .rotator
        .request(&provider, "audio/speech", req)
        .await?;
    tracing::info!(
        provider = %provider,
        status = %upstream.status(),
        "upstream audio speech response"
    );
    audio_response(upstream, &response_format).await
}

async fn audio_response(
    upstream: reqwest::Response,
    response_format: &str,
) -> Result<Response, AppError> {
    let status = upstream.status();
    let stream = upstream
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, audio_content_type(response_format))
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition(response_format)?,
        )
        .header("x-accel-buffering", HeaderValue::from_static("no"))
        .body(Body::from_stream(stream))
        .map_err(|e| AppError::Internal(e.to_string()))
}

fn audio_content_type(response_format: &str) -> &'static str {
    match response_format {
        "mp3" => "audio/mpeg",
        "opus" => "audio/opus",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "pcm" => "audio/pcm",
        _ => "audio/mpeg",
    }
}

fn content_disposition(response_format: &str) -> Result<HeaderValue, AppError> {
    HeaderValue::from_str(&format!("attachment; filename=speech.{response_format}"))
        .map_err(|e| AppError::Internal(e.to_string()))
}

async fn transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let Some(content_type) = content_type(&headers) else {
        return Err(AppError::BadRequest("missing content-type".into()));
    };

    let upstream = if is_multipart(content_type) {
        tracing::info!(
            method = "POST",
            provider = "openai",
            upstream_path = "audio/transcriptions",
            "forwarding audio transcriptions request"
        );
        let upstream = state
            .rotator
            .request_raw("openai", "audio/transcriptions", body, content_type)
            .await?;
        tracing::info!(
            provider = "openai",
            status = %upstream.status(),
            "upstream audio transcriptions response"
        );
        upstream
    } else if is_json(content_type) {
        let mut req = json_body(body)?;
        let model = req
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        normalize_model_in_body(&mut req, "openai");
        tracing::info!(
            method = "POST",
            provider = "openai",
            model = %model,
            upstream_path = "audio/transcriptions",
            "forwarding audio transcriptions request"
        );
        let upstream = state
            .rotator
            .request("openai", "audio/transcriptions", req)
            .await?;
        tracing::info!(
            provider = "openai",
            status = %upstream.status(),
            "upstream audio transcriptions response"
        );
        upstream
    } else {
        return Err(AppError::BadRequest(format!(
            "unsupported content-type: {content_type}"
        )));
    };

    upstream_response(upstream).await
}
